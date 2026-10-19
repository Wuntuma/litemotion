//! Headless UI smoke test: drives real egui frames (no window) against a real
//! project with real imported media. It proves the screens lay out and paint
//! without panicking and that the editing paths work end to end. It does NOT
//! replace trying the app by hand (mouse dragging, native file dialogs).
use super::*;
use litemotion_engine::animation::{apply_preset, Direction, Preset};
use std::process::Command;
use std::sync::Mutex;

/// Tests share process-wide env vars (data dir, audio backend), so run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn gen(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let st = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).arg(&out).status().expect("ffmpeg");
    assert!(st.success());
    out
}

fn frames(app: &mut App, ctx: &egui::Context, w: f32, h: f32, n: usize, events: Vec<egui::Event>, mods: Modifiers) {
    for i in 0..n {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h)));
        input.time = Some(i as f64 * 0.016);
        if i == 0 {
            input.events = events.clone();
            input.modifiers = mods;
        }
        let _ = ctx.run(input, |ctx| app.frame(ctx));
    }
}

fn key(k: Key, m: Modifiers) -> Vec<egui::Event> {
    vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m }]
}

fn clip_count(app: &App) -> usize {
    app.ed.as_ref().unwrap().project.tracks.iter().map(|t| t.clips.len()).sum()
}

#[test]
fn editor_ui_end_to_end() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("lm-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("LITEMOTION_DATA_DIR", tmp.join("data"));
    std::env::set_var("LITEMOTION_CACHE_DIR", tmp.join("cache"));

    let ctx = egui::Context::default();
    let mut app = App::build(&ctx);
    app.show_help = false;
    let none = Modifiers::NONE;
    // Like eframe on Windows/Linux: Ctrl sets both `ctrl` and `command`.
    let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    let ctrl_shift = Modifiers { shift: true, ..ctrl };

    // Home screen paints.
    frames(&mut app, &ctx, 1280.0, 800.0, 3, vec![], none);
    assert!(app.screen == Screen::Home);

    // New project with the default form (16:9, 1080p, 30 fps).
    app.new_project();
    assert!(app.screen == Screen::Edit);
    let p = &app.ed.as_ref().unwrap().project;
    assert_eq!((p.width, p.height, p.fps), (1920, 1080, 30.0));

    // Real media.
    let v = gen(&tmp, "clip.mp4", &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=2", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"]);
    let img = gen(&tmp, "logo.png", &["-f", "lavfi", "-i", "color=c=red:s=120x120", "-frames:v", "1"]);
    let wav = gen(&tmp, "music.wav", &["-f", "lavfi", "-i", "sine=frequency=880:duration=3"]);
    app.import_paths(vec![v, img, wav, tmp.join("nope.mp4")]);
    assert!(app.err.is_some(), "a missing file must produce a clear message");
    app.err = None;
    let ids: Vec<u64> = app.ed.as_ref().unwrap().project.media.iter().map(|m| m.id).collect();
    assert_eq!(ids.len(), 3, "video, picture and audio imported; the missing file was refused");
    for id in &ids {
        app.add_media_clip(*id);
    }
    app.add_generated("title");
    app.add_generated("subtitle");
    app.add_generated("circle");
    app.add_generated("solid");
    assert_eq!(clip_count(&app), 7);
    assert!(app.err.is_none(), "{:?}", app.err);

    // Animate the title via a preset and a hand-made keyframe.
    {
        let ed = app.ed.as_mut().unwrap();
        let tid = ed.project.tracks.iter().flat_map(|t| &t.clips).find(|c| matches!(c.content, Content::Text(_))).unwrap().id;
        ed.edit("anim", |p| {
            let c = p.clip_mut(tid).unwrap();
            apply_preset(c, Preset::SlideIn(Direction::Left), secs(0.8));
            c.set_key(litemotion_engine::animation::Prop::Opacity, secs(1.0), 0.5, Interp::Linear);
            Ok(())
        })
        .unwrap();
    }

    // Every kind of clip selected, in desktop and phone layouts, with each drawer.
    let all: Vec<u64> = app.ed.as_ref().unwrap().project.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
    app.seek(secs(0.5));
    for id in &all {
        app.ed.as_mut().unwrap().selection = vec![*id];
        frames(&mut app, &ctx, 1280.0, 800.0, 2, vec![], none);
        for d in [Drawer::Media, Drawer::Props] {
            app.drawer = d;
            frames(&mut app, &ctx, 400.0, 800.0, 2, vec![], none);
        }
    }
    app.ed.as_mut().unwrap().selection = all.clone();
    frames(&mut app, &ctx, 1280.0, 800.0, 2, vec![], none); // multi-select panel
    app.ed.as_mut().unwrap().selection.clear();

    // Dialogs paint.
    app.export.open = true;
    app.settings_open = true;
    app.show_help = true;
    app.guides = true;
    frames(&mut app, &ctx, 1280.0, 800.0, 2, vec![], none);
    app.export.open = false;
    app.settings_open = false;
    app.show_help = false;

    // Preview and thumbnails arrive from the background workers.
    app.seek(secs(1.0));
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while (app.tex.is_none() || app.thumbs.len() < 2 || app.waves.is_empty()) && Instant::now() < deadline {
        frames(&mut app, &ctx, 1280.0, 800.0, 1, vec![], none);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(app.tex.is_some(), "preview frame never arrived");
    assert!(app.thumbs.len() >= 2, "thumbnails never arrived");
    assert!(!app.waves.is_empty(), "waveform never arrived");

    // Keyboard: S splits the selected video clip at the playhead; Ctrl+Z undoes it.
    let vid = app.ed.as_ref().unwrap().project.tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().clips.iter().find(|c| matches!(c.content, Content::Media { .. })).unwrap().id;
    app.ed.as_mut().unwrap().selection = vec![vid];
    app.seek(secs(1.0));
    let before = clip_count(&app);
    frames(&mut app, &ctx, 1280.0, 800.0, 2, key(Key::S, none), none);
    assert_eq!(clip_count(&app), before + 1, "S should split at the playhead");
    frames(&mut app, &ctx, 1280.0, 800.0, 2, key(Key::Z, ctrl), ctrl);
    assert_eq!(clip_count(&app), before, "Ctrl+Z should undo the split");
    frames(&mut app, &ctx, 1280.0, 800.0, 2, key(Key::Z, ctrl_shift), ctrl_shift);
    assert_eq!(clip_count(&app), before + 1, "Ctrl+Shift+Z should redo");

    // Duplicate, copy/paste, delete.
    app.action_duplicate();
    assert_eq!(clip_count(&app), before + 2);
    app.ed.as_mut().unwrap().copy_selection();
    app.seek(secs(30.0));
    app.action_paste();
    assert_eq!(clip_count(&app), before + 3);
    app.action_delete();
    assert_eq!(clip_count(&app), before + 2);

    // Save, go home, reopen: same timeline.
    let path = tmp.join("demo.lmproj");
    app.path = Some(path.clone());
    assert!(app.save(false), "{:?}", app.err);
    assert!(!app.dirty());
    let saved = app.ed.as_ref().unwrap().project.clone();
    app.go_home();
    frames(&mut app, &ctx, 1280.0, 800.0, 2, vec![], none);
    assert!(app.recent.iter().any(|r| r.name == saved.name), "saved project should be in the recent list");
    app.open_project(&path);
    assert!(app.err.is_none(), "{:?}", app.err);
    assert_eq!(app.ed.as_ref().unwrap().project, saved, "reopened project must match what was saved");
    assert!(app.missing.is_empty());
    frames(&mut app, &ctx, 1280.0, 800.0, 2, vec![], none);

    // Missing media produces the relink dialog instead of a crash.
    std::fs::remove_file(tmp.join("music.wav")).unwrap();
    app.go_home();
    app.open_project(&path);
    assert_eq!(app.missing.len(), 1);
    frames(&mut app, &ctx, 1280.0, 800.0, 3, vec![], none);

    let _ = std::fs::remove_dir_all(&tmp);
}

