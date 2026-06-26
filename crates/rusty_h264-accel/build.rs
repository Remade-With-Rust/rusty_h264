//! Assembles openh264's BSD-2 x86 asm kernels with nasm and links them.
//!
//! Paths are overridable via env: `OPENH264_DIR` (the cloned openh264 tree) and
//! `NASM` (the nasm executable). Defaults point at this machine's checkout; the
//! productionised crate will vendor the `.asm` files so no external clone is needed.
//!
//! We assemble openh264's full primary asm set (common + encoder + decoder +
//! preprocessing). Each `.asm` becomes one `.obj`; the safe Rust FFI wrappers that
//! call into them live in `src/`. Kernels are wired into the encoder incrementally,
//! always *alongside* the pure-Rust scalar versions (selected by `--features asm`).

use std::process::Command;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let oh = std::env::var("OPENH264_DIR")
        .unwrap_or_else(|_| "C:/Users/talmo/coding/openh264".to_string());
    let nasm = std::env::var("NASM")
        .unwrap_or_else(|_| "C:/Users/talmo/nasm-portable/nasm-2.16.03/nasm.exe".to_string());

    // nasm include search paths: each layer's x86 dir (for `%include "asm_inc.asm"`
    // and layer-local includes).
    let inc_dirs = [
        "codec/common/x86",
        "codec/encoder/core/x86",
        "codec/decoder/core/x86",
        "codec/processing/src/x86",
    ];

    // openh264's full primary asm set. `asm_inc.asm` is macros-only (included by the
    // others), so it is NOT assembled directly. Object names are derived from the
    // full relative path to avoid stem collisions (common/dct.asm vs encoder/dct.asm).
    let asm_files = [
        // --- common ---
        "codec/common/x86/cpuid.asm",
        "codec/common/x86/dct.asm",
        "codec/common/x86/deblock.asm",
        "codec/common/x86/expand_picture.asm",
        "codec/common/x86/intra_pred_com.asm",
        "codec/common/x86/mb_copy.asm",
        "codec/common/x86/mc_chroma.asm",
        "codec/common/x86/mc_luma.asm",
        "codec/common/x86/satd_sad.asm",
        "codec/common/x86/vaa.asm",
        // --- encoder core ---
        "codec/encoder/core/x86/coeff.asm",
        "codec/encoder/core/x86/dct.asm",
        "codec/encoder/core/x86/intra_pred.asm",
        "codec/encoder/core/x86/matrix_transpose.asm",
        "codec/encoder/core/x86/memzero.asm",
        "codec/encoder/core/x86/quant.asm",
        "codec/encoder/core/x86/sample_sc.asm",
        "codec/encoder/core/x86/score.asm",
        // --- decoder core ---
        "codec/decoder/core/x86/dct.asm",
        "codec/decoder/core/x86/intra_pred.asm",
        // --- preprocessing ---
        "codec/processing/src/x86/denoisefilter.asm",
        "codec/processing/src/x86/downsample_bilinear.asm",
        "codec/processing/src/x86/vaa.asm",
    ];

    let mut build = cc::Build::new();
    let mut nasm_args: Vec<String> = vec![
        "-f".into(), "win64".into(),
        "-DWIN64".into(), "-DHAVE_AVX2".into(),
    ];
    for d in inc_dirs {
        nasm_args.push("-I".into());
        nasm_args.push(format!("{oh}/{d}/"));
    }

    for rel in asm_files {
        let asm = format!("{oh}/{rel}");
        // Unique object stem from the relative path: a/b/c.asm -> a_b_c.
        let stem = rel
            .trim_end_matches(".asm")
            .replace(['/', '\\'], "_");
        let obj = format!("{out_dir}/{stem}.obj");
        let mut args = nasm_args.clone();
        args.push(asm.clone());
        args.push("-o".into());
        args.push(obj.clone());
        let status = Command::new(&nasm)
            .args(&args)
            .status()
            .expect("failed to run nasm — set NASM to the nasm.exe path");
        assert!(status.success(), "nasm failed assembling {asm}");
        build.object(&obj);
        println!("cargo:rerun-if-changed={asm}");
    }
    build.compile("wels_asm");
    println!("cargo:rerun-if-changed=build.rs");
}
