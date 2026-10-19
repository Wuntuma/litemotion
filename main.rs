//! LiteMotion Studio: the desktop editor. All editing logic lives in
//! `litemotion-engine`; this crate only draws it and talks to the workers.
mod preview_audio;
mod props_ui;
mod timeline_ui;
mod util;
mod worker;
#[cfg(test)]
mod smoke_test;

use egui::{pos2, vec2, Align2, Color32, Key, Modifiers, Rect, Sense, TextureHandle, TextureOptions};
use litemotion_engine::animation::Interp;
use litemotion_engine::media::{self, ImportMode};
use litemotion_engine::project::io;
use litemotion_engine::project::model::*;
use litemotion_engine::project::store::{self, Autosaver, Recovery, Settings};
use litemotion_engine::rendering::export::*;
use litemotion_engine::time::*;
use litemotion_engine::timeline::editor::Editor;
use litemotion_engine::EngineError;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use timeline_ui::Drag;
use util::*;
use worker::*;

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Home,
    Edit,
}

#[derive(PartialEq, Clone, Copy)]
enum Drawer {
    None,
    Media,
    Props,
}

#[derive(Clone, Copy, PartialEq)]
enum Next {
    Home,
    Quit,
}

struct NewForm {
    name: String,
    aspect: usize, // index into ASPECTS; ASPECTS.len() = custom
    short: u32,
    fps: usize,
    cw: u32,
    ch: u32,
}

struct ExportUi {
    open: bool,
    res: usize,
    fps: usize,
    quality: u8,
    audio: bool,
    handle: Option<ExportHandle>,
    result: Option<std::result::Result<ExportReport, String>>,
}

