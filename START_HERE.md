# LiteMotion Studio: how to run it

You get a real editor window. Pick ONE route.

## Route A: on your own Windows PC (about 15 minutes the first time)
1. Unzip this folder somewhere (for example `C:\LiteMotion`).
2. Install **Rust**: https://rustup.rs (download `rustup-init.exe`, accept the defaults;
   it will offer to install the Visual Studio C++ build tools, say yes).
3. Install **FFmpeg**: open PowerShell and run `winget install Gyan.FFmpeg`. Close all windows afterwards.
4. Double-click **`run-windows.bat`**. The first run compiles for several minutes, then the
   editor opens with a sample project. (Mac/Linux: run `./run.sh` instead.)

## Route B: get a ready-made Windows program from GitHub (no Rust needed)
1. Make a free account at github.com and create a new **private** repository.
2. Upload everything in this folder to it (on the repo page: Add file, Upload files; drag the
   whole folder contents in, including the hidden `.github` folder).
3. Open the **Actions** tab, choose **Release build**, click **Run workflow**.
4. After about 15 minutes, open the finished run and download **litemotion-windows-x86_64**
   from "Artifacts". Unzip it, install FFmpeg (step 3 above) and run `litemotion.exe`.
   Windows may warn that the app is unrecognised: it isn't signed yet.

## Things to try once it opens
- Click the ruler to move the playhead; click **Play** (Space).
- Click a clip, then change values on the right. Press **Key** next to a value to animate it.
- Drag a clip to move it, drag its edges to trim, press **S** to split, **Ctrl+Z** to undo.
- **Import** your own video, picture or music, then **Add**. Use **Create** for titles and shapes.
- **Export** makes an MP4.

Not tested on Windows or macOS yet. If something goes wrong, copy the message you see and send it to me.
