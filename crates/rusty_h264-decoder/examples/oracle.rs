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

use rusty_h264_common::nal::{split_annex_b, NalUnitType};
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

/// Decodes every access unit, returning all frames' YUV concatenated as I420 in
/// **display order**: pictures come out of the decoder in decode order, and are
/// reordered by PicOrderCnt within each GOP (a reference decoder outputs display
/// order; with B-pictures that differs from decode order). POC resets at each
/// IDR, so the previous GOP is flushed (POC-sorted) before the IDR's GOP starts.
fn our_decode_all(stream: &[u8]) -> Result<Vec<u8>, String> {
    let mut dec = Decoder::new();
    let mut out = Vec::new();
    let mut gop: Vec<(i32, Vec<u8>)> = Vec::new();
    let flush = |gop: &mut Vec<(i32, Vec<u8>)>, out: &mut Vec<u8>| {
        gop.sort_by_key(|(poc, _)| *poc);
        for (_, buf) in gop.drain(..) {
            out.extend_from_slice(&buf);
        }
    };
    for au in access_units(stream) {
        if au_has_idr(au) {
            flush(&mut gop, &mut out); // output the prior GOP before the new IDR
        }
        if let Some(frame) = dec.decode(au).map_err(|e| e.to_string())? {
            let mut buf = Vec::with_capacity(frame.y.len() + frame.u.len() + frame.v.len());
            buf.extend_from_slice(&frame.y);
            buf.extend_from_slice(&frame.u);
            buf.extend_from_slice(&frame.v);
            gop.push((dec.last_poc(), buf));
        }
    }
    flush(&mut gop, &mut out);
    Ok(out)
}

/// Whether an access unit contains an IDR coded-slice NAL.
fn au_has_idr(au: &[u8]) -> bool {
    split_annex_b(au)
        .iter()
        .any(|n| !n.is_empty() && NalUnitType::from_id(n[0]) == NalUnitType::IdrSlice)
}

/// Splits an Annex-B stream into access units (byte slices of the original, so
/// start codes are preserved for the decoder). Cuts after each VCL slice NAL —
/// correct for single-slice-per-picture streams (multi-slice is a known gap).
fn access_units(stream: &[u8]) -> Vec<&[u8]> {
    // Offsets where each NAL's payload begins (after the start code), via the
    // shared splitter, mapped back to start-code offsets.
    let nals = split_annex_b(stream);
    // Reconstruct start positions by locating each NAL slice within the stream.
    let mut cuts: Vec<(usize, bool)> = Vec::new(); // (start-of-startcode, is_vcl)
    let mut search_from = 0;
    for nal in &nals {
        // Find this NAL slice's start within the stream (slices are in order).
        let off = find_subslice(stream, nal, search_from);
        let Some(payload_off) = off else { continue };
        search_from = payload_off + nal.len();
        // The start code is the 3 (or 4) bytes immediately before payload_off.
        let sc = if payload_off >= 4 && stream[payload_off - 4..payload_off] == [0, 0, 0, 1] {
            payload_off - 4
        } else {
            payload_off.saturating_sub(3)
        };
        let is_vcl = matches!(
            NalUnitType::from_id(nal[0]),
            NalUnitType::IdrSlice | NalUnitType::NonIdrSlice
        );
        cuts.push((sc, is_vcl));
    }
    // Build AUs: each ends right after a VCL NAL.
    let mut aus = Vec::new();
    let mut start = 0;
    for i in 0..cuts.len() {
        if cuts[i].1 {
            let end = cuts.get(i + 1).map(|c| c.0).unwrap_or(stream.len());
            aus.push(&stream[start..end]);
            start = end;
        }
    }
    if aus.is_empty() && !stream.is_empty() {
        aus.push(stream);
    }
    aus
}

fn find_subslice(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
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
