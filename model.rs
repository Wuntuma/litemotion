//! Versioned, serialisable project model. See docs/PROJECT_FORMAT.md.
use crate::animation::AnimTrack;
use crate::error::{EngineError, Result};
use crate::time::{Us, US_PER_SEC};
use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;
pub type Color = [u8; 4];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MediaRef {
    pub id: u64,
    pub kind: MediaKind,
    /// Absolute path as last seen on the machine that saved the project.
    pub path: String,
    /// Path relative to the project file's folder (forward slashes). Tried first.
    #[serde(default)]
    pub rel_path: Option<String>,
    pub duration_us: Us,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub has_audio: bool,
    /// Channel count of the first audio stream (0 = unknown).
    #[serde(default)]
    pub audio_channels: u32,
    #[serde(default)]
    pub size_bytes: u64,
    /// Optional low-resolution proxy used only for preview.
    #[serde(default)]
    pub proxy_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Overlay,
    Text,
    Audio,
}
impl TrackKind {
    pub fn is_visual(self) -> bool {
        self != TrackKind::Audio
    }
    pub fn label(self) -> &'static str {
        match self {
            TrackKind::Video => "Video",
            TrackKind::Overlay => "Graphics",
            TrackKind::Text => "Text",
            TrackKind::Audio => "Audio",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u64,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub clips: Vec<Clip>,
}
fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    #[default]
    Fit,
    Fill,
    Stretch,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    #[default]
    Rectangle,
    Ellipse,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FontId {
    #[default]
    Sans,
    SansBold,
    Serif,
    Mono,
}
impl FontId {
    pub const ALL: [FontId; 4] = [FontId::Sans, FontId::SansBold, FontId::Serif, FontId::Mono];
    pub fn label(self) -> &'static str {
        match self {
            FontId::Sans => "Sans",
            FontId::SansBold => "Sans Bold",
            FontId::Serif => "Serif",
            FontId::Mono => "Mono",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub text: String,
    #[serde(default)]
    pub font: FontId,
    /// Font size as a percentage of canvas height (resolution independent).
    pub size_pct: f32,
    pub color: Color,
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub outline: bool,
    #[serde(default)]
    pub outline_color: Color,
    #[serde(default)]
    pub shadow: bool,
    /// Typewriter reveal speed (characters per second). None = show all.
    #[serde(default)]
    pub typewriter_cps: Option<f32>,
}
impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            text: "Your title".into(),
            font: FontId::SansBold,
            size_pct: 9.0,
            color: [255, 255, 255, 255],
            align: Align::Center,
            outline: false,
            outline_color: [0, 0, 0, 255],
            shadow: true,
            typewriter_cps: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Media { media_id: u64 },
    Text(TextStyle),
    /// Size in canvas fractions.
    Shape { shape: ShapeKind, color: Color, w: f32, h: f32 },
    Solid { color: Color },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Crossfade,
    Dissolve,
    FadeFromBlack,
    FadeToBlack,
}
impl TransitionKind {
    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "Crossfade",
            TransitionKind::Dissolve => "Dissolve",
            TransitionKind::FadeFromBlack => "Fade from black",
            TransitionKind::FadeToBlack => "Fade to black",
        }
    }
    pub fn valid_in(self) -> bool {
        !matches!(self, TransitionKind::FadeToBlack)
    }
    pub fn valid_out(self) -> bool {
        matches!(self, TransitionKind::FadeToBlack)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration: Us,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ColorAdjust {
    pub brightness: f32, // -1..1, 0 = none
    pub contrast: f32,   // 0..2, 1 = none
    pub saturation: f32, // 0..3, 1 = none
}
impl Default for ColorAdjust {
    fn default() -> Self {
        ColorAdjust { brightness: 0.0, contrast: 1.0, saturation: 1.0 }
    }
}
impl ColorAdjust {
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 1.0 && self.saturation == 1.0
    }
}

