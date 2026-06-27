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
    println!("cargo:rerun-if-env-changed=OPENH264_DIR");
    println!("cargo:rerun-if-env-changed=NASM");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    // The openh264 `.asm` kernels are vendored under `vendor/` (BSD-2; see
    // vendor/LICENSE.openh264), so no external checkout is needed — only `nasm`.
    // `OPENH264_DIR` still overrides the source dir for development against a live
    // openh264 tree.
    let oh = std::env::var("OPENH264_DIR")
        .unwrap_or_else(|_| format!("{}/vendor", env!("CARGO_MANIFEST_DIR")));
    let nasm = std::env::var("NASM").unwrap_or_else(|_| "nasm".to_string());

    // If nasm isn't available, build NOTHING — the crate still compiles as a lib
    // (the `extern "C"` symbols only need to resolve when something actually links
    // them, i.e. a downstream `--features asm` *binary*). This keeps the crate
    // publishable + docs.rs-buildable; enabling `asm` without nasm then surfaces a
    // clear link error rather than a build-script panic.
    if std::process::Command::new(&nasm).arg("-v").output().is_err() {
        println!(
            "cargo:warning=rusty_h264-accel: `nasm` not found — skipping asm kernels. \
             Install nasm (e.g. `apt install nasm` / `brew install nasm`) to enable the \
             `asm` feature's SIMD, or set NASM to the nasm path."
        );
        return;
    }

    // nasm include search path: the common x86 dir holds `asm_inc.asm`, which every
    // kernel `%include`s.
    let inc_dirs = [
        "codec/common/x86",
        "codec/encoder/core/x86",
        "codec/decoder/core/x86",
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
    ];

    // Per-target object format + calling-convention define, matching openh264's
    // asm_inc.asm (`WIN64` / `UNIX64`, plus `PREFIX` for Mach-O's leading-underscore
    // C symbols). x86-64 only — the kernels are 64-bit.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let (obj_fmt, plat_defs): (&str, &[&str]) = match target_os.as_str() {
        "windows" => ("win64", &["-DWIN64"]),
        "macos" | "ios" => ("macho64", &["-DUNIX64", "-DPREFIX"]),
        _ => ("elf64", &["-DUNIX64"]),
    };
    let mut build = cc::Build::new();
    let mut nasm_args: Vec<String> = vec!["-f".into(), obj_fmt.into(), "-DHAVE_AVX2".into()];
    nasm_args.extend(plat_defs.iter().map(|s| s.to_string()));
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
