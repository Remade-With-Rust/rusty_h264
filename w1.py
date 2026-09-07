import sys; sys.path.insert(0,'F:/coding/rs_h264')
from safeedit import edit
P='crates/rusty_h264-common/src/deblock.rs'

# --- 1. the element trait -------------------------------------------------
OLD_A = '''#[inline]
fn pk_nz(p: &MbPack, k: usize) -> bool {
    (p.nnz_mask >> k) & 1 != 0
}'''
NEW_A = '''#[inline]
fn pk_nz(p: &MbPack, k: usize) -> bool {
    (p.nnz_mask >> k) & 1 != 0
}

/// The element type a boundary-strength derivation writes into.
///
/// WHY this exists: every value the derivation produces is a bS in `0..=4`, so
/// it fits in a `u8` -- and `MbBs`, the form BOTH hot callers store, IS `u8`.
/// The core used to write `[[i32; 4]; 4]` unconditionally, which cost the
/// decoder a 128-byte zero-init and 32 `i32` stores per macroblock, immediately
/// followed by 32 narrowing loads and 32 `u8` stores to build the 32-byte record
/// it actually keeps. Four times the intermediate traffic for a value that never
/// leaves `0..=4`.
///
/// Generic instead of a second hand-written body so the two forms cannot drift:
/// there is one derivation, and `derive_mb_records_bs` / `derive_mb_records` are
/// the same code at two widths. The `i32` monomorphization is reached only by
/// `derive_mb_packed` (the encoder / blind-tile arm), so the decode binary links
/// the `u8` one alone and DCE drops the other.
pub trait BsElem: Copy {
    /// Coefficient-driven strength.
    const S2: Self;
    /// Intra internal-edge strength.
    const S3: Self;
    /// Intra macroblock-edge strength.
    const S4: Self;
    /// `0` or `1` -- the motion-mask bit.
    fn bit(b: bool) -> Self;
    /// `0` or `2` -- the coefficient test.
    fn twice(b: bool) -> Self;
}

impl BsElem for i32 {
    const S2: i32 = 2;
    const S3: i32 = 3;
    const S4: i32 = 4;
    #[inline(always)]
    fn bit(b: bool) -> i32 {
        b as i32
    }
    #[inline(always)]
    fn twice(b: bool) -> i32 {
        2 * b as i32
    }
}

impl BsElem for u8 {
    const S2: u8 = 2;
    const S3: u8 = 3;
    const S4: u8 = 4;
    #[inline(always)]
    fn bit(b: bool) -> u8 {
        b as u8
    }
    #[inline(always)]
    fn twice(b: bool) -> u8 {
        2 * b as u8
    }
}'''

# --- 2. pk_bs_inter becomes generic --------------------------------------
OLD_B = '''#[inline]
fn pk_bs_inter(p: &MbPack, pk: usize, q: &MbPack, qk: usize) -> i32 {
    if pk_nz(p, pk) | pk_nz(q, qk) {
        return 2;
    }'''
NEW_B = '''#[inline]
fn pk_bs_inter<T: BsElem>(p: &MbPack, pk: usize, q: &MbPack, qk: usize) -> T {
    if pk_nz(p, pk) | pk_nz(q, qk) {
        return T::S2;
    }'''
OLD_C = '''    pk_differs(p, pk, q, qk) as i32
}'''
NEW_C = '''    T::bit(pk_differs(p, pk, q, qk))
}'''

# --- 3. the core becomes generic -----------------------------------------
OLD_D = '''pub fn derive_mb_records(
    cur: &MbPack,
    left: Option<&MbPack>,
    top: Option<&MbPack>,
    mb_t8: bool,
    bs_v: &mut [[i32; 4]; 4],
    bs_h: &mut [[i32; 4]; 4],
) -> bool {'''
NEW_D = '''pub fn derive_mb_records<T: BsElem>(
    cur: &MbPack,
    left: Option<&MbPack>,
    top: Option<&MbPack>,
    mb_t8: bool,
    bs_v: &mut [[T; 4]; 4],
    bs_h: &mut [[T; 4]; 4],
) -> bool {'''

