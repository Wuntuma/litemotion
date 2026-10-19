#!/usr/bin/env bash
# Builds LiteMotion Studio and opens the sample project (Linux / macOS).
set -e
command -v cargo >/dev/null || { echo "Install Rust first: https://rustup.rs"; exit 1; }
command -v ffmpeg >/dev/null || { echo "Install FFmpeg first (Linux: sudo apt install ffmpeg, macOS: brew install ffmpeg)"; exit 1; }
cargo run --release -p litemotion-app -- samples/demo/sample.lmproj
