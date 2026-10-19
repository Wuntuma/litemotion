use litemotion_engine::animation::*;
use litemotion_engine::project::io::*;
use litemotion_engine::project::model::*;
use litemotion_engine::project::store::*;
use litemotion_engine::timeline::editor::Editor;

fn proj() -> Project {
    Project::new("Test", 1080, 1920, 30.0)
}
fn track(p: &Project, k: TrackKind) -> u64 {
    p.tracks.iter().find(|t| t.kind == k).unwrap().id
}
fn solid(p: &mut Project, start: i64, dur: i64) -> Clip {
    let id = p.alloc_id();
    Clip::new(id, Content::Solid { color: [10, 20, 30, 255] }, start, dur)
}

#[test]
fn a_project_creation_and_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::new("My video", 1080, 1920, 30.0);
    let tid = track(&p, TrackKind::Video);
    let c = solid(&mut p, 0, 2_000_000);
    p.add_clip(tid, c, false).unwrap();
    let path = dir.path().join("a.lmproj");
    save(&p, &path).unwrap();
    let q = load(&path).unwrap();
    assert_eq!(p, q);
    assert_eq!((q.width, q.height, q.fps), (1080, 1920, 30.0));
}

#[test]
fn project_validation_rejects_bad_input() {
    let mut p = proj();
    p.width = 100_000;
    assert!(p.validate().is_err());
    let mut p = proj();
    p.fps = f64::NAN;
    assert!(p.validate().is_err());
    // Newer format is refused with a helpful message.
    let mut v: serde_json::Value = serde_json::from_slice(&to_json(&proj()).unwrap()).unwrap();
    v["format_version"] = 99.into();
    let e = parse(&serde_json::to_vec(&v).unwrap()).unwrap_err().to_string();
    assert!(e.contains("newer"), "{e}");
    // Not JSON / not a project.
    assert!(parse(b"not json").is_err());
    assert!(parse(b"{\"hello\":1}").is_err());
    // Clip referring to unknown media.
    let mut p = proj();
    let tid = track(&p, TrackKind::Video);
    let id = p.alloc_id();
    p.tracks.iter_mut().find(|t| t.id == tid).unwrap().clips.push(Clip::new(id, Content::Media { media_id: 777 }, 0, 1_000_000));
    assert!(p.validate().is_err());
    // Non-finite number smuggled via JSON can't happen, but out-of-range speed can.
    let mut p = proj();
    let c = solid(&mut p, 0, 1_000_000);
    let tid = track(&p, TrackKind::Video);
    let mut c = c;
    c.speed = 1000.0;
    p.tracks.iter_mut().find(|t| t.id == tid).unwrap().clips.push(c);
    assert!(p.validate().is_err());
}

#[test]
fn c_timeline_edit_move_trim_split_undo_redo() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let c = solid(&mut p, 1_000_000, 4_000_000);
    let id = p.add_clip(tid, c, false).unwrap();
    let mut ed = Editor::new(p);

    ed.edit("Move", |p| p.move_clip(id, 2_000_000, None)).unwrap();
    assert_eq!(ed.project.clip(id).unwrap().start, 2_000_000);

    ed.edit("Trim start", |p| p.trim_start(id, 3_000_000)).unwrap();
    let c = ed.project.clip(id).unwrap();
    assert_eq!((c.start, c.duration, c.end()), (3_000_000, 3_000_000, 6_000_000));

    ed.edit("Trim end", |p| p.trim_end(id, 5_000_000)).unwrap();
    assert_eq!(ed.project.clip(id).unwrap().duration, 2_000_000);

    let right = ed.edit("Split", |p| p.split_clip(id, 4_000_000)).unwrap();
    assert_eq!(ed.project.clip(id).unwrap().end(), 4_000_000);
    assert_eq!(ed.project.clip(right).unwrap().start, 4_000_000);
    assert_eq!(ed.project.clip(right).unwrap().end(), 5_000_000);

    // Undo everything, step by step, then redo everything.
    let snapshots = ed.project.clone();
    while ed.undo() {}
    assert_eq!(ed.project.clip(id).unwrap().start, 1_000_000);
    assert_eq!(ed.project.clip(id).unwrap().duration, 4_000_000);
    assert!(ed.project.clip(right).is_none());
    while ed.redo() {}
    assert_eq!(ed.project, snapshots);
}