pub struct App {
    screen: Screen,
    pub ed: Option<Editor>,
    path: Option<PathBuf>,
    pub playhead: Us,
    playing: bool,
    pub zoom: f32,
    pub scroll_x: f32,
    pub drag: Option<Drag>,
    pub snap: bool,
    pub compact: bool,
    pub thumbs: HashMap<u64, TextureHandle>,
    pub waves: HashMap<u64, Vec<f32>>,
    requested: HashSet<u64>,
    pub confirm_remove_track: Option<u64>,
    settings: Settings,
    preview: PreviewWorker,
    audio: preview_audio::PreviewAudio,
    sound_on: bool,
    jobs: JobsWorker,
    tex: Option<TextureHandle>,
    tag: u64,
    last_key: Option<(u64, Us, u32, String)>,
    proj_arc: Option<(u64, Arc<Project>)>,
    autosaver: Autosaver,
    last_autosave: (u64, Instant),
    err: Option<String>,
    form: NewForm,
    export: ExportUi,
    recoveries: Vec<Recovery>,
    missing: Vec<u64>,
    drawer: Drawer,
    search: String,
    sort_newest: bool,
    copy_media: bool,
    guides: bool,
    fullscreen: bool,
    settings_open: bool,
    show_help: bool,
    leave: Option<Next>,
    interp: Interp,
    title: String,
    recent: Vec<store::RecentEntry>,
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_min_inner_size([360.0, 480.0]).with_title("LiteMotion Studio"),
        ..Default::default()
    };
    eframe::run_native("LiteMotion Studio", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

impl App {
    fn new(cc: &eframe::CreationContext) -> App {
        let mut app = App::build(&cc.egui_ctx);
        // `litemotion project.lmproj` (or double-clicking a project) opens it directly.
        if let Some(arg) = std::env::args_os().nth(1) {
            app.open_project(Path::new(&arg));
        }
        app
    }

    fn build(egui_ctx: &egui::Context) -> App {
        let settings = Settings::load();
        let ctx = egui_ctx.clone();
        let app = App {
            screen: Screen::Home,
            ed: None,
            path: None,
            playhead: 0,
            playing: false,
            zoom: 80.0,
            scroll_x: 0.0,
            drag: None,
            snap: true,
            compact: false,
            thumbs: HashMap::new(),
            waves: HashMap::new(),
            requested: HashSet::new(),
            confirm_remove_track: None,
            preview: PreviewWorker::start(ctx.clone()),
            audio: Default::default(),
            sound_on: true,
            jobs: JobsWorker::start(ctx),
            tex: None,
            tag: 0,
            last_key: None,
            proj_arc: None,
            autosaver: Autosaver::start(&format!("s{}", store::now_unix())),
            last_autosave: (0, Instant::now()),
            err: None,
            form: NewForm { name: "My video".into(), aspect: 2, short: 1080, fps: 2, cw: 1920, ch: 1080 },
            export: ExportUi { open: false, res: 1, fps: 2, quality: 70, audio: true, handle: None, result: None },
            recoveries: store::find_recoveries(),
            missing: vec![],
            drawer: Drawer::None,
            search: String::new(),
            sort_newest: false,
            copy_media: false,
            guides: false,
            fullscreen: false,
            settings_open: false,
            show_help: !settings.onboarding_done,
            leave: None,
            interp: Interp::EaseInOut,
            title: String::new(),
            recent: store::recent_list(),
            settings,
        };
        app.apply_theme(egui_ctx);
        app
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals(if self.settings.light_theme { egui::Visuals::light() } else { egui::Visuals::dark() });
        ctx.set_zoom_factor(self.settings.ui_scale.clamp(0.8, 2.0));
    }

    pub fn error(&mut self, s: String) {
        self.err = Some(s);
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    pub fn seek(&mut self, t: Us) {
        if self.playing {
            self.stop_playback();
        }
        self.playhead = t.max(0);
    }

    fn stop_playback(&mut self) {
        self.playing = false;
        self.audio.stop();
        self.preview.send(PreviewReq::Stop);
        self.last_key = None;
    }

    // ------------------------------------------------------------ projects

    fn set_editor(&mut self, project: Project, path: Option<PathBuf>, dirty: bool) {
        self.jobs.cancel_all();
        self.stop_playback();
        self.thumbs.clear();
        self.waves.clear();
        self.requested.clear();
        self.tex = None;
        self.playhead = 0;
        self.scroll_x = 0.0;
        let dir = path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        self.missing = io::missing_media(&project, dir.as_deref());
        let mut ed = Editor::new(project);
        ed.dirty = dirty;
        self.ed = Some(ed);
        self.path = path;
        self.screen = Screen::Edit;
        self.last_autosave = (0, Instant::now());
        self.queue_media_jobs();
    }

    fn new_project(&mut self) {
        let f = &self.form;
        let (w, h) = if f.aspect < ASPECTS.len() { size_for(ASPECTS[f.aspect].w, ASPECTS[f.aspect].h, f.short) } else { (f.cw.clamp(64, 7680) & !1, f.ch.clamp(64, 7680) & !1) };
        let fps = FPS_CHOICES[f.fps];
        let name = if f.name.trim().is_empty() { "Untitled".to_string() } else { f.name.trim().to_string() };
        let mut p = Project::new(&name, w, h, fps);
        p.add_track(TrackKind::Video, "Video 1");
        p.add_track(TrackKind::Overlay, "Graphics 1");
        p.add_track(TrackKind::Text, "Text 1");
        p.add_track(TrackKind::Audio, "Audio 1");
        self.set_editor(p, None, true);
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("LiteMotion project", &["lmproj"]).pick_file() {
            self.open_project(&p);
        }
    }

    fn open_project(&mut self, path: &Path) {
        match io::load_with_fallback(path) {
            Ok((p, used_backup)) => {
                let name = p.name.clone();
                self.set_editor(p, Some(path.to_path_buf()), false);
                let _ = store::recent_add(path, &name);
                self.recent = store::recent_list();
                if used_backup {
                    self.error("The project file was damaged, so the previous saved copy was opened instead. Recent edits may be missing.".into());
                }
            }
            Err(e) => self.error(format!("Couldn't open this project: {e}")),
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let Some(ed) = self.ed.as_mut() else { return false };
        let mut path = self.path.clone();
        if save_as || path.is_none() {
            let name = format!("{}.lmproj", ed.project.name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"));
            match rfd::FileDialog::new().set_file_name(&name).add_filter("LiteMotion project", &["lmproj"]).save_file() {
                Some(p) => path = Some(p.with_extension("lmproj")),
                None => return false,
            }
        }
        let path = path.unwrap();
        if let Some(dir) = path.parent() {
            io::refresh_media_paths(&mut ed.project, dir);
        }
        match io::save(&ed.project, &path) {
            Ok(()) => {
                ed.mark_saved();
                let name = ed.project.name.clone();
                self.path = Some(path.clone());
                self.autosaver.clear();
                let _ = store::recent_add(&path, &name);
                self.recent = store::recent_list();
                true
            }
            Err(e) => {
                self.error(format!("Couldn't save: {e}. Your project is still open; try another location."));
                false
            }
        }
    }

    fn dirty(&self) -> bool {
        self.ed.as_ref().map_or(false, |e| e.dirty)
    }

    // ------------------------------------------------------------ media

    fn import_paths(&mut self, paths: Vec<PathBuf>) {
        let dir = self.project_dir();
        let mode = if self.copy_media { ImportMode::Copy } else { ImportMode::Reference };
        let assets = dir.as_ref().map(|d| d.join("media"));
        let mut errors = vec![];
        let Some(ed) = self.ed.as_mut() else { return };
        for p in paths {
            let r = ed.edit("Import media", |pr| media::import(pr, &p, mode, dir.as_deref(), assets.as_deref()));
            if let Err(e) = r {
                errors.push(format!("{}: {e}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
        }
        if !errors.is_empty() {
            self.error(format!("Some files couldn't be imported:\n{}", errors.join("\n")));
        }
        self.queue_media_jobs();
    }

    fn queue_media_jobs(&mut self) {
        let dir = self.project_dir();
        let Some(ed) = self.ed.as_ref() else { return };
        for m in &ed.project.media {
            if self.requested.contains(&m.id) {
                continue;
            }
            let Some(path) = io::resolve_media(m, dir.as_deref()) else { continue };
            self.requested.insert(m.id);
            match m.kind {
                MediaKind::Audio => self.jobs.submit(Job::Wave { id: m.id, path, dur: m.duration_us }),
                k => {
                    let at = if k == MediaKind::Video { (m.duration_us / 4).min(secs(1.0)) } else { 0 };
                    self.jobs.submit(Job::Thumb { id: m.id, path: path.clone(), kind: k, w: m.width, h: m.height, at });
                }
            }
        }
    }

    fn add_media_clip(&mut self, media_id: u64) {
        let ph = self.playhead;
        let Some(ed) = self.ed.as_mut() else { return };
        let r = ed.edit("Add to timeline", |p| {
            let kind = match p.media(media_id).map(|m| m.kind) {
                Some(MediaKind::Audio) => TrackKind::Audio,
                Some(MediaKind::Image) => TrackKind::Overlay,
                _ => TrackKind::Video,
            };
            let clip = p.media_clip(media_id, ph)?;
            let tid = p.track_with_room(kind, ph, clip.duration);
            p.add_clip(tid, clip, true)
        });
        match r {
            Ok(id) => ed.selection = vec![id],
            Err(e) => self.error(e.to_string()),
        }
    }

    fn add_generated(&mut self, what: &str) {
        let ph = self.playhead;
        let Some(ed) = self.ed.as_mut() else { return };
        let r = ed.edit(&format!("Add {what}"), |p| {
            let (kind, content, dur) = match what {
                "title" => (TrackKind::Text, Content::Text(TextStyle::default()), secs(3.0)),
                "subtitle" => (TrackKind::Text, Content::Text(TextStyle { text: "Your caption".into(), font: FontId::Sans, size_pct: 5.0, ..TextStyle::default() }), secs(3.0)),
                "square" => (TrackKind::Overlay, Content::Shape { shape: ShapeKind::Rectangle, color: [79, 140, 255, 255], w: 0.3, h: 0.3 }, secs(3.0)),
                "circle" => (TrackKind::Overlay, Content::Shape { shape: ShapeKind::Ellipse, color: [255, 120, 80, 255], w: 0.3, h: 0.3 }, secs(3.0)),
                _ => (TrackKind::Video, Content::Solid { color: [30, 30, 40, 255] }, secs(5.0)),
            };
            let id = p.alloc_id();
            let clip = Clip::new(id, content, ph, dur);
            let tid = p.track_with_room(kind, ph, dur);
            p.add_clip(tid, clip, true)
        });
        match r {
            Ok(id) => ed.selection = vec![id],
            Err(e) => self.error(e.to_string()),
        }
    }

    // ------------------------------------------------------------ timeline actions

    pub fn action_split(&mut self) -> Result<(), EngineError> {
        let ph = self.playhead;
        let Some(ed) = self.ed.as_mut() else { return Ok(()) };
        let ids: Vec<u64> = ed.selection.iter().copied().filter(|id| ed.project.clip(*id).map_or(false, |c| ph > c.start && ph < c.end())).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let new = ed.edit("Split", |p| ids.iter().map(|id| p.split_clip(*id, ph)).collect::<Result<Vec<_>, _>>())?;
        ed.selection.extend(new);
        Ok(())
    }

    pub fn action_delete(&mut self) {
        let Some(ed) = self.ed.as_mut() else { return };
        let ids = ed.selection.clone();
        if ids.is_empty() {
            return;
        }
        let r = ed.edit("Delete", |p| ids.iter().try_for_each(|id| p.delete_clip(*id)));
        ed.selection.clear();
        if let Err(e) = r {
            self.error(e.to_string());
        }
    }

    pub fn action_duplicate(&mut self) {
        let Some(ed) = self.ed.as_mut() else { return };
        let ids = ed.selection.clone();
        let r = ed.edit("Duplicate", |p| ids.iter().map(|id| p.duplicate_clip(*id)).collect::<Result<Vec<_>, _>>());
        match r {
            Ok(new) if !new.is_empty() => ed.selection = new,
            Ok(_) => {}
            Err(e) => self.error(e.to_string()),
        }
    }

    pub fn action_paste(&mut self) {
        let ph = self.playhead;
        let Some(ed) = self.ed.as_mut() else { return };
        let clips = ed.clipboard.clone();
        if clips.is_empty() {
            return;
        }
        match ed.edit("Paste", |p| p.paste_clips(&clips, ph)) {
            Ok(new) => ed.selection = new,
            Err(e) => self.error(e.to_string()),
        }
    }

    // ------------------------------------------------------------ background results

    fn poll_workers(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.jobs.rx.try_recv() {
            match r {
                JobResult::Thumb(id, t) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([t.w as usize, t.h as usize], &t.rgba);
                    self.thumbs.insert(id, ctx.load_texture(format!("thumb{id}"), img, TextureOptions::LINEAR));
                }
                JobResult::Wave(id, w) => {
                    self.waves.insert(id, w);
                }
                JobResult::Failed(..) => {} // a missing preview thumbnail isn't worth a dialog
            }
        }
        while let Ok(f) = self.preview.rx.try_recv() {
            if f.tag != self.tag {
                continue;
            }
            if f.ended {
                self.playing = false;
                self.audio.stop();
                self.last_key = None;
                continue;
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([f.bmp.w as usize, f.bmp.h as usize], &f.bmp.data);
            match &mut self.tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("preview", img, TextureOptions::LINEAR)),
            }
            if f.playing {
                self.playhead = f.t;
            }
        }
    }

    fn preview_size(&self) -> (u32, u32) {
        let Some(ed) = self.ed.as_ref() else { return (320, 180) };
        let short = match self.settings.preview_quality.as_str() {
            "low" => 240,
            "high" => 720,
            _ => 480,
        };
        let p = &ed.project;
        size_for(p.width, p.height, short.min(p.width.min(p.height)))
    }

    fn preview_params(&mut self) -> Option<PreviewParams> {
        let rev = self.ed.as_ref()?.revision;
        if !matches!(&self.proj_arc, Some((r, _)) if *r == rev) {
            self.proj_arc = Some((rev, Arc::new(self.ed.as_ref()?.project.clone())));
        }
        let (w, h) = self.preview_size();
        Some(PreviewParams { proj: self.proj_arc.as_ref()?.1.clone(), dir: self.project_dir(), w, h, use_proxy: false, auto_quality: self.settings.preview_quality == "auto" })
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.stop_playback();
            return;
        }
        let Some(ed) = self.ed.as_ref() else { return };
        if ed.project.duration() == 0 {
            return;
        }
        if self.playhead >= ed.project.duration() {
            self.playhead = 0;
        }
        if let Some(p) = self.preview_params() {
            self.tag += 1;
            self.playing = true;
            let clock = if self.sound_on { self.audio.start(p.proj.clone(), p.dir.clone(), self.playhead) } else { None };
            self.preview.send(PreviewReq::Play { p, t0: self.playhead, tag: self.tag, audio: clock });
        }
    }

    fn request_still(&mut self) {
        let Some(rev) = self.ed.as_ref().map(|e| e.revision) else { return };
        if self.playing {
            if self.last_key.as_ref().map_or(false, |k| k.0 != rev) {
                self.stop_playback(); // edited during playback
            } else {
                return;
            }
        }
        let key = (rev, self.playhead, self.preview_size().0, self.settings.preview_quality.clone());
        if self.last_key.as_ref() == Some(&key) {
            return;
        }
        self.last_key = Some(key);
        if let Some(p) = self.preview_params() {
            self.tag += 1;
            self.preview.send(PreviewReq::Still { p, t: self.playhead, tag: self.tag });
        }
    }

    fn autosave_tick(&mut self) {
        let Some(ed) = self.ed.as_ref() else { return };
        if ed.dirty && ed.revision != self.last_autosave.0 && self.last_autosave.1.elapsed().as_secs() >= self.settings.autosave_secs.max(5) {
            self.autosaver.submit(self.path.as_deref(), &ed.project);
            self.last_autosave = (ed.revision, Instant::now());
        }
    }

    // ------------------------------------------------------------ keyboard

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() || self.ed.is_none() || self.screen != Screen::Edit {
            return;
        }
        let (redo, undo, save, dup, copy, paste, space, del, split, left, right, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::D),
                i.consume_key(Modifiers::COMMAND, Key::C),
                i.consume_key(Modifiers::COMMAND, Key::V),
                i.consume_key(Modifiers::NONE, Key::Space),
                i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace),
                i.consume_key(Modifiers::NONE, Key::S),
                i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                i.consume_key(Modifiers::NONE, Key::ArrowRight),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let fps = self.ed.as_ref().map_or(30.0, |e| e.project.fps);
        let step = frame_to_us(1, fps);
        if redo {
            self.do_redo();
        } else if undo {
            self.do_undo();
        }
        if save {
            self.save(false);
        }
        if dup {
            self.action_duplicate();
        }
        if copy {
            if let Some(ed) = self.ed.as_mut() {
                ed.copy_selection();
            }
        }
        if paste {
            self.action_paste();
        }
        if space {
            self.toggle_play();
        }
        if del {
            self.action_delete();
        }
        if split {
            if let Err(e) = self.action_split() {
                self.error(e.to_string());
            }
        }
        if left {
            self.seek(self.playhead - step);
        }
        if right {
            self.seek(self.playhead + step);
        }
        if esc && self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
    }

    fn do_undo(&mut self) {
        if let Some(ed) = self.ed.as_mut() {
            ed.undo();
        }
    }
    fn do_redo(&mut self) {
        if let Some(ed) = self.ed.as_mut() {
            ed.redo();
        }
    }
    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    // ------------------------------------------------------------ screens

    fn home_ui(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading(egui::RichText::new("LiteMotion Studio").size(30.0));
            ui.label("Simple video editing and animation that works offline.");
            ui.add_space(16.0);
        });
        let w = ui.available_width().min(560.0);
        ui.vertical_centered(|ui| {
            ui.allocate_ui(vec2(w, 0.0), |ui| {
                ui.group(|ui| {
                    ui.set_width(w - 12.0);
                    ui.strong("New project");
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut self.form.name);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Shape");
                        let cur = if self.form.aspect < ASPECTS.len() { ASPECTS[self.form.aspect].label } else { "Custom" };
                        egui::ComboBox::from_id_salt("aspect").selected_text(cur).show_ui(ui, |ui| {
                            for (i, a) in ASPECTS.iter().enumerate() {
                                ui.selectable_value(&mut self.form.aspect, i, a.label);
                            }
                            ui.selectable_value(&mut self.form.aspect, ASPECTS.len(), "Custom size");
                        });
                        if self.form.aspect < ASPECTS.len() {
                            egui::ComboBox::from_id_salt("res").selected_text(format!("{}p", self.form.short)).show_ui(ui, |ui| {
                                for (l, s) in RESOLUTIONS.iter().take(2) {
                                    ui.selectable_value(&mut self.form.short, *s, *l);
                                }
                            });
                        } else {
                            ui.add(egui::DragValue::new(&mut self.form.cw).range(64..=7680).suffix(" px wide"));
                            ui.add(egui::DragValue::new(&mut self.form.ch).range(64..=7680).suffix(" px high"));
                        }
                        egui::ComboBox::from_id_salt("fps").selected_text(format!("{} fps", FPS_CHOICES[self.form.fps])).show_ui(ui, |ui| {
                            for (i, f) in FPS_CHOICES.iter().enumerate() {
                                ui.selectable_value(&mut self.form.fps, i, format!("{f} fps"));
                            }
                        });
                    });
                    if ui.add_sized([ui.available_width(), 38.0], egui::Button::new("Create project")).clicked() {
                        self.new_project();
                    }
                });
                ui.add_space(8.0);
                if ui.add_sized([w, 38.0], egui::Button::new("Open project...")).clicked() {
                    self.open_dialog();
                }
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.set_width(w - 12.0);
                    ui.strong("Continue recent project");
                    let recent = self.recent.clone();
                    if recent.is_empty() {
                        ui.weak("Nothing here yet.");
                    }
                    for r in recent.iter().take(6) {
                        let exists = Path::new(&r.path).exists();
                        if ui.add_enabled(exists, egui::Button::new(&r.name).min_size(vec2(ui.available_width(), 32.0))).on_hover_text(&r.path).clicked() {
                            self.open_project(Path::new(&r.path));
                        }
                    }
                });
            });
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let big = if self.compact { vec2(44.0, 36.0) } else { vec2(0.0, 24.0) };
            if ui.add(egui::Button::new("Home").min_size(big)).clicked() {
                if self.dirty() {
                    self.leave = Some(Next::Home);
                } else {
                    self.go_home();
                }
            }
            if ui.add(egui::Button::new("Open").min_size(big)).clicked() {
                self.open_dialog();
            }
            if ui.add(egui::Button::new("Save").min_size(big)).on_hover_text("Ctrl+S").clicked() {
                self.save(false);
            }
            let (cu, cr) = self.ed.as_ref().map_or((false, false), |e| (e.can_undo(), e.can_redo()));
            if ui.add_enabled(cu, egui::Button::new("Undo").min_size(big)).on_hover_text("Ctrl+Z").clicked() {
                self.do_undo();
            }
            if ui.add_enabled(cr, egui::Button::new("Redo").min_size(big)).on_hover_text("Ctrl+Shift+Z").clicked() {
                self.do_redo();
            }
            if ui.add(egui::Button::new("Import").min_size(big)).on_hover_text("Add videos, pictures or music (you can also drop files here)").clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("Media", &["mp4", "mov", "webm", "mkv", "wav", "mp3", "aac", "m4a", "flac", "png", "jpg", "jpeg", "webp"]).pick_files() {
                    self.import_paths(files);
                }
            }
            if ui.add(egui::Button::new("Export").fill(ACCENT).min_size(big)).clicked() {
                self.export.open = true;
                self.export.result = None;
                if let Some(ed) = self.ed.as_ref() {
                    self.export.audio = ed.project.export.include_audio;
                    self.export.quality = ed.project.export.quality;
                }
            }
            if ui.add(egui::Button::new("Settings").min_size(big)).clicked() {
                self.settings_open = true;
            }
            if self.compact {
                ui.separator();
                ui.selectable_value(&mut self.drawer, Drawer::Media, "Media");
                ui.selectable_value(&mut self.drawer, Drawer::Props, "Edit");
                if ui.button("Hide").clicked() {
                    self.drawer = Drawer::None;
                }
            }
            if let Some(ed) = self.ed.as_ref() {
                ui.weak(format!("{}{}", ed.project.name, if ed.dirty { " *" } else { "" }));
            }
        });
    }

    fn go_home(&mut self) {
        self.stop_playback();
        self.jobs.cancel_all();
        self.autosaver.clear();
        self.ed = None;
        self.path = None;
        self.screen = Screen::Home;
        self.recent = store::recent_list();
    }

    fn media_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search").desired_width(ui.available_width() - 80.0));
            ui.checkbox(&mut self.sort_newest, "Newest");
        });
        ui.checkbox(&mut self.copy_media, "Copy files into the project folder").on_hover_text("Off: the project points at your original files. On: copies them next to the saved project. Save the project first.");
        let mut add: Option<u64> = None;
        let mut gen: Option<&'static str> = None;
        let q = self.search.to_lowercase();
        let dir = self.project_dir();
        let mut items: Vec<MediaRef> = self.ed.as_ref().map(|e| e.project.media.clone()).unwrap_or_default();
        items.retain(|m| Path::new(&m.path).file_name().map_or(false, |n| n.to_string_lossy().to_lowercase().contains(&q)));
        if self.sort_newest {
            items.reverse();
        } else {
            items.sort_by_key(|m| Path::new(&m.path).file_name().map(|n| n.to_string_lossy().to_lowercase()));
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.strong("Create");
            ui.horizontal_wrapped(|ui| {
                for (label, key) in [("Title", "title"), ("Caption", "subtitle"), ("Square", "square"), ("Circle", "circle"), ("Background", "solid")] {
                    if ui.button(label).clicked() {
                        gen = Some(key);
                    }
                }
                if ui.button("Lower third").on_hover_text("Name bar with a title and subtitle").clicked() {
                    gen = Some("lower");
                }
            });
            ui.separator();
            ui.strong("Your media");
            if items.is_empty() {
                ui.weak("Nothing imported yet. Use Import or drop files onto the window.");
            }
            for m in &items {
                ui.horizontal(|ui| {
                    if let Some(t) = self.thumbs.get(&m.id) {
                        let s = t.size_vec2();
                        let h = 40.0;
                        ui.image((t.id(), vec2(s.x / s.y * h, h)));
                    } else {
                        let (r, _) = ui.allocate_exact_size(vec2(56.0, 40.0), Sense::hover());
                        ui.painter().rect_filled(r, 3.0, if m.kind == MediaKind::Audio { Color32::from_rgb(120, 92, 190) } else { Color32::from_gray(70) });
                    }
                    ui.vertical(|ui| {
                        let name = Path::new(&m.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.label(name);
                        let missing = io::resolve_media(m, dir.as_deref()).is_none();
                        let kind = match m.kind {
                            MediaKind::Video => "Video",
                            MediaKind::Audio => "Audio",
                            MediaKind::Image => "Picture",
                        };
                        if missing {
                            ui.colored_label(Color32::LIGHT_RED, "File missing");
                        } else {
                            ui.weak(format!("{kind}  {}", if m.kind == MediaKind::Image { String::new() } else { fmt_secs(to_secs(m.duration_us) as f32) }));
                        }
                    });
                    if ui.button("Add").on_hover_text("Place at the playhead").clicked() {
                        add = Some(m.id);
                    }
                });
            }
        });
        if let Some(id) = add {
            self.add_media_clip(id);
        }
        match gen {
            Some("lower") => {
                let ph = self.playhead;
                if let Some(ed) = self.ed.as_mut() {
                    match ed.edit("Add lower third", |p| p.lower_third("Name", "Role or place", ph, secs(4.0))) {
                        Ok(ids) => ed.selection = ids,
                        Err(e) => self.err = Some(e.to_string()),
                    }
                }
            }
            Some(k) => self.add_generated(k),
            None => {}
        }
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let bar_h = if self.compact { 46.0 } else { 34.0 };
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(vec2(avail.x, (avail.y - bar_h).max(40.0)), Sense::click());
        ui.painter().rect_filled(rect, 0.0, Color32::from_gray(18));
        if let Some(ed) = self.ed.as_ref() {
            let (pw, ph) = (ed.project.width as f32, ed.project.height as f32);
            let s = ((rect.width() - 8.0) / pw).min((rect.height() - 8.0) / ph).max(0.01);
            let dest = Rect::from_center_size(rect.center(), vec2(pw * s, ph * s));
            ui.painter().rect_filled(dest, 0.0, Color32::BLACK);
            if let Some(t) = &self.tex {
                ui.painter().image(t.id(), dest, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            if self.guides {
                let inner = dest.shrink2(vec2(dest.width() * 0.05, dest.height() * 0.05));
                ui.painter().rect_stroke(inner, 0.0, egui::Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 220, 80, 200)));
                ui.painter().line_segment([dest.center_top(), dest.center_bottom()], egui::Stroke::new(0.5_f32, Color32::from_white_alpha(70)));
                ui.painter().line_segment([dest.left_center(), dest.right_center()], egui::Stroke::new(0.5_f32, Color32::from_white_alpha(70)));
            }
            if ed.project.duration() == 0 {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Import media or add a title to begin", egui::FontId::proportional(16.0), Color32::from_gray(150));
            }
        }
        if resp.clicked() {
            self.toggle_play();
        }
        let Some((fps, dur)) = self.ed.as_ref().map(|e| (e.project.fps, e.project.duration())) else { return };
        ui.horizontal_wrapped(|ui| {
            let b = if self.compact { vec2(48.0, 36.0) } else { vec2(0.0, 24.0) };
            if ui.add(egui::Button::new("|<").min_size(b)).on_hover_text("Back to start").clicked() {
                self.seek(0);
            }
            if ui.add(egui::Button::new("<").min_size(b)).on_hover_text("Previous frame (Left arrow)").clicked() {
                self.seek(self.playhead - frame_to_us(1, fps));
            }
            let label = if self.playing { "Pause" } else { "Play" };
            if ui.add(egui::Button::new(label).fill(ACCENT).min_size(vec2(64.0, b.y))).on_hover_text("Space").clicked() {
                self.toggle_play();
            }
            if ui.add(egui::Button::new(">").min_size(b)).on_hover_text("Next frame (Right arrow)").clicked() {
                self.seek(self.playhead + frame_to_us(1, fps));
            }
            ui.monospace(format!("{} / {}", format_timecode(self.playhead, fps), format_timecode(dur, fps)));
            let cur = self.settings.preview_quality.clone();
            egui::ComboBox::from_id_salt("pq").selected_text(format!("Preview: {}", cur)).show_ui(ui, |ui| {
                for q in ["auto", "low", "medium", "high"] {
                    if ui.selectable_label(cur == q, q).clicked() {
                        self.settings.preview_quality = q.into();
                        let _ = self.settings.save();
                    }
                }
            });
            ui.checkbox(&mut self.guides, "Guides").on_hover_text("Safe area and center lines");
            if ui.checkbox(&mut self.sound_on, "Sound").on_hover_text("Hear the audio while previewing").changed() && self.playing {
                self.stop_playback();
            }
            if self.audio.failed {
                ui.weak("No sound device found");
            }
            if ui.button("Full screen").clicked() {
                let on = !self.fullscreen;
                self.set_fullscreen(ui.ctx(), on);
            }
        });
    }

    // ------------------------------------------------------------ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(e) = self.err.clone() {
            egui::Window::new("Something needs your attention").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label(e);
                if ui.button("OK").clicked() {
                    self.err = None;
                }
            });
        }
        if let Some(tid) = self.confirm_remove_track {
            egui::Window::new("Remove track?").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label("This track has clips on it. They will be removed too. You can undo this.");
                ui.horizontal(|ui| {
                    if ui.button("Remove").clicked() {
                        if let Some(ed) = self.ed.as_mut() {
                            let _ = ed.edit("Remove track", |p| {
                                p.tracks.retain(|t| t.id != tid);
                                Ok(())
                            });
                            let p = &ed.project;
                            ed.selection.retain(|id| p.clip(*id).is_some());
                        }
                        self.confirm_remove_track = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_remove_track = None;
                    }
                });
            });
        }
        if let Some(next) = self.leave {
            egui::Window::new("Save your changes?").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label("This project has changes that haven't been saved.");
                ui.horizontal(|ui| {
                    let mut done = false;
                    if ui.button("Save").clicked() {
                        done = self.save(false);
                    }
                    if ui.button("Don't save").clicked() {
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.leave = None;
                    }
                    if done {
                        self.leave = None;
                        match next {
                            Next::Home => self.go_home(),
                            Next::Quit => {
                                self.autosaver.clear();
                                if let Some(ed) = self.ed.as_mut() {
                                    ed.dirty = false;
                                }
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    }
                });
            });
        }
        if !self.recoveries.is_empty() && self.screen == Screen::Home {
            let mut action: Option<(usize, bool)> = None;
            egui::Window::new("Unsaved work found").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label("LiteMotion closed unexpectedly last time. You can get this work back.");
                for (i, r) in self.recoveries.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(&r.project.name);
                        if ui.button("Recover").clicked() {
                            action = Some((i, true));
                        }
                        if ui.button("Discard").clicked() {
                            action = Some((i, false));
                        }
                    });
                }
            });
            if let Some((i, recover)) = action {
                let r = self.recoveries.remove(i);
                if recover {
                    self.set_editor(r.project.clone(), r.origin.clone(), true);
                }
                store::discard_recovery(&r);
            }
        }
        if !self.missing.is_empty() && self.screen == Screen::Edit {
            self.relink_window(ctx);
        }
        if self.settings_open {
            let mut open = true;
            egui::Window::new("Settings").open(&mut open).collapsible(false).show(ctx, |ui| {
                let mut changed = false;
                changed |= ui.checkbox(&mut self.settings.light_theme, "Light theme").changed();
                changed |= ui.add(egui::Slider::new(&mut self.settings.ui_scale, 0.8..=1.8).text("Text and button size")).changed();
                changed |= ui.add(egui::Slider::new(&mut self.settings.autosave_secs, 10..=600).text("Autosave every (seconds)")).changed();
                if ui.button("Show the quick tour again").clicked() {
                    self.show_help = true;
                }
                if changed {
                    let _ = self.settings.save();
                    self.apply_theme(ctx);
                }
                ui.weak("Everything stays on this computer. LiteMotion never uses the internet.");
            });
            self.settings_open = open;
        }
        if self.show_help {
            egui::Window::new("Quick tour").collapsible(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label("1. Import: add videos, pictures and music.\n2. Add: put them on the timeline at the playhead.\n3. Drag clip edges to trim. Press S to split.\n4. Select a clip, then use Animate to add movement or fades.\n5. Export when you are happy. Save often (Ctrl+S).");
                if ui.button("Got it").clicked() {
                    self.show_help = false;
                    self.settings.onboarding_done = true;
                    let _ = self.settings.save();
                }
            });
        }
        self.export_window(ctx);
    }

    fn relink_window(&mut self, ctx: &egui::Context) {
        let dir = self.project_dir();
        let mut fixed = false;
        egui::Window::new("Some files can't be found").collapsible(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label("These files were moved or deleted. Point LiteMotion at their new location.");
            let list = self.missing.clone();
            for id in list {
                let Some(name) = self.ed.as_ref().and_then(|e| e.project.media(id).map(|m| Path::new(&m.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())) else { continue };
                ui.horizontal(|ui| {
                    ui.label(&name);
                    if ui.button("Locate...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().set_file_name(&name).pick_file() {
                            if let Some(ed) = self.ed.as_mut() {
                                if let Err(e) = ed.edit("Relink media", |pr| media::relink(pr, id, &p, dir.as_deref())) {
                                    self.err = Some(e.to_string());
                                }
                            }
                            fixed = true;
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Search a folder...").clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_folder() {
                        if let Some(ed) = self.ed.as_mut() {
                            let _ = ed.edit("Relink media", |pr| {
                                media::relink_in_folder(pr, &f, dir.as_deref());
                                Ok(())
                            });
                        }
                        fixed = true;
                    }
                }
                if ui.button("Continue without them").clicked() {
                    self.missing.clear();
                }
            });
        });
        if fixed {
            let dir = self.project_dir();
            if let Some(ed) = self.ed.as_ref() {
                self.missing = io::missing_media(&ed.project, dir.as_deref());
            }
            self.requested.clear();
            self.queue_media_jobs();
        }
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        if !self.export.open {
            return;
        }
        // Finished?
        if self.export.handle.as_ref().map_or(false, |h| h.is_finished()) {
            let h = self.export.handle.take().unwrap();
            self.export.result = Some(h.wait().map_err(|e| e.to_string()));
        }
        if self.export.handle.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let Some((pw, ph, pfps)) = self.ed.as_ref().map(|e| (e.project.width, e.project.height, e.project.fps)) else { return };
        let mut open = true;
        let mut start = false;
        egui::Window::new("Export video").open(&mut open).collapsible(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            if let Some(h) = &self.export.handle {
                let p = h.progress();
                ui.label(&p.status);
                ui.add(egui::ProgressBar::new(p.fraction).show_percentage());
                ui.label(format!("Elapsed {:.0}s{}", p.elapsed_secs, p.eta_secs.map(|e| format!(", about {:.0}s left", e)).unwrap_or_default()));
                if ui.button("Cancel").clicked() {
                    h.cancel();
                }
                return;
            }
            match &self.export.result {
                Some(Ok(r)) => {
                    ui.strong("Your video is ready");
                    ui.label(format!("{:.1}s, {}x{}, {:.0} fps, {}", r.duration_s, r.width, r.height, r.fps, if r.has_audio { "with sound" } else { "no sound" }));
                    ui.weak(r.path.display().to_string());
                    for w in &r.warnings {
                        ui.colored_label(Color32::YELLOW, w);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Play it").clicked() {
                            open_externally(&r.path);
                        }
                        if ui.button("Show in folder").clicked() {
                            reveal_folder(&r.path);
                        }
                    });
                    ui.separator();
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, format!("The export didn't finish: {e}"));
                    ui.separator();
                }
                None => {}
            }
            egui::ComboBox::from_label("Size").selected_text(RESOLUTIONS[self.export.res].0).show_ui(ui, |ui| {
                for (i, (l, _)) in RESOLUTIONS.iter().enumerate() {
                    ui.selectable_value(&mut self.export.res, i, *l);
                }
            });
            let (w, h) = size_for(pw, ph, RESOLUTIONS[self.export.res].1);
            ui.weak(format!("{w} x {h} pixels"));
            egui::ComboBox::from_label("Smoothness").selected_text(format!("{} fps", FPS_CHOICES[self.export.fps])).show_ui(ui, |ui| {
                for (i, f) in FPS_CHOICES.iter().enumerate() {
                    ui.selectable_value(&mut self.export.fps, i, format!("{f} fps"));
                }
            });
            ui.add(egui::Slider::new(&mut self.export.quality, 10..=100).text("Quality"));
            ui.checkbox(&mut self.export.audio, "Include sound");
            ui.weak(format!("Project is {pfps} fps. Format: MP4 (H.264 + AAC)."));
            if ui.add(egui::Button::new("Choose file and export").fill(ACCENT)).clicked() {
                start = true;
            }
        });
        if !open {
            if let Some(h) = &self.export.handle {
                h.cancel();
            }
            self.export.open = false;
        }
        if start {
            let name = self.ed.as_ref().map(|e| e.project.name.clone()).unwrap_or_default();
            if let Some(out) = rfd::FileDialog::new().set_file_name(&format!("{name}.mp4")).add_filter("MP4 video", &["mp4"]).save_file() {
                let out = out.with_extension("mp4");
                let (w, h) = size_for(pw, ph, RESOLUTIONS[self.export.res].1);
                let ed = self.ed.as_ref().unwrap();
                let mut o = ExportOptions::from_project(&ed.project, out, self.project_dir());
                o.width = w;
                o.height = h;
                o.fps = FPS_CHOICES[self.export.fps];
                o.quality = self.export.quality;
                o.include_audio = self.export.audio;
                o.overwrite = true; // the system save dialog already asked
                match start_export(ed.project.clone(), o) {
                    Ok(h) => {
                        self.export.handle = Some(h);
                        self.export.result = None;
                    }
                    Err(e) => self.export.result = Some(Err(e.to_string())),
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
}

impl App {
    /// One UI frame. Separate from `eframe::App::update` so tests can drive it.
    fn frame(&mut self, ctx: &egui::Context) {
        self.compact = ctx.screen_rect().width() < 760.0;
        self.poll_workers(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.leave = Some(Next::Quit);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() && self.screen == Screen::Edit {
            self.import_paths(dropped);
        }
        self.keys(ctx);
        self.autosave_tick();

        match self.screen {
            Screen::Home => {
                egui::CentralPanel::default().show(ctx, |ui| self.home_ui(ui));
            }
            Screen::Edit => {
                let title = self.ed.as_ref().map(|e| format!("{}{} - LiteMotion Studio", e.project.name, if e.dirty { " *" } else { "" })).unwrap_or_default();
                if title != self.title {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                    self.title = title;
                }
                if self.fullscreen {
                    egui::CentralPanel::default().show(ctx, |ui| self.preview_ui(ui));
                } else {
                    egui::TopBottomPanel::top("bar").show(ctx, |ui| self.top_bar(ui));
                    let th = self.track_height() * 4.0 + 80.0;
                    egui::TopBottomPanel::bottom("timeline").resizable(true).default_height(th.min(ctx.screen_rect().height() * 0.4)).show(ctx, |ui| self.timeline_ui(ui));
                    if self.compact {
                        match self.drawer {
                            Drawer::Media => {
                                egui::TopBottomPanel::bottom("drawer").resizable(true).default_height(180.0).show(ctx, |ui| self.media_panel(ui));
                            }
                            Drawer::Props => {
                                egui::TopBottomPanel::bottom("drawer").resizable(true).default_height(180.0).show(ctx, |ui| self.props_ui(ui));
                            }
                            Drawer::None => {}
                        }
                    } else {
                        egui::SidePanel::left("media").resizable(true).default_width(270.0).show(ctx, |ui| self.media_panel(ui));
                        egui::SidePanel::right("props").resizable(true).default_width(290.0).show(ctx, |ui| self.props_ui(ui));
                    }
                    egui::CentralPanel::default().show(ctx, |ui| self.preview_ui(ui));
                }
                self.request_still();
            }
        }
        self.dialogs(ctx);
    }
}
