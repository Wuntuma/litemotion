mod common;
use litemotion_engine::animation::*;
use litemotion_engine::media::{self, ImportMode};
use litemotion_engine::project::model::*;
use litemotion_engine::rendering::export::*;
use litemotion_engine::rendering::source::StreamProvider;
use litemotion_engine::rendering::{compose, LayerCache};
use litemotion_engine::EngineError;
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn track(p: &Project, k: TrackKind) -> u64 {
    p.tracks.iter().find(|t| t.kind == k).unwrap().id
}

#[test]
fn b_import_inspects_real_files() {
    let d = tempfile::tempdir().unwrap();
    let (v, i, m) = (common::video(d.path()), common::image(d.path()), common::music(d.path()));
    let mut p = Project::new("t", 640, 360, 30.0);
    let vid = media::import(&mut p, &v, ImportMode::Reference, None, None).unwrap();
    let img = media::import(&mut p, &i, ImportMode::Reference, None, None).unwrap();
    let aud = media::import(&mut p, &m, ImportMode::Reference, None, None).unwrap();
    let mv = p.media(vid).unwrap();
    assert_eq!((mv.kind, mv.width, mv.height, mv.has_audio), (MediaKind::Video, 320, 240, true));
    assert!((mv.duration_us - 2_000_000).abs() < 100_000, "{}", mv.duration_us);
    assert!((mv.fps - 30.0).abs() < 0.1);
    assert_eq!(p.media(img).unwrap().kind, MediaKind::Image);
    let ma = p.media(aud).unwrap();
    assert_eq!(ma.kind, MediaKind::Audio);
    assert!((ma.duration_us - 3_000_000).abs() < 100_000);
    // Importing the same file twice reuses the entry.
    assert_eq!(media::import(&mut p, &v, ImportMode::Reference, None, None).unwrap(), vid);
    // Copy mode makes a self-contained copy and leaves the original alone.
    let assets = d.path().join("assets");
    let c = media::import(&mut p, &m, ImportMode::Copy, Some(d.path()), Some(&assets)).unwrap();
    assert_ne!(c, aud);
    assert!(assets.join("music.wav").exists() && m.exists());
}

#[test]
fn i_error_handling_is_clear() {
    let d = tempfile::tempdir().unwrap();
    let mut p = Project::new("t", 640, 360, 30.0);
    // missing
    let e = media::import(&mut p, &d.path().join("nope.mp4"), ImportMode::Reference, None, None).unwrap_err();
    assert!(matches!(e, EngineError::Missing(_)), "{e}");
    // unsupported extension
    let txt = d.path().join("a.txt");
    std::fs::write(&txt, "hello").unwrap();
    assert!(matches!(media::probe(&txt).unwrap_err(), EngineError::Media(_)));
    // corrupt file with a video extension
    let bad = d.path().join("bad.mp4");
    std::fs::write(&bad, vec![0x42u8; 4096]).unwrap();
    let e = media::probe(&bad).unwrap_err();
    assert!(matches!(e, EngineError::Media(_)), "{e}");
    // empty file
    let empty = d.path().join("empty.mp4");
    std::fs::write(&empty, "").unwrap();
    assert!(media::probe(&empty).unwrap_err().to_string().contains("empty"));
    // truncated real file
    let v = common::video(d.path());
    let bytes = std::fs::read(&v).unwrap();
    let trunc = d.path().join("trunc.mp4");
    std::fs::write(&trunc, &bytes[..bytes.len() / 3]).unwrap();
    assert!(media::probe(&trunc).is_err());
    // Export preflight: empty timeline, bad extension, existing output, missing media.
    let o = ExportOptions::from_project(&p, d.path().join("o.mp4"), None);
    assert!(preflight(&p, &o).unwrap_err().to_string().contains("empty"));
    let tid = track(&p, TrackKind::Overlay);
    let id = p.alloc_id();
    p.tracks.iter_mut().find(|t| t.id == tid).unwrap().clips.push(Clip::new(id, Content::Solid { color: [1, 2, 3, 255] }, 0, 1_000_000));
    assert!(preflight(&p, &o).is_ok());
    let mut bad_ext = o.clone();
    bad_ext.out_path = d.path().join("o.avi");
    assert!(preflight(&p, &bad_ext).is_err());
    std::fs::write(d.path().join("exists.mp4"), "x").unwrap();
    let mut ex = o.clone();
    ex.out_path = d.path().join("exists.mp4");
    assert!(matches!(preflight(&p, &ex).unwrap_err(), EngineError::OutputExists(_)));
    let mut nodir = o.clone();
    nodir.out_path = d.path().join("no/such/dir/x.mp4");
    assert!(preflight(&p, &nodir).is_err());
    // Missing media at export time
    let vid = media::import(&mut p, &v, ImportMode::Reference, None, None).unwrap();
    let vt = track(&p, TrackKind::Video);
    let c = p.media_clip(vid, 0).unwrap();
    p.add_clip(vt, c, true).unwrap();
    std::fs::remove_file(&v).unwrap();
    let e = preflight(&p, &o).unwrap_err().to_string();
    assert!(e.contains("missing") && e.contains("clip.mp4"), "{e}");
    // Relink flow
    std::fs::create_dir_all(d.path().join("moved/deeper")).unwrap();
    common::video(&d.path().join("moved/deeper"));
    let fixed = media::relink_in_folder(&mut p, d.path(), None);
    assert_eq!(fixed, 1);
    assert!(preflight(&p, &o).is_ok());
}

