"""WIN: fold the two beta compares into one.

Every lt4/eq4 filter decision contains

    m = (alpha > |p0-q0|) & (beta > |p1-p0|) & (beta > |q1-q0|)

and the last two share their threshold, so

    (beta > d1) & (beta > d2)  ==  beta > max(d1, d2)

which turns one compare plus one AND into one max: a net -1 op wherever it
appears. Both diffs are non-negative and < 256 (u8 inputs widened to i16), so a
SIGNED max is exact and stays on the SSE2 / NEON baseline.

This is an algebraic change -- it removes an operation rather than renaming a
shared term -- which is the class that actually pays here.
"""
import io

P = 'crates/rusty_h264-accel/src/deblock_simd.rs'
raw = io.open(P, encoding='utf-8', newline='').read()
CR = raw.count('\r\n') > raw.count('\n') / 2
s = raw.replace('\r\n', '\n') if CR else raw

NOTE_SSE = ("        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and\n"
            "        // one AND become one max. Both diffs are >= 0 and < 256, so signed max\n"
            "        // is exact.\n")
NOTE_NEON = "        // WIN (NEON twin): same folded beta compare as the SSE2 path.\n"

subs = [
    # ---- chroma kernels, SSE2: p1v/p0v/q0v/q1v naming ----
    ("        let mut m = _mm_cmpgt_epi16(a, absdiff(p0v, q0v));\n"
     "        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, absdiff(p1v, p0v)));\n"
     "        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, absdiff(q1v, q0v)));\n",
     NOTE_SSE +
     "        let dmax = _mm_max_epi16(absdiff(p1v, p0v), absdiff(q1v, q0v));\n"
     "        let mut m = _mm_cmpgt_epi16(a, absdiff(p0v, q0v));\n"
     "        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, dmax));\n",
     'sse2 chroma m'),
    # ---- chroma kernels, NEON ----
    ("        let mut m = vcgtq_s16(a, absdiff(p0v, q0v));\n"
     "        m = vandq_u16(m, vcgtq_s16(b, absdiff(p1v, p0v)));\n"
     "        m = vandq_u16(m, vcgtq_s16(b, absdiff(q1v, q0v)));\n",
     NOTE_NEON +
     "        let dmax = vmaxq_s16(absdiff(p1v, p0v), absdiff(q1v, q0v));\n"
     "        let mut m = vcgtq_s16(a, absdiff(p0v, q0v));\n"
     "        m = vandq_u16(m, vcgtq_s16(b, dmax));\n",
     'neon chroma m'),
    # ---- luma cores, SSE2: bare p0/q0 naming ----
    ("        let mut m = _mm_cmpgt_epi16(alpha, absdiff(p0, q0));\n"
     "        m = _mm_and_si128(m, _mm_cmpgt_epi16(beta, absdiff(p1, p0)));\n"
     "        m = _mm_and_si128(m, _mm_cmpgt_epi16(beta, absdiff(q1, q0)));\n",
     NOTE_SSE +
     "        let dmax = _mm_max_epi16(absdiff(p1, p0), absdiff(q1, q0));\n"
     "        let mut m = _mm_cmpgt_epi16(alpha, absdiff(p0, q0));\n"
     "        m = _mm_and_si128(m, _mm_cmpgt_epi16(beta, dmax));\n",
     'sse2 luma m'),
    # ---- luma cores, NEON ----
    ("        let mut m = vcgtq_s16(alpha, absdiff(p0, q0));\n"
     "        m = vandq_u16(m, vcgtq_s16(beta, absdiff(p1, p0)));\n"
     "        m = vandq_u16(m, vcgtq_s16(beta, absdiff(q1, q0)));\n",
     NOTE_NEON +
     "        let dmax = vmaxq_s16(absdiff(p1, p0), absdiff(q1, q0));\n"
     "        let mut m = vcgtq_s16(alpha, absdiff(p0, q0));\n"
     "        m = vandq_u16(m, vcgtq_s16(beta, dmax));\n",
     'neon luma m'),
]

for old, new, lbl in subs:
    n = s.count(old)
    print("  %-16s sites=%d" % (lbl, n))
    if n:
        s = s.replace(old, new)

if CR:
    s = s.replace('\n', '\r\n')
io.open(P, 'w', encoding='utf-8', newline='').write(s)
print("folded beta compare applied")