fn run_for(app: &mut App, ctx: &egui::Context, secs_: f64) {
    let end = Instant::now() + std::time::Duration::from_secs_f64(secs_);
    while Instant::now() < end {
        frames(app, ctx, 1280.0, 800.0, 1, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

#[test]
fn preview_video_follows_the_audio_clock() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("lm-audio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("LITEMOTION_DATA_DIR", tmp.join("data"));
    std::env::set_var("LITEMOTION_CACHE_DIR", tmp.join("cache"));
    std::env::set_var("LITEMOTION_AUDIO_BACKEND", "null"); // real-time device that makes no sound
    let ctx = egui::Context::default();
    let mut app = App::build(&ctx);
    app.show_help = false;
    app.new_project();

    // Silent project (just a title): plays on the wall clock, no audio clock.
    app.add_generated("title");
    app.seek(0);
    app.toggle_play();
    assert!(app.playing);
    run_for(&mut app, &ctx, 1.2);
    assert!(app.audio.current.is_none(), "nothing to hear, so no audio clock");
    let t = to_secs(app.playhead);
    assert!(t > 0.6 && t < 1.5, "silent preview should advance with wall time, got {t}");
    app.stop_playback();

    // Project with sound: the video position must equal what the device really played.
    let wav = gen(&tmp, "music.wav", &["-f", "lavfi", "-i", "sine=frequency=880:duration=6"]);
    app.import_paths(vec![wav]);
    let mid = app.ed.as_ref().unwrap().project.media[0].id;
    app.add_media_clip(mid);
    app.seek(secs(0.5));
    app.toggle_play();
    run_for(&mut app, &ctx, 1.8);
    let clock = app.audio.current.clone().expect("audio clock while playing");
    let played = clock.played_secs();
    frames(&mut app, &ctx, 1280.0, 800.0, 1, vec![], Modifiers::NONE);
    let pos = to_secs(app.playhead);
    assert!(played > 0.9, "device should have played about a second or more, got {played}");
    assert!((pos - (0.5 + played)).abs() < 0.25, "video at {pos}s but audio says {}s", 0.5 + played);
    assert!(clock.buffered_frames() < 60_000, "producer must not run far ahead of the device");

    // Stopping silences everything immediately.
    app.stop_playback();
    let frozen = clock.played_secs();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(clock.played_secs(), frozen);
    assert!(app.audio.current.is_none());

    // Sound off: wall clock again.
    app.sound_on = false;
    app.seek(0);
    app.toggle_play();
    run_for(&mut app, &ctx, 0.8);
    assert!(app.audio.current.is_none() && to_secs(app.playhead) > 0.3);
    app.stop_playback();
    let _ = std::fs::remove_dir_all(&tmp);
}