#[test]
fn unsafe_media_paths_are_never_given_to_ffmpeg() {
    let mut p = Project::new("t", 320, 240, 30.0);
    p.media.push(MediaRef { id: 50, kind: MediaKind::Video, path: "http://example.invalid/x.mp4".into(), rel_path: None, duration_us: 1_000_000, width: 320, height: 240, fps: 30.0, has_audio: false, audio_channels: 0, size_bytes: 1, proxy_path: None });
    let tid = track(&p, TrackKind::Video);
    p.tracks.iter_mut().find(|t| t.id == tid).unwrap().clips.push(Clip::new(51, Content::Media { media_id: 50 }, 0, 1_000_000));
    let d = tempfile::tempdir().unwrap();
    let o = ExportOptions::from_project(&p, d.path().join("o.mp4"), None);
    assert!(preflight(&p, &o).unwrap_err().to_string().contains("missing"));
}

#[test]
fn f_text_over_video_renders_pixels_and_animates() {
    let d = tempfile::tempdir().unwrap();
    let v = common::video(d.path());
    let mut p = Project::new("t", 640, 360, 30.0);
    let vid = media::import(&mut p, &v, ImportMode::Reference, None, None).unwrap();
    let vt = track(&p, TrackKind::Video);
    let c = p.media_clip(vid, 0).unwrap();
    p.add_clip(vt, c, true).unwrap();
    let tt = track(&p, TrackKind::Text);
    let tid = p.alloc_id();
    let mut t = Clip::new(tid, Content::Text(TextStyle { text: "HELLO".into(), size_pct: 20.0, color: [255, 0, 255, 255], shadow: false, ..Default::default() }), 0, 2_000_000);
    apply_preset(&mut t, Preset::FadeIn, 1_000_000);
    p.add_clip(tt, t, false).unwrap();

    let render = |p: &Project, t: i64| {
        let mut prov = StreamProvider::new(30.0, None, false);
        compose(p, 640, 360, t, &mut prov, &mut LayerCache::default())
    };
    let mut bare = p.clone();
    bare.tracks.iter_mut().find(|t| t.kind == TrackKind::Text).unwrap().clips.clear();
    let differing = |a: &litemotion_engine::rendering::Bitmap, b: &litemotion_engine::rendering::Bitmap| {
        a.data.chunks_exact(4).zip(b.data.chunks_exact(4)).filter(|(x, y)| x != y).count()
    };
    // Same instant, with and without the title: identical at t=0 (opacity 0)...
    assert_eq!(differing(&render(&p, 0), &render(&bare, 0)), 0, "title is fully transparent at t=0");
    // ...and clearly different once it has faded in.
    let (with, without) = (render(&p, 1_500_000), render(&bare, 1_500_000));
    let n = differing(&with, &without);
    assert!(n > 1500, "title pixels visible after the fade-in: {n}");
    let magenta = with.data.chunks_exact(4).zip(without.data.chunks_exact(4)).filter(|(x, y)| x != y && x[0] > 200 && x[1] < 60 && x[2] > 200).count();
    assert!(magenta > 500, "title glyphs use the chosen colour: {magenta}");
    // Pillarbox bars at the left edge are black (Fit mode preserves aspect).
    assert_eq!(&with.data[0..4], &[0, 0, 0, 255]);
}

