import sys; sys.path.insert(0,'F:/coding/rs_h264')
from safeedit import edit

OLD='''    derive_mb_records(cur, left, top, mb_t8, bs_v, bs_h)
}'''
NEW='''    derive_mb_records(cur, left, top, mb_t8, bs_v, bs_h)
}

/// The derivation writing STRAIGHT INTO the 32-byte record the caller keeps.
///
/// This is `derive_mb_records` at `T = u8`, which is the width every shipping
/// consumer stores. The decoder used to hand the core a `[[i32; 4]; 4]` pair,
/// zero it, let the core write `i32`s into it, then walk all 32 entries casting
/// each down into a fresh `MbBs` -- 128 bytes of scratch and a full narrowing
/// pass per macroblock to carry values that are never outside `0..=4`.
///
/// Identical output by construction: `out` arrives zeroed, the core writes the
/// same lanes it always wrote (edge 0 only when the neighbour exists, edges 1..4
/// only when the macroblock is not flat-inter), and every unwritten lane keeps
/// the zero the narrowing pass used to copy onto it.
#[inline]
pub fn derive_mb_records_bs(
    cur: &MbPack,
    left: Option<&MbPack>,
    top: Option<&MbPack>,
    mb_t8: bool,
    out: &mut MbBs,
) -> bool {
    derive_mb_records(cur, left, top, mb_t8, &mut out.v, &mut out.h)
}'''
edit('crates/rusty_h264-common/src/deblock.rs',[(OLD,NEW,'derive_mb_records_bs',1)])

# --- decoder call site ---------------------------------------------------
OLD2='''                    let mb_t8 = t8_row[mb_x];
                    let (mut bv, mut bh) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
                    rusty_h264_common::deblock::census_note_packed();
                    let flat = derive_mb_records(cur, left, top, mb_t8, &mut bv, &mut bh);'''
NEW2='''                    let mb_t8 = t8_row[mb_x];
                    // WIN: derive DIRECTLY into the 32-byte record this loop
                    // stores. The pair of `[[i32; 4]; 4]` scratch arrays that
                    // used to sit here were 128 bytes zeroed per macroblock, and
                    // every value written into them was a bS in 0..=4 that a
                    // narrowing pass below then copied, one lane at a time, into
                    // exactly this `MbBs`. The core is generic over the width now
                    // (`BsElem`), so the u8 form is the same derivation, not a
                    // second copy of it, and the i32 monomorphization stays out
                    // of the decode binary entirely.
                    let mut m = MbBs::default();
                    rusty_h264_common::deblock::census_note_packed();
                    let flat = rusty_h264_common::deblock::derive_mb_records_bs(
                        cur, left, top, mb_t8, &mut m,
                    );'''
edit('crates/rusty_h264-decoder/src/mb16.rs',[(OLD2,NEW2,'decoder derives into MbBs',1)])

OLD3='''                    let m = if flat {
                        if stats {
                            edcstat::bump(&edcstat::DBS_FLAT, 1);
                        }
                        debug_assert!(bv[1..] == [[0i32; 4]; 3] && bh[1..] == [[0i32; 4]; 3]);
                        let mut m = MbBs::default();
                        m.v[0] = core::array::from_fn(|sg| bv[0][sg] as u8);
                        m.h[0] = core::array::from_fn(|sg| bh[0][sg] as u8);
                        m
                    } else {
                        // Written once, not zeroed by `default()` and then written.
                        MbBs {
                            v: core::array::from_fn(|e| core::array::from_fn(|sg| bv[e][sg] as u8)),
                            h: core::array::from_fn(|e| core::array::from_fn(|sg| bh[e][sg] as u8)),
                        }
                    };'''
NEW3='''                    // FLAT-AWARE NARROWING IS GONE, not merely cheaper: with the
                    // derivation writing `u8` in place there is nothing to narrow.
                    // A flat macroblock (census DBSDERIVE: 96.8% screen_text,
                    // 90.2% FourPeople, 82.1% akiyo -- the dominant class) leaves
                    // edges 1..4 at the zero `MbBs::default()` already wrote,
                    // which is what the widen-then-narrow pair used to spend 24
                    // copies of a known zero arriving at.
                    if flat && stats {
                        edcstat::bump(&edcstat::DBS_FLAT, 1);
                    }
                    debug_assert!(!flat || (m.v[1..] == [[0u8; 4]; 3] && m.h[1..] == [[0u8; 4]; 3]));'''
edit('crates/rusty_h264-decoder/src/mb16.rs',[(OLD3,NEW3,'drop the narrowing pass',1)],require_growth=False)
