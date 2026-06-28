//! CAVLC — Context-Adaptive Variable-Length Coding of residual blocks.
//!
//! This is the entropy coder that actually compresses: a 4×4 block of quantized
//! coefficients (in zig-zag scan order) is coded as `coeff_token` (count +
//! trailing ones), trailing-one signs, the remaining levels, `total_zeros`, and
//! per-coefficient `run_before`. The VLC tables below are the exact H.264 tables
//! (matching the reference decoders our output is validated against).
//!
//! [`encode_residual_block`] and [`decode_residual_block`] are an exact inverse
//! pair, parameterized by `nc` (the neighbor-derived context that selects the
//! `coeff_token` table) and `max_coeff` (16 for a full 4×4, 15 for an AC block,
//! 4 for chroma DC). Neighbor `nc` bookkeeping lives in the macroblock layer.

use crate::{BitReader, BitWriter};
use crate::bit_reader::OutOfData;

/// 4×4 zig-zag scan: scan position → raster index.
pub const ZIGZAG_4X4: [usize; 16] = [
    0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15,
];

/// Zig-zag scan of a raster 4×4 block (full DC+AC), **unrolled** like openh264's
/// `WelsScan4x4DcAc` — constant indices, so no `ZIGZAG_4X4[i]` table read and no
/// per-element bounds check (the looped `block[ZIGZAG_4X4[i]]` form forces one).
#[inline]
pub fn scan_4x4_dcac(d: &[i32; 16]) -> [i32; 16] {
    [
        d[0], d[1], d[4], d[8], d[5], d[2], d[3], d[6], d[9], d[12], d[13], d[10], d[7], d[11],
        d[14], d[15],
    ]
}

/// Zig-zag scan of the 15 AC coefficients (skipping DC), unrolled like openh264's
/// `WelsScan4x4Ac` (`= ZIGZAG_4X4[1..]`).
#[inline]
pub fn scan_4x4_ac(d: &[i32; 16]) -> [i32; 15] {
    [
        d[1], d[4], d[8], d[5], d[2], d[3], d[6], d[9], d[12], d[13], d[10], d[7], d[11], d[14],
        d[15],
    ]
}

/// Inverse of [`scan_4x4_dcac`] (decoder): scatter scan-order coefficients back to
/// the raster 4×4 block, unrolled (no `ZIGZAG_4X4[i]` table read / bounds check).
#[inline]
pub fn un_scan_4x4_dcac(s: &[i32; 16]) -> [i32; 16] {
    let mut d = [0i32; 16];
    d[0] = s[0];
    d[1] = s[1];
    d[4] = s[2];
    d[8] = s[3];
    d[5] = s[4];
    d[2] = s[5];
    d[3] = s[6];
    d[6] = s[7];
    d[9] = s[8];
    d[12] = s[9];
    d[13] = s[10];
    d[10] = s[11];
    d[7] = s[12];
    d[11] = s[13];
    d[14] = s[14];
    d[15] = s[15];
    d
}

/// Inverse of [`scan_4x4_ac`] (decoder): scatter the 15 AC coefficients into the
/// existing block's raster positions (leaving DC `[0]` untouched). `s[0..15]` used.
#[inline]
pub fn un_scan_4x4_ac_into(s: &[i32], d: &mut [i32; 16]) {
    d[1] = s[0];
    d[4] = s[1];
    d[8] = s[2];
    d[5] = s[3];
    d[2] = s[4];
    d[3] = s[5];
    d[6] = s[6];
    d[9] = s[7];
    d[12] = s[8];
    d[13] = s[9];
    d[10] = s[10];
    d[7] = s[11];
    d[11] = s[12];
    d[14] = s[13];
    d[15] = s[14];
}

