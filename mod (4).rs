//! Media inspection, thumbnails, waveforms, proxies and relinking.
//! Uses ffprobe/ffmpeg as sidecar processes; files are streamed, never loaded
//! whole into memory.
use crate::error::{EngineError, Result};
use crate::platform::{ffmpeg_cmd, ffprobe, input_args};
use crate::project::model::*;
use crate::time::{secs, Us};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const VIDEO_EXTS: &[&str] = &["mp4", "mov", "webm", "mkv", "m4v", "avi"];
pub const AUDIO_EXTS: &[&str] = &["wav", "mp3", "aac", "flac", "m4a", "ogg"];
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];
const MAX_MEDIA_BYTES: u64 = 200 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub duration_us: Us,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
    pub audio_channels: u32,
    pub size_bytes: u64,
    pub video_codec: String,
}

fn ext(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn parse_rate(s: &str) -> f64 {
    let mut it = s.split('/');
    let n: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let d: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1.0);
    if d > 0.0 {
        n / d
    } else {
        0.0
    }
}

/// Run a command with a timeout, returning stdout.
fn run_with_timeout(mut cmd: Command, limit: Duration) -> Result<Vec<u8>> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| EngineError::NoFfmpeg(e.to_string()))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = err.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(st) => {
                let o = t_out.join().unwrap_or_default();
                let e = t_err.join().unwrap_or_default();
                return if st.success() { Ok(o) } else { Err(EngineError::Media(String::from_utf8_lossy(&e).trim().to_string())) };
            }
            None if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(EngineError::Media("the file took too long to inspect".into()));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Inspect a file. Errors are phrased for end users.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    let md = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => EngineError::Missing(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => EngineError::Io(format!("no permission to read {}", path.display())),
        _ => EngineError::Io(e.to_string()),
    })?;
    if !md.is_file() {
        return Err(EngineError::Media("that isn't a file".into()));
    }
    if md.len() == 0 {
        return Err(EngineError::Media("the file is empty".into()));
    }
    if md.len() > MAX_MEDIA_BYTES {
        return Err(EngineError::Media("the file is too large".into()));
    }
    let e = ext(path);
    if ![VIDEO_EXTS, AUDIO_EXTS, IMAGE_EXTS].iter().any(|l| l.contains(&e.as_str())) {
        return Err(EngineError::Media(format!("'.{e}' files aren't supported")));
    }
    let path = crate::platform::simplify_path(path.canonicalize()?);
    let mut cmd = Command::new(ffprobe()?);
    cmd.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-protocol_whitelist", "file"]).arg(&path);
    crate::platform::hide_console(&mut cmd);
    let out = run_with_timeout(cmd, Duration::from_secs(30)).map_err(|e| match e {
        EngineError::Media(m) if !m.is_empty() => EngineError::Media(format!("it may be damaged ({})", m.lines().next().unwrap_or(""))),
        EngineError::Media(_) => EngineError::Media("it may be damaged or in an unsupported format".into()),
        o => o,
    })?;
    let v: Value = serde_json::from_slice(&out).map_err(|_| EngineError::Media("couldn't read file details".into()))?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let is_pic = |s: &Value| s["disposition"]["attached_pic"].as_i64() == Some(1);
    let vs = streams.iter().find(|s| s["codec_type"] == "video" && !is_pic(s));
    let au = streams.iter().find(|s| s["codec_type"] == "audio");
    let fdur = v["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok());
    let is_image_ext = IMAGE_EXTS.contains(&e.as_str());

    if let (true, Some(vs)) = (is_image_ext, streams.iter().find(|s| s["codec_type"] == "video")) {
        let (w, h) = (vs["width"].as_u64().unwrap_or(0) as u32, vs["height"].as_u64().unwrap_or(0) as u32);
        if w == 0 || h == 0 {
            return Err(EngineError::Media("the image has no size".into()));
        }
        return Ok(MediaInfo {
            kind: MediaKind::Image,
            duration_us: 5_000_000,
            width: w,
            height: h,
            fps: 0.0,
            has_audio: false,
            audio_channels: 0,
            size_bytes: md.len(),
            video_codec: vs["codec_name"].as_str().unwrap_or("").into(),
        });
    }
    if is_image_ext {
        return Err(EngineError::Media("no picture found in this image".into()));
    }
    if let Some(vs) = vs {
        let (w, h) = (vs["width"].as_u64().unwrap_or(0) as u32, vs["height"].as_u64().unwrap_or(0) as u32);
        if w == 0 || h == 0 {
            return Err(EngineError::Media("the video has no picture size".into()));
        }
        let dur = vs["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).or(fdur).unwrap_or(0.0);
        if dur <= 0.0 {
            return Err(EngineError::Media("couldn't determine the video's length".into()));
        }
        let mut fps = parse_rate(vs["avg_frame_rate"].as_str().unwrap_or("0/1"));
        if fps <= 0.0 {
            fps = parse_rate(vs["r_frame_rate"].as_str().unwrap_or("0/1"));
        }
        return Ok(MediaInfo {
            kind: MediaKind::Video,
            duration_us: secs(dur),
            width: w,
            height: h,
            fps: if fps > 0.0 { fps } else { 30.0 },
            has_audio: au.is_some(),
            audio_channels: au.and_then(|a| a["channels"].as_u64()).unwrap_or(0) as u32,
            size_bytes: md.len(),
            video_codec: vs["codec_name"].as_str().unwrap_or("").into(),
        });
    }
    if au.is_some() {
        let dur = au.and_then(|a| a["duration"].as_str().and_then(|d| d.parse::<f64>().ok())).or(fdur).unwrap_or(0.0);
        if dur <= 0.0 {
            return Err(EngineError::Media("couldn't determine the audio length".into()));
        }
        return Ok(MediaInfo { kind: MediaKind::Audio, duration_us: secs(dur), width: 0, height: 0, fps: 0.0, has_audio: true, audio_channels: au.and_then(|a| a["channels"].as_u64()).unwrap_or(0) as u32, size_bytes: md.len(), video_codec: String::new() });
    }
    Err(EngineError::Media("no video, audio or picture found in this file".into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    /// Keep a reference to the original file (default; nothing is copied).
    Reference,
    /// Copy into the project's assets folder to make it self-contained.
    Copy,
}

/// Import a file into the project's media list. For `Copy`, `assets_dir` is
/// the folder that receives the copy. Returns the new media id.
pub fn import(project: &mut Project, path: &Path, mode: ImportMode, project_dir: Option<&Path>, assets_dir: Option<&Path>) -> Result<u64> {
    let path = crate::platform::simplify_path(path.canonicalize().map_err(|_| EngineError::Missing(path.display().to_string()))?);
    let info = probe(&path)?;
    let mut stored = path.clone();
    if mode == ImportMode::Copy {
        let dir = assets_dir.ok_or_else(|| EngineError::Invalid("save the project first to copy media into it".into()))?;
        std::fs::create_dir_all(dir)?;
        let name = path.file_name().unwrap();
        let mut dest = dir.join(name);
        let mut n = 1;
        while dest.exists() {
            dest = dir.join(format!("{}-{n}.{}", path.file_stem().unwrap().to_string_lossy(), ext(&path)));
            n += 1;
        }
        std::fs::copy(&path, &dest)?;
        stored = crate::platform::simplify_path(dest.canonicalize()?);
    }
    // Re-use an existing entry for the same file.
    if let Some(m) = project.media.iter().find(|m| Path::new(&m.path) == stored) {
        return Ok(m.id);
    }
    let id = project.alloc_id();
    project.media.push(MediaRef {
        id,
        kind: info.kind,
        rel_path: project_dir.and_then(|d| crate::project::io::relative_to(d, &stored)),
        path: stored.to_string_lossy().into_owned(),
        duration_us: info.duration_us,
        width: info.width,
        height: info.height,
        fps: info.fps,
        has_audio: info.has_audio,
        audio_channels: info.audio_channels,
        size_bytes: info.size_bytes,
        proxy_path: None,
    });
    Ok(id)
}

/// Point a missing media entry at a new file after checking that it matches
/// the original kind.
pub fn relink(project: &mut Project, media_id: u64, new_path: &Path, project_dir: Option<&Path>) -> Result<()> {
    let info = probe(new_path)?;
    let new_path = crate::platform::simplify_path(new_path.canonicalize()?);
    let m = project.media.iter_mut().find(|m| m.id == media_id).ok_or_else(|| EngineError::Invalid("unknown media".into()))?;
    if info.kind != m.kind {
        return Err(EngineError::Media("that file is a different kind of media".into()));
    }
    m.path = new_path.to_string_lossy().into_owned();
    m.rel_path = project_dir.and_then(|d| crate::project::io::relative_to(d, &new_path));
    m.duration_us = info.duration_us;
    m.width = info.width;
    m.height = info.height;
    m.has_audio = info.has_audio;
    m.audio_channels = info.audio_channels;
    m.size_bytes = info.size_bytes;
    Ok(())
}

/// Look for files by name in `folder` (recursively, bounded) and relink every
/// missing entry found. Returns how many were fixed.
pub fn relink_in_folder(project: &mut Project, folder: &Path, project_dir: Option<&Path>) -> usize {
    let missing = crate::project::io::missing_media(project, project_dir);
    let mut fixed = 0;
    for id in missing {
        let Some(name) = project.media(id).and_then(|m| Path::new(&m.path).file_name().map(|n| n.to_owned())) else { continue };
        if let Some(found) = find_file(folder, &name, 4, &mut 5000) {
            if relink(project, id, &found, project_dir).is_ok() {
                fixed += 1;
            }
        }
    }
    fixed
}

fn find_file(dir: &Path, name: &std::ffi::OsStr, depth: u32, budget: &mut i32) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = vec![];
    for e in rd.flatten() {
        *budget -= 1;
        if *budget <= 0 {
            return None;
        }
        let p = e.path();
        if p.file_name() == Some(name) && p.is_file() {
            return Some(p);
        }
        if depth > 0 && p.is_dir() {
            subdirs.push(p);
        }
    }
    subdirs.into_iter().find_map(|d| find_file(&d, name, depth - 1, budget))
}

// --------------------------------------------------------------- thumbnails

#[derive(Clone)]
pub struct Thumb {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

fn fnv(data: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Generate (or load from the bounded disk cache) a small thumbnail. Only the
/// requested frame is decoded, scaled down by FFmpeg before it reaches us.
pub fn thumbnail(path: &Path, info_kind: MediaKind, at: Us, max_w: u32, src_w: u32, src_h: u32) -> Result<Thumb> {
    let w = max_w.min(src_w.max(2)).max(2) & !1;
    let h = (((w as u64 * src_h.max(1) as u64) / src_w.max(1) as u64) as u32).max(2) & !1;
    let md = std::fs::metadata(path)?;
    let mtime = md.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let key = fnv(&format!("{}|{}|{}|{}|{}", path.display(), md.len(), mtime, at / 100_000, w));
    let dir = crate::project::store::cache_dir().join("thumbs");
    let file = dir.join(format!("{key:016x}.png"));
    if let Ok(img) = image::open(&file) {
        let rgba = img.to_rgba8();
        return Ok(Thumb { w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() });
    }
    let raw = decode_one_frame(path, info_kind, at, w, h)?;
    let _ = std::fs::create_dir_all(&dir);
    if let Some(img) = image::RgbaImage::from_raw(w, h, raw.clone()) {
        let _ = img.save(&file);
    }
    prune_cache(&dir, 64 * 1024 * 1024);
    Ok(Thumb { w, h, rgba: raw })
}

/// Decode exactly one frame at `at`, scaled to w*h RGBA.
pub fn decode_one_frame(path: &Path, kind: MediaKind, at: Us, w: u32, h: u32) -> Result<Vec<u8>> {
    let mut c = ffmpeg_cmd()?;
    if kind == MediaKind::Video {
        c.arg("-ss").arg(format!("{:.3}", crate::time::to_secs(at.max(0))));
    }
    input_args(&mut c, path)?;
    c.args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}:flags=bilinear,format=rgba"), "-f", "rawvideo", "-"]);
    let out = run_with_timeout(c, Duration::from_secs(30))?;
    if out.len() != (w * h * 4) as usize {
        return Err(EngineError::Media("couldn't decode a picture from this file".into()));
    }
    Ok(out)
}

/// Delete oldest cache files until the directory is below `max_bytes`.
pub fn prune_cache(dir: &Path, max_bytes: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= max_bytes {
        return;
    }
    files.sort();
    for (_, len, p) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

// ----------------------------------------------------------------- waveform

/// Peak amplitudes (0..1) in `buckets` slices, computed by streaming 8 kHz
/// mono audio through a small buffer. Honours `cancel`.
pub fn waveform(path: &Path, duration_us: Us, buckets: usize, cancel: &AtomicBool) -> Result<Vec<f32>> {
    let mut c = ffmpeg_cmd()?;
    input_args(&mut c, path)?;
    c.args(["-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "-"]);
    c.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| EngineError::NoFfmpeg(e.to_string()))?;
    let mut out = child.stdout.take().unwrap();
    let total = ((duration_us as f64 / 1e6) * 8000.0).max(1.0) as usize;
    let per = (total / buckets.max(1)).max(1);
    let mut peaks = vec![0f32; buckets.max(1)];
    let mut buf = vec![0u8; 16384];
    let (mut idx, mut carry) = (0usize, None::<u8>);
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Cancelled);
        }
        let n = out.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut bytes: Vec<u8> = vec![];
        if let Some(c) = carry.take() {
            bytes.push(c);
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() % 2 == 1 {
            carry = bytes.pop();
        }
        for pair in bytes.chunks_exact(2) {
            let s = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0;
            let b = (idx / per).min(peaks.len() - 1);
            peaks[b] = peaks[b].max(s.abs());
            idx += 1;
        }
    }
    let _ = child.wait();
    Ok(peaks)
}

// -------------------------------------------------------------------- proxy

/// Create a low-resolution H.264 proxy used for smoother preview. The
/// original file is never modified and export always uses the original.
pub fn make_proxy(src: &Path, dest: &Path, height: u32, cancel: &AtomicBool) -> Result<()> {
    let mut c = ffmpeg_cmd()?;
    c.arg("-y");
    input_args(&mut c, src)?;
    let part = dest.with_extension("part.mp4");
    c.args(["-vf", &format!("scale=-2:{}", height & !1), "-c:v", "libx264", "-preset", "ultrafast", "-crf", "28", "-c:a", "aac", "-b:a", "96k", "-f", "mp4"]).arg(&part);
    c.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| EngineError::NoFfmpeg(e.to_string()))?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Err(EngineError::Cancelled);
        }
        if let Some(st) = child.try_wait()? {
            if st.success() {
                std::fs::rename(&part, dest)?;
                return Ok(());
            }
            let _ = std::fs::remove_file(&part);
            return Err(EngineError::Media("couldn't create the preview proxy".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
