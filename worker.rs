//! Background workers: preview rendering/playback, thumbnails and waveforms.
//! The UI thread never decodes or composites; it only uploads finished frames.
use litemotion_engine::media::{self, Thumb};
use litemotion_engine::project::model::*;
use litemotion_engine::rendering::source::{StillProvider, StreamProvider};
use litemotion_engine::rendering::{compose, Bitmap, LayerCache};
use litemotion_engine::time::{frame_to_us, us_to_frame, Us};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use litemotion_audio::Shared;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct PreviewParams {
    pub proj: Arc<Project>,
    pub dir: Option<PathBuf>,
    pub w: u32,
    pub h: u32,
    pub use_proxy: bool,
    /// Auto quality lowers the resolution when playback can't keep up.
    pub auto_quality: bool,
}

pub enum PreviewReq {
    Still { p: PreviewParams, t: Us, tag: u64 },
    /// `audio` is the sound clock to follow; None = silent, use the wall clock.
    Play { p: PreviewParams, t0: Us, tag: u64, audio: Option<Arc<Shared>> },
    Stop,
}

pub struct PreviewFrame {
    pub tag: u64,
    pub t: Us,
    pub bmp: Bitmap,
    pub playing: bool,
    pub ended: bool,
}

pub struct PreviewWorker {
    tx: Sender<PreviewReq>,
    pub rx: Receiver<PreviewFrame>,
}

impl PreviewWorker {
    pub fn start(ctx: egui::Context) -> PreviewWorker {
        let (tx, rx_req) = channel::<PreviewReq>();
        let (tx_frame, rx) = channel::<PreviewFrame>();
        std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || run(rx_req, tx_frame, ctx))
            .expect("spawn preview worker");
        PreviewWorker { tx, rx }
    }
    pub fn send(&self, r: PreviewReq) {
        let _ = self.tx.send(r);
    }
}

fn run(rx: Receiver<PreviewReq>, tx: Sender<PreviewFrame>, ctx: egui::Context) {
    let mut still: Option<(StillProvider, Option<PathBuf>, bool)> = None;
    let mut cache = LayerCache::default();
    let mut next: Option<PreviewReq> = None;
    loop {
        let mut req = match next.take() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(r) => r,
                Err(_) => return,
            },
        };
        // Only the newest queued request matters (scrubbing sends many).
        while let Ok(n) = rx.try_recv() {
            req = n;
        }
        match req {
            PreviewReq::Stop => {}
            PreviewReq::Still { p, t, tag } => {
                let reuse = matches!(&still, Some((_, d, u)) if *d == p.dir && *u == p.use_proxy);
                if !reuse {
                    still = Some((StillProvider::new(p.dir.as_deref(), p.use_proxy, 96 * 1024 * 1024), p.dir.clone(), p.use_proxy));
                }
                let prov = &mut still.as_mut().unwrap().0;
                let bmp = compose(&p.proj, p.w, p.h, t, prov, &mut cache);
                let _ = tx.send(PreviewFrame { tag, t, bmp, playing: false, ended: false });
                ctx.request_repaint();
            }
            PreviewReq::Play { p, t0, tag, audio } => {
                next = play(&rx, &tx, &ctx, p, t0, tag, audio, &mut cache);
            }
        }
    }
}

