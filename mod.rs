//! OS/tool integration: locating FFmpeg, spawning it safely, hardware encoders.
use crate::error::{EngineError, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Locate a bundled or system tool (`ffmpeg`, `ffprobe`). Order: env override,
/// next to the executable (how installers ship it), then PATH.
pub fn find_tool(name: &str) -> Result<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    if let Ok(v) = std::env::var(format!("LITEMOTION_{}", name.to_uppercase())) {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            for d in [dir.to_path_buf(), dir.join("bin")] {
                let p = d.join(&exe);
                if p.is_file() {
                    return Ok(p);
                }
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&paths) {
            let p = d.join(&exe);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    Err(EngineError::NoFfmpeg(format!("couldn't find '{name}'. Install FFmpeg or place it next to LiteMotion.")))
}

pub fn ffmpeg() -> Result<PathBuf> {
    static P: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();
    P.get_or_init(|| find_tool("ffmpeg").map_err(|e| e.to_string()))
        .clone()
        .map_err(EngineError::NoFfmpeg)
}
pub fn ffprobe() -> Result<PathBuf> {
    static P: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();
    P.get_or_init(|| find_tool("ffprobe").map_err(|e| e.to_string()))
        .clone()
        .map_err(EngineError::NoFfmpeg)
}

/// Base FFmpeg command with safe defaults. `-protocol_whitelist file,pipe`
/// guarantees an untrusted project can never make FFmpeg open a network
/// location or a `concat:`/`subfile:` style pseudo-path.
pub fn ffmpeg_cmd() -> Result<Command> {
    let mut c = Command::new(ffmpeg()?);
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    c.stdin(Stdio::null());
    hide_console(&mut c);
    Ok(c)
}

/// Add `-i <path>` for a local file only.
pub fn input_args(cmd: &mut Command, path: &std::path::Path) -> Result<()> {
    let s = path.to_string_lossy();
    if !path.is_absolute() || s.contains('\0') || s.contains("://") {
        return Err(EngineError::Media(format!("refusing unsafe media path: {s}")));
    }
    cmd.args(["-protocol_whitelist", "file,pipe", "-i"]).arg(path);
    Ok(())
}

#[cfg(windows)]
pub fn hide_console(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
pub fn hide_console(_: &mut Command) {}

#[derive(Clone, Debug)]
pub struct Encoders {
    pub libx264: bool,
    pub openh264: bool,
    pub aac: bool,
    /// Hardware H.264 encoders FFmpeg lists AND that passed a 3-frame test.
    pub hardware: Vec<String>,
}

/// Detect usable encoders (cached). A hardware encoder is only reported if a
/// real test encode succeeded on this machine.
pub fn encoders() -> Result<&'static Encoders> {
    static E: OnceLock<Encoders> = OnceLock::new();
    if let Some(e) = E.get() {
        return Ok(e);
    }
    let out = ffmpeg_cmd()?.arg("-encoders").output().map_err(|e| EngineError::NoFfmpeg(e.to_string()))?;
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    let has = |n: &str| txt.lines().any(|l| l.split_whitespace().nth(1) == Some(n));
    let mut hardware = vec![];
    for h in ["h264_nvenc", "h264_videotoolbox", "h264_qsv", "h264_amf", "h264_vaapi", "h264_mediacodec"] {
        if has(h) && test_encoder(h) {
            hardware.push(h.to_string());
        }
    }
    let e = Encoders { libx264: has("libx264"), openh264: has("libopenh264"), aac: has("aac"), hardware };
    Ok(E.get_or_init(|| e))
}

fn test_encoder(name: &str) -> bool {
    let Ok(mut c) = ffmpeg_cmd() else { return false };
    c.args(["-f", "lavfi", "-i", "color=c=black:s=256x256:r=30", "-frames:v", "3", "-c:v", name, "-f", "null", "-"]);
    c.stdout(Stdio::null()).stderr(Stdio::null());
    c.status().map(|s| s.success()).unwrap_or(false)
}

pub fn free_space_bytes(dir: &std::path::Path) -> Option<u64> {
    fs2::available_space(dir).ok()
}

/// Windows `canonicalize` returns `\\?\C:\dir\file` paths. Not every tool
/// (FFmpeg included) copes with that prefix, so drop it when a plain drive path
/// remains. UNC shares (`\\?\UNC\...`) and everything on other systems are untouched.
pub fn simplify_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().into_owned();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            return PathBuf::from(rest);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::simplify_path;
    use std::path::PathBuf;

    #[test]
    fn strips_the_windows_extended_prefix_only_for_drive_paths() {
        assert_eq!(simplify_path(PathBuf::from(r"\\?\C:\Users\me\a.mp4")), PathBuf::from(r"C:\Users\me\a.mp4"));
        assert_eq!(simplify_path(PathBuf::from(r"\\?\UNC\server\share\a.mp4")), PathBuf::from(r"\\?\UNC\server\share\a.mp4"));
        assert_eq!(simplify_path(PathBuf::from("/home/me/a.mp4")), PathBuf::from("/home/me/a.mp4"));
        assert_eq!(simplify_path(PathBuf::from(r"C:\plain.mp4")), PathBuf::from(r"C:\plain.mp4"));
    }
}
