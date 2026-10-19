//! All times in the engine are signed integer microseconds ("Us").
pub type Us = i64;
pub const US_PER_SEC: Us = 1_000_000;

pub fn secs(s: f64) -> Us {
    (s * 1e6).round() as Us
}
pub fn to_secs(u: Us) -> f64 {
    u as f64 / 1e6
}
/// Timeline time of frame `f` at `fps`.
pub fn frame_to_us(f: i64, fps: f64) -> Us {
    (f as f64 * 1e6 / fps).round() as Us
}
/// Index of the frame displayed at time `u` (floor, with a tiny epsilon so
/// that frame_to_us(f) maps back to f).
pub fn us_to_frame(u: Us, fps: f64) -> i64 {
    (u as f64 * fps / 1e6 + 1e-6).floor() as i64
}
pub fn format_timecode(u: Us, fps: f64) -> String {
    let u = u.max(0);
    let total = u / US_PER_SEC;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    let f = us_to_frame(u % US_PER_SEC, fps);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{f:02}")
    } else {
        format!("{m:02}:{s:02}.{f:02}")
    }
}
