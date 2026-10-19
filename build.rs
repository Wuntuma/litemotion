fn main() {
    println!("cargo:rerun-if-changed=csrc/shim.c");
    println!("cargo:rerun-if-changed=csrc/miniaudio.h");
    cc::Build::new().file("csrc/shim.c").include("csrc").opt_level(2).warnings(false).compile("lm_audio");
    // miniaudio loads the OS audio backends (ALSA, PulseAudio, CoreAudio, WASAPI)
    // at run time, so no audio development packages are needed to build.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "macos" {
        // Harmless if miniaudio loads these at run time instead.
        for f in ["CoreFoundation", "CoreAudio", "AudioToolbox"] {
            println!("cargo:rustc-link-lib=framework={f}");
        }
    }
    if os == "linux" {
        for l in ["dl", "pthread", "m"] {
            println!("cargo:rustc-link-lib={l}");
        }
    }
}