/// Crop as fractions removed from each edge of the source.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Crop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Clip {
    pub id: u64,
    pub content: Content,
    /// Timeline start and length.
    pub start: Us,
    pub duration: Us,
    /// Offset into the source media where playback begins (source time).
    #[serde(default)]
    pub source_in: Us,
    /// Playback speed (1.0 = normal). Source span consumed = duration * speed.
    #[serde(default = "one")]
    pub speed: f32,
    // Static transform (canvas fractions; 0,0 = centred). Keyframes override.
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub rotation: f32,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "one")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub fade_in: Us,
    #[serde(default)]
    pub fade_out: Us,
    #[serde(default)]
    pub fit: FitMode,
    #[serde(default)]
    pub crop: Crop,
    #[serde(default)]
    pub color: ColorAdjust,
    #[serde(default)]
    pub transition_in: Option<Transition>,
    #[serde(default)]
    pub transition_out: Option<Transition>,
    #[serde(default)]
    pub keyframes: Vec<AnimTrack>,
}
fn one<T: From<u8>>() -> T {
    T::from(1)
}

impl Clip {
    pub fn new(id: u64, content: Content, start: Us, duration: Us) -> Clip {
        Clip {
            id,
            content,
            start,
            duration,
            source_in: 0,
            speed: 1.0,
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            opacity: 1.0,
            volume: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
            fit: FitMode::Fit,
            crop: Crop::default(),
            color: ColorAdjust::default(),
            transition_in: None,
            transition_out: None,
            keyframes: vec![],
        }
    }
    pub fn end(&self) -> Us {
        self.start + self.duration
    }
    pub fn media_id(&self) -> Option<u64> {
        if let Content::Media { media_id } = self.content {
            Some(media_id)
        } else {
            None
        }
    }
    /// Source time consumed by this clip.
    pub fn source_span(&self) -> Us {
        (self.duration as f64 * self.speed as f64).round() as Us
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// 0 (smallest) .. 100 (best). Mapped to x264 CRF.
    pub quality: u8,
    pub include_audio: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub next_id: u64,
    #[serde(default)]
    pub media: Vec<MediaRef>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    pub export: ExportSettings,
}

/// Practical size caps, applied when loading untrusted project files.
pub const MAX_DIMENSION: u32 = 8192;
pub const MAX_CLIPS: usize = 20_000;
pub const MAX_TEXT_LEN: usize = 10_000;
pub const MAX_KEYS_PER_TRACK: usize = 10_000;

impl Project {
    pub fn new(name: &str, width: u32, height: u32, fps: f64) -> Project {
        let mut p = Project {
            format_version: FORMAT_VERSION,
            name: name.to_string(),
            width,
            height,
            fps,
            next_id: 1,
            media: vec![],
            tracks: vec![],
            export: ExportSettings { width, height, fps, quality: 70, include_audio: true },
        };
        // A sensible starting layout: text above graphics above video, plus music.
        for (kind, name) in [
            (TrackKind::Video, "Video 1"),
            (TrackKind::Overlay, "Graphics 1"),
            (TrackKind::Text, "Text 1"),
            (TrackKind::Audio, "Audio 1"),
        ] {
            p.add_track(kind, name);
        }
        p
    }
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    pub fn add_track(&mut self, kind: TrackKind, name: &str) -> u64 {
        let id = self.alloc_id();
        let t = Track { id, kind, name: name.into(), visible: true, muted: false, clips: vec![] };
        // Keep audio tracks after visual ones so z-order = list order for visuals.
        if kind.is_visual() {
            let pos = self.tracks.iter().rposition(|t| t.kind.is_visual()).map(|i| i + 1).unwrap_or(0);
            self.tracks.insert(pos, t);
        } else {
            self.tracks.push(t);
        }
        id
    }
    pub fn media(&self, id: u64) -> Option<&MediaRef> {
        self.media.iter().find(|m| m.id == id)
    }
    pub fn duration(&self) -> Us {
        self.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.end()).max().unwrap_or(0)
    }
    pub fn frame_count(&self) -> i64 {
        (self.duration() as f64 * self.fps / US_PER_SEC as f64).ceil() as i64
    }
    pub fn find_clip(&self, id: u64) -> Option<(usize, usize)> {
        for (ti, t) in self.tracks.iter().enumerate() {
            if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
                return Some((ti, ci));
            }
        }
        None
    }
    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.find_clip(id).map(|(t, c)| &self.tracks[t].clips[c])
    }
    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> {
        self.find_clip(id).map(|(t, c)| &mut self.tracks[t].clips[c])
    }

    /// Structural validation. Run on every load so malformed or hostile
    /// project files are rejected with a clear message instead of crashing.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(EngineError::Invalid(m));
        if self.width < 16 || self.height < 16 || self.width > MAX_DIMENSION || self.height > MAX_DIMENSION {
            return bad(format!("canvas size {}x{} is outside 16..{MAX_DIMENSION}", self.width, self.height));
        }
        if !(self.fps.is_finite() && (1.0..=240.0).contains(&self.fps)) {
            return bad(format!("frame rate {} is outside 1..240", self.fps));
        }
        let total: usize = self.tracks.iter().map(|t| t.clips.len()).sum();
        if total > MAX_CLIPS {
            return bad(format!("too many clips ({total})"));
        }
        let mut ids = std::collections::HashSet::new();
        for m in &self.media {
            if !ids.insert(m.id) {
                return bad(format!("duplicate id {}", m.id));
            }
            if m.path.is_empty() || m.path.contains('\0') {
                return bad("a media entry has an invalid path".into());
            }
            if m.duration_us < 0 {
                return bad("negative media duration".into());
            }
        }
        for t in &self.tracks {
            if !ids.insert(t.id) {
                return bad(format!("duplicate id {}", t.id));
            }
            for c in &t.clips {
                if !ids.insert(c.id) {
                    return bad(format!("duplicate id {}", c.id));
                }
                let nums = [c.x, c.y, c.scale, c.rotation, c.opacity, c.volume, c.speed];
                if nums.iter().any(|n| !n.is_finite()) {
                    return bad(format!("clip {} has a non-finite number", c.id));
                }
                if c.duration <= 0 || c.start < 0 || c.source_in < 0 {
                    return bad(format!("clip {} has an invalid time range", c.id));
                }
                if !(0.05..=16.0).contains(&c.speed) {
                    return bad(format!("clip {} has speed {} outside 0.05..16", c.id, c.speed));
                }
                match &c.content {
                    Content::Media { media_id } => {
                        let Some(m) = self.media(*media_id) else {
                            return bad(format!("clip {} refers to unknown media {media_id}", c.id));
                        };
                        let ok = match (t.kind, m.kind) {
                            (TrackKind::Audio, MediaKind::Audio) => true,
                            (TrackKind::Audio, MediaKind::Video) => true,
                            (TrackKind::Video | TrackKind::Overlay, MediaKind::Video | MediaKind::Image) => true,
                            _ => false,
                        };
                        if !ok {
                            return bad(format!("clip {} is on the wrong kind of track", c.id));
                        }
                    }
                    Content::Text(s) => {
                        if s.text.len() > MAX_TEXT_LEN || !s.size_pct.is_finite() {
                            return bad(format!("clip {} has invalid text", c.id));
                        }
                    }
                    _ => {}
                }
                for a in &c.keyframes {
                    if a.keys.len() > MAX_KEYS_PER_TRACK
                        || a.keys.windows(2).any(|w| w[0].t >= w[1].t)
                        || a.keys.iter().any(|k| !k.v.is_finite())
                    {
                        return bad(format!("clip {} has invalid keyframes", c.id));
                    }
                }
            }
        }
        Ok(())
    }
}
