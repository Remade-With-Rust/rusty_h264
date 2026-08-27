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