#[test]
fn e_animated_image_position_and_opacity() {
    let d = tempfile::tempdir().unwrap();
    let img = common::image(d.path());
    let mut p = Project::new("t", 400, 400, 30.0);
    let iid = media::import(&mut p, &img, ImportMode::Reference, None, None).unwrap();
    let ot = track(&p, TrackKind::Overlay);
    let mut c = p.media_clip(iid, 0).unwrap();
    c.duration = 2_000_000;
    c.scale = 0.5;
    c.set_key(Prop::X, 0, -0.3, Interp::Linear);
    c.set_key(Prop::X, 2_000_000, 0.3, Interp::Linear);
    c.set_key(Prop::Opacity, 0, 0.0, Interp::Linear);
    c.set_key(Prop::Opacity, 2_000_000, 1.0, Interp::Linear);
    p.add_clip(ot, c, false).unwrap();
    let centroid = |t: i64| {
        let mut prov = StreamProvider::new(30.0, None, false);
        let f = compose(&p, 400, 400, t, &mut prov, &mut LayerCache::default());
        let (mut sx, mut n, mut maxr) = (0f64, 0f64, 0u8);
        for (i, px) in f.data.chunks_exact(4).enumerate() {
            if px[0] > px[1] + 20 {
                sx += (i % 400) as f64;
                n += 1.0;
                maxr = maxr.max(px[0]);
            }
        }
        (if n > 0.0 { sx / n } else { f64::NAN }, maxr)
    };
    let (x1, r1) = centroid(1_000_000);
    let (x2, r2) = centroid(1_900_000);
    assert!((x1 - 200.0).abs() < 4.0, "centre at midpoint: {x1}");
    assert!((x2 - (200.0 + 0.3 * 400.0 * 0.95 + 0.0)).abs() < 12.0 || x2 > 300.0, "moved right: {x2}");
    assert!(r2 > r1, "opacity increased: {r1} -> {r2}");
    assert!((r1 as f32 - 127.0).abs() < 8.0, "half opacity over black ≈127: {r1}");
}

fn ffprobe_json(p: &std::path::Path) -> serde_json::Value {
    let o = Command::new("ffprobe").args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"]).arg(p).output().unwrap();
    serde_json::from_slice(&o.stdout).unwrap()
}

fn rms_window(path: &std::path::Path, from: f64, to: f64) -> f64 {
    // Independent decoder: ffmpeg + astats.
    let o = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-ss", &from.to_string(), "-t", &(to - from).to_string(), "-i"])
        .arg(path)
        .args(["-vn", "-af", "astats=metadata=0:reset=0", "-f", "null", "-"])
        .output()
        .unwrap();
    let txt = String::from_utf8_lossy(&o.stderr).to_string();
    let line = txt.lines().filter(|l| l.contains("RMS level dB")).last().unwrap_or("");
    let v: f64 = line.rsplit(':').next().unwrap_or("-inf").trim().replace("-inf", "-200").parse().unwrap_or(-200.0);
    v
}

