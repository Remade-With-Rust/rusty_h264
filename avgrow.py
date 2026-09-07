import sys
sys.path.insert(0, 'F:/coding/rs_h264')
from safeedit import edit

# 1) replace the block helper with a ROW helper that chunks into kernel widths
OLD_HELPER_HEAD = '''pub fn avg_strided_into(
    a: &[u8],
    a_stride: usize,
    b: &[u8],
    b_stride: usize,
    w: usize,
    h: usize,
    out: &mut [u8],
) {
    let n = w * h;
    #[cfg(accel)]
    if (w == 16 || w == 8 || w == 4)
        && out.len() >= n
        && a.len() >= (h - 1) * a_stride + w
        && b.len() >= (h - 1) * b_stride + w
    {
        rusty_h264_accel::pixel_avg(&mut out[..n], a, a_stride, b, b_stride, w, h);
        return;
    }
    // Scalar oracle and the non-accel path.
    for r in 0..h {
        let (ar, br) = (&a[r * a_stride..], &b[r * b_stride..]);
        for c in 0..w {
            out[r * w + c] = ((ar[c] as u16 + br[c] as u16 + 1) >> 1) as u8;
        }
    }
}'''

NEW_HELPER = '''pub fn avg_row_into(a: &[u8], b: &[u8], w: usize, out: &mut [u8]) {
    #[cfg(accel)]
    if out.len() >= w && a.len() >= w && b.len() >= w {
        // The kernel is defined at widths 16/8/4, so walk the row in those
        // pieces. Every caller here is a whole number of macroblocks wide, so
        // the 16-wide loop does all the work and the 8/4 tails never run on
        // luma; chroma lands on the 8-wide step.
        let mut c = 0;
        while c + 16 <= w {
            rusty_h264_accel::pixel_avg(&mut out[c..c + 16], &a[c..], 16, &b[c..], 16, 16, 1);
            c += 16;
        }
        while c + 8 <= w {
            rusty_h264_accel::pixel_avg(&mut out[c..c + 8], &a[c..], 8, &b[c..], 8, 8, 1);
            c += 8;
        }
        while c + 4 <= w {
            rusty_h264_accel::pixel_avg(&mut out[c..c + 4], &a[c..], 4, &b[c..], 4, 4, 1);
            c += 4;
        }
        if c == w {
            return;
        }
    }
    // Scalar oracle, and the path on a non-accel build or a width the kernel
    // cannot cover exactly.
    for (d, (&x, &y)) in out[..w].iter_mut().zip(a[..w].iter().zip(&b[..w])) {
        *d = ((x as u16 + y as u16 + 1) >> 1) as u8;
    }
}'''
edit('crates/rusty_h264-common/src/inter.rs',
     [(OLD_HELPER_HEAD, NEW_HELPER, 'common: block helper -> row helper', 1)],
     require_growth=False)
