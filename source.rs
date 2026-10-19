//! Frame providers: streaming (export/playback) and still (scrubbing).
use super::compositor::{Bitmap, FrameProvider};
use crate::platform::{ffmpeg_cmd, input_args};
use crate::project::io::resolve_media;
use crate::project::model::*;
use crate::time::{to_secs, Us};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::Arc;

/// FFmpeg video-filter chain that crops, fits, colour-adjusts and retimes.
fn vf_chain(clip: &Clip, size: (u32, u32), fps: Option<f64>) -> String {
    let mut v: Vec<String> = vec![];
    let c = clip.crop;
    if c.left > 0.0 || c.right > 0.0 || c.top > 0.0 || c.bottom > 0.0 {
        v.push(format!(
            "crop=trunc(iw*{:.5}/2)*2:trunc(ih*{:.5}/2)*2:iw*{:.5}:ih*{:.5}",
            1.0 - c.left - c.right,
            1.0 - c.top - c.bottom,
            c.left,
            c.top
        ));
    }
    let (w, h) = size;
    match clip.fit {
        FitMode::Fill => v.push(format!("scale={w}:{h}:force_original_aspect_ratio=increase:flags=bicubic,crop={w}:{h}")),
        _ => v.push(format!("scale={w}:{h}:flags=bicubic")),
    }
    if !clip.color.is_identity() {
        v.push(format!("eq=brightness={:.3}:contrast={:.3}:saturation={:.3}", clip.color.brightness, clip.color.contrast, clip.color.saturation));
    }
    if let Some(fps) = fps {
        if (clip.speed - 1.0).abs() > 1e-4 {
            v.push(format!("setpts=PTS/{:.5}", clip.speed));
        }
        v.push(format!("fps={fps}"));
    }
    v.push("format=rgba".into());
    v.join(",")
}

