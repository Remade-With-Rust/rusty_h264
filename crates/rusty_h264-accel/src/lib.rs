//! SIMD acceleration for rusty_h264 — **portable Rust intrinsics**, with a shrinking
//! remainder of vendored openh264 x86 assembly.
//!
//! This crate is deliberately **not** `#![forbid(unsafe_code)]`: it is the one place
//! `unsafe` lives, behind safe wrappers, so the codec core stays `forbid(unsafe)`.
//!
//! ## Structure, and where it is going
//!
//! `docs/add_SIMD_rip_ASM.md` is ripping the assembly out kernel by kernel. Two things
//! follow from that, and this file is arranged around them:
//!
//! * **`x86_asm`** holds everything still backed by openh264 NASM. It is gated on
//!   `target_arch = "x86_64"` and shrinks with every phase of the campaign.
//! * **Portable modules** (`chroma_mc`, …) hold Rust intrinsics with an x86-64 path, an
//!   aarch64 NEON path, and a scalar reference that all three are tested bit-identical
//!   against. These compile and run on **every** architecture.
//!
//! Until the campaign finishes, the crate is a mix. The whole crate used to be
//! `#![cfg(target_arch = "x86_64")]` — compiled to nothing on ARM, which is why aarch64
//! ran fully scalar. That gate now sits on the `x86_asm` module alone, so portable
//! kernels reach ARM as they land.
//!
//! **Order matters: replace, then rip.** The vendored assembly measures ~1.94x on decode
//! (paired, N=5, 34/35 reps above 1.0), so deleting a kernel before its portable
//! replacement is bit-identical and no slower would ship a real regression.
//!
//! openh264 asm is BSD-2 licensed; attribution lives in `vendor/LICENSE.openh264`.
#![allow(non_snake_case)]

// --- portable: every architecture --------------------------------------------------
mod chroma_mc;
pub use chroma_mc::{mc_chroma_w4, mc_chroma_w8};
mod deblock_simd;
pub use deblock_simd::{
    deblock_chroma_eq4_h, deblock_chroma_eq4_v, deblock_chroma_lt4_h, deblock_chroma_lt4_v,
    deblock_luma_eq4_h, deblock_luma_eq4_v, deblock_luma_lt4_h, deblock_luma_lt4_v,
};
mod luma_mc;
mod satd_sad;
// Portable transform/quant. MEASURED SLOWER than the openh264 assembly on x86-64
// (fast preset 1.253 against a 12.7% floor; quality 1.031, within floor), so x86-64
// keeps the assembly and this serves every OTHER architecture — which previously had
// no implementation at all. Reopen the x86 swap with SIMD intrinsics; the scalar
// shape was not enough here, unlike the 4x4 kernels LLVM does vectorise well.
// On x86-64 the whole module is reached only by its unit tests (the asm serves
// production), so allow dead_code there rather than losing the warnings elsewhere.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
mod transform_quant;
#[cfg(not(target_arch = "x86_64"))]
pub use transform_quant::{dct_four_t4, idct_four_t4_rec, quant_four_4x4};
pub use satd_sad::{
    sad_16x16, sad_16x8, sad_8x16, satd_16x16, satd_16x8, satd_4x4, satd_8x16, satd_8x8,
};
pub use luma_mc::{mc_centre, mc_hor20, mc_hor_qpel, mc_ver02, mc_ver02_avg, mc_ver_qpel, pixel_avg};

// --- still assembly-backed: x86-64 only ---------------------------------------------
#[cfg(target_arch = "x86_64")]
mod x86_asm;
#[cfg(target_arch = "x86_64")]
pub use x86_asm::*;
