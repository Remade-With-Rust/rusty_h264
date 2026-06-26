//! YUV oracle: decode each `.264` in a directory with **both** our decoder and
//! Cisco's `h264dec` reference, and diff the output frame-for-frame. This is the
//! correctness oracle (the round-trip only proves we agree with our own encoder;
//! this proves we agree with a reference decoder on real streams).
//!
//! Usage:
//!   H264DEC=/path/to/h264dec.exe \
//!   cargo run -p rusty_h264-decoder --example oracle -- <dir-of-.264>
//!
//! Per file it prints one of:
//!   MATCH     our YUV == reference YUV (N frames)
//!   DIFF      sizes/bytes differ (first divergence located)
//!   OURS-REJ  we returned a DecodeError (out-of-scope tool) — not a bug
//!   OURS-PANIC we panicked (a bug — should never happen)
//!   REF-FAIL  the reference decoder couldn't decode it either (skipped)

use rusty_h264_decoder::Decoder;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::process::Command;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: oracle <dir> (set H264DEC)");
    let h264dec = std::env::var("H264DEC").unwrap_or_else(|_| {
        "C:/Users/talmo/coding/openh264/builddir_rs/codec/console/dec/h264dec.exe".to_string()
    });
    let tmp = std::env::temp_dir();

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "264"))
        .collect();
    entries.sort();

    std::panic::set_hook(Box::new(|_| {}));
    let (mut matched, mut diff, mut rej, mut panic, mut reffail) = (0, 0, 0, 0, 0);

    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let data = std::fs::read(path).expect("read file");

        // Reference decode.
        let ref_yuv = match reference_decode(&h264dec, path, &tmp) {
            Some(y) if !y.is_empty() => y,
            _ => {
                reffail += 1;
                println!("  REF-FAIL   {name}");
                continue;
            }
        };

        // Our decode (all frames concatenated), guarded against panics.
        let ours = catch_unwind(AssertUnwindSafe(|| our_decode_all(&data)));
        match ours {
            Err(_) => {
                panic += 1;
                println!("  OURS-PANIC {name}");
            }
            Ok(Err(e)) => {
                rej += 1;
                println!("  OURS-REJ   {name}  -> {e}");
            }
            Ok(Ok(our_yuv)) => {
                if our_yuv == ref_yuv {
                    matched += 1;
                    println!("  MATCH      {name}  ({} bytes)", our_yuv.len());
                } else {
                    diff += 1;
                    println!("  DIFF       {name}  {}", describe_diff(&our_yuv, &ref_yuv));
                }
            }
        }
    }

    let _ = std::panic::take_hook();
    println!(
        "\n{} files: {matched} MATCH, {diff} DIFF, {rej} ours-rejected, {panic} ours-panicked, {reffail} ref-failed",
        entries.len()
    );
}

/// Runs the reference decoder, returning the concatenated I420 bytes.
fn reference_decode(h264dec: &str, input: &Path, tmp: &Path) -> Option<Vec<u8>> {
    let out = tmp.join(format!(
        "oracle_{}.yuv",
        input.file_stem().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&out);
    let status = Command::new(h264dec)
        .arg(input)
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&out).ok()?;
    let _ = std::fs::remove_file(&out);
    Some(bytes)
}

/// Decodes the whole stream via the public `decode_stream` API and concatenates
/// the display-order frames as I420 — the same layout `h264dec` writes.
fn our_decode_all(stream: &[u8]) -> Result<Vec<u8>, String> {
    let frames = Decoder::new().decode_stream(stream).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for f in &frames {
        out.extend_from_slice(&f.y);
        out.extend_from_slice(&f.u);
        out.extend_from_slice(&f.v);
    }
    Ok(out)
}

fn describe_diff(ours: &[u8], reference: &[u8]) -> String {
    if ours.len() != reference.len() {
        return format!("size ours={} ref={}", ours.len(), reference.len());
    }
    let mut ndiff = 0usize;
    let mut maxd = 0i32;
    let mut first = None;
    for (i, (a, b)) in ours.iter().zip(reference).enumerate() {
        if a != b {
            ndiff += 1;
            maxd = maxd.max((*a as i32 - *b as i32).abs());
            if first.is_none() {
                first = Some(i);
            }
        }
    }
    format!(
        "{ndiff}/{} bytes differ (max |Δ|={maxd}), first @ {}",
        ours.len(),
        first.unwrap_or(0)
    )
}
