//! Background export: software-composited video frames are piped to FFmpeg
//! (H.264 + AAC in MP4) together with a separately mixed audio track.
use super::compositor::{compose, LayerCache};
use super::source::StreamProvider;
use crate::audio;
use crate::error::{EngineError, Result};
use crate::platform::{self, ffmpeg_cmd};
use crate::project::io::{missing_media, PROJECT_EXT};
use crate::project::model::*;
use crate::time::{frame_to_us, to_secs};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub out_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// 0..=100
    pub quality: u8,
    pub include_audio: bool,
    /// Caller has already confirmed replacing an existing file.
    pub overwrite: bool,
    /// Try a hardware H.264 encoder (falls back to software automatically).
    pub use_hardware: bool,
    /// Folder of the project file, for resolving relative media paths.
    pub project_dir: Option<PathBuf>,
}

impl ExportOptions {
    pub fn from_project(p: &Project, out_path: PathBuf, project_dir: Option<PathBuf>) -> ExportOptions {
        ExportOptions {
            out_path,
            width: p.export.width,
            height: p.export.height,
            fps: p.export.fps,
            quality: p.export.quality,
            include_audio: p.export.include_audio,
            overwrite: false,
            use_hardware: false,
            project_dir,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub fraction: f32,
    pub status: String,
    pub elapsed_secs: f32,
    pub eta_secs: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct ExportReport {
    pub path: PathBuf,
    pub duration_s: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
    pub encoder: String,
    pub wall_secs: f32,
    /// Mismatches found when the finished file was inspected. Empty = verified.
    pub warnings: Vec<String>,
}

pub struct ExportHandle {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    join: Option<JoinHandle<Result<ExportReport>>>,
}
impl ExportHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn progress(&self) -> Progress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().map_or(true, |j| j.is_finished())
    }
    pub fn wait(mut self) -> Result<ExportReport> {
        self.join.take().map(|j| j.join().unwrap_or_else(|_| Err(EngineError::Export("the export worker crashed".into())))).unwrap_or(Err(EngineError::Cancelled))
    }
}

pub fn crf_for_quality(q: u8) -> u32 {
    (35.0 - q.min(100) as f32 * 0.17).round() as u32
}

fn bitrate_for(w: u32, h: u32, fps: f64, q: u8) -> u64 {
    let bpp = 0.04 + q.min(100) as f64 / 100.0 * 0.16;
    (w as f64 * h as f64 * fps * bpp) as u64
}

/// Everything that can be checked before any work is done.
pub fn preflight(p: &Project, o: &ExportOptions) -> Result<()> {
    let dur = p.duration();
    if dur <= 0 {
        return Err(EngineError::Export("the timeline is empty — add something to export".into()));
    }
    if o.width < 16 || o.height < 16 || o.width > 8192 || o.height > 8192 {
        return Err(EngineError::Export("output size must be between 16 and 8192 pixels".into()));
    }
    if !(1.0..=240.0).contains(&o.fps) {
        return Err(EngineError::Export("frame rate must be between 1 and 240".into()));
    }
    let gone = missing_media(p, o.project_dir.as_deref());
    if !gone.is_empty() {
        let names: Vec<String> = gone
            .iter()
            .filter_map(|id| p.media(*id))
            .map(|m| Path::new(&m.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        return Err(EngineError::Export(format!("these media files are missing: {}. Relink them first.", names.join(", "))));
    }
    let ext = o.out_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == PROJECT_EXT {
        return Err(EngineError::Export("the output must be a video file (.mp4), not a project file".into()));
    }
    if ext != "mp4" {
        return Err(EngineError::Export("the output file name must end in .mp4".into()));
    }
    let dir = o.out_path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !dir.is_dir() {
        return Err(EngineError::Export(format!("the output folder doesn't exist: {}", dir.display())));
    }
    if o.out_path.exists() && !o.overwrite {
        return Err(EngineError::OutputExists(o.out_path.display().to_string()));
    }
    let secs = to_secs(dur);
    let video = secs * bitrate_for(o.width, o.height, o.fps, o.quality) as f64 / 8.0;
    let audio_tmp = if o.include_audio { secs * audio::SAMPLE_RATE as f64 * 4.0 } else { 0.0 };
    let need = ((video * 1.3 + audio_tmp) as u64).max(1 << 20);
    if let Some(have) = platform::free_space_bytes(dir) {
        if have < need {
            return Err(EngineError::DiskSpace { need_mb: need >> 20, have_mb: have >> 20 });
        }
    }
    // Probe writability up front so we fail before rendering.
    let probe = dir.join(format!(".lm-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"x").map_err(|e| EngineError::Export(format!("can't write to the output folder ({e})")))?;
    let _ = std::fs::remove_file(&probe);
    let enc = platform::encoders()?;
    if !enc.libx264 && !enc.openh264 && enc.hardware.is_empty() {
        return Err(EngineError::Export("this FFmpeg build has no H.264 encoder".into()));
    }
    if o.include_audio && !enc.aac {
        return Err(EngineError::Export("this FFmpeg build has no AAC encoder".into()));
    }
    Ok(())
}

pub fn start_export(project: Project, opts: ExportOptions) -> Result<ExportHandle> {
    let mut opts = opts;
    opts.width &= !1; // H.264 4:2:0 needs even dimensions
    opts.height &= !1;
    preflight(&project, &opts)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(Progress { status: "Starting…".into(), ..Default::default() }));
    let (c2, p2) = (cancel.clone(), progress.clone());
    let join = std::thread::Builder::new()
        .name("export".into())
        .spawn(move || run_export(&project, &opts, &c2, &p2))
        .map_err(|e| EngineError::Export(e.to_string()))?;
    Ok(ExportHandle { cancel, progress, join: Some(join) })
}

/// Convenience for CLI/tests: run to completion on this thread's behalf.
pub fn export_blocking(project: &Project, opts: &ExportOptions) -> Result<ExportReport> {
    start_export(project.clone(), opts.clone())?.wait()
}

struct Cleanup(Vec<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn set(p: &Arc<Mutex<Progress>>, start: Instant, fraction: f32, status: &str) {
    if let Ok(mut g) = p.lock() {
        let el = start.elapsed().as_secs_f32();
        g.fraction = fraction.clamp(0.0, 1.0);
        g.status = status.to_string();
        g.elapsed_secs = el;
        g.eta_secs = if fraction > 0.03 { Some(el / fraction * (1.0 - fraction)) } else { None };
    }
}

fn run_export(project: &Project, o: &ExportOptions, cancel: &AtomicBool, prog: &Arc<Mutex<Progress>>) -> Result<ExportReport> {
    let start = Instant::now();
    let frames = (project.duration() as f64 * o.fps / 1e6).ceil() as i64;
    let video_us = frame_to_us(frames, o.fps);
    let dir = o.out_path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let stem = o.out_path.file_name().unwrap().to_string_lossy().into_owned();
    let part = dir.join(format!(".{stem}.part"));
    let audio_tmp = dir.join(format!(".{stem}.audio.tmp"));
    let mut cleanup = Cleanup(vec![part.clone(), audio_tmp.clone()]);

    // ---- pass 1: audio
    let mut has_audio = false;
    if o.include_audio && !audio::audible_clips(project).is_empty() {
        set(prog, start, 0.0, "Mixing audio…");
        let n = audio::mix_to_file(project, o.project_dir.as_deref(), &audio_tmp, video_us, cancel, &mut |f| set(prog, start, f * 0.08, "Mixing audio…"))?;
        has_audio = n > 0;
    }

    // ---- choose encoder
    let enc = platform::encoders()?;
    let hw = if o.use_hardware { enc.hardware.first().cloned() } else { None };
    let (vcodec, mut vargs): (String, Vec<String>) = if let Some(h) = hw {
        (h.clone(), vec!["-c:v".into(), h, "-b:v".into(), bitrate_for(o.width, o.height, o.fps, o.quality).to_string()])
    } else if enc.libx264 {
        ("libx264".into(), vec!["-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-crf".into(), crf_for_quality(o.quality).to_string()])
    } else if enc.openh264 {
        ("libopenh264".into(), vec!["-c:v".into(), "libopenh264".into(), "-b:v".into(), bitrate_for(o.width, o.height, o.fps, o.quality).to_string()])
    } else {
        let h = enc.hardware[0].clone();
        (h.clone(), vec!["-c:v".into(), h, "-b:v".into(), bitrate_for(o.width, o.height, o.fps, o.quality).to_string()])
    };
    vargs.extend(["-pix_fmt".into(), "yuv420p".into()]);

    // ---- pass 2: video
    let mut cmd = ffmpeg_cmd()?;
    cmd.args(["-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{}x{}", o.width, o.height), "-r", &format!("{}", o.fps), "-i", "pipe:0"]);
    if has_audio {
        cmd.args(["-f", "s16le", "-ar", &audio::SAMPLE_RATE.to_string(), "-ac", "2", "-i"]).arg(&audio_tmp);
    }
    cmd.args(&vargs);
    if has_audio {
        cmd.args(["-c:a", "aac", "-b:a", "192k"]);
    }
    cmd.args(["-movflags", "+faststart", "-f", "mp4"]).arg(&part);
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| EngineError::NoFfmpeg(e.to_string()))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let mut provider = StreamProvider::new(o.fps, o.project_dir.as_deref(), false);
    let mut cache = LayerCache::default();
    let base = if has_audio { 0.08 } else { 0.0 };
    let mut write_err = None;
    for f in 0..frames {
        if cancel.load(Ordering::Relaxed) {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Cancelled);
        }
        let t = frame_to_us(f, o.fps);
        let active: Vec<u64> = project.tracks.iter().flat_map(|tr| tr.clips.iter()).filter(|c| c.start <= t && t < c.end()).map(|c| c.id).collect();
        provider.retain_active(&active);
        let canvas = compose(project, o.width, o.height, t, &mut provider, &mut cache);
        if let Err(e) = stdin.write_all(&canvas.data) {
            write_err = Some(e);
            break;
        }
        if f % 4 == 0 {
            set(prog, start, base + (1.0 - base) * 0.98 * (f as f32 / frames as f32), "Rendering video…");
        }
    }
    drop(stdin);
    set(prog, start, 0.98, "Finishing file…");
    let status = child.wait()?;
    let err_text = err_thread.join().unwrap_or_default();
    if write_err.is_some() || !status.success() {
        let msg = err_text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("the encoder stopped unexpectedly");
        return Err(EngineError::Export(format!("{vcodec} failed: {msg}")));
    }
    if !provider.errors.is_empty() {
        // Decoder problems mean frames are missing; refuse to call it a success.
        return Err(EngineError::Export(format!("some media couldn't be decoded: {}", provider.errors.join("; "))));
    }
    // Finalise only after success so a partial file never looks complete.
    if o.out_path.exists() {
        std::fs::remove_file(&o.out_path).map_err(|e| EngineError::Export(format!("couldn't replace the existing file ({e})")))?;
    }
    std::fs::rename(&part, &o.out_path)?;
    cleanup.0.retain(|p| p != &part);
    drop(cleanup);

    set(prog, start, 1.0, "Checking the result…");
    let mut report = verify(&o.out_path, o, video_us, has_audio)?;
    report.encoder = vcodec;
    report.wall_secs = start.elapsed().as_secs_f32();
    set(prog, start, 1.0, "Done");
    Ok(report)
}

/// Inspect the finished file and compare it with what we meant to produce.
pub fn verify(path: &Path, o: &ExportOptions, expect_us: i64, expect_audio: bool) -> Result<ExportReport> {
    let ffprobe = platform::ffprobe()?;
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-protocol_whitelist", "file"])
        .arg(path)
        .output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| EngineError::Export("the exported file can't be read back".into()))?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let vs = streams.iter().find(|s| s["codec_type"] == "video").ok_or_else(|| EngineError::Export("the exported file has no video".into()))?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let dur: f64 = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    let (w, h) = (vs["width"].as_u64().unwrap_or(0) as u32, vs["height"].as_u64().unwrap_or(0) as u32);
    let rate = vs["avg_frame_rate"].as_str().unwrap_or("0/1");
    let mut it = rate.split('/');
    let (n, d): (f64, f64) = (it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0), it.next().and_then(|x| x.parse().ok()).unwrap_or(1.0));
    let fps = if d > 0.0 { n / d } else { 0.0 };
    let mut warnings = vec![];
    if (dur - to_secs(expect_us)).abs() > 0.15 + 1.0 / o.fps {
        warnings.push(format!("length is {dur:.2}s, expected {:.2}s", to_secs(expect_us)));
    }
    if (w, h) != (o.width, o.height) {
        warnings.push(format!("size is {w}x{h}, expected {}x{}", o.width, o.height));
    }
    if (fps - o.fps).abs() > 0.05 {
        warnings.push(format!("frame rate is {fps:.2}, expected {}", o.fps));
    }
    if expect_audio && !has_audio {
        warnings.push("the file has no audio track".into());
    }
    Ok(ExportReport { path: path.to_path_buf(), duration_s: dur, width: w, height: h, fps, has_audio, encoder: String::new(), wall_secs: 0.0, warnings })
}
