//! Assembles openh264's BSD-2 x86 asm kernels with nasm and links them.
//!
//! Paths are overridable via env: `OPENH264_DIR` (the cloned openh264 tree) and
//! `NASM` (the nasm executable). Defaults point at this machine's checkout; the
//! productionised crate will vendor the `.asm` files so no external clone is needed.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let oh = std::env::var("OPENH264_DIR")
        .unwrap_or_else(|_| "C:/Users/talmo/coding/openh264".to_string());
    let nasm = std::env::var("NASM")
        .unwrap_or_else(|_| "C:/Users/talmo/nasm-portable/nasm-2.16.03/nasm.exe".to_string());
    let inc = format!("{oh}/codec/common/x86/");

    // The kernels to assemble. Start with SATD (toolchain proof); expand here.
    let asm_files = ["codec/common/x86/satd_sad.asm"];

    let mut build = cc::Build::new();
    for rel in asm_files {
        let asm = format!("{oh}/{rel}");
        let stem = PathBuf::from(&asm)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let obj = format!("{out_dir}/{stem}.obj");
        let status = Command::new(&nasm)
            .args([
                "-f", "win64", "-DWIN64", "-DHAVE_AVX2", "-I", &inc, &asm, "-o", &obj,
            ])
            .status()
            .expect("failed to run nasm — set NASM to the nasm.exe path");
        assert!(status.success(), "nasm failed assembling {asm}");
        build.object(&obj);
        println!("cargo:rerun-if-changed={asm}");
    }
    build.compile("wels_asm");
    println!("cargo:rerun-if-changed=build.rs");
}
