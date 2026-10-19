//! LiteMotion engine: project model, timeline editing, animation, media
//! inspection, software compositor, audio mixer and FFmpeg-backed export.
//! No GUI code lives here so every feature is testable headlessly.

pub mod animation;
pub mod audio;
pub mod error;
pub mod media;
pub mod platform;
pub mod project;
pub mod rendering;
pub mod time;
pub mod timeline;

pub use error::{EngineError, Result};
