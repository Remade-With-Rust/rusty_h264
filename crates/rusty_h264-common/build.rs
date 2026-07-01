//! Defines the internal `accel` cfg = "the `asm` feature is on AND we target x86_64".
//!
//! The vendored openh264 SIMD kernels in `rusty_h264-accel` are x86-64 only, so the
//! `accel` code paths must not be compiled on other architectures even when a downstream
//! crate enables the (default) `asm` feature. On non-x86_64 targets `accel` stays unset
//! and the codec uses its pure-Rust scalar path — so e.g. `rff` builds on arm64 macOS
//! with default features and without nasm.
fn main() {
    // Declare the custom cfg so `unexpected_cfgs` (Rust 1.80+) stays quiet; older Cargo
    // ignores the line (and lacks the lint anyway).
    println!("cargo::rustc-check-cfg=cfg(accel)");
    let asm = std::env::var_os("CARGO_FEATURE_ASM").is_some();
    let x86_64 = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64");
    if asm && x86_64 {
        // Single-colon form: understood by every Cargo version.
        println!("cargo:rustc-cfg=accel");
    }
}
