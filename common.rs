#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn gen(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(&out)
        .status()
        .expect("ffmpeg available");
    assert!(st.success(), "generating {name} failed");
    out
}

/// 2 s, 320x240, 30 fps colour test pattern with a 440 Hz tone.
pub fn video(dir: &Path) -> PathBuf {
    gen(dir, "clip.mp4", &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=2", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
}
pub fn silent_video(dir: &Path) -> PathBuf {
    gen(dir, "silent.mp4", &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=2", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
}
pub fn music(dir: &Path) -> PathBuf {
    gen(dir, "music.wav", &["-f", "lavfi", "-i", "sine=frequency=880:duration=3"])
}
pub fn image(dir: &Path) -> PathBuf {
    gen(dir, "logo.png", &["-f", "lavfi", "-i", "color=c=red:s=120x120", "-frames:v", "1"])
}
/// Plain dark-blue background video (no red), so overlay colour checks are unambiguous.
pub fn flat_video(dir: &Path) -> PathBuf {
    gen(dir, "flat.mp4", &["-f", "lavfi", "-i", "color=c=0x203040:size=320x240:rate=30:duration=2", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
}
