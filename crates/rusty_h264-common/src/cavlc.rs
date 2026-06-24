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

/// Decodes a `coded_block_pattern` (`me(v)`) for an Intra macroblock.
pub fn read_cbp_intra(r: &mut BitReader) -> Result<u32, OutOfData> {
    let code_num = r.read_ue()? as usize;
    Ok(*CBP_INTRA.get(code_num).unwrap_or(&0) as u32)
}

/// Encodes a `coded_block_pattern` (`me(v)`) for an Intra macroblock.
pub fn write_cbp_intra(w: &mut BitWriter, cbp: u32) {
    let code_num = CBP_INTRA.iter().position(|&c| c as u32 == cbp).unwrap_or(0) as u32;
    w.write_ue(code_num);
}

/// Decodes a `coded_block_pattern` (`me(v)`) for an Inter macroblock.
pub fn read_cbp_inter(r: &mut BitReader) -> Result<u32, OutOfData> {
    let code_num = r.read_ue()? as usize;
    Ok(*CBP_INTER.get(code_num).unwrap_or(&0) as u32)
}

/// Encodes a `coded_block_pattern` (`me(v)`) for an Inter macroblock.
pub fn write_cbp_inter(w: &mut BitWriter, cbp: u32) {
    let code_num = CBP_INTER.iter().position(|&c| c as u32 == cbp).unwrap_or(0) as u32;
    w.write_ue(code_num);
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
fn read_vlc(
    r: &mut BitReader,
    lens: &[u8],
    bits: &[u8],
    candidates: impl Iterator<Item = usize> + Clone,
) -> Result<usize, OutOfData> {
    let mut acc = 0u32;
    let mut nbits = 0u8;
    loop {
        acc = (acc << 1) | (r.read_bit()? as u32);
        nbits += 1;
        for idx in candidates.clone() {
            if lens[idx] == nbits && bits[idx] as u32 == acc {
                return Ok(idx);
            }
        }
        if nbits > 16 {
            return Err(OutOfData);
        }
    }
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
    // Short forms with prefix < 15.
    if suffix_length == 0 {
        if code < 14 {
            put_zeros_one(w, code);
            return;
        } else if code < 30 {
            put_zeros_one(w, 14);
            w.write_bits(code - 14, 4);
            return;
        }
    } else {
        let prefix = code >> suffix_length;
        if prefix < 15 {
            put_zeros_one(w, prefix);
            w.write_bits(code & ((1 << suffix_length) - 1), suffix_length);
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
    for _ in 0..n {
        w.write_bit(false);
    }
    w.write_bit(true);
}

/// Reads a `level_prefix` (count of leading zeros before a `1`).
fn read_level_prefix(r: &mut BitReader) -> Result<u32, OutOfData> {
    let mut n = 0;
    while !r.read_bit()? {
        n += 1;
        if n > 60 {
            return Err(OutOfData);
        }
    }
    Ok(n)
}

/// Encodes a 4×4 residual block (`coeffs` in zig-zag scan order) as CAVLC.
///
/// - `max_coeff`: 16 (full), 15 (AC), or 4 (chroma DC).
/// - `nc`: neighbor context; pass `-1` for chroma DC.
pub fn encode_residual_block(w: &mut BitWriter, coeffs: &[i32], max_coeff: usize, nc: i32) {
    debug_assert!(coeffs.len() >= max_coeff);
    let chroma_dc = nc == -1;

    // Positions (ascending scan order) of non-zero coefficients.
    let positions: Vec<usize> = (0..max_coeff).filter(|&i| coeffs[i] != 0).collect();
    let total_coeff = positions.len();

    // Levels high→low frequency.
    let levels_hi_lo: Vec<i32> = positions.iter().rev().map(|&p| coeffs[p]).collect();

    // Trailing ones: leading ±1 entries of the high→low list, capped at 3.
    let mut trailing_ones = 0usize;
    for &lv in &levels_hi_lo {
        if lv.abs() == 1 && trailing_ones < 3 {
            trailing_ones += 1;
        } else {
            break;
        }
    }

    // --- coeff_token ---
    let tok_idx = total_coeff * 4 + trailing_ones;
    if chroma_dc {
        put(w, CHROMA_DC_COEFF_TOKEN_LEN[tok_idx], CHROMA_DC_COEFF_TOKEN_BITS[tok_idx]);
    } else {
        let t = coeff_token_table(nc);
        put(w, COEFF_TOKEN_LEN[t][tok_idx], COEFF_TOKEN_BITS[t][tok_idx]);
    }
    if total_coeff == 0 {
        return;
    }

    // --- trailing-one signs (high→low): 0 = +, 1 = - ---
    for &lv in levels_hi_lo.iter().take(trailing_ones) {
        w.write_bit(lv < 0);
    }

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

    // --- total_zeros ---
    let last = *positions.last().unwrap();
    let total_zeros = last + 1 - total_coeff;
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
    // runVal[i] for the high→low list; sum == total_zeros.
    let mut run_val = vec![0usize; total_coeff];
    for (m, &p) in positions.iter().enumerate() {
        let gap = if m == 0 { p } else { p - positions[m - 1] - 1 };
        run_val[total_coeff - 1 - m] = gap;
    }
    let mut zeros_left = total_zeros;
    for &run in run_val.iter().take(total_coeff - 1) {
        if zeros_left == 0 {
            break;
        }
        let t = zeros_left.min(7) - 1;
        put(w, RUN_LEN[t][run], RUN_BITS[t][run]);
        zeros_left -= run;
    }
}

/// Decodes a CAVLC residual block into `max_coeff` zig-zag-ordered coefficients.
pub fn decode_residual_block(
    r: &mut BitReader,
    max_coeff: usize,
    nc: i32,
) -> Result<Vec<i32>, OutOfData> {
    let chroma_dc = nc == -1;
    let mut out = vec![0i32; max_coeff];

    // --- coeff_token ---
    let (total_coeff, trailing_ones) = if chroma_dc {
        let cand = (0..20).filter(|&i| CHROMA_DC_COEFF_TOKEN_LEN[i] > 0);
        let idx = read_vlc(r, &CHROMA_DC_COEFF_TOKEN_LEN, &CHROMA_DC_COEFF_TOKEN_BITS, cand)?;
        (idx / 4, idx % 4)
    } else {
        let t = coeff_token_table(nc);
        let lens = &COEFF_TOKEN_LEN[t];
        let bits = &COEFF_TOKEN_BITS[t];
        let cand = (0..68).filter(|&i| lens[i] > 0);
        let idx = read_vlc(r, lens, bits, cand)?;
        (idx / 4, idx % 4)
    };
    if total_coeff == 0 {
        return Ok(out);
    }

    // --- trailing-one signs + remaining levels, high→low ---
    let mut levels_hi_lo = vec![0i32; total_coeff];
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
            let lens = &CHROMA_DC_TOTAL_ZEROS_LEN[total_coeff - 1];
            let bits = &CHROMA_DC_TOTAL_ZEROS_BITS[total_coeff - 1];
            read_vlc(r, lens, bits, (0..4).filter(|&i| lens[i] > 0))?
        } else {
            let lens = &TOTAL_ZEROS_LEN[total_coeff - 1];
            let bits = &TOTAL_ZEROS_BITS[total_coeff - 1];
            read_vlc(r, lens, bits, (0..16).filter(|&i| lens[i] > 0))?
        }
    } else {
        0
    };

    // --- run_before ---
    let mut run_val = vec![0usize; total_coeff];
    let mut zeros_left = total_zeros;
    for run in run_val.iter_mut().take(total_coeff - 1) {
        if zeros_left == 0 {
            break;
        }
        let t = zeros_left.min(7) - 1;
        let lens = &RUN_LEN[t];
        let bits = &RUN_BITS[t];
        let val = read_vlc(r, lens, bits, (0..15).filter(|&i| lens[i] > 0))?;
        *run = val;
        zeros_left -= val;
    }
    if total_coeff >= 1 {
        run_val[total_coeff - 1] = zeros_left;
    }

    // --- reconstruct scan-order coefficients ---
    let mut coeff_num: isize = -1;
    for i in (0..total_coeff).rev() {
        coeff_num += run_val[i] as isize + 1;
        out[coeff_num as usize] = levels_hi_lo[i];
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
