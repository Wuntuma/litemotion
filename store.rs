//! Local app data: settings, recent projects, autosave and crash recovery.
//! Everything lives under the OS data directory; nothing leaves the device.
use super::io::{atomic_write, parse, to_json};
use super::model::Project;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LITEMOTION_DATA_DIR") {
        return PathBuf::from(d);
    }
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("LiteMotion")
}
pub fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LITEMOTION_CACHE_DIR") {
        return PathBuf::from(d);
    }
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("LiteMotion")
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    pub autosave_secs: u64,
    /// "auto" | "low" | "medium" | "high"
    pub preview_quality: String,
    pub light_theme: bool,
    pub ui_scale: f32,
    pub onboarding_done: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Settings { autosave_secs: 60, preview_quality: "auto".into(), light_theme: false, ui_scale: 1.0, onboarding_done: false }
    }
}
impl Settings {
    pub fn load() -> Settings {
        fs::read(data_dir().join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(data_dir())?;
        atomic_write(&data_dir().join("settings.json"), &serde_json::to_vec_pretty(self)?, false)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RecentEntry {
    pub path: String,
    pub name: String,
    pub opened_unix: u64,
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn recent_list() -> Vec<RecentEntry> {
    let v: Vec<RecentEntry> =
        fs::read(data_dir().join("recent.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.into_iter().filter(|e| Path::new(&e.path).is_file()).collect()
}

pub fn recent_add(path: &Path, name: &str) -> Result<()> {
    let mut v = recent_list();
    let p = path.to_string_lossy().into_owned();
    v.retain(|e| e.path != p);
    v.insert(0, RecentEntry { path: p, name: name.into(), opened_unix: now_unix() });
    v.truncate(12);
    fs::create_dir_all(data_dir())?;
    atomic_write(&data_dir().join("recent.json"), &serde_json::to_vec_pretty(&v)?, false)
}

// ------------------------------------------------------------- autosave

#[derive(Serialize, Deserialize)]
struct AutosaveEnvelope {
    /// Where the project normally lives (None = never saved).
    origin: Option<String>,
    saved_unix: u64,
    project: Project,
}

fn autosave_dir() -> PathBuf {
    data_dir().join("autosave")
}
fn autosave_file(session: &str) -> PathBuf {
    autosave_dir().join(format!("{session}.lmauto"))
}

/// Background autosaver. `submit` clones nothing heavy (a Project is small
/// metadata) and returns immediately; serialisation and disk I/O happen on a
/// worker thread, so editing and playback are never blocked.
pub struct Autosaver {
    tx: mpsc::Sender<Msg>,
    session: String,
}
enum Msg {
    Save(Box<(Option<String>, Project)>),
    Clear,
    Stop,
}

impl Autosaver {
    pub fn start(session: &str) -> Autosaver {
        let (tx, rx) = mpsc::channel::<Msg>();
        let file = autosave_file(session);
        thread::spawn(move || {
            while let Ok(m) = rx.recv() {
                match m {
                    Msg::Save(b) => {
                        // Coalesce a burst of requests into the newest one.
                        let mut latest = b;
                        while let Ok(next) = rx.try_recv() {
                            match next {
                                Msg::Save(n) => latest = n,
                                Msg::Clear => {
                                    let _ = fs::remove_file(&file);
                                    continue;
                                }
                                Msg::Stop => return,
                            }
                        }
                        let (origin, project) = *latest;
                        let env = AutosaveEnvelope { origin, saved_unix: now_unix(), project };
                        if let Ok(bytes) = serde_json::to_vec(&env) {
                            let _ = fs::create_dir_all(autosave_dir());
                            let _ = atomic_write(&file, &bytes, false);
                        }
                    }
                    Msg::Clear => {
                        let _ = fs::remove_file(&file);
                    }
                    Msg::Stop => return,
                }
            }
        });
        Autosaver { tx, session: session.to_string() }
    }
    pub fn submit(&self, origin: Option<&Path>, p: &Project) {
        let _ = self.tx.send(Msg::Save(Box::new((origin.map(|p| p.to_string_lossy().into_owned()), p.clone()))));
    }
    /// Call after a successful manual save or a clean close.
    pub fn clear(&self) {
        let _ = self.tx.send(Msg::Clear);
    }
    pub fn session(&self) -> &str {
        &self.session
    }
}
impl Drop for Autosaver {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug, Clone)]
pub struct Recovery {
    pub file: PathBuf,
    pub origin: Option<PathBuf>,
    pub saved_unix: u64,
    pub project: Project,
}

/// Autosave files left behind by sessions that didn't exit cleanly.
pub fn find_recoveries() -> Vec<Recovery> {
    let Ok(rd) = fs::read_dir(autosave_dir()) else { return vec![] };
    let mut out = vec![];
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("lmauto") {
            continue;
        }
        let parsed = fs::read(&path).ok().and_then(|b| {
            let env: serde_json::Value = serde_json::from_slice(&b).ok()?;
            let origin = env.get("origin").and_then(|o| o.as_str()).map(PathBuf::from);
            let saved = env.get("saved_unix").and_then(|s| s.as_u64()).unwrap_or(0);
            let proj_bytes = serde_json::to_vec(env.get("project")?).ok()?;
            Some((origin, saved, parse(&proj_bytes).ok()?))
        });
        match parsed {
            Some((origin, saved_unix, project)) => out.push(Recovery { file: path, origin, saved_unix, project }),
            None => {
                let _ = fs::rename(&path, path.with_extension("lmauto.corrupt"));
            }
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.saved_unix));
    out
}

pub fn discard_recovery(r: &Recovery) {
    let _ = fs::remove_file(&r.file);
}

pub fn write_project_for_tests(p: &Project, path: &Path) -> Result<()> {
    atomic_write(path, &to_json(p)?, true)
}