#[test]
fn overlap_and_failed_edits_do_not_change_project() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let a = solid(&mut p, 0, 2_000_000);
    let a = p.add_clip(tid, a, false).unwrap();
    let b = solid(&mut p, 2_000_000, 2_000_000);
    let b = p.add_clip(tid, b, false).unwrap();
    let mut ed = Editor::new(p);
    assert!(ed.edit("Move", |p| p.move_clip(b, 1_000_000, None)).is_err());
    assert!(!ed.can_undo(), "a failed edit must not create an undo step");
    assert_eq!(ed.project.clip(b).unwrap().start, 2_000_000);
    assert!(ed.edit("Trim", |p| p.trim_end(a, 3_000_000)).is_err());
    // Wrong track kind
    let text_track = track(&ed.project, TrackKind::Text);
    assert!(ed.edit("Move", |p| p.move_clip(a, 0, Some(text_track))).is_err());
}

#[test]
fn crossfade_allows_overlap_only_with_transition() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let a = solid(&mut p, 0, 3_000_000);
    p.add_clip(tid, a, false).unwrap();
    let mut b = solid(&mut p, 2_500_000, 3_000_000);
    assert!(p.add_clip(tid, b.clone(), false).is_err());
    b.transition_in = Some(Transition { kind: TransitionKind::Crossfade, duration: 500_000 });
    assert!(p.add_clip(tid, b, false).is_ok());
}

#[test]
fn snapping_and_duplicate_and_paste() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let a = solid(&mut p, 0, 1_000_000);
    let a = p.add_clip(tid, a, false).unwrap();
    let b = solid(&mut p, 3_000_000, 1_000_000);
    let b = p.add_clip(tid, b, false).unwrap();
    // Within threshold snaps to the end of clip a (1.0 s); outside doesn't.
    assert_eq!(p.snap_move(b, 1_040_000, 0, 100_000), 1_000_000);
    assert_eq!(p.snap_move(b, 1_400_000, 0, 100_000), 1_400_000);
    // Playhead is a snap target.
    assert_eq!(p.snap(2_030_000, &[], 2_000_000, 100_000), 2_000_000);
    let d = p.duplicate_clip(a).unwrap();
    assert_eq!(p.clip(d).unwrap().start, 1_000_000);
    let items = vec![(tid, p.clip(a).unwrap().clone())];
    let ids = p.paste_clips(&items, 10_000_000).unwrap();
    assert_eq!(p.clip(ids[0]).unwrap().start, 10_000_000);
    assert!(p.validate().is_ok());
}

#[test]
fn speed_changes_duration_and_source_span() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let a = solid(&mut p, 0, 4_000_000);
    let a = p.add_clip(tid, a, false).unwrap();
    p.set_speed(a, 2.0).unwrap();
    assert_eq!(p.clip(a).unwrap().duration, 2_000_000);
    assert!(p.set_speed(a, 100.0).is_err());
}

