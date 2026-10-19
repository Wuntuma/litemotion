use litemotion_engine::project::model::*;
use std::path::Path;

pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(79, 140, 255);

pub fn clip_label(p: &Project, c: &Clip) -> String {
    match &c.content {
        Content::Media { media_id } => p
            .media(*media_id)
            .and_then(|m| Path::new(&m.path).file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Missing media".into()),
        Content::Text(t) => t.text.lines().next().unwrap_or("Text").to_string(),
        Content::Shape { shape, .. } => match shape {
            ShapeKind::Rectangle => "Rectangle".into(),
            ShapeKind::Ellipse => "Circle".into(),
        },
        Content::Solid { .. } => "Background".into(),
    }
}

pub fn clip_color(p: &Project, c: &Clip, dark: bool) -> egui::Color32 {
    let (r, g, b) = match &c.content {
        Content::Media { media_id } => match p.media(*media_id).map(|m| m.kind) {
            Some(MediaKind::Audio) => (120, 92, 190),
            Some(MediaKind::Image) => (58, 150, 110),
            _ => (62, 110, 190),
        },
        Content::Text(_) => (200, 130, 50),
        Content::Shape { .. } => (50, 150, 160),
        Content::Solid { .. } => (110, 110, 120),
    };
    if dark {
        egui::Color32::from_rgb(r, g, b)
    } else {
        egui::Color32::from_rgb((r as u16 * 3 / 4 + 60) as u8, (g as u16 * 3 / 4 + 60) as u8, (b as u16 * 3 / 4 + 60) as u8)
    }
}

pub fn fmt_secs(s: f32) -> String {
    let t = s.max(0.0) as u32;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// Open a file or folder with the system's default application.
pub fn open_externally(path: &Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

pub fn reveal_folder(file: &Path) {
    if let Some(dir) = file.parent() {
        open_externally(dir);
    }
}

/// (label, short-side) export resolutions.
pub const RESOLUTIONS: &[(&str, u32)] = &[("720p", 720), ("1080p", 1080), ("4K (needs a powerful computer)", 2160)];
pub const FPS_CHOICES: &[f64] = &[24.0, 25.0, 30.0, 50.0, 60.0];

/// Output size for a target short side, keeping the project's aspect ratio.
pub fn size_for(pw: u32, ph: u32, short: u32) -> (u32, u32) {
    let s = short as f64 / pw.min(ph) as f64;
    ((((pw as f64 * s).round() as u32) & !1).max(16), (((ph as f64 * s).round() as u32) & !1).max(16))
}

pub struct AspectPreset {
    pub label: &'static str,
    pub w: u32,
    pub h: u32,
}
pub const ASPECTS: &[AspectPreset] = &[
    AspectPreset { label: "Vertical 9:16", w: 9, h: 16 },
    AspectPreset { label: "Square 1:1", w: 1, h: 1 },
    AspectPreset { label: "Landscape 16:9", w: 16, h: 9 },
    AspectPreset { label: "Portrait 4:5", w: 4, h: 5 },
];
