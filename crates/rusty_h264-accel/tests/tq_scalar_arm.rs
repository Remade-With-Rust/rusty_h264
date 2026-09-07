// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Dev tools also accumulate fields and helpers kept for the NEXT investigation;
// dead code here is a scratchpad, not a defect.
#![allow(dead_code, unused)]
#![allow(clippy::unnecessary_unwrap, clippy::zombie_processes)]
// The RFF_TQ_SCALAR env knob is a build-time constant unless the crate is built with
// `--features knobs` (routing round 2026-09-05), so this liveness gate runs only then:
//   cargo test -p rusty_h264-accel --features knobs --test tq_scalar_arm
#![cfg(feature = "knobs")]

//! H10 oracle-arm liveness gate: prove `RFF_TQ_SCALAR=1` actually engages.
//!
//! Integration test = its own process, so setting the env var before the first
//! dispatcher call beats the knob's once-per-process cache — the thing a unit
//! test inside the crate's shared test process cannot guarantee. The output
//! equality asserts are then the differential oracle run through the PUBLIC
//! dispatchers on the forced arm (not the scalar fns called directly, which is
//! what the in-crate tests already do).

use rusty_h264_accel::{dct_four_t4, idct_four_t4_rec, quant_four_4x4};

#[test]
fn scalar_arm_engages_and_matches() {
    // Before ANY dispatcher call in this process.
    std::env::set_var("RFF_TQ_SCALAR", "1");
    assert!(
        rusty_h264_accel::tq_scalar_forced(),
        "RFF_TQ_SCALAR=1 must engage the scalar oracle arm"
    );

    // Deterministic pseudo-random pixels (no external RNG dep).
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 24) as u8
    };
    let src: Vec<u8> = (0..64).map(|_| next()).collect();
    let pred: Vec<u8> = (0..64).map(|_| next()).collect();

    // dct → quant → idct-recon through the forced-scalar public dispatchers.
    let mut dct = [0i16; 64];
    dct_four_t4(&mut dct, &src, 8, &pred, 8);
    let ff = [4i16; 8];
    let mf = [5243i16; 8];
    quant_four_4x4(&mut dct, &ff, &mf);
    let mut rec = [0u8; 64];
    idct_four_t4_rec(&mut rec, 8, &pred, 8, &dct);

    // The same chain on the scalar twins directly must agree bit-for-bit.
    let mut dct2 = [0i16; 64];
    rusty_h264_accel::dct_four_t4_scalar(&mut dct2, &src, 8, &pred, 8);
    rusty_h264_accel::quant_four_4x4_scalar(&mut dct2, &ff, &mf);
    let mut rec2 = [0u8; 64];
    rusty_h264_accel::idct_four_t4_rec_scalar(&mut rec2, 8, &pred, 8, &dct2);
    assert_eq!(dct, dct2, "forced arm must be the scalar twin (dct+quant)");
    assert_eq!(rec, rec2, "forced arm must be the scalar twin (idct-recon)");
}