fn media_path(m: &MediaRef, project_dir: Option<&Path>, use_proxy: bool) -> Option<PathBuf> {
    if use_proxy {
        if let Some(p) = &m.proxy_path {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    resolve_media(m, project_dir)
}

struct Decoder {
    child: Child,
    out: ChildStdout,
    size: (u32, u32),
    /// Local frame index of the NEXT frame to be read.
    next: i64,
    last: Option<Arc<Bitmap>>,
    eof: bool,
}
impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sequential decoder per clip. Each active video clip owns one FFmpeg
/// process that streams raw frames; memory use is one frame per clip.
pub struct StreamProvider {
    fps: f64,
    project_dir: Option<PathBuf>,
    use_proxy: bool,
    decoders: HashMap<u64, Decoder>,
    images: HashMap<(u64, u32, u32, u64), Arc<Bitmap>>,
    pub errors: Vec<String>,
}

impl StreamProvider {
    pub fn new(fps: f64, project_dir: Option<&Path>, use_proxy: bool) -> StreamProvider {
        StreamProvider { fps, project_dir: project_dir.map(Path::to_path_buf), use_proxy, decoders: HashMap::new(), images: HashMap::new(), errors: vec![] }
    }

    fn spawn(&self, clip: &Clip, m: &MediaRef, size: (u32, u32), start_idx: i64) -> crate::Result<Decoder> {
        let path = media_path(m, self.project_dir.as_deref(), self.use_proxy).ok_or_else(|| crate::EngineError::Missing(m.path.clone()))?;
        let src_start = clip.source_in + (to_secs(0) + start_idx as f64 / self.fps * clip.speed as f64 * 1e6) as Us;
        let span = clip.source_span() - (src_start - clip.source_in);
        let mut c = ffmpeg_cmd()?;
        c.arg("-ss").arg(format!("{:.4}", to_secs(src_start)));
        input_args(&mut c, &path)?;
        c.args(["-an", "-sn", "-t", &format!("{:.4}", to_secs(span.max(1_000)) + 0.5), "-vf", &vf_chain(clip, size, Some(self.fps)), "-f", "rawvideo", "-"]);
        c.stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().map_err(|e| crate::EngineError::NoFfmpeg(e.to_string()))?;
        let out = child.stdout.take().unwrap();
        Ok(Decoder { child, out, size, next: start_idx, last: None, eof: false })
    }

    /// Release decoders for clips that are no longer active.
    pub fn retain_active(&mut self, active: &[u64]) {
        self.decoders.retain(|id, _| active.contains(id));
    }
}

impl FrameProvider for StreamProvider {
    fn media_frame(&mut self, clip: &Clip, m: &MediaRef, local_t: Us, size: (u32, u32)) -> Option<Arc<Bitmap>> {
        if m.kind == MediaKind::Image {
            let key = (m.id, size.0, size.1, crop_key(clip));
            if let Some(b) = self.images.get(&key) {
                return Some(b.clone());
            }
            let path = resolve_media(m, self.project_dir.as_deref())?;
            let mut c = ffmpeg_cmd().ok()?;
            input_args(&mut c, &path).ok()?;
            c.args(["-frames:v", "1", "-vf", &vf_chain(clip, size, None), "-f", "rawvideo", "-"]);
            c.stdout(Stdio::piped()).stderr(Stdio::null());
            let out = c.output().ok()?;
            if out.stdout.len() != (size.0 * size.1 * 4) as usize {
                self.errors.push(format!("couldn't decode image {}", m.path));
                return None;
            }
            let b = Arc::new(Bitmap { w: size.0, h: size.1, data: out.stdout });
            if self.images.len() > 16 {
                self.images.clear();
            }
            self.images.insert(key, b.clone());
            return Some(b);
        }
        let idx = (to_secs(local_t) * self.fps + 1e-6).floor() as i64;
        let need_new = match self.decoders.get(&clip.id) {
            None => true,
            Some(d) => d.size != size || idx < d.next - 1 || (idx - d.next > 20 && !d.eof),
        };
        if need_new {
            match self.spawn(clip, m, size, idx) {
                Ok(d) => {
                    self.decoders.insert(clip.id, d);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !self.errors.contains(&msg) {
                        self.errors.push(msg);
                    }
                    return None;
                }
            }
        }
        let d = self.decoders.get_mut(&clip.id)?;
        let frame_bytes = (size.0 * size.1 * 4) as usize;
        while d.next <= idx && !d.eof {
            let mut buf = vec![0u8; frame_bytes];
            match d.out.read_exact(&mut buf) {
                Ok(()) => {
                    d.last = Some(Arc::new(Bitmap { w: size.0, h: size.1, data: buf }));
                    d.next += 1;
                }
                Err(_) => d.eof = true, // source ended early: hold last frame
            }
        }
        d.last.clone()
    }
}

fn crop_key(c: &Clip) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    [c.crop.left, c.crop.top, c.crop.right, c.crop.bottom].map(f32::to_bits).hash(&mut h);
    (c.fit as u8).hash(&mut h);
    h.finish()
}

/// Random-access provider for scrubbing/paused preview: decodes a single frame
/// per request, with a byte-bounded LRU cache.
pub struct StillProvider {
    project_dir: Option<PathBuf>,
    use_proxy: bool,
    cache: Vec<((u64, i64, u32, u32, u64), Arc<Bitmap>)>,
    max_bytes: usize,
}
impl StillProvider {
    pub fn new(project_dir: Option<&Path>, use_proxy: bool, max_bytes: usize) -> StillProvider {
        StillProvider { project_dir: project_dir.map(Path::to_path_buf), use_proxy, cache: vec![], max_bytes }
    }
    fn evict(&mut self) {
        let mut total: usize = self.cache.iter().map(|(_, b)| b.bytes()).sum();
        while total > self.max_bytes && !self.cache.is_empty() {
            total -= self.cache.remove(0).1.bytes();
        }
    }
}
impl FrameProvider for StillProvider {
    fn media_frame(&mut self, clip: &Clip, m: &MediaRef, local_t: Us, size: (u32, u32)) -> Option<Arc<Bitmap>> {
        // Quantise to ~30 fps so scrubbing re-uses nearby frames.
        let q = (to_secs(local_t) * 30.0).floor() as i64;
        // Key includes the full filter chain so any visual edit invalidates it.
        let key = (clip.id, q, size.0, size.1, {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            vf_chain(clip, size, None).hash(&mut h);
            (clip.source_in, clip.speed.to_bits(), &m.path).hash(&mut h);
            h.finish()
        });
        if let Some(pos) = self.cache.iter().position(|(k, _)| *k == key) {
            let e = self.cache.remove(pos);
            let b = e.1.clone();
            self.cache.push(e);
            return Some(b);
        }
        let path = media_path(m, self.project_dir.as_deref(), self.use_proxy)?;
        let src_t = clip.source_in + (local_t as f64 * clip.speed as f64) as Us;
        let mut c = ffmpeg_cmd().ok()?;
        if m.kind == MediaKind::Video {
            c.arg("-ss").arg(format!("{:.4}", to_secs(src_t)));
        }
        input_args(&mut c, &path).ok()?;
        c.args(["-frames:v", "1", "-vf", &vf_chain(clip, size, None), "-f", "rawvideo", "-"]);
        c.stdout(Stdio::piped()).stderr(Stdio::null());
        let out = c.output().ok()?;
        if out.stdout.len() != (size.0 * size.1 * 4) as usize {
            return None;
        }
        let b = Arc::new(Bitmap { w: size.0, h: size.1, data: out.stdout });
        self.cache.push((key, b.clone()));
        self.evict();
        Some(b)
    }
}
