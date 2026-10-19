//! Headless LiteMotion tool: inspect, validate, export and build the sample.
use litemotion_engine::animation::*;
use litemotion_engine::media::{self, ImportMode};
use litemotion_engine::platform;
use litemotion_engine::project::io;
use litemotion_engine::project::model::*;
use litemotion_engine::rendering::export::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn gen(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let ff = platform::ffmpeg().unwrap_or_else(|e| die(e));
    let st = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).arg(&out).status().unwrap_or_else(|e| die(e));
    if !st.success() {
        die(format!("couldn't generate {name}"));
    }
    out
}

fn run_export(p: &Project, out: PathBuf, dir: Option<PathBuf>, overwrite: bool, hw: bool) {
    let mut o = ExportOptions::from_project(p, out, dir);
    o.overwrite = overwrite;
    o.use_hardware = hw;
    let h = start_export(p.clone(), o).unwrap_or_else(|e| die(e));
    let mut last = -1i32;
    while !h.is_finished() {
        let pr = h.progress();
        let pct = (pr.fraction * 100.0) as i32;
        if pct != last && pct % 10 == 0 {
            eprintln!("  {pct:>3}%  {}  ({:.1}s elapsed)", pr.status, pr.elapsed_secs);
            last = pct;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    match h.wait() {
        Ok(r) => {
            println!("exported {} — {:.2}s {}x{} @ {:.2} fps, audio: {}, encoder: {}, took {:.1}s", r.path.display(), r.duration_s, r.width, r.height, r.fps, r.has_audio, r.encoder, r.wall_secs);
            if r.warnings.is_empty() {
                println!("verified: length, size, frame rate and audio match the project");
            } else {
                for w in &r.warnings {
                    println!("WARNING: {w}");
                }
                std::process::exit(2);
            }
        }
        Err(e) => die(e),
    }
}

/// Builds the sample project from synthetic (FFmpeg-generated, therefore freely
/// distributable) media and exports it.
fn demo(dir: &Path, seconds: u32, w: u32, h: u32) {
    std::fs::create_dir_all(dir.join("media")).unwrap();
    let md = dir.join("media");
    let s = seconds.to_string();
    let vid = gen(&md, "background.mp4", &["-f", "lavfi", "-i", &format!("testsrc2=size=640x360:rate=30:duration={s}"), "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "30"]);
    let logo = gen(&md, "logo.png", &["-f", "lavfi", "-i", "color=c=0xff6a00:s=256x256,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(hypot(X-128,Y-128),120),255,0)'", "-frames:v", "1"]);
    let music = gen(&md, "music.wav", &["-f", "lavfi", "-i", &format!("sine=frequency=330:duration={s}")]);

    let mut p = Project::new("LiteMotion sample", w, h, 30.0);
    let (v, l, m) = (
        media::import(&mut p, &vid, ImportMode::Reference, Some(dir), None).unwrap_or_else(|e| die(e)),
        media::import(&mut p, &logo, ImportMode::Reference, Some(dir), None).unwrap_or_else(|e| die(e)),
        media::import(&mut p, &music, ImportMode::Reference, Some(dir), None).unwrap_or_else(|e| die(e)),
    );
    let tid = |p: &Project, k| p.tracks.iter().find(|t| t.kind == k).unwrap().id;
    let c = p.media_clip(v, 0).unwrap();
    let (vt, ot, tt, at) = (tid(&p, TrackKind::Video), tid(&p, TrackKind::Overlay), tid(&p, TrackKind::Text), tid(&p, TrackKind::Audio));
    let cid = p.add_clip(vt, c, true).unwrap();
    p.clip_mut(cid).unwrap().transition_in = Some(Transition { kind: TransitionKind::FadeFromBlack, duration: 500_000 });
    let mut logo_clip = p.media_clip(l, 1_000_000).unwrap();
    logo_clip.duration = (seconds as i64 - 1) * 1_000_000;
    logo_clip.scale = 0.35;
    logo_clip.x = 0.32;
    logo_clip.y = -0.3;
    apply_preset(&mut logo_clip, Preset::LogoReveal, 800_000);
    logo_clip.set_key(Prop::Rotation, 0, 0.0, Interp::Linear);
    logo_clip.set_key(Prop::Rotation, logo_clip.duration, 360.0, Interp::EaseInOut);
    p.add_clip(ot, logo_clip, false).unwrap();
    let t_id = p.alloc_id();
    let mut title = Clip::new(t_id, Content::Text(TextStyle { text: "Hello, LiteMotion".into(), size_pct: 10.0, outline: true, ..Default::default() }), 500_000, (seconds as i64 - 1) * 1_000_000);
    apply_preset(&mut title, Preset::SlideIn(Direction::Up), 700_000);
    apply_preset(&mut title, Preset::FadeOut, 500_000);
    p.add_clip(tt, title, false).unwrap();
    p.lower_third("Offline video editing", "No account. No upload.", 1_500_000, (seconds as i64 - 2) * 1_000_000).unwrap();
    let mut mc = p.media_clip(m, 0).unwrap();
    mc.volume = 0.4;
    mc.fade_in = 500_000;
    mc.fade_out = 500_000;
    p.add_clip(at, mc, false).unwrap();

    let proj_path = dir.join("sample.lmproj");
    io::save(&p, &proj_path).unwrap_or_else(|e| die(e));
    println!("saved {}", proj_path.display());
    run_export(&p, dir.join("sample.mp4"), Some(dir.to_path_buf()), true, false);
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("info") => {
            println!("ffmpeg:  {}", platform::ffmpeg().map(|p| p.display().to_string()).unwrap_or_else(|e| e.to_string()));
            println!("ffprobe: {}", platform::ffprobe().map(|p| p.display().to_string()).unwrap_or_else(|e| e.to_string()));
            match platform::encoders() {
                Ok(e) => println!("encoders: libx264={} openh264={} aac={} hardware(tested)={:?}", e.libx264, e.openh264, e.aac, e.hardware),
                Err(e) => println!("encoders: {e}"),
            }
        }
        Some("probe") if a.len() == 2 => match media::probe(Path::new(&a[1])) {
            Ok(i) => println!("{i:#?}"),
            Err(e) => die(e),
        },
        Some("validate") if a.len() == 2 => match io::load(Path::new(&a[1])) {
            Ok(p) => {
                let dir = Path::new(&a[1]).parent().map(Path::to_path_buf);
                let miss = io::missing_media(&p, dir.as_deref());
                println!("ok: '{}' {}x{} @ {} fps, {:.2}s, {} media, {} missing", p.name, p.width, p.height, p.fps, litemotion_engine::time::to_secs(p.duration()), p.media.len(), miss.len());
            }
            Err(e) => die(e),
        },
        Some("export") if a.len() >= 3 => {
            let proj = Path::new(&a[1]);
            let (p, bak) = io::load_with_fallback(proj).unwrap_or_else(|e| die(e));
            if bak {
                eprintln!("note: the project file was damaged; using the backup copy");
            }
            let dir = proj.canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf));
            let mut p = p;
            if let Some(s) = a.iter().position(|x| x == "--size") {
                let wh: Vec<u32> = a[s + 1].split('x').filter_map(|v| v.parse().ok()).collect();
                if wh.len() == 2 {
                    p.export.width = wh[0];
                    p.export.height = wh[1];
                }
            }
            run_export(&p, PathBuf::from(&a[2]), dir, a.iter().any(|x| x == "--overwrite"), a.iter().any(|x| x == "--hw"));
        }
        Some("demo") if a.len() >= 2 => {
            let secs = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(6);
            demo(Path::new(&a[1]), secs, 1280, 720);
        }
        _ => {
            eprintln!("usage: litemotion-cli info | probe <file> | validate <proj> | export <proj> <out.mp4> [--overwrite] [--hw] [--size WxH] | demo <dir> [seconds]");
            std::process::exit(64);
        }
    }
}
