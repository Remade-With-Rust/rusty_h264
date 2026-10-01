//! Constant tables for the high-bit-depth decoder (ITU-T H.264 and the layouts of FFmpeg's
//! `h264` decoder, whose neighbour caches this port keeps).

// Macroblock type flags (FFmpeg's `MB_TYPE_*` values, so the port reads like the reference).
pub const MB_TYPE_INTRA4X4: u32 = 1 << 0;
pub const MB_TYPE_INTRA16X16: u32 = 1 << 1;
pub const MB_TYPE_INTRA_PCM: u32 = 1 << 2;
pub const MB_TYPE_16X16: u32 = 1 << 3;
pub const MB_TYPE_16X8: u32 = 1 << 4;
pub const MB_TYPE_8X16: u32 = 1 << 5;
pub const MB_TYPE_8X8: u32 = 1 << 6;
pub const MB_TYPE_DIRECT2: u32 = 1 << 8;
pub const MB_TYPE_REF0: u32 = 1 << 9;
pub const MB_TYPE_P0L0: u32 = 1 << 12;
pub const MB_TYPE_P1L0: u32 = 1 << 13;
pub const MB_TYPE_P0L1: u32 = 1 << 14;
pub const MB_TYPE_P1L1: u32 = 1 << 15;
pub const MB_TYPE_L0: u32 = MB_TYPE_P0L0 | MB_TYPE_P1L0;
pub const MB_TYPE_L1: u32 = MB_TYPE_P0L1 | MB_TYPE_P1L1;
pub const MB_TYPE_L0L1: u32 = MB_TYPE_L0 | MB_TYPE_L1;
pub const MB_TYPE_SKIP: u32 = 1 << 17;
pub const MB_TYPE_8X8DCT: u32 = 0x0100_0000;

#[inline]
pub fn is_intra(t: u32) -> bool {
    t & 7 != 0
}
#[inline]
pub fn is_intra4x4(t: u32) -> bool {
    t & MB_TYPE_INTRA4X4 != 0
}
#[inline]
pub fn is_intra16x16(t: u32) -> bool {
    t & MB_TYPE_INTRA16X16 != 0
}
#[inline]
pub fn is_pcm(t: u32) -> bool {
    t & MB_TYPE_INTRA_PCM != 0
}
#[inline]
pub fn is_inter(t: u32) -> bool {
    t & (MB_TYPE_16X16 | MB_TYPE_16X8 | MB_TYPE_8X16 | MB_TYPE_8X8) != 0
}
#[inline]
pub fn is_skip(t: u32) -> bool {
    t & MB_TYPE_SKIP != 0
}
#[inline]
pub fn is_direct(t: u32) -> bool {
    t & MB_TYPE_DIRECT2 != 0
}
#[inline]
pub fn is_8x8dct(t: u32) -> bool {
    t & MB_TYPE_8X8DCT != 0
}
#[inline]
pub fn is_dir(t: u32, part: u32, list: usize) -> bool {
    t & (MB_TYPE_P0L0 << (part + 2 * list as u32)) != 0
}
#[inline]
pub fn uses_list(t: u32, list: usize) -> bool {
    t & ((MB_TYPE_P0L0 | MB_TYPE_P1L0) << (2 * list as u32)) != 0
}

/// Unavailable neighbour partition (`PART_NOT_AVAILABLE`) and unused list (`LIST_NOT_USED`).
pub const PART_NOT_AVAILABLE: i8 = -2;
pub const LIST_NOT_USED: i8 = -1;

