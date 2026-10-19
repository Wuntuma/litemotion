# LiteMotion Studio

A small, offline video editor and 2D animation tool. No account, no network
access, nothing uploaded.

## Architecture decision
**Rust shared engine + egui desktop app + FFmpeg as an external tool.**

- `crates/engine`: project model, timeline editing with undo/redo, keyframe
  animation, media probing, software compositor, audio mixer, FFmpeg export.
  No GUI code, so all of it is testable headlessly.
- `crates/app`: the editor window (egui/eframe, native file dialogs via `rfd`).
- `crates/audio`: live sound output for the preview (vendored `miniaudio`
  v0.11.25, public domain / MIT-0, with a ~60-line C shim and a Rust ring buffer
  that doubles as the playback clock).
- `crates/cli`: headless tool (`litemotion-cli`): inspect, validate, export and
  rebuild the sample.

Why: Rust gives a small native binary, no garbage collector, and one codebase
for the editing logic; egui keeps the UI dependency light (no browser engine,
no Electron); FFmpeg is a proven decoder/encoder, so no codecs are written here.
Qt and Flutter were not used because neither toolchain was available to build
and test in this environment, and I would not ship what I couldn't run.

## Run / build
Requires Rust (stable) and FFmpeg + ffprobe. LiteMotion looks for them in
`$LITEMOTION_FFMPEG` / `$LITEMOTION_FFPROBE`, next to the executable (or in a
`bin/` folder there), then on `PATH`.

```
cargo run --release -p litemotion-app                 # editor
cargo run --release -p litemotion-app -- my.lmproj    # open a project directly
cargo run --release -p litemotion-cli -- --help       # headless tool
cargo test --workspace                                # all tests
```
Linux build needs the usual X11/Wayland + OpenGL libraries (and for the file
dialogs a desktop portal). A sample project is in `samples/demo`.

## What works (verified, Linux x86-64, 2 cores, 8 GB, software rendering)
Automated: 29 tests pass (`cargo test --workspace`): 14 for project/timeline/
animation/persistence, 11 for media probing, export, verification and audio
mixing, 2 for the audio device clock, and 2 headless UI tests that drive real
egui frames with real imported media (home screen, all clip types in desktop and
phone-width layouts, dialogs, background previews, thumbnails, waveforms,
keyboard shortcuts, save/reopen, missing-media dialog, and video following the
audio clock).

By hand, in the real window under a virtual display, with synthetic mouse
input: opening a project, seeking by clicking the ruler, selecting a clip and
seeing its properties, dragging a clip, trimming a clip edge, Play advancing in
real time, Undo restoring both edits.

Not driven by hand: the Export dialog in the window (it opens a native save
dialog, which a virtual display can't answer). The same export code is covered
by the engine tests and `litemotion-cli export`.

Live preview sound: mixing from the playhead is tested to equal the export's
audio from that point (within 0.002 of full scale), cancelling leaves no FFmpeg
processes behind, and the video position is tested to track what the audio
device has actually played (within 0.25 s). The device tests run on miniaudio's
silent real-time backend, so **I have not heard it on real speakers**; try it on
your machine. With no sound hardware miniaudio falls back to that silent
backend, so playback still works.

Features: projects (aspect/resolution/fps presets, custom size), import by
dialog or drag-and-drop (video, audio, pictures), thumbnails and waveforms in
the background, timeline (move, trim, split, delete, duplicate, copy/paste,
snapping, zoom, hide/mute, multi-select), undo/redo, titles/captions/lower
thirds/shapes/backgrounds, transforms, crop, fit, speed, volume, fades, colour
adjust, transitions, keyframes (position, size, rotation, opacity, volume) with
four easing modes and ready-made animations, preview quality (auto lowers
resolution when playback can't keep up), autosave and crash recovery, relinking
moved media, MP4 (H.264 + AAC) export with progress, cancel and verification.

## Not verified / known limitations (please read)
- **Only Linux was built and run.** Windows and macOS build/test/release
  workflows are written (`.github/workflows`) but have never run, because this
  environment can't run them and the repo has not been pushed anywhere. There
  are no installers, code signing or notarisation yet.
- **Android and iOS are not implemented.** The narrow-window layout (bigger
  buttons, drawers instead of side panels) exists and was exercised headlessly,
  but it has never run on a phone, and the FFmpeg-as-a-program approach does not
  carry over to mobile without a different media backend.
- **Live preview sound is untested on real audio hardware** (see above). Seeking
  or editing during playback stops it; there is no scrub audio.
- **Proxy media:** the engine can make proxies, but the app has no control for
  it yet.
- Dragging moves one clip at a time (several selected clips can be deleted,
  copied and duplicated together).
- Export has not been timed on real hardware; hardware encoders are detected
  but untested here. No performance report yet.
- Fit-to-screen is automatic (the preview always fits); there is no manual zoom.
- Bundled third-party code: `miniaudio` (public domain / MIT-0). Everything else
  comes from crates.io; the dependency licence report is in `docs/THIRD_PARTY_LICENSES.md`
  (one weak-copyleft crate, `option-ext` under MPL-2.0, is flagged there).
- FFmpeg builds with x264 are GPL: if you ship FFmpeg with the app, follow its
  licence (details in the licence report). The packages the release workflow
  builds do not include FFmpeg.
- The egui interface has not been reviewed by a screen reader.

## Next steps
Proxy button, multi-clip dragging, Windows/macOS
builds (CI first run), installers and signing, performance measurements, mobile backend.

See `docs/BUILDING.md` (per-platform build steps, CI, status), `docs/PROJECT_FORMAT.md` (file format) and `docs/THIRD_PARTY_LICENSES.md` (dependency licences, regenerate with `python3 scripts/license_report.py`).
