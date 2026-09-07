import sys, io, re
sys.path.insert(0, 'F:/coding/rs_h264')
from safeedit import edit

P = 'crates/rusty_h264-decoder/src/mb16.rs'
NOTE = ('                // WIN: the bi-average is the pixel_avg kernel (pavgb computes\n'
        '                // (a + b + 1) >> 1 exactly); this was a scalar byte loop.\n')

subs = []
# recon_b_skip_fp: 16 rows of 16, per-row kernel call
subs.append((
'''                        rusty_h264_common::inter::avg_strided_into(
                            &ly0[s0 + r * l0..],
                            l0,
                            &ly1[s1 + r * l1..],
                            l1,
                            16,
                            1,
                            &mut self.rec_y[d..d + 16],
                        );''',
'''                        rusty_h264_common::inter::avg_row_into(
                            &ly0[s0 + r * l0..],
                            &ly1[s1 + r * l1..],
                            16,
                            &mut self.rec_y[d..d + 16],
                        );''', 'recon_b_skip_fp -> avg_row_into', 1))

edit(P, subs, require_growth=False)

# the four banded-span zip loops: replace each with a row-helper call
raw = io.open(P, encoding='utf-8', newline='').read()
cr = raw.count('\r\n') > raw.count('\n') / 2
s = raw.replace('\r\n', '\n') if cr else raw
pat = re.compile(
    r'( *)for \(\(dst, a\), b\) in (self\.rec_y|plane)\[d\.\.d \+ (w|wc)\]\n'
    r' *\.iter_mut\(\)\n'
    r' *\.zip\(&(rc0|ly0)\[s0\.\.s0 \+ (?:w|wc)\]\)\n'
    r' *\.zip\(&(rc1|ly1)\[s1\.\.s1 \+ (?:w|wc)\]\)\n'
    r' *\{\n'
    r' *\*dst = \(\(\*a as u16 \+ \*b as u16 \+ 1\) >> 1\) as u8;\n'
    r' *\}\n')
def rep(m):
    ind, dstp, wv, src0, src1 = m.group(1), m.group(2), m.group(3), m.group(4), m.group(6)
    return (f'{ind}// WIN: this bi-average was a scalar byte loop; `pixel_avg` computes\n'
            f'{ind}// (a + b + 1) >> 1 exactly with pavgb, and the row helper walks the\n'
            f'{ind}// span in kernel-legal widths.\n'
            f'{ind}rusty_h264_common::inter::avg_row_into(\n'
            f'{ind}    &{src0}[s0..s0 + {wv}],\n'
            f'{ind}    &{src1}[s1..s1 + {wv}],\n'
            f'{ind}    {wv},\n'
            f'{ind}    &mut {dstp}[d..d + {wv}],\n'
            f'{ind});\n')
s2, n = pat.subn(rep, s)
print("  banded-span zip loops replaced: sites=%d" % n)
if cr: s2 = s2.replace('\n', '\r\n')
import os, tempfile
d = os.path.dirname(os.path.abspath(P)); fd, tmp = tempfile.mkstemp(dir=d); os.close(fd)
io.open(tmp, 'w', encoding='utf-8', newline='').write(s2); os.replace(tmp, P)
