use thiserror::Error;

pub type Result<T> = std::result::Result<T, EngineError>;

/// Errors carry plain-language messages that the UI shows directly.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("{0}")]
    Invalid(String),
    #[error("File not found: {0}")]
    Missing(String),
    #[error("Couldn't read or write a file: {0}")]
    Io(String),
    #[error("This file isn't supported or is damaged: {0}")]
    Media(String),
    #[error("FFmpeg isn't available: {0}")]
    NoFfmpeg(String),
    #[error("Not enough disk space: need about {need_mb} MB, only {have_mb} MB free")]
    DiskSpace { need_mb: u64, have_mb: u64 },
    #[error("The output file already exists: {0}")]
    OutputExists(String),
    #[error("Export failed: {0}")]
    Export(String),
    #[error("Cancelled")]
    Cancelled,
    #[error("Edit not possible: {0}")]
    Edit(String),
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e.to_string())
    }
}
impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::Invalid(format!("project data is malformed ({e})"))
    }
}