#[test]
fn g_h_j_full_export_with_title_logo_music() {
    let d = tempfile::tempdir().unwrap();
    let v = common::flat_video(d.path());
    let m = common::music(d.path());
    let img = common::image(d.path());
    let mut p = Project::new("Demo", 640, 360, 30.0);
    let vid = media::import(&mut p, &v, ImportMode::Reference, None, None).unwrap();
    let mid = media::import(&mut p, &m, ImportMode::Reference, None, None).unwrap();
    let iid = media::import(&mut p, &img, ImportMode::Reference, None, None).unwrap();
    let vt = track(&p, TrackKind::Video);
    let c = p.media_clip(vid, 0).unwrap();
    p.add_clip(vt, c, true).unwrap();
    let ot = track(&p, TrackKind::Overlay);
    let mut logo = p.media_clip(iid, 500_000).unwrap();
    logo.duration = 1_500_000;
    logo.scale = 0.3;
    apply_preset(&mut logo, Preset::LogoReveal, 500_000);
    p.add_clip(ot, logo, false).unwrap();
    let tt = track(&p, TrackKind::Text);
    let tid = p.alloc_id();
    let mut t = Clip::new(tid, Content::Text(TextStyle { text: "LiteMotion".into(), outline: true, ..Default::default() }), 0, 2_000_000);
    apply_preset(&mut t, Preset::SlideIn(Direction::Up), 600_000);
    p.add_clip(tt, t, false).unwrap();
    // Music starts at 1.0 s (so a sync error would be audible as energy before 1.0 s) at half volume.
    let at = track(&p, TrackKind::Audio);
    let mut mc = p.media_clip(mid, 1_000_000).unwrap();
    mc.duration = 1_000_000;
    mc.volume = 0.5;
    mc.fade_in = 100_000;
    p.add_clip(at, mc, false).unwrap();
    assert_eq!(p.duration(), 2_000_000);

    let out = d.path().join("out.mp4");
    let o = ExportOptions::from_project(&p, out.clone(), None);
    let handle = start_export(p.clone(), o.clone()).unwrap();
    let mut last = 0.0;
    while !handle.is_finished() {
        let pr = handle.progress();
        assert!(pr.fraction >= last - 1e-6, "progress never goes backwards");
        last = pr.fraction;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rep = handle.wait().unwrap();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    assert_eq!((rep.width, rep.height), (640, 360));
    assert!(rep.has_audio);
    assert!((rep.duration_s - 2.0).abs() < 0.1, "{}", rep.duration_s);
    assert!((rep.fps - 30.0).abs() < 0.1);

    // Independent verification with ffprobe/ffmpeg (a different program than the writer).
    let j = ffprobe_json(&out);
    let kinds: Vec<_> = j["streams"].as_array().unwrap().iter().map(|s| s["codec_name"].as_str().unwrap().to_string()).collect();
    assert!(kinds.contains(&"h264".to_string()) && kinds.contains(&"aac".to_string()), "{kinds:?}");
    let st = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&out).args(["-f", "null", "-"]).status().unwrap();
    assert!(st.success(), "independent decoder plays the file end to end");
    // Audio sync: silent before 1.0 s, tone after.
    let before = rms_window(&out, 0.1, 0.8);
    let after = rms_window(&out, 1.2, 1.9);
    assert!(before < -60.0, "no audio before the music clip starts: {before} dB");
    assert!(after > -29.0 && after < -25.0, "music present afterwards at half volume (sine -21 dB x 0.5 = -27 dB): {after} dB");
    // Frame content: logo (red) present at t=1.0, absent at t=0.1.
    let grab = |t: f64| {
        let o = Command::new("ffmpeg").args(["-v", "error", "-ss", &t.to_string(), "-i"]).arg(&out).args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]).output().unwrap();
        o.stdout
    };
    let red = |b: &[u8]| b.chunks_exact(3).filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60).count();
    assert!(red(&grab(1.0)) > 1000, "logo visible");
    assert_eq!(red(&grab(0.1)), 0, "logo not yet visible");
    // No leftovers.
    let leftovers: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".part") || n.contains(".audio.tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // Re-exporting to the same path requires confirmation.
    assert!(matches!(start_export(p.clone(), o.clone()).err().unwrap(), EngineError::OutputExists(_)));
    let mut o2 = o.clone();
    o2.overwrite = true;
    assert!(export_blocking(&p, &o2).is_ok());
}

