//! Live sound for the preview: mixes from the playhead on a background thread
//! into the audio device's buffer. The device opens on first Play.
use litemotion_audio::{Device, Shared};
use litemotion_engine::audio::{audible_clips, mix_range};
use litemotion_engine::project::model::Project;
use litemotion_engine::time::Us;
use litemotion_engine::EngineError;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Keep about half a second buffered: enough to ride out hiccups, short enough
/// that stopping or editing is immediate.
const TARGET_BUFFER_FRAMES: usize = 24_000;

#[derive(Default)]
pub struct PreviewAudio {
    dev: Option<Device>,
    cancel: Option<Arc<AtomicBool>>,
    worker: Option<JoinHandle<()>>,
    /// The current run's buffer and clock (None when idle or silent).
    pub current: Option<Arc<Shared>>,
    /// The device couldn't be opened; the preview stays silent.
    pub failed: bool,
}

impl PreviewAudio {
    /// Begin playing the project's audio from `t0`. Returns the audio clock to
    /// follow, or None when there is nothing to hear or no usable device.
    pub fn start(&mut self, proj: Arc<Project>, dir: Option<PathBuf>, t0: Us) -> Option<Arc<Shared>> {
        self.stop();
        if audible_clips(&proj).is_empty() {
            return None;
        }
        if self.dev.is_none() && !self.failed {
            match Device::open() {
                Ok(d) => self.dev = Some(d),
                Err(_) => self.failed = true,
            }
        }
        let dev = self.dev.as_mut()?;
        let shared = dev.shared();
        shared.reset();
        let cancel = Arc::new(AtomicBool::new(false));
        let (sh, c2) = (shared.clone(), cancel.clone());
        let end = proj.duration();
        let worker = std::thread::Builder::new().name("preview-audio".into()).spawn(move || {
            let r = mix_range(&proj, dir.as_deref(), t0, end, &c2, &mut |_| {}, &mut |blk| {
                while sh.buffered_frames() > TARGET_BUFFER_FRAMES {
                    if c2.load(Ordering::Relaxed) {
                        return Err(EngineError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                if c2.load(Ordering::Relaxed) {
                    return Err(EngineError::Cancelled);
                }
                sh.push(blk);
                Ok(())
            });
            let _ = r; // errors (e.g. media vanished) just end the sound; the video keeps going
            sh.finish();
        });
        let Ok(worker) = worker else { return None };
        if !dev.start() {
            self.failed = true;
            cancel.store(true, Ordering::Relaxed);
            let _ = worker.join();
            return None;
        }
        self.cancel = Some(cancel);
        self.worker = Some(worker);
        self.current = Some(shared.clone());
        Some(shared)
    }

    pub fn stop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        if let Some(d) = self.dev.as_mut() {
            d.stop();
        }
        self.current = None;
    }
}

impl Drop for PreviewAudio {
    fn drop(&mut self) {
        self.stop();
    }
}
