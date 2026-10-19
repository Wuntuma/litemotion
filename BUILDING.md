# Building and running LiteMotion

LiteMotion is a Rust workspace (engine, app, cli, audio). The app starts from
`crates/app`; it needs **FFmpeg + ffprobe** at run time (and for the tests).

## Status by platform (honest)

| Platform | Built and tested here | Notes |
|---|---|---|
| Linux x86-64 | Yes: all tests pass, window run under a virtual display | |
| Windows 10/11 (x64) | **No** | CI is configured (`.github/workflows/ci.yml`) but has not been run yet. |
| macOS (Apple Silicon, Intel) | **No** | Same: CI configured, never run. |
| Android, iOS | Not supported | The editor is a desktop app; see README. |

The sandbox this was developed in could not install the Windows/macOS Rust
targets, so nothing was cross-compiled. Expect the first CI run on those
systems to find a few problems; the likely suspects are listed below.

## Prerequisites

Common: Rust stable (https://rustup.rs) and FFmpeg with `libx264` and `aac`.

- **Linux**: `build-essential` (a C compiler), `pkg-config`. At run time the
  window needs the usual desktop libraries (X11 or Wayland, `libxkbcommon`,
  OpenGL); sound needs ALSA or PulseAudio/PipeWire; file dialogs use
  xdg-desktop-portal. Nothing here needs `-dev` packages. FFmpeg:
  `sudo apt install ffmpeg`.
- **Windows**: Visual Studio Build Tools with "Desktop development with C++"
  (the audio crate compiles one C file). FFmpeg: `choco install ffmpeg` or
  `winget install Gyan.FFmpeg`, or copy `ffmpeg.exe` and `ffprobe.exe` next to
  `litemotion.exe` (or into a `bin` folder there).
- **macOS**: Xcode command line tools (`xcode-select --install`). FFmpeg:
  `brew install ffmpeg`.

## Build, run, test

```
cargo build --release -p litemotion-app -p litemotion-cli
cargo run --release -p litemotion-app                  # the editor
cargo run --release -p litemotion-app -- my.lmproj     # open a project
cargo run --release -p litemotion-cli -- --help        # headless tool
cargo test --workspace --locked
```

## Environment variables

| Variable | Effect |
|---|---|
| `LITEMOTION_FFMPEG`, `LITEMOTION_FFPROBE` | Full path to the tools (checked first; then next to the executable, then `PATH`). |
| `LITEMOTION_DATA_DIR`, `LITEMOTION_CACHE_DIR` | Where settings, recent list, autosave and thumbnails live. Tests use temporary folders. |
| `LITEMOTION_AUDIO_BACKEND=null` | Silent, real-time audio backend (for machines or CI without a sound card). |

## Continuous integration

- `ci.yml` (push and pull request): installs FFmpeg, builds and runs the whole
  test suite on Ubuntu, Windows and macOS, and checks that
  `docs/THIRD_PARTY_LICENSES.md` is up to date (`python3 scripts/license_report.py`).
- `release.yml` (manual run, or a `v*` tag): builds release binaries for Linux
  x86-64, Windows x64, macOS Apple Silicon and macOS Intel (the Intel build is
  cross-compiled on the Apple Silicon runner and not run), packages them with
  docs and the sample, uploads them as workflow artifacts and, for tags, creates
  a **draft** GitHub release for you to review. Packages are plain
  zip/tar.gz archives: there are no installers, no code signing and no macOS
  notarisation yet, so Windows SmartScreen and macOS Gatekeeper will warn.
- FFmpeg is not bundled in the packages (licensing: see
  `THIRD_PARTY_LICENSES.md`); users install it or drop it into the `bin` folder.

The repository needs to be pushed to GitHub for either workflow to run.

## First things to check when CI runs on Windows/macOS

1. FFmpeg from Chocolatey/Homebrew has `libx264` and `aac` (the sanity step prints this).
2. Tests that compare file paths: Windows paths use `\` and canonical `\\?\`
   prefixes (the engine strips the prefix before handing paths to FFmpeg; this
   is unit-tested only as string handling).
3. The audio crate compiles `miniaudio` with MSVC / Apple clang. macOS links
   CoreFoundation, CoreAudio and AudioToolbox explicitly.
4. Timing-based tests (`audio` clock, preview following the audio clock) use
   generous bounds but may need loosening on slow shared runners.