#[test]
fn h_export_cancel_leaves_no_partial_file() {
    let d = tempfile::tempdir().unwrap();
    let mut p = Project::new("long", 1280, 720, 30.0);
    let ot = track(&p, TrackKind::Overlay);
    let id = p.alloc_id();
    p.tracks.iter_mut().find(|t| t.id == ot).unwrap().clips.push(Clip::new(id, Content::Solid { color: [200, 50, 50, 255] }, 0, 60_000_000));
    let out = d.path().join("never.mp4");
    let h = start_export(p.clone(), ExportOptions::from_project(&p, out.clone(), None)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    h.cancel();
    let r = h.wait();
    assert!(matches!(r, Err(EngineError::Cancelled)), "{r:?}");
    assert!(!out.exists(), "a cancelled export must not leave a finished-looking file");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "temp files removed");
}

#[test]
fn speed_crop_color_and_video_audio_export() {
    let d = tempfile::tempdir().unwrap();
    let v = common::video(d.path());
    let mut p = Project::new("t", 320, 240, 30.0);
    let vid = media::import(&mut p, &v, ImportMode::Reference, None, None).unwrap();
    let vt = track(&p, TrackKind::Video);
    let c = p.media_clip(vid, 0).unwrap();
    let cid = p.add_clip(vt, c, true).unwrap();
    p.set_speed(cid, 2.0).unwrap();
    {
        let c = p.clip_mut(cid).unwrap();
        c.crop = Crop { left: 0.1, right: 0.1, top: 0.0, bottom: 0.0 };
        c.color = ColorAdjust { brightness: 0.05, contrast: 1.1, saturation: 0.5 };
        c.transition_out = Some(Transition { kind: TransitionKind::FadeToBlack, duration: 300_000 });
        c.transition_in = Some(Transition { kind: TransitionKind::FadeFromBlack, duration: 300_000 });
    }
    assert!((p.duration() - 1_000_000).abs() < 2000);
    let out = d.path().join("o.mp4");
    let rep = export_blocking(&p, &ExportOptions::from_project(&p, out.clone(), None)).unwrap();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    assert!(rep.has_audio, "video's own audio is exported");
    assert!((rep.duration_s - 1.0).abs() < 0.1);
    // Fade from/to black: first and last frames are dark.
    let grab = |t: f64| Command::new("ffmpeg").args(["-v", "error", "-ss", &t.to_string(), "-i"]).arg(&out).args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"]).output().unwrap().stdout;
    let mean = |b: Vec<u8>| b.iter().map(|&x| x as f64).sum::<f64>() / b.len() as f64;
    assert!(mean(grab(0.0)) < 12.0, "fade from black");
    assert!(mean(grab(0.5)) > 40.0);
}

