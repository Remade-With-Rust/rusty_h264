// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Dev tools also accumulate fields and helpers kept for the NEXT investigation;
// dead code here is a scratchpad, not a defect.
#![allow(dead_code, unused)]
#![allow(clippy::unnecessary_unwrap, clippy::zombie_processes)]
// Repro probe: fresh-process decode_stream vs in-process decode-after-encode.
fn main() {
    let f = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&f).unwrap();
    let frames = rusty_h264_decoder::Decoder::new().decode_stream(&bytes).unwrap();
    let mut raw = Vec::new();
    for fr in &frames {
        raw.extend_from_slice(&fr.y);
        raw.extend_from_slice(&fr.u);
        raw.extend_from_slice(&fr.v);
    }
    std::fs::write(format!("{f}.dectest.yuv"), &raw).unwrap();
    eprintln!("{} frames", frames.len());
}