/// Block index -> position in the 8-wide caches (FFmpeg's `scan8`): luma 0-15, Cb 16-31, Cr
/// 32-47 (4:2:0 uses the first four of each chroma set), then the DC slots.
pub const SCAN8: [usize; 51] = [
    4 + 8,
    5 + 8,
    4 + 2 * 8,
    5 + 2 * 8,
    6 + 8,
    7 + 8,
    6 + 2 * 8,
    7 + 2 * 8,
    4 + 3 * 8,
    5 + 3 * 8,
    4 + 4 * 8,
    5 + 4 * 8,
    6 + 3 * 8,
    7 + 3 * 8,
    6 + 4 * 8,
    7 + 4 * 8,
    4 + 6 * 8,
    5 + 6 * 8,
    4 + 7 * 8,
    5 + 7 * 8,
    6 + 6 * 8,
    7 + 6 * 8,
    6 + 7 * 8,
    7 + 7 * 8,
    4 + 8 * 8,
    5 + 8 * 8,
    4 + 9 * 8,
    5 + 9 * 8,
    6 + 8 * 8,
    7 + 8 * 8,
    6 + 9 * 8,
    7 + 9 * 8,
    4 + 11 * 8,
    5 + 11 * 8,
    4 + 12 * 8,
    5 + 12 * 8,
    6 + 11 * 8,
    7 + 11 * 8,
    6 + 12 * 8,
    7 + 12 * 8,
    4 + 13 * 8,
    5 + 13 * 8,
    4 + 14 * 8,
    5 + 14 * 8,
    6 + 13 * 8,
    7 + 13 * 8,
    6 + 14 * 8,
    7 + 14 * 8,
    0,
    5 * 8,
    10 * 8,
];
pub const LUMA_DC_BLOCK_INDEX: usize = 48;
pub const CHROMA_DC_BLOCK_INDEX: usize = 49;

/// `left_block_options[0]` (frame macroblocks): which of the left neighbour's blocks border
/// each row, for intra modes/nnz/mvd lookups.
pub const LEFT_BLOCK: [usize; 16] = [
    0,
    1,
    2,
    3,
    7,
    10,
    8,
    11,
    3,
    3 + 4,
    3 + 8,
    3 + 12,
    1 + 16,
    1 + 32,
    1 + 20,
    1 + 36,
];

/// `(type, pred_mode, cbp)` of I macroblock types 0-25.
pub const I_MB_TYPE_INFO: [(u32, i32, u32); 26] = {
    let mut t = [(MB_TYPE_INTRA16X16, 0, 0); 26];
    t[0] = (MB_TYPE_INTRA4X4, -1, 0);
    let modes = [2, 1, 0, 3];
    let mut i = 0;
    while i < 24 {
        let cbp = match i / 4 {
            0 => 0,
            1 => 16,
            2 => 32,
            3 => 15,
            4 => 15 + 16,
            _ => 15 + 32,
        };
        t[1 + i] = (MB_TYPE_INTRA16X16, modes[i % 4], cbp);
        i += 1;
    }
    t[25] = (MB_TYPE_INTRA_PCM, -1, 0);
    t
};

/// `(type, partition_count)`.
pub const P_MB_TYPE_INFO: [(u32, u32); 5] = [
    (MB_TYPE_16X16 | MB_TYPE_P0L0, 1),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 2),
    (MB_TYPE_8X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 4),
    (MB_TYPE_8X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0 | MB_TYPE_REF0, 4),
];

pub const P_SUB_MB_TYPE_INFO: [(u32, u32); 4] = [
    (MB_TYPE_16X16 | MB_TYPE_P0L0, 1),
    (MB_TYPE_16X8 | MB_TYPE_P0L0, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0, 2),
    (MB_TYPE_8X8 | MB_TYPE_P0L0, 4),
];

pub const B_MB_TYPE_INFO: [(u32, u32); 23] = [
    (MB_TYPE_DIRECT2 | MB_TYPE_L0L1, 1),
    (MB_TYPE_16X16 | MB_TYPE_P0L0, 1),
    (MB_TYPE_16X16 | MB_TYPE_P0L1, 1),
    (MB_TYPE_16X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1, 1),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P1L1, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P1L1, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L1 | MB_TYPE_P1L0, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L1 | MB_TYPE_P1L0, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0 | MB_TYPE_P1L1, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P1L0 | MB_TYPE_P1L1, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 2),
    (
        MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1,
        2,
    ),
    (
        MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1,
        2,
    ),
    (
        MB_TYPE_8X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1,
        4,
    ),
];

