//! Offline audio mixer. Each audio-bearing clip is decoded by its own FFmpeg
//! process as raw PCM and mixed in small blocks into a temporary file, so
//! memory use is a few blocks regardless of project length.
use crate::error::{EngineError, Result};
use crate::platform::{ffmpeg_cmd, input_args};
use crate::project::io::resolve_media;
use crate::project::model::*;
use crate::time::{to_secs, Us};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

pub const SAMPLE_RATE: u32 = 48_000;
const CH: usize = 2;
const BLOCK: usize = 1024; // samples per channel per block (~21 ms)

pub fn atempo_chain(speed: f32) -> String {
    let mut s = speed as f64;
    let mut parts = vec![];
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s *= 2.0;
    }
    parts.push(format!("atempo={s:.5}"));
    parts.join(",")
}

struct Source<'a> {
    clip: &'a Clip,
    start_sample: i64,
    end_sample: i64,
    child: Option<(Child, ChildStdout)>,
    done: bool,
}

impl Drop for Source<'_> {
    fn drop(&mut self) {
        // Every exit path (done, cancelled, error) must reap FFmpeg children.
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub fn us_to_sample(t: Us) -> i64 {
    (t as f64 * SAMPLE_RATE as f64 / 1e6).round() as i64
}

/// Clips that contribute sound: audio-track clips and the audio of video clips.
pub fn audible_clips(p: &Project) -> Vec<&Clip> {
    let mut v = vec![];
    for t in p.tracks.iter().filter(|t| !t.muted) {
        for c in &t.clips {
            if c.muted {
                continue;
            }
            if let Content::Media { media_id } = c.content {
                if let Some(m) = p.media(media_id) {
                    let sounds = match t.kind {
                        TrackKind::Audio => m.has_audio || m.kind == MediaKind::Audio,
                        TrackKind::Video | TrackKind::Overlay => m.kind == MediaKind::Video && m.has_audio && t.visible,
                        TrackKind::Text => false,
                    };
                    if sounds {
                        v.push(c);
                    }
                }
            }
        }
    }
    v
}

/// Gain for a clip at clip-local time `t`: clip volume (keyframable) x fades.
pub fn gain_at(c: &Clip, t: Us) -> f32 {
    let mut g = c.value_at(crate::animation::Prop::Volume, t).max(0.0);
    if c.fade_in > 0 && t < c.fade_in {
        g *= (t as f32 / c.fade_in as f32).clamp(0.0, 1.0);
    }
    let rem = c.duration - t;
    if c.fade_out > 0 && rem < c.fade_out {
        g *= (rem as f32 / c.fade_out as f32).clamp(0.0, 1.0);
    }
    g
}

/// Mix the project's audio to raw signed 16-bit stereo PCM at 48 kHz.
/// Returns the number of sample frames written (0 = silent project).
pub fn mix_to_file(
    p: &Project,
    project_dir: Option<&Path>,
    out_path: &Path,
    total_us: Us,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<i64> {
    if audible_clips(p).is_empty() {
        return Ok(0);
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(out_path)?);
    mix_range(p, project_dir, 0, total_us, cancel, progress, &mut |blk| {
        for v in blk {
            // Hard limiter keeps summed tracks from wrapping.
            let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
            out.write_all(&s.to_le_bytes())?;
        }
        Ok(())
    })?;
    out.flush()?;
    Ok(us_to_sample(total_us))
}

/// Mix `[start_us, total_us)` of the project into interleaved stereo f32
/// blocks (48 kHz) handed to `sink` in order. Used for export (from 0) and for
/// live preview (from the playhead). Memory use is a few blocks; every FFmpeg
/// child is reaped on any exit path.
pub fn mix_range(
    p: &Project,
    project_dir: Option<&Path>,
    start_us: Us,
    total_us: Us,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
    sink: &mut dyn FnMut(&[f32]) -> Result<()>,
) -> Result<()> {
    let clips = audible_clips(p);
    if clips.is_empty() {
        return Ok(());
    }
    let total = us_to_sample(total_us);
    let first = us_to_sample(start_us.max(0));
    let mut sources: Vec<Source> = clips
        .into_iter()
        .map(|c| Source { clip: c, start_sample: us_to_sample(c.start), end_sample: us_to_sample(c.end()), child: None, done: false })
        .collect();
    let mut acc = vec![0f32; BLOCK * CH];
    let mut tmp = vec![0u8; BLOCK * CH * 4];
    let mut g0 = first;
    while g0 < total {
        if cancel.load(Ordering::Relaxed) {
            return Err(EngineError::Cancelled);
        }
        let g1 = (g0 + BLOCK as i64).min(total);
        let n = (g1 - g0) as usize;
        acc[..n * CH].iter_mut().for_each(|x| *x = 0.0);
        for s in sources.iter_mut() {
            if s.done || s.end_sample <= g0 || s.start_sample >= g1 {
                if !s.done && s.end_sample <= g0 {
                    s.done = true;
                    if let Some((mut ch, _)) = s.child.take() {
                        let _ = ch.kill();
                        let _ = ch.wait();
                    }
                }
                continue;
            }
            if s.child.is_none() {
                let media = match s.clip.content {
                    Content::Media { media_id } => p.media(media_id).ok_or_else(|| EngineError::Export("missing media".into()))?,
                    _ => continue,
                };
                let path = resolve_media(media, project_dir).ok_or_else(|| EngineError::Missing(media.path.clone()))?;
                let mut c = ffmpeg_cmd()?;
                // Starting mid-clip: skip into the source by the timeline offset x speed.
                let skip_tl = (g0.max(s.start_sample) - s.start_sample) as f64 / SAMPLE_RATE as f64;
                let skip_src = skip_tl * s.clip.speed as f64;
                c.arg("-ss").arg(format!("{:.6}", to_secs(s.clip.source_in) + skip_src));
                input_args(&mut c, &path)?;
                // FFmpeg's default mono->stereo upmix is 3 dB quieter; duplicate the channel instead.
                let mut af = atempo_chain(s.clip.speed);
                if media.audio_channels == 1 {
                    af.push_str(",pan=stereo|c0=c0|c1=c0");
                }
                c.args(["-vn", "-sn", "-t", &format!("{:.4}", (to_secs(s.clip.source_span()) - skip_src).max(0.0) + 0.2), "-af", &af, "-ac", "2", "-ar", &SAMPLE_RATE.to_string(), "-f", "f32le", "-"]);
                c.stdout(Stdio::piped()).stderr(Stdio::null());
                let mut child = c.spawn().map_err(|e| EngineError::NoFfmpeg(e.to_string()))?;
                let stdout = child.stdout.take().unwrap();
                s.child = Some((child, stdout));
            }
            let from = g0.max(s.start_sample);
            let to = g1.min(s.end_sample);
            let cnt = (to - from) as usize;
            let (_, pipe) = s.child.as_mut().unwrap();
            let buf = &mut tmp[..cnt * CH * 4];
            let got = read_full(pipe, buf);
            // Gain ramps linearly across the block for click-free automation.
            let tl0 = (from - s.start_sample) as f64 * 1e6 / SAMPLE_RATE as f64;
            let tl1 = (to - s.start_sample) as f64 * 1e6 / SAMPLE_RATE as f64;
            let (ga, gb) = (gain_at(s.clip, tl0 as Us), gain_at(s.clip, tl1 as Us));
            let off = (from - g0) as usize;
            for i in 0..cnt {
                if (i + 1) * CH * 4 > got {
                    break; // source ended early -> silence
                }
                let gain = ga + (gb - ga) * (i as f32 / cnt.max(1) as f32);
                for ch in 0..CH {
                    let b = &buf[(i * CH + ch) * 4..(i * CH + ch) * 4 + 4];
                    acc[(off + i) * CH + ch] += f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * gain;
                }
            }
        }
        sink(&acc[..n * CH])?;
        g0 = g1;
        if (g0 / BLOCK as i64) % 64 == 0 {
            progress((g0 - first) as f32 / (total - first).max(1) as f32);
        }
    }
    Ok(())
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n
}
