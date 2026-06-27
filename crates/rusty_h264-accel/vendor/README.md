# Vendored openh264 x86 assembly

These `.asm` kernels are copied verbatim from [Cisco openh264]
(https://github.com/cisco/openh264) (`codec/{common,encoder,decoder}/.../x86/`),
which is **BSD-2-Clause** — see `LICENSE.openh264` (© 2013 Cisco Systems). They are
assembled by `build.rs` with `nasm` when the `asm` feature is enabled. Vendored so
`rusty_h264-accel` is self-contained (no external openh264 checkout needed).
