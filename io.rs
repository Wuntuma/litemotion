//! Project load/save: atomic writes, rolling backup, migration, validation.
use super::model::*;
use crate::error::{EngineError, Result};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROJECT_EXT: &str = "lmproj";
/// Hard cap on project file size when loading (guards against hostile files).
pub const MAX_PROJECT_BYTES: u64 = 64 * 1024 * 1024;

pub fn backup_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Write bytes atomically: temp file in the same folder -> fsync -> rename.
/// The previous good copy is kept as `<file>.bak`.
pub fn atomic_write(path: &Path, bytes: &[u8], keep_backup: bool) -> Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !dir.exists() {
        return Err(EngineError::Io(format!("folder doesn't exist: {}", dir.display())));
    }
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("project"),
        std::process::id()
    ));
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(EngineError::Io(format!("couldn't write {}: {e}", path.display())));
    }
    if keep_backup && path.exists() {
        // Best effort: a failed backup must not block saving.
        let _ = fs::copy(path, backup_path(path));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(EngineError::Io(format!("couldn't finalise {}: {e}", path.display())));
    }
    Ok(())
}

pub fn to_json(p: &Project) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(p)?)
}

pub fn save(p: &Project, path: &Path) -> Result<()> {
    p.validate()?;
    atomic_write(path, &to_json(p)?, true)
}

/// Migration chain. Each step upgrades a JSON document by exactly one version.
/// Version 1 is the first released format, so there are no steps yet; this is
/// where future steps are registered (see docs/PROJECT_FORMAT.md).
fn migrate_step(_from: u32, _v: &mut Value) -> Result<()> {
    Err(EngineError::Invalid("no migration available".into()))
}

pub fn migrate(mut v: Value) -> Result<Value> {
    let ver = v
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| EngineError::Invalid("this isn't a LiteMotion project (no format_version)".into()))?
        as u32;
    if ver > FORMAT_VERSION {
        return Err(EngineError::Invalid(format!(
            "this project was made by a newer LiteMotion (format {ver}); please update the app"
        )));
    }
    if ver == 0 {
        return Err(EngineError::Invalid("unsupported project format 0".into()));
    }
    let mut cur = ver;
    while cur < FORMAT_VERSION {
        migrate_step(cur, &mut v)?;
        cur += 1;
        v["format_version"] = Value::from(cur);
    }
    Ok(v)
}

pub fn parse(bytes: &[u8]) -> Result<Project> {
    let v: Value = serde_json::from_slice(bytes)?;
    let v = migrate(v)?;
    let p: Project = serde_json::from_value(v)?;
    p.validate()?;
    Ok(p)
}

pub fn load(path: &Path) -> Result<Project> {
    let md = fs::metadata(path).map_err(|_| EngineError::Missing(path.display().to_string()))?;
    if !md.is_file() {
        return Err(EngineError::Invalid(format!("{} is not a file", path.display())));
    }
    if md.len() > MAX_PROJECT_BYTES {
        return Err(EngineError::Invalid("project file is unreasonably large".into()));
    }
    parse(&fs::read(path)?)
}

/// Load, falling back to the `.bak` copy when the main file is damaged.
/// Returns (project, used_backup).
pub fn load_with_fallback(path: &Path) -> Result<(Project, bool)> {
    match load(path) {
        Ok(p) => Ok((p, false)),
        Err(first) => {
            let bak = backup_path(path);
            if bak.exists() {
                if let Ok(p) = load(&bak) {
                    return Ok((p, true));
                }
            }
            Err(first)
        }
    }
}

// ------------------------------------------------------- media path logic

fn normalize_rel(p: &Path) -> String {
    p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Compute a project-relative path for `media` when possible.
pub fn relative_to(project_dir: &Path, media: &Path) -> Option<String> {
    let (pd, md) = (project_dir.canonicalize().ok()?, media.canonicalize().ok()?);
    let (pc, mc): (Vec<_>, Vec<_>) = (pd.components().collect(), md.components().collect());
    let common = pc.iter().zip(mc.iter()).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None; // different drive/root
    }
    let mut out = PathBuf::new();
    for _ in common..pc.len() {
        out.push("..");
    }
    for c in &mc[common..] {
        out.push(c.as_os_str());
    }
    Some(normalize_rel(&out))
}

/// Resolve a media reference to an existing regular file, or None if missing.
/// Order: relative path (so projects survive being moved with their media),
/// then the stored absolute path. Paths that aren't plain local files
/// (URLs, protocol prefixes) are never accepted.
pub fn resolve_media(m: &MediaRef, project_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = vec![];
    if let (Some(rel), Some(dir)) = (&m.rel_path, project_dir) {
        candidates.push(dir.join(rel));
    }
    candidates.push(PathBuf::from(&m.path));
    candidates
        .into_iter()
        .filter(|p| p.is_absolute() || project_dir.is_some())
        .filter(|p| !p.to_string_lossy().contains("://"))
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
        .map(crate::platform::simplify_path)
}

pub fn missing_media(p: &Project, project_dir: Option<&Path>) -> Vec<u64> {
    p.media.iter().filter(|m| resolve_media(m, project_dir).is_none()).map(|m| m.id).collect()
}

/// Update stored paths after a Save As / move so relative paths stay valid.
pub fn refresh_media_paths(p: &mut Project, project_dir: &Path) {
    for m in &mut p.media {
        if let Some(abs) = resolve_media(m, Some(project_dir)) {
            m.rel_path = relative_to(project_dir, &abs);
            m.path = abs.to_string_lossy().into_owned();
        }
    }
}