#[test]
fn e_keyframes_interpolation_and_presets() {
    let mut c = Clip::new(1, Content::Solid { color: [0; 4] }, 0, 2_000_000);
    c.set_key(Prop::Opacity, 0, 0.0, Interp::Linear);
    c.set_key(Prop::Opacity, 1_000_000, 1.0, Interp::Linear);
    assert!((c.value_at(Prop::Opacity, 500_000) - 0.5).abs() < 1e-6);
    assert_eq!(c.value_at(Prop::Opacity, 5_000_000), 1.0); // holds last value
    c.set_key(Prop::X, 0, 0.0, Interp::EaseIn);
    c.set_key(Prop::X, 1_000_000, 1.0, Interp::Linear);
    let mid = c.value_at(Prop::X, 500_000);
    assert!((mid - 0.25).abs() < 1e-6, "ease-in at 50% must be 0.25, got {mid}");
    c.set_key(Prop::Y, 0, 0.0, Interp::EaseOut);
    c.set_key(Prop::Y, 1_000_000, 1.0, Interp::Linear);
    assert!((c.value_at(Prop::Y, 500_000) - 0.75).abs() < 1e-6);
    c.set_key(Prop::Rotation, 0, 0.0, Interp::EaseInOut);
    c.set_key(Prop::Rotation, 1_000_000, 100.0, Interp::Linear);
    assert!((c.value_at(Prop::Rotation, 500_000) - 50.0).abs() < 1e-4);
    // Replace + remove + navigation
    c.set_key(Prop::Opacity, 1_000_000, 0.5, Interp::Linear);
    assert_eq!(c.anim(Prop::Opacity).unwrap().keys.len(), 2);
    assert_eq!(c.next_key_time(0), Some(1_000_000));
    assert_eq!(c.prev_key_time(1_000_000), Some(0));
    assert!(c.remove_key(Prop::Opacity, 1_000_000));
    // Presets create ordinary editable keyframes.
    let mut t = Clip::new(2, Content::Text(TextStyle::default()), 0, 3_000_000);
    apply_preset(&mut t, Preset::Pop, 500_000);
    let keys = &t.anim(Prop::Scale).unwrap().keys;
    assert_eq!(keys.len(), 3);
    assert!(keys[1].v > 1.0 && keys[2].v == 1.0);
    apply_preset(&mut t, Preset::SlideIn(Direction::Left), 400_000);
    assert!(t.value_at(Prop::X, 0) < -0.5 && t.value_at(Prop::X, 400_000) == 0.0);
    apply_preset(&mut t, Preset::FadeOut, 1_000_000);
    assert_eq!(t.value_at(Prop::Opacity, 3_000_000), 0.0);
    apply_preset(&mut t, Preset::Typewriter(10.0), 0);
    if let Content::Text(s) = &t.content {
        assert_eq!(s.typewriter_cps, Some(10.0));
    }
}

#[test]
fn split_preserves_animation() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let mut c = solid(&mut p, 0, 4_000_000);
    c.set_key(Prop::X, 0, 0.0, Interp::Linear);
    c.set_key(Prop::X, 4_000_000, 0.4, Interp::Linear);
    let id = p.add_clip(tid, c, false).unwrap();
    let r = p.split_clip(id, 1_000_000).unwrap();
    let left = p.clip(id).unwrap();
    let right = p.clip(r).unwrap();
    assert!((left.value_at(Prop::X, 500_000) - 0.05).abs() < 1e-6);
    assert!((right.value_at(Prop::X, 0) - 0.1).abs() < 1e-6);
    assert!((right.value_at(Prop::X, 3_000_000) - 0.4).abs() < 1e-6);
    // Same absolute timeline value before and after the split.
    assert!((right.value_at(Prop::X, 1_000_000) - 0.2).abs() < 1e-6);
    // Trimming the start keeps motion pinned to content.
    p.trim_start(r, 2_000_000).unwrap();
    assert!((p.clip(r).unwrap().value_at(Prop::X, 0) - 0.2).abs() < 1e-6);
}

#[test]
fn d_persistence_keeps_everything() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = proj();
    let tid = track(&p, TrackKind::Text);
    let mut c = Clip::new(p.alloc_id(), Content::Text(TextStyle { text: "Héllo — 你好".into(), outline: true, ..Default::default() }), 500_000, 3_000_000);
    apply_preset(&mut c, Preset::Pop, 600_000);
    c.transition_in = Some(Transition { kind: TransitionKind::Dissolve, duration: 300_000 });
    c.color = ColorAdjust { brightness: 0.1, contrast: 1.2, saturation: 0.8 };
    c.crop = Crop { left: 0.1, ..Default::default() };
    p.add_clip(tid, c, false).unwrap();
    let path = dir.path().join("d.lmproj");
    save(&p, &path).unwrap();
    assert_eq!(load(&path).unwrap(), p);
}

