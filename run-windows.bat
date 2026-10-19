@echo off
REM One-click: builds LiteMotion Studio and opens the sample project.
REM First run takes several minutes (it compiles everything). Later runs start fast.
where cargo >nul 2>nul
if errorlevel 1 (
  echo Rust is not installed. Install it from https://rustup.rs  ^(accept the defaults, including the C++ build tools^), then run this file again.
  pause
  exit /b 1
)
where ffmpeg >nul 2>nul
if errorlevel 1 (
  echo FFmpeg is not installed. Open a new window and run:  winget install Gyan.FFmpeg
  echo Then close all windows and run this file again.
  pause
  exit /b 1
)
cargo run --release -p litemotion-app -- samples\demo\sample.lmproj
pause