#[test]
fn waveform_and_thumbnail_are_bounded_and_correct() {
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("LITEMOTION_CACHE_DIR", d.path().join("cache"));
    let m = common::music(d.path());
    let info = media::probe(&m).unwrap();
    let peaks = media::waveform(&m, info.duration_us, 200, &AtomicBool::new(false)).unwrap();
    assert_eq!(peaks.len(), 200);
    assert!(peaks.iter().filter(|p| **p > 0.1).count() > 180, "tone fills the waveform");
    let v = common::video(d.path());
    let vi = media::probe(&v).unwrap();
    let t = media::thumbnail(&v, MediaKind::Video, 500_000, 96, vi.width, vi.height).unwrap();
    assert_eq!((t.w, t.h), (96, 72));
    let t2 = media::thumbnail(&v, MediaKind::Video, 500_000, 96, vi.width, vi.height).unwrap();
    assert_eq!(t.rgba, t2.rgba, "second call is served from the disk cache");
    assert!(std::fs::read_dir(d.path().join("cache/thumbs")).unwrap().count() >= 1);
    // Cancelling a waveform job stops it promptly.
    let r = media::waveform(&m, info.duration_us, 50, &AtomicBool::new(true));
    assert!(matches!(r, Err(EngineError::Cancelled)));
    // Proxy generation produces a smaller, playable file; original untouched.
    let proxy = d.path().join("proxy.mp4");
    media::make_proxy(&v, &proxy, 120, &AtomicBool::new(false)).unwrap();
    assert_eq!(media::probe(&proxy).unwrap().height, 120);
}

fn collect_mix(p: &Project, from: i64) -> Vec<f32> {
    let mut v = vec![];
    litemotion_engine::audio::mix_range(p, None, from, p.duration(), &AtomicBool::new(false), &mut |_| {}, &mut |b| {
        v.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    v
}

/// Live preview starts mixing at the playhead. That must be the same audio
/// the export contains from that point, or preview and export would disagree.
#[test]
fn mixing_from_the_playhead_matches_the_full_mix() {
    let d = tempfile::tempdir().unwrap();
    let m = common::music(d.path());
    let mut p = Project::new("t", 640, 360, 30.0);
    let a = p.add_track(TrackKind::Audio, "Audio");
    let mid = media::import(&mut p, &m, ImportMode::Reference, None, None).unwrap();
    let mut c = p.media_clip(mid, 500_000).unwrap();
    c.source_in = 200_000;
    c.duration = 2_000_000;
    c.volume = 0.5;
    p.add_clip(a, c, false).unwrap();

    let full = collect_mix(&p, 0);
    let rate = litemotion_engine::audio::SAMPLE_RATE as usize;
    assert_eq!(full.len(), 2 * rate * 5 / 2, "0..2.5s of stereo samples");
    for start_us in [0i64, 300_000, 500_000, 1_300_000, 2_400_000] {
        let part = collect_mix(&p, start_us);
        let off = litemotion_engine::audio::us_to_sample(start_us) as usize * 2;
        assert_eq!(part.len(), full.len() - off, "length from {start_us}");
        let worst = part.iter().zip(&full[off..]).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "mix from {start_us}us differs from the full mix by {worst}");
    }
    // Sanity: the sound is really there (ffmpeg's sine is 0.125 peak; x0.5 gain -> ~0.044 RMS), and silent before the clip.
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    assert!((rms(&full[2 * rate..2 * rate * 2]) - 0.0442).abs() < 0.01);
    assert!(rms(&full[..rate / 4]) < 1e-6);
}

/// Cancelling a mix must not leave FFmpeg processes behind.
#[cfg(unix)] // uses pgrep
#[test]
fn cancelled_mix_reaps_ffmpeg() {
    let d = tempfile::tempdir().unwrap();
    let m = common::music(d.path());
    let mut p = Project::new("t", 640, 360, 30.0);
    let a = p.add_track(TrackKind::Audio, "Audio");
    let mid = media::import(&mut p, &m, ImportMode::Reference, None, None).unwrap();
    let c = p.media_clip(mid, 0).unwrap();
    p.add_clip(a, c, false).unwrap();
    let cancel = AtomicBool::new(false);
    let mut n = 0;
    let r = litemotion_engine::audio::mix_range(&p, None, 0, p.duration(), &cancel, &mut |_| {}, &mut |_| {
        n += 1;
        if n == 3 {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    });
    assert!(matches!(r, Err(EngineError::Cancelled)));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let out = Command::new("pgrep").args(["-f", &format!("{}", m.display())]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty(), "ffmpeg still running: {}", String::from_utf8_lossy(&out.stdout));
}