/// `coded_block_pattern` for Intra macroblocks (4:2:0), indexed by `codeNum`
/// (the `me(v)` mapping, spec Table 9-4). Maps code number → CBP value.
#[rustfmt::skip]
const CBP_INTRA: [u8; 48] = [
    47, 31, 15, 0, 23, 27, 29, 30, 7, 11, 13, 14, 39, 43, 45, 46,
    16, 3, 5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44, 1, 2, 4,
    8, 17, 18, 20, 24, 6, 9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

/// `coded_block_pattern` for Inter macroblocks (4:2:0), indexed by `codeNum`.
#[rustfmt::skip]
const CBP_INTER: [u8; 48] = [
    0, 16, 1, 2, 4, 8, 32, 3, 5, 10, 12, 15, 47, 7, 11, 13,
    14, 6, 9, 31, 35, 37, 42, 44, 33, 34, 36, 40, 39, 43, 45, 46,
    17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

/// Inverse of a `codeNum → cbp` table: `cbp → codeNum`, for a direct lookup in
/// the encoder instead of a 48-entry linear `.position()` scan per macroblock.
const fn invert_cbp(table: &[u8; 48]) -> [u8; 48] {
    let mut inv = [0u8; 48];
    let mut i = 0;
    while i < 48 {
        inv[table[i] as usize] = i as u8;
        i += 1;
    }
    inv
}
const INV_CBP_INTRA: [u8; 48] = invert_cbp(&CBP_INTRA);
const INV_CBP_INTER: [u8; 48] = invert_cbp(&CBP_INTER);

/// Decodes a `coded_block_pattern` (`me(v)`) for an Intra macroblock.
pub fn read_cbp_intra(r: &mut BitReader) -> Result<u32, OutOfData> {
    let code_num = r.read_ue()? as usize;
    Ok(*CBP_INTRA.get(code_num).unwrap_or(&0) as u32)
}

/// Encodes a `coded_block_pattern` (`me(v)`) for an Intra macroblock.
pub fn write_cbp_intra(w: &mut BitWriter, cbp: u32) {
    w.write_ue(INV_CBP_INTRA[cbp as usize] as u32);
}

/// Decodes a `coded_block_pattern` (`me(v)`) for an Inter macroblock.
pub fn read_cbp_inter(r: &mut BitReader) -> Result<u32, OutOfData> {
    let code_num = r.read_ue()? as usize;
    Ok(*CBP_INTER.get(code_num).unwrap_or(&0) as u32)
}

/// Encodes a `coded_block_pattern` (`me(v)`) for an Inter macroblock.
pub fn write_cbp_inter(w: &mut BitWriter, cbp: u32) {
    w.write_ue(INV_CBP_INTER[cbp as usize] as u32);
}

// ---- coeff_token, four nC tables. Index = TotalCoeff*4 + TrailingOnes. ----

#[rustfmt::skip]
const COEFF_TOKEN_LEN: [[u8; 68]; 4] = [
    [
        1,0,0,0,
        6,2,0,0,  8,6,3,0,  9,8,7,5,  10,9,8,6,
        11,10,9,7, 13,11,10,8, 13,13,11,9, 13,13,13,10,
        14,14,13,11, 14,14,14,13, 15,15,14,14, 15,15,15,14,
        16,15,15,15, 16,16,16,15, 16,16,16,16, 16,16,16,16,
    ],
    [
        2,0,0,0,
        6,2,0,0,  6,5,3,0,  7,6,6,4,  8,6,6,4,
        8,7,7,5,  9,8,8,6,  11,9,9,6,  11,11,11,7,
        12,11,11,9, 12,12,12,11, 12,12,12,11, 13,13,13,12,
        13,13,13,13, 13,14,13,13, 14,14,14,13, 14,14,14,14,
    ],
    [
        4,0,0,0,
        6,4,0,0,  6,5,4,0,  6,5,5,4,  7,5,5,4,
        7,5,5,4,  7,6,6,4,  7,6,6,4,  8,7,7,5,
        8,8,7,6,  9,8,8,7,  9,9,8,8,  9,9,9,8,
        10,9,9,9, 10,10,10,10, 10,10,10,10, 10,10,10,10,
    ],
    [
        6,0,0,0,
        6,6,0,0,  6,6,6,0,  6,6,6,6,  6,6,6,6,
        6,6,6,6,  6,6,6,6,  6,6,6,6,  6,6,6,6,
        6,6,6,6,  6,6,6,6,  6,6,6,6,  6,6,6,6,
        6,6,6,6,  6,6,6,6,  6,6,6,6,  6,6,6,6,
    ],
];

#[rustfmt::skip]
const COEFF_TOKEN_BITS: [[u8; 68]; 4] = [
    [
        1,0,0,0,
        5,1,0,0,  7,4,1,0,  7,6,5,3,  7,6,5,3,
        7,6,5,4,  15,6,5,4,  11,14,5,4,  8,10,13,4,
        15,14,9,4, 11,10,13,12, 15,14,9,12, 11,10,13,8,
        15,1,9,12, 11,14,13,8,  7,10,9,12,  4,6,5,8,
    ],
    [
        3,0,0,0,
        11,2,0,0,  7,7,3,0,  7,10,9,5,  7,6,5,4,
        4,6,5,6,  7,6,5,8,  15,6,5,4,  11,14,13,4,
        15,10,9,4, 11,14,13,12, 8,10,9,8, 15,14,13,12,
        11,10,9,12, 7,11,6,8,  9,8,10,1,  7,6,5,4,
    ],
    [
        15,0,0,0,
        15,14,0,0, 11,15,13,0, 8,12,14,12, 15,10,11,11,
        11,8,9,10, 9,14,13,9,  8,10,9,8, 15,14,13,13,
        11,14,10,12, 15,10,13,12, 11,14,9,12, 8,10,13,8,
        13,7,9,12,  9,12,11,10,  5,8,7,6,  1,4,3,2,
    ],
    [
        3,0,0,0,
        0,1,0,0,  4,5,6,0,  8,9,10,11,  12,13,14,15,
        16,17,18,19, 20,21,22,23, 24,25,26,27, 28,29,30,31,
        32,33,34,35, 36,37,38,39, 40,41,42,43, 44,45,46,47,
        48,49,50,51, 52,53,54,55, 56,57,58,59, 60,61,62,63,
    ],
];

/// chroma DC (2×2) coeff_token, index = TotalCoeff*4 + TrailingOnes.
#[rustfmt::skip]
const CHROMA_DC_COEFF_TOKEN_LEN: [u8; 20] = [
    2,0,0,0,  6,1,0,0,  6,6,3,0,  6,7,7,6,  6,8,8,7,
];
#[rustfmt::skip]
const CHROMA_DC_COEFF_TOKEN_BITS: [u8; 20] = [
    1,0,0,0,  7,1,0,0,  4,6,1,0,  3,3,2,5,  2,3,2,0,
];

/// total_zeros, indexed `[TotalCoeff-1][total_zeros]` (TotalCoeff 1..15).
#[rustfmt::skip]
const TOTAL_ZEROS_LEN: [[u8; 16]; 15] = [
    [1,3,3,4,4,5,5,6,6,7,7,8,8,9,9,9],
    [3,3,3,3,3,4,4,4,4,5,5,6,6,6,6,0],
    [4,3,3,3,4,4,3,3,4,5,5,6,5,6,0,0],
    [5,3,4,4,3,3,3,4,3,4,5,5,5,0,0,0],
    [4,4,4,3,3,3,3,3,4,5,4,5,0,0,0,0],
    [6,5,3,3,3,3,3,3,4,3,6,0,0,0,0,0],
    [6,5,3,3,3,2,3,4,3,6,0,0,0,0,0,0],
    [6,4,5,3,2,2,3,3,6,0,0,0,0,0,0,0],
    [6,6,4,2,2,3,2,5,0,0,0,0,0,0,0,0],
    [5,5,3,2,2,2,4,0,0,0,0,0,0,0,0,0],
    [4,4,3,3,1,3,0,0,0,0,0,0,0,0,0,0],
    [4,4,2,1,3,0,0,0,0,0,0,0,0,0,0,0],
    [3,3,1,2,0,0,0,0,0,0,0,0,0,0,0,0],
    [2,2,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
];
#[rustfmt::skip]
const TOTAL_ZEROS_BITS: [[u8; 16]; 15] = [
    [1,3,2,3,2,3,2,3,2,3,2,3,2,3,2,1],
    [7,6,5,4,3,5,4,3,2,3,2,3,2,1,0,0],
    [5,7,6,5,4,3,4,3,2,3,2,1,1,0,0,0],
    [3,7,5,4,6,5,4,3,3,2,2,1,0,0,0,0],
    [5,4,3,7,6,5,4,3,2,1,1,0,0,0,0,0],
    [1,1,7,6,5,4,3,2,1,1,0,0,0,0,0,0],
    [1,1,5,4,3,3,2,1,1,0,0,0,0,0,0,0],
    [1,1,1,3,3,2,2,1,0,0,0,0,0,0,0,0],
    [1,0,1,3,2,1,1,1,0,0,0,0,0,0,0,0],
    [1,0,1,3,2,1,1,0,0,0,0,0,0,0,0,0],
    [0,1,1,2,1,3,0,0,0,0,0,0,0,0,0,0],
    [0,1,1,1,1,0,0,0,0,0,0,0,0,0,0,0],
    [0,1,1,1,0,0,0,0,0,0,0,0,0,0,0,0],
    [0,1,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [0,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
];

/// chroma DC total_zeros, indexed `[TotalCoeff-1][total_zeros]` (TotalCoeff 1..3).
#[rustfmt::skip]
const CHROMA_DC_TOTAL_ZEROS_LEN: [[u8; 4]; 3] = [
    [1,2,3,3],
    [1,2,2,0],
    [1,1,0,0],
];
#[rustfmt::skip]
const CHROMA_DC_TOTAL_ZEROS_BITS: [[u8; 4]; 3] = [
    [1,1,1,0],
    [1,1,0,0],
    [1,0,0,0],
];

/// run_before, indexed `[min(zerosLeft,7)-1][run_before]`.
#[rustfmt::skip]
const RUN_LEN: [[u8; 15]; 7] = [
    [1,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [1,2,2,0,0,0,0,0,0,0,0,0,0,0,0],
    [2,2,2,2,0,0,0,0,0,0,0,0,0,0,0],
    [2,2,2,3,3,0,0,0,0,0,0,0,0,0,0],
    [2,2,3,3,3,3,0,0,0,0,0,0,0,0,0],
    [2,3,3,3,3,3,3,0,0,0,0,0,0,0,0],
    [3,3,3,3,3,3,3,4,5,6,7,8,9,10,11],
];
#[rustfmt::skip]
const RUN_BITS: [[u8; 15]; 7] = [
    [1,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [1,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [3,2,1,0,0,0,0,0,0,0,0,0,0,0,0],
    [3,2,1,1,0,0,0,0,0,0,0,0,0,0,0],
    [3,2,3,2,1,0,0,0,0,0,0,0,0,0,0],
    [3,0,1,3,2,5,4,0,0,0,0,0,0,0,0],
    [7,6,5,4,3,2,1,1,1,1,1,1,1,1,1],
];

/// Selects the coeff_token VLC table from the context `nc`. `nc == -1` is the
/// chroma-DC table (handled separately by the caller).
fn coeff_token_table(nc: i32) -> usize {
    if nc < 2 {
        0
    } else if nc < 4 {
        1
    } else if nc < 8 {
        2
    } else {
        3
    }
}

/// Writes a `(len, bits)` VLC codeword.
fn put(w: &mut BitWriter, len: u8, bits: u8) {
    debug_assert!(len > 0, "writing an undefined VLC entry");
    w.write_bits(bits as u32, len as u32);
}

/// Reads a VLC by matching accumulated bits against a length/bits table over
/// the given candidate symbol indices. Returns the matched symbol index.
/// A flat VLC lookup table. The H.264 VLC tables are prefix-free, so a single
/// `peek_bits(width)` + index decodes any codeword in O(1) — replacing the old
/// bit-at-a-time scan over every candidate. `entry[peeked]` packs
/// `(symbol << 5) | length`; `length == 0` marks "no codeword" (corrupt input).
struct Vlc {
    width: u32,
    entry: Vec<u16>,
}

impl Vlc {
    /// Builds the lookup from a `(len, code)` table. For each codeword of length
    /// `l` and value `v`, every `width`-bit peek whose top `l` bits equal `v`
    /// (the contiguous range `[v<<(width-l), (v+1)<<(width-l))`) maps to it. Codes
    /// are prefix-free, so the ranges never overlap.
    fn build(lens: &[u8], bits: &[u8]) -> Vlc {
        let width = lens.iter().copied().max().unwrap_or(0) as u32;
        let mut entry = vec![0u16; 1usize << width];
        for (i, (&l, &v)) in lens.iter().zip(bits.iter()).enumerate() {
            if l == 0 {
                continue;
            }
            let base = (v as usize) << (width - l as u32);
            let span = 1usize << (width - l as u32);
            let packed = ((i as u16) << 5) | l as u16;
            for e in &mut entry[base..base + span] {
                *e = packed;
            }
        }
        Vlc { width, entry }
    }

    #[inline]
    fn read(&self, r: &mut BitReader) -> Result<usize, OutOfData> {
        let packed = self.entry[r.peek_bits(self.width) as usize];
        let len = (packed & 0x1F) as u32;
        if len == 0 {
            return Err(OutOfData); // peeked bits matched no codeword → corrupt
        }
        r.skip_bits(len)?;
        Ok((packed >> 5) as usize)
    }
}

/// The CAVLC VLC lookup tables, built once on first decode (≈240 KB).
struct VlcTables {
    coeff_token: [Vlc; 4],
    chroma_dc_coeff_token: Vlc,
    total_zeros: [Vlc; 15],
    chroma_dc_total_zeros: [Vlc; 3],
    run_before: [Vlc; 7],
}

fn vlc_tables() -> &'static VlcTables {
    use std::sync::OnceLock;
    static T: OnceLock<VlcTables> = OnceLock::new();
    T.get_or_init(|| VlcTables {
        coeff_token: std::array::from_fn(|t| Vlc::build(&COEFF_TOKEN_LEN[t], &COEFF_TOKEN_BITS[t])),
        chroma_dc_coeff_token: Vlc::build(&CHROMA_DC_COEFF_TOKEN_LEN, &CHROMA_DC_COEFF_TOKEN_BITS),
        total_zeros: std::array::from_fn(|t| Vlc::build(&TOTAL_ZEROS_LEN[t], &TOTAL_ZEROS_BITS[t])),
        chroma_dc_total_zeros: std::array::from_fn(|t| {
            Vlc::build(&CHROMA_DC_TOTAL_ZEROS_LEN[t], &CHROMA_DC_TOTAL_ZEROS_BITS[t])
        }),
        run_before: std::array::from_fn(|t| Vlc::build(&RUN_LEN[t], &RUN_BITS[t])),
    })
}

/// Maps a signed level to its base `levelCode` (before the first-level offset).
fn level_to_code(level: i32) -> i32 {
    if level > 0 {
        (level << 1) - 2
    } else {
        (-level << 1) - 1
    }
}

/// Writes one `level_prefix`/`level_suffix` for `code` at the given suffix
/// length, the exact inverse of [`decode_residual_block`]'s level parsing.
///
/// For small codes the prefix is the unary part and the suffix is `suffix_length`
/// bits. Above that, `level_prefix` escapes to 15 with a 12-bit suffix; and for
/// codes too large for 12 bits, to the **extended escape** (`level_prefix ≥ 16`,
/// suffix `level_prefix − 3` bits), without which large levels — common at very
/// low QP — would silently truncate.
fn write_level(w: &mut BitWriter, code: i32, suffix_length: u32) {
    let code = code as u32;
    // Short forms with prefix < 15 — prefix+suffix PACKED into one write (the
    // value `(1<<suffixsize)|suffix` in `prefix+1+suffixsize` bits, leading zeros
    // implicit). Mirrors openh264's single `CAVLC_BS_WRITE` per level.
    if suffix_length == 0 {
        if code < 14 {
            w.write_bits(1, code + 1);
            return;
        } else if code < 30 {
            w.write_bits((1u32 << 4) | (code - 14), 14 + 1 + 4);
            return;
        }
    } else {
        let prefix = code >> suffix_length;
        if prefix < 15 {
            let suffix = code & ((1 << suffix_length) - 1);
            w.write_bits((1u32 << suffix_length) | suffix, prefix + 1 + suffix_length);
            return;
        }
    }
    // Prefix ≥ 15. `rem` is the value beyond the prefix-15 base; the decoder's
    // `+15` for the suffix_length-0 case makes both bases (30 and 15<<sl) align.
    let base = if suffix_length == 0 { 30 } else { 15u32 << suffix_length };
    let rem = code - base;
    if rem < 4096 {
        put_zeros_one(w, 15);
        w.write_bits(rem, 12);
        return;
    }
    // Extended escape: grow the prefix until the suffix fits. For prefix `p`,
    // the suffix is `p − 3` bits and encodes `rem − (2^(p−3) − 4096)`.
    let mut p = 16u32;
    while rem > (1u32 << (p - 2)) - 4097 {
        p += 1;
    }
    put_zeros_one(w, p);
    w.write_bits(rem - ((1 << (p - 3)) - 4096), p - 3);
}

/// Writes `n` zero bits followed by a `1` (the unary `level_prefix`).
fn put_zeros_one(w: &mut BitWriter, n: u32) {
    // `n` zero bits then a `1` is just the value `1` written in `n + 1` bits — one
    // write, not `n + 1`. (The level-prefix unary code, emitted for every coeff.)
    if n < 32 {
        w.write_bits(1, n + 1);
    } else {
        w.write_bits(0, n - 31);
        w.write_bits(1, 32);
    }
}

/// Reads a `level_prefix` (count of leading zeros before a `1`).
fn read_level_prefix(r: &mut BitReader) -> Result<u32, OutOfData> {
    // Unary prefix = leading zeros before a 1. Count them in the peek window in
    // one CLZ when the codeword (lz+1 bits) fits the 24-bit window.
    let window = r.peek_bits(24);
    let lz = window.leading_zeros() - 8;
    if lz < 24 {
        r.skip_bits(lz + 1)?;
        return Ok(lz);
    }
    let mut n = 0;
    while !r.read_bit()? {
        n += 1;
        // A conformant 4×4 coefficient never needs a prefix this long; beyond
        // this the level computation (`1 << (prefix-3)`) would overflow, so a
        // longer run means corrupt input.
        if n > 32 {
            return Err(OutOfData);
        }
    }
    Ok(n)
}

/// Encodes a 4×4 residual block (`coeffs` in zig-zag scan order) as CAVLC and
/// returns `total_coeff` (the non-zero count), which callers reuse as the block's
/// `nnz` — saving a separate counting pass.
///
/// - `max_coeff`: 16 (full), 15 (AC), or 4 (chroma DC).
/// - `nc`: neighbor context; pass `-1` for chroma DC.
pub fn encode_residual_block(w: &mut BitWriter, coeffs: &[i32], max_coeff: usize, nc: i32) -> usize {
    debug_assert!(coeffs.len() >= max_coeff);
    debug_assert!(max_coeff <= 16);
    let chroma_dc = nc == -1;

    // openh264 `CavlcParamCal`: ONE descending pass yields levels[] (high→low),
    // run[] (high→low), total_coeff, and total_zeros at once — no positions array,
    // no second/third pass. Bit-identical to the per-position derivation. Runs for
    // every coded 4×4 block, so the saved passes matter.
    let mut levels = [0i32; 16];
    let mut run_val = [0usize; 16];
    let mut total_coeff = 0usize;
    let mut total_zeros = 0usize;
    let mut idx = max_coeff as isize - 1;
    while idx >= 0 && coeffs[idx as usize] == 0 {
        idx -= 1;
    }
    while idx >= 0 {
        levels[total_coeff] = coeffs[idx as usize];
        idx -= 1;
        let mut count_zero = 0usize;
        while idx >= 0 && coeffs[idx as usize] == 0 {
            count_zero += 1;
            idx -= 1;
        }
        total_zeros += count_zero;
        run_val[total_coeff] = count_zero;
        total_coeff += 1;
    }
    let levels_hi_lo = &levels[..total_coeff];

    // Trailing ones: leading ±1 entries of the high→low list, capped at 3.
    let mut trailing_ones = 0usize;
    for &lv in levels_hi_lo {
        if lv.abs() == 1 && trailing_ones < 3 {
            trailing_ones += 1;
        } else {
            break;
        }
    }

    // --- coeff_token + trailing-one signs, PACKED into one write (openh264:
    // `n += iTrailingOnes; iValue = (iValue << iTrailingOnes) + uiSign`) ---
    let tok_idx = total_coeff * 4 + trailing_ones;
    let (ct_len, ct_bits) = if chroma_dc {
        (CHROMA_DC_COEFF_TOKEN_LEN[tok_idx], CHROMA_DC_COEFF_TOKEN_BITS[tok_idx])
    } else {
        let t = coeff_token_table(nc);
        (COEFF_TOKEN_LEN[t][tok_idx], COEFF_TOKEN_BITS[t][tok_idx])
    };
    if total_coeff == 0 {
        put(w, ct_len, ct_bits);
        return 0;
    }
    // sign bits for the trailing ones (high→low): 1 = negative.
    let mut sign = 0u32;
    for &lv in levels_hi_lo.iter().take(trailing_ones) {
        sign = (sign << 1) | (lv < 0) as u32;
    }
    w.write_bits(
        ((ct_bits as u32) << trailing_ones) | sign,
        ct_len as u32 + trailing_ones as u32,
    );

    // --- remaining levels (high→low) ---
    let mut suffix_length = if total_coeff > 10 && trailing_ones < 3 { 1 } else { 0 };
    for (k, &lv) in levels_hi_lo.iter().enumerate().skip(trailing_ones) {
        let mut code = level_to_code(lv);
        if k == trailing_ones && trailing_ones < 3 {
            code -= 2;
        }
        write_level(w, code, suffix_length);
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if lv.abs() > (3 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // --- total_zeros (computed in the single pass above) ---
    if total_coeff < max_coeff {
        if chroma_dc {
            let row = &CHROMA_DC_TOTAL_ZEROS_LEN[total_coeff - 1];
            let brow = &CHROMA_DC_TOTAL_ZEROS_BITS[total_coeff - 1];
            put(w, row[total_zeros], brow[total_zeros]);
        } else {
            put(
                w,
                TOTAL_ZEROS_LEN[total_coeff - 1][total_zeros],
                TOTAL_ZEROS_BITS[total_coeff - 1][total_zeros],
            );
        }
    }

    // --- run_before (high→low), skipping once no zeros remain ---
    // run_val[] (high→low) came from the single pass above.
    let mut zeros_left = total_zeros;
    for &run in run_val[..total_coeff].iter().take(total_coeff - 1) {
        if zeros_left == 0 {
            break;
        }
        let t = zeros_left.min(7) - 1;
        put(w, RUN_LEN[t][run], RUN_BITS[t][run]);
        zeros_left -= run;
    }
    total_coeff
}

/// Decodes a CAVLC residual block into zig-zag-ordered coefficients. The first
/// `max_coeff` entries of the returned fixed array are valid (the rest stay zero);
/// returning `[i32; 16]` avoids a per-block heap allocation in the decode loop.
pub fn decode_residual_block(
    r: &mut BitReader,
    max_coeff: usize,
    nc: i32,
) -> Result<[i32; 16], OutOfData> {
    let _g = crate::prof::scope(crate::prof::Stage::Entropy);
    let chroma_dc = nc == -1;
    let mut out = [0i32; 16];

    // --- coeff_token ---
    let tabs = vlc_tables();
    let (total_coeff, trailing_ones) = if chroma_dc {
        let idx = tabs.chroma_dc_coeff_token.read(r)?;
        (idx / 4, idx % 4)
    } else {
        let idx = tabs.coeff_token[coeff_token_table(nc)].read(r)?;
        (idx / 4, idx % 4)
    };
    if total_coeff == 0 {
        return Ok(out);
    }
    // A block cannot hold more coefficients than it has positions. A corrupt
    // coeff_token that claims otherwise would index the total_zeros tables and
    // the output array out of bounds — reject it.
    if total_coeff > max_coeff {
        return Err(OutOfData);
    }

    // --- trailing-one signs + remaining levels, high→low (stack, no alloc) ---
    let mut levels_hi_lo = [0i32; 16];
    for level in levels_hi_lo.iter_mut().take(trailing_ones) {
        *level = if r.read_bit()? { -1 } else { 1 };
    }
    let mut suffix_length = if total_coeff > 10 && trailing_ones < 3 { 1 } else { 0 };
    // `k` indexes the level array and gates the first-non-T1-level offset.
    #[allow(clippy::needless_range_loop)]
    for k in trailing_ones..total_coeff {
        let level_prefix = read_level_prefix(r)?;
        let level_suffix_size = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let level_suffix = if level_suffix_size > 0 {
            r.read_bits(level_suffix_size)?
        } else {
            0
        };
        let mut level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix as i32;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        if k == trailing_ones && trailing_ones < 3 {
            level_code += 2;
        }
        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        // Residual coefficients are 16-bit (spec §8.5; ffmpeg stores int16). A
        // value outside that range is non-conformant and would overflow the
        // dequant/inverse-transform multiplies — reject the block.
        if !(-32768..=32767).contains(&level) {
            return Err(OutOfData);
        }
        levels_hi_lo[k] = level;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.abs() > (3 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // --- total_zeros ---
    let total_zeros = if total_coeff < max_coeff {
        if chroma_dc {
            tabs.chroma_dc_total_zeros[total_coeff - 1].read(r)?
        } else {
            tabs.total_zeros[total_coeff - 1].read(r)?
        }
    } else {
        0
    };

    // --- run_before (stack, no alloc) ---
    let mut run_val = [0usize; 16];
    let mut zeros_left = total_zeros;
    for run in run_val.iter_mut().take(total_coeff - 1) {
        if zeros_left == 0 {
            break;
        }
        let t = zeros_left.min(7) - 1;
        let val = tabs.run_before[t].read(r)?;
        *run = val;
        // A corrupt run_before may exceed the zeros remaining; reject rather
        // than underflow.
        zeros_left = zeros_left.checked_sub(val).ok_or(OutOfData)?;
    }
    if total_coeff >= 1 {
        run_val[total_coeff - 1] = zeros_left;
    }

    // --- reconstruct scan-order coefficients ---
    let mut coeff_num: isize = -1;
    for i in (0..total_coeff).rev() {
        coeff_num += run_val[i] as isize + 1;
        // Defensive: with the guards above this stays in 0..max_coeff, but never
        // let an attacker-shaped run drive an out-of-bounds write.
        let pos = coeff_num as usize;
        if pos >= out.len() {
            return Err(OutOfData);
        }
        out[pos] = levels_hi_lo[i];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(block: &[i32], max_coeff: usize, nc: i32) {
        let mut w = BitWriter::new();
        encode_residual_block(&mut w, block, max_coeff, nc);
        w.align_zero();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let decoded = decode_residual_block(&mut r, max_coeff, nc).expect("decode");
        assert_eq!(&decoded[..max_coeff], &block[..max_coeff], "nc={nc} max={max_coeff}");
    }

    #[test]
    fn all_zero_block() {
        roundtrip(&[0; 16], 16, 0);
        roundtrip(&[0; 16], 15, 0);
        roundtrip(&[0; 4], 4, -1);
    }

    #[test]
    fn single_dc() {
        let mut b = [0i32; 16];
        b[0] = 5;
        roundtrip(&b, 16, 0);
        b[0] = -3;
        roundtrip(&b, 16, 0);
    }

    #[test]
    fn trailing_ones_and_levels() {
        // DC=3, then some zeros, then ±1 trailing ones at higher frequency.
        let b = [3, 0, 1, -1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        roundtrip(&b, 16, 0);
    }

    #[test]
    fn many_coeffs_all_contexts() {
        let b = [1, -2, 3, -1, 1, 1, -1, 2, -3, 1, -1, 1, 1, -1, 1, -1];
        for nc in [0, 2, 4, 8, 20] {
            roundtrip(&b, 16, nc);
        }
    }

    #[test]
    fn chroma_dc_blocks() {
        roundtrip(&[2, -1, 0, 1], 4, -1);
        roundtrip(&[0, 0, 0, -1], 4, -1);
        roundtrip(&[1, 1, 1, 1], 4, -1);
    }

    #[test]
    fn large_levels_use_escape() {
        let b = [200, -150, 47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        roundtrip(&b, 16, 0);
    }

    #[test]
    fn extreme_levels_use_extended_escape() {
        // Levels far beyond the 12-bit suffix range force level_prefix ≥ 16;
        // these occur at very low QP and previously truncated. Cover both
        // suffix_length==0 (single big DC) and grown-suffix_length (a run of
        // large levels) paths, and signs.
        roundtrip(&[5000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, 0);
        roundtrip(&[-7000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, 0);
        roundtrip(&[30000, -25000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, 0);
        let big = [9000, -9000, 8000, -8000, 7000, -7000, 6000, -6000, 5000, -5000, 4500,
            -4500, 4200, -4200, 4096, -4096];
        roundtrip(&big, 16, 0);
        // chroma DC and AC blocks with extreme levels too
        roundtrip(&[6000, -6000, 5000, -5000], 4, -1);
    }

    #[test]
    fn pseudo_random_blocks() {
        // Deterministic LCG; exercise many shapes across contexts and sizes.
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        for _ in 0..2000 {
            let mut b = [0i32; 16];
            let density = (next() % 16) as usize;
            for slot in b.iter_mut().take(density) {
                // small signed values, biased toward ±1
                let v = (next() % 7) as i32 - 3;
                *slot = v;
            }
            // shuffle into scan positions
            for i in (1..16).rev() {
                let j = (next() as usize) % (i + 1);
                b.swap(i, j);
            }
            let max_coeff = if next() % 3 == 0 { 15 } else { 16 };
            let nc = [0i32, 2, 4, 8][(next() % 4) as usize];
            roundtrip(&b, max_coeff, nc);
        }
    }
}
