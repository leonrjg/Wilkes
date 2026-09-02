//! Compiles the Objective-C side of the Vision recognizer.
//!
//! Only when `recognize-vision` is on *and* the target is macOS. The feature is
//! not itself gated on the platform — Cargo features are additive across a
//! workspace and a feature that fails to build off-Darwin would make a
//! `--all-features` check on any other platform a lie — so the platform test
//! lives here, where the answer is known.

fn main() {
    println!("cargo:rerun-if-changed=src/extract/image/vision_shim.m");

    let wanted = std::env::var_os("CARGO_FEATURE_RECOGNIZE_VISION").is_some();
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if !wanted || !macos {
        return;
    }

    cc::Build::new()
        .file("src/extract/image/vision_shim.m")
        .flag("-fobjc-arc")
        .flag("-fmodules")
        .compile("wilkes_vision_shim");

    for framework in ["Foundation", "CoreGraphics", "Vision"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