OLD_E = '''    if let Some(l) = left {
        bs_v[0] = if cur_intra || !l.inter {
            [4; 4]
        } else {
            core::array::from_fn(|seg| pk_bs_inter(l, seg * 4 + 3, cur, seg * 4))
        };
    }
    if let Some(t) = top {
        bs_h[0] = if cur_intra || !t.inter {
            [4; 4]
        } else {
            core::array::from_fn(|seg| pk_bs_inter(t, 12 + seg, cur, seg))
        };
    }'''
NEW_E = '''    if let Some(l) = left {
        bs_v[0] = if cur_intra || !l.inter {
            [T::S4; 4]
        } else {
            core::array::from_fn(|seg| pk_bs_inter(l, seg * 4 + 3, cur, seg * 4))
        };
    }
    if let Some(t) = top {
        bs_h[0] = if cur_intra || !t.inter {
            [T::S4; 4]
        } else {
            core::array::from_fn(|seg| pk_bs_inter(t, 12 + seg, cur, seg))
        };
    }'''

OLD_F = '''        if cur_intra {
            bs_v[be] = [3; 4];
            bs_h[be] = [3; 4];
        } else if uniform {
            // Coefficients alone; the whole edge group is a shift-and-or on the mask.
            bs_v[be] = core::array::from_fn(|seg| {
                2 * (pk_nz(cur, seg * 4 + be) | pk_nz(cur, seg * 4 + be - 1)) as i32
            });
            bs_h[be] = core::array::from_fn(|seg| {
                2 * (pk_nz(cur, be * 4 + seg) | pk_nz(cur, (be - 1) * 4 + seg)) as i32
            });
        } else {'''
NEW_F = '''        if cur_intra {
            bs_v[be] = [T::S3; 4];
            bs_h[be] = [T::S3; 4];
        } else if uniform {
            // Coefficients alone; the whole edge group is a shift-and-or on the mask.
            bs_v[be] = core::array::from_fn(|seg| {
                T::twice(pk_nz(cur, seg * 4 + be) | pk_nz(cur, seg * 4 + be - 1))
            });
            bs_h[be] = core::array::from_fn(|seg| {
                T::twice(pk_nz(cur, be * 4 + seg) | pk_nz(cur, (be - 1) * 4 + seg))
            });
        } else {'''

OLD_G = '''            bs_v[be] = core::array::from_fn(|seg| {
                let k = seg * 4 + be;
                if pk_nz(cur, k) | pk_nz(cur, k - 1) {
                    2
                } else {
                    ((left >> k) & 1) as i32
                }
            });
            bs_h[be] = core::array::from_fn(|seg| {
                let k = be * 4 + seg;
                if pk_nz(cur, k) | pk_nz(cur, k - 4) {
                    2
                } else {
                    ((up >> k) & 1) as i32
                }
            });'''
NEW_G = '''            bs_v[be] = core::array::from_fn(|seg| {
                let k = seg * 4 + be;
                if pk_nz(cur, k) | pk_nz(cur, k - 1) {
                    T::S2
                } else {
                    T::bit((left >> k) & 1 != 0)
                }
            });
            bs_h[be] = core::array::from_fn(|seg| {
                let k = be * 4 + seg;
                if pk_nz(cur, k) | pk_nz(cur, k - 4) {
                    T::S2
                } else {
                    T::bit((up >> k) & 1 != 0)
                }
            });'''

edit(P, [(OLD_A,NEW_A,'BsElem trait',1),
         (OLD_B,NEW_B,'pk_bs_inter generic head',1),
         (OLD_C,NEW_C,'pk_bs_inter generic tail',1),
         (OLD_D,NEW_D,'derive_mb_records generic sig',1),
         (OLD_E,NEW_E,'edge-0 arms',1),
         (OLD_F,NEW_F,'intra/uniform arms',1),
         (OLD_G,NEW_G,'general arm',1)])