pub const B_SUB_MB_TYPE_INFO: [(u32, u32); 13] = [
    (MB_TYPE_DIRECT2, 1),
    (MB_TYPE_16X16 | MB_TYPE_P0L0, 1),
    (MB_TYPE_16X16 | MB_TYPE_P0L1, 1),
    (MB_TYPE_16X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1, 1),
    (MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 2),
    (MB_TYPE_16X8 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 2),
    (MB_TYPE_8X16 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 2),
    (
        MB_TYPE_16X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1,
        2,
    ),
    (
        MB_TYPE_8X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1,
        2,
    ),
    (MB_TYPE_8X8 | MB_TYPE_P0L0 | MB_TYPE_P1L0, 4),
    (MB_TYPE_8X8 | MB_TYPE_P0L1 | MB_TYPE_P1L1, 4),
    (
        MB_TYPE_8X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_P1L0 | MB_TYPE_P1L1,
        4,
    ),
];

/// 4x4 zig-zag scan -> raster position (row * 4 + column).
pub const ZIGZAG4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// 8x8 zig-zag scan -> raster position.
pub const ZIGZAG8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// `normAdjust4x4(m, i, j)` values by class (spec 8.5.9).
const NORM4: [[i32; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

pub fn norm_adjust4(m: usize, pos: usize) -> i32 {
    let (i, j) = (pos / 4, pos % 4);
    let class = if i % 2 == 0 && j % 2 == 0 {
        0
    } else if i % 2 == 1 && j % 2 == 1 {
        1
    } else {
        2
    };
    NORM4[m][class]
}

const NORM8: [[i32; 6]; 6] = [
    [20, 18, 32, 19, 25, 24],
    [22, 19, 35, 21, 28, 26],
    [26, 23, 42, 24, 33, 31],
    [28, 25, 45, 26, 35, 33],
    [32, 28, 51, 30, 40, 38],
    [36, 32, 58, 34, 46, 43],
];

pub fn norm_adjust8(m: usize, pos: usize) -> i32 {
    let (i, j) = (pos / 8, pos % 8);
    let class = if i % 4 == 0 && j % 4 == 0 {
        0
    } else if i % 2 == 1 && j % 2 == 1 {
        1
    } else if i % 4 == 2 && j % 4 == 2 {
        2
    } else if (i % 4 == 0 && j % 2 == 1) || (i % 2 == 1 && j % 4 == 0) {
        3
    } else if (i % 4 == 0 && j % 4 == 2) || (i % 4 == 2 && j % 4 == 0) {
        4
    } else {
        5
    };
    NORM8[m][class]
}

/// `QPc` for `qPI` 0..=51 (spec Table 8-15); below 30 they're equal.
pub const CHROMA_QP: [u8; 52] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39,
    39, 39,
];

/// Deblocking thresholds by `indexA`/`indexB` (spec Table 8-16), 8-bit scale.
pub const ALPHA: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20,
    22, 25, 28, 32, 36, 40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226,
    255, 255,
];
pub const BETA: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 6, 6, 7, 7, 8, 8,
    9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18,
];
/// `tC0` by `indexA` for bS 1..3 (spec Table 8-17).
pub const TC0: [[u8; 3]; 52] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 1, 1],
    [0, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 2, 3],
    [1, 2, 3],
    [2, 2, 3],
    [2, 2, 4],
    [2, 3, 4],
    [2, 3, 4],
    [3, 3, 5],
    [3, 4, 6],
    [3, 4, 6],
    [4, 5, 7],
    [4, 5, 8],
    [4, 6, 9],
    [5, 7, 10],
    [6, 8, 11],
    [6, 8, 13],
    [7, 10, 14],
    [8, 11, 16],
    [9, 12, 18],
    [10, 13, 20],
    [11, 15, 23],
    [13, 17, 25],
];

