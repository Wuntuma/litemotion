//! Live audio output for the preview.
//!
//! A producer thread pushes mixed stereo f32 frames (48 kHz) into [`Shared`];
//! the sound card's callback drains them. The number of frames the device has
//! actually consumed is the playback clock, so video can follow the audio and
//! stay in sync. If the device can't be opened the caller falls back to the
//! wall clock (silent preview).
use std::collections::VecDeque;
use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

pub const RATE: u32 = 48_000;
/// Wait for this much audio before starting, so the first moments don't stutter.
const PREROLL_FRAMES: usize = 4_800;

#[repr(C)]
struct RawDevice {
    _p: [u8; 0],
}
extern "C" {
    fn lm_audio_open(cb: extern "C" fn(*mut c_void, *mut f32, c_uint), user: *mut c_void, null_backend: c_int, rate: c_uint) -> *mut RawDevice;
    fn lm_audio_start(d: *mut RawDevice) -> c_int;
    fn lm_audio_stop(d: *mut RawDevice);
    fn lm_audio_close(d: *mut RawDevice);
}

/// Ring buffer + clock shared by the producer, the audio callback and the video loop.
pub struct Shared {
    buf: Mutex<VecDeque<f32>>,
    played: AtomicU64,
    ready: AtomicBool,
    finished: AtomicBool,
}

impl Shared {
    fn new() -> Arc<Shared> {
        Arc::new(Shared { buf: Mutex::new(VecDeque::new()), played: AtomicU64::new(0), ready: AtomicBool::new(false), finished: AtomicBool::new(false) })
    }
    /// Forget everything: a new playback run starts.
    pub fn reset(&self) {
        let mut b = self.buf.lock().unwrap();
        b.clear();
        self.played.store(0, Relaxed);
        self.ready.store(false, Relaxed);
        self.finished.store(false, Relaxed);
    }
    /// Producer: append interleaved stereo samples.
    pub fn push(&self, samples: &[f32]) {
        self.buf.lock().unwrap().extend(samples.iter().copied());
    }
    pub fn buffered_frames(&self) -> usize {
        self.buf.lock().unwrap().len() / 2
    }
    /// Producer: nothing more is coming (end of project, error or cancel).
    pub fn finish(&self) {
        self.finished.store(true, Relaxed);
    }
    /// Frames the device has really played since `reset`: the audio clock.
    pub fn played_frames(&self) -> u64 {
        self.played.load(Relaxed)
    }
    pub fn played_secs(&self) -> f64 {
        self.played_frames() as f64 / RATE as f64
    }
    /// True once playback has begun (enough audio was buffered).
    pub fn is_ready(&self) -> bool {
        self.ready.load(Relaxed)
    }
    /// Runs on the audio thread. Never blocks: on contention it plays silence.
    fn fill(&self, out: &mut [f32]) {
        out.fill(0.0);
        let Ok(mut b) = self.buf.try_lock() else { return };
        let avail = b.len() / 2;
        if !self.ready.load(Relaxed) {
            if avail >= PREROLL_FRAMES || (avail > 0 && self.finished.load(Relaxed)) {
                self.ready.store(true, Relaxed);
            } else {
                return;
            }
        }
        let n = (out.len() / 2).min(avail);
        for s in out[..n * 2].iter_mut() {
            *s = b.pop_front().unwrap_or(0.0);
        }
        // Only real audio advances the clock, so video waits out an underrun.
        self.played.fetch_add(n as u64, Relaxed);
    }
}

extern "C" fn trampoline(user: *mut c_void, out: *mut f32, frames: c_uint) {
    // SAFETY: `user` is the Arc<Shared> raw pointer kept alive by `Device`
    // until after the device is closed; `out` holds `frames` stereo frames.
    let shared = unsafe { &*(user as *const Shared) };
    let out = unsafe { std::slice::from_raw_parts_mut(out, frames as usize * 2) };
    shared.fill(out);
}

pub struct Device {
    raw: *mut RawDevice,
    user: *const Shared,
    shared: Arc<Shared>,
    running: bool,
}
// The raw handle is only touched from the owning thread (start/stop/drop).
unsafe impl Send for Device {}

impl Device {
    /// Open the default output. `LITEMOTION_AUDIO_BACKEND=null` selects a
    /// silent, real-time backend (used by tests and machines without sound).
    pub fn open() -> Result<Device, String> {
        let null = std::env::var("LITEMOTION_AUDIO_BACKEND").map_or(false, |v| v == "null");
        let shared = Shared::new();
        let user = Arc::into_raw(shared.clone());
        let raw = unsafe { lm_audio_open(trampoline, user as *mut c_void, null as c_int, RATE) };
        if raw.is_null() {
            drop(unsafe { Arc::from_raw(user) });
            return Err("no audio output device could be opened".into());
        }
        Ok(Device { raw, user, shared, running: false })
    }
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }
    pub fn start(&mut self) -> bool {
        if !self.running {
            self.running = unsafe { lm_audio_start(self.raw) } != 0;
        }
        self.running
    }
    pub fn stop(&mut self) {
        if self.running {
            unsafe { lm_audio_stop(self.raw) };
            self.running = false;
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { lm_audio_close(self.raw) };
        drop(unsafe { Arc::from_raw(self.user) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn null_device() -> Device {
        std::env::set_var("LITEMOTION_AUDIO_BACKEND", "null");
        Device::open().expect("null backend always opens")
    }

    #[test]
    fn clock_follows_what_the_device_really_played() {
        let mut d = null_device();
        let s = d.shared();
        s.reset();
        s.push(&vec![0.1; 2 * RATE as usize]); // 1 s of audio
        assert!(d.start());
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(600));
        let (played, wall) = (s.played_secs(), t0.elapsed().as_secs_f64());
        assert!(s.is_ready());
        // Real-time pace: played audio tracks wall time (generous bounds for loaded CI machines).
        assert!(played > 0.3 && played <= wall + 0.05, "played {played}s in {wall}s");
        d.stop();
        let frozen = s.played_secs();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.played_secs(), frozen, "clock must stop when the device stops");
    }

    #[test]
    fn waits_for_preroll_and_pauses_on_underrun() {
        let mut d = null_device();
        let s = d.shared();
        s.reset();
        assert!(d.start());
        s.push(&vec![0.1; 2 * 1000]); // less than the preroll
        std::thread::sleep(Duration::from_millis(250));
        assert!(!s.is_ready() && s.played_frames() == 0, "must not start on a tiny buffer");
        s.push(&vec![0.1; 2 * RATE as usize / 10]); // now enough
        std::thread::sleep(Duration::from_millis(400));
        assert!(s.is_ready());
        let after_drain = s.played_frames();
        assert_eq!(after_drain as usize, 1000 + RATE as usize / 10, "played exactly what was supplied; underrun adds nothing");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.played_frames(), after_drain);
        s.reset();
        assert_eq!((s.played_frames(), s.buffered_frames(), s.is_ready()), (0, 0, false));
    }
}
