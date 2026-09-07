// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Library face of the bench harness.
//!
//! Exists for ONE reason: so the bin and every example share a single copy of
//! the campaign-gating arithmetic in [`metrics`]. Before this file, `ssim_db`
//! lived in 2 Rust copies with DIFFERENT dB caps (90 vs 60), `bits()` lived in
//! 3 with two different clamp policies under one name, and the BD-rate
//! fit/integration was duplicated wholesale — the three-copies-will-drift law
//! (fast-transcendentals plan, A4/A6) applied to the instruments themselves.
//! An instrument fork is worse than a codec fork: two tools "measuring BD-rate"
//! with different arithmetic gate campaigns against different rulers.

pub mod metrics;