/// Real-time playback loop. Returns a request that interrupted it, if any.
#[allow(clippy::too_many_arguments)]
fn play(rx: &Receiver<PreviewReq>, tx: &Sender<PreviewFrame>, ctx: &egui::Context, p: PreviewParams, t0: Us, tag: u64, audio: Option<Arc<Shared>>, cache: &mut LayerCache) -> Option<PreviewReq> {
    let fps = p.proj.fps;
    let total = p.proj.frame_count();
    let (mut w, mut h) = (p.w, p.h);
    let mut prov = StreamProvider::new(fps, p.dir.as_deref(), p.use_proxy);
    let f0 = us_to_frame(t0, fps);
    let started = Instant::now();
    // Master clock: the sound card when there is sound (video follows what is
    // actually heard), else the wall clock. If the device never starts
    // consuming audio within 2 s, fall back to the wall clock.
    let mut wall: Option<Instant> = if audio.is_none() { Some(started) } else { None };
    let mut slow_frames = 0;
    let mut last_sent = -1i64;
    loop {
        if let Ok(mut n) = rx.try_recv() {
            while let Ok(m) = rx.try_recv() {
                n = m;
            }
            return Some(n);
        }
        let elapsed = match (&audio, wall) {
            (_, Some(w)) => w.elapsed().as_secs_f64(),
            (Some(a), None) if a.is_ready() => a.played_secs(),
            (_, None) => {
                if started.elapsed().as_secs_f64() > 2.0 {
                    wall = Some(Instant::now());
                }
                0.0 // holding on the first frame until the sound starts
            }
        };
        let f = f0 + (elapsed * fps) as i64;
        if f >= total {
            let _ = tx.send(PreviewFrame { tag, t: frame_to_us(total, fps), bmp: Bitmap::new(2, 2, [0, 0, 0, 255]), playing: false, ended: true });
            ctx.request_repaint();
            return None;
        }
        if f == last_sent {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let t = frame_to_us(f, fps);
        let active: Vec<u64> = p.proj.tracks.iter().flat_map(|tr| tr.clips.iter()).filter(|c| c.start <= t && t < c.end()).map(|c| c.id).collect();
        prov.retain_active(&active);
        let t_start = Instant::now();
        let bmp = compose(&p.proj, w, h, t, &mut prov, cache);
        let cost = t_start.elapsed().as_secs_f64();
        last_sent = f;
        let _ = tx.send(PreviewFrame { tag, t, bmp, playing: true, ended: false });
        ctx.request_repaint();
        // Adaptive preview quality: shrink the preview (never the export) when
        // frames take longer than their slot, several times in a row.
        if p.auto_quality {
            if cost > 1.4 / fps {
                slow_frames += 1;
            } else {
                slow_frames = 0;
            }
            if slow_frames >= 6 && w > 240 {
                w = ((w as f32 * 0.75) as u32 & !1).max(160);
                h = ((h as f32 * 0.75) as u32 & !1).max(90);
                slow_frames = 0;
                prov = StreamProvider::new(fps, p.dir.as_deref(), p.use_proxy);
            }
        }
    }
}

// ----------------------------------------------------------------- jobs

pub enum Job {
    Thumb { id: u64, path: PathBuf, kind: MediaKind, w: u32, h: u32, at: Us },
    Wave { id: u64, path: PathBuf, dur: Us },
}
pub enum JobResult {
    Thumb(u64, Thumb),
    Wave(u64, Vec<f32>),
    Failed(u64, String),
}

pub struct JobsWorker {
    tx: Sender<(u64, Job)>,
    pub rx: Receiver<JobResult>,
    epoch: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}
impl JobsWorker {
    pub fn start(ctx: egui::Context) -> JobsWorker {
        let (tx, rx_job) = channel::<(u64, Job)>();
        let (tx_res, rx) = channel::<JobResult>();
        let epoch = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (e2, c2) = (epoch.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("jobs".into())
            .spawn(move || {
                while let Ok((ep, job)) = rx_job.recv() {
                    if ep != e2.load(Ordering::Relaxed) {
                        continue; // obsolete: the project was closed meanwhile
                    }
                    let res = match job {
                        Job::Thumb { id, path, kind, w, h, at } => match media::thumbnail(&path, kind, at, 160, w.max(2), h.max(2)) {
                            Ok(t) => JobResult::Thumb(id, t),
                            Err(e) => JobResult::Failed(id, e.to_string()),
                        },
                        Job::Wave { id, path, dur } => match media::waveform(&path, dur, 1500, &c2) {
                            Ok(w) => JobResult::Wave(id, w),
                            Err(e) => JobResult::Failed(id, e.to_string()),
                        },
                    };
                    let _ = tx_res.send(res);
                    ctx.request_repaint();
                }
            })
            .expect("spawn jobs worker");
        JobsWorker { tx, rx, epoch, cancel }
    }
    pub fn submit(&self, job: Job) {
        let _ = self.tx.send((self.epoch.load(Ordering::Relaxed), job));
    }
    /// Cancel queued and running work (called when another project opens).
    pub fn cancel_all(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(30));
        self.cancel.store(false, Ordering::Relaxed);
    }
}