// CABAC residual context offsets (frame coding), by block category.
pub const CBF_BASE: [usize; 6] = [85, 89, 93, 97, 101, 1012];
pub const SIG_BASE: [usize; 6] = [105, 105 + 15, 105 + 29, 105 + 44, 105 + 47, 402];
pub const LAST_BASE: [usize; 6] = [166, 166 + 15, 166 + 29, 166 + 44, 166 + 47, 417];
pub const ABS_BASE: [usize; 6] = [227, 227 + 10, 227 + 20, 227 + 30, 227 + 39, 426];

pub const SIG_8X8: [u8; 63] = [
    0, 1, 2, 3, 4, 5, 5, 4, 4, 3, 3, 4, 4, 4, 5, 5, 4, 4, 4, 4, 3, 3, 6, 7, 7, 7, 8, 9, 10, 9, 8,
    7, 7, 6, 11, 12, 13, 11, 6, 7, 8, 9, 14, 10, 9, 8, 6, 11, 12, 13, 11, 6, 9, 14, 10, 9, 11, 12,
    13, 11, 14, 10, 12,
];
pub const LAST_8X8: [u8; 63] = [
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8,
];
pub const ABS_LEVEL1_CTX: [usize; 8] = [1, 2, 3, 4, 0, 0, 0, 0];
pub const ABS_LEVELGT1_CTX: [usize; 8] = [5, 5, 5, 5, 6, 7, 8, 9];
pub const ABS_LEVEL_TRANSITION: [[usize; 8]; 2] =
    [[1, 2, 3, 3, 4, 5, 6, 7], [4, 4, 4, 4, 5, 6, 7, 7]];

// Intra prediction modes, FFmpeg's numbering (the extra DC variants come from remapping
// modes whose neighbours are missing).
pub const VERT_PRED: i8 = 0;
pub const HOR_PRED: i8 = 1;
pub const DC_PRED: i8 = 2;
pub const DIAG_DOWN_LEFT_PRED: i8 = 3;
pub const DIAG_DOWN_RIGHT_PRED: i8 = 4;
pub const VERT_RIGHT_PRED: i8 = 5;
pub const HOR_DOWN_PRED: i8 = 6;
pub const VERT_LEFT_PRED: i8 = 7;
pub const HOR_UP_PRED: i8 = 8;
pub const LEFT_DC_PRED: i8 = 9;
pub const TOP_DC_PRED: i8 = 10;
pub const DC_128_PRED: i8 = 11;

pub const DC_PRED8X8: i32 = 0;
pub const HOR_PRED8X8: i32 = 1;
pub const VERT_PRED8X8: i32 = 2;
pub const PLANE_PRED8X8: i32 = 3;
pub const LEFT_DC_PRED8X8: i32 = 4;
pub const TOP_DC_PRED8X8: i32 = 5;
pub const DC_128_PRED8X8: i32 = 6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i_mb_types_follow_the_spec_table() {
        // mb_type 5: I_16x16_0_1_0 -> pred 0 (vertical), chroma cbp 1.
        assert_eq!(I_MB_TYPE_INFO[5], (MB_TYPE_INTRA16X16, 2, 16));
        assert_eq!(I_MB_TYPE_INFO[1], (MB_TYPE_INTRA16X16, 2, 0));
        assert_eq!(I_MB_TYPE_INFO[13], (MB_TYPE_INTRA16X16, 2, 15));
        assert_eq!(I_MB_TYPE_INFO[24], (MB_TYPE_INTRA16X16, 3, 15 + 32));
    }

    #[test]
    fn norm_adjust_classes() {
        assert_eq!(norm_adjust4(0, 0), 10);
        assert_eq!(norm_adjust4(0, 5), 16);
        assert_eq!(norm_adjust4(0, 1), 13);
        assert_eq!(norm_adjust8(0, 0), 20);
        assert_eq!(norm_adjust8(0, 9), 18);
        assert_eq!(norm_adjust8(0, 18), 32);
        assert_eq!(norm_adjust8(0, 1), 19);
        assert_eq!(norm_adjust8(0, 2), 25);
        assert_eq!(norm_adjust8(0, 3), 19);
        assert_eq!(norm_adjust8(0, 10), 24);
    }
}