#[test]
fn n_atomic_save_backup_and_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("n.lmproj");
    let mut p = proj();
    save(&p, &path).unwrap();
    p.name = "second".into();
    save(&p, &path).unwrap();
    assert!(backup_path(&path).exists(), "previous version kept as .bak");
    assert_eq!(load(&backup_path(&path)).unwrap().name, "Test");
    // Simulate a crash that left the main file truncated.
    let good = std::fs::read(&path).unwrap();
    std::fs::write(&path, &good[..good.len() / 2]).unwrap();
    assert!(load(&path).is_err());
    let (q, used_backup) = load_with_fallback(&path).unwrap();
    assert!(used_backup);
    assert_eq!(q.name, "Test");
    // A stray temp file from an interrupted write never replaces the project.
    assert!(std::fs::read_dir(dir.path()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("tmp") || e.file_name().to_string_lossy().starts_with('.')));
}

#[test]
fn n_autosave_recovery_cycle() {
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("LITEMOTION_DATA_DIR", data.path());
    let mut p = proj();
    p.name = "unsaved work".into();
    {
        let a = Autosaver::start("test-session");
        a.submit(None, &p);
        // Give the worker a moment, then DROP without clear() = simulated crash.
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let found = find_recoveries();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].project.name, "unsaved work");
    discard_recovery(&found[0]);
    assert!(find_recoveries().is_empty());
    // A clean session removes its autosave.
    let a = Autosaver::start("clean");
    a.submit(None, &p);
    std::thread::sleep(std::time::Duration::from_millis(200));
    a.clear();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(find_recoveries().is_empty());
    // Corrupt autosave is set aside, not fatal.
    std::fs::create_dir_all(data.path().join("autosave")).unwrap();
    std::fs::write(data.path().join("autosave/bad.lmauto"), b"{garbage").unwrap();
    assert!(find_recoveries().is_empty());
}

#[test]
fn coalesced_edits_are_one_undo_step() {
    let mut p = proj();
    let tid = track(&p, TrackKind::Overlay);
    let c = solid(&mut p, 0, 1_000_000);
    let id = p.add_clip(tid, c, false).unwrap();
    let mut ed = Editor::new(p);
    for i in 1..=20 {
        ed.edit_coalesced("opacity", |p| {
            p.clip_mut(id).unwrap().opacity = 1.0 - i as f32 * 0.04;
            Ok(())
        })
        .unwrap();
    }
    assert!(ed.undo());
    assert_eq!(ed.project.clip(id).unwrap().opacity, 1.0);
    assert!(!ed.can_undo());
}

#[test]
fn relative_media_paths_survive_moving_project() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a/proj");
    let media_dir = root.path().join("a/media");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&media_dir).unwrap();
    let f = media_dir.join("x.mp4");
    std::fs::write(&f, b"x").unwrap();
    let m = MediaRef { id: 1, kind: MediaKind::Video, path: f.to_string_lossy().into(), rel_path: relative_to(&a, &f), duration_us: 1, width: 1, height: 1, fps: 30.0, has_audio: false, audio_channels: 0, size_bytes: 1, proxy_path: None };
    assert_eq!(m.rel_path.as_deref(), Some("../media/x.mp4"));
    // Move the whole tree: absolute path is stale, relative still resolves.
    let moved = root.path().join("b");
    std::fs::rename(root.path().join("a"), &moved).unwrap();
    let found = resolve_media(&m, Some(&moved.join("proj"))).expect("relative path resolves");
    assert!(found.ends_with("b/media/x.mp4"));
    // Without a project dir the stale absolute path is reported missing.
    assert!(resolve_media(&m, None).is_none());
    // URLs and odd paths are never resolved.
    let mut evil = m.clone();
    evil.path = "http://example.com/x.mp4".into();
    evil.rel_path = None;
    assert!(resolve_media(&evil, Some(&moved)).is_none());
}
