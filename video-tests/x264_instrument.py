#!/usr/bin/env python3
"""Add an rdtsc stage profiler to the external x264 reference checkout.

This is the x264-side twin of `rusty_h264-common/src/prof.rs`: the same idea
(one RAII scope at the top of each stage function, cycles + call counts into
static buckets, one dump at exit) so the two encoders' stage breakdowns are
read the same way and can be put side by side.

Idempotent — re-running on an already-instrumented tree is a no-op.

    python video-tests/x264_instrument.py [path-to-x264]   # default ../_ref_x264

The instrumented binary is a MEASUREMENT reference only. It never enters the
rs_h264 build; our crates stay pure, `forbid(unsafe_code)` Rust.

NOTE: the buckets are plain (non-atomic) counters, so profiled runs must use
`--threads 1`. That is what we want anyway — per-function attribution against
our single-threaded encoder core.
"""
import os
import re
import sys

ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "_ref_x264")
ROOT = os.path.abspath(ROOT)

# id, display name, "nested inside" (for reading the dump), file, function signature anchor
TAPS = [
    ("LOOKAHEAD", "lookahead/slicetype", "",        "encoder/slicetype.c",  "void x264_slicetype_analyse( x264_t *h, int intra_minigop )"),
    ("ANALYSE",   "mb-analyse(decision)", "",       "encoder/analyse.c",    "void x264_macroblock_analyse( x264_t *h )"),
    ("ME",        "me-search",           "ANALYSE", "encoder/me.c",         "void x264_me_search_ref( x264_t *h, x264_me_t *m, int16_t (*mvc)[2], int i_mvc, int *p_halfpel_thresh )"),
    ("INTRA",     "intra-cost",          "ANALYSE", "encoder/analyse.c",    "static void mb_analyse_intra( x264_t *h, x264_mb_analysis_t *a, int i_satd_inter )"),
    ("ENCODE",    "mb-encode(T/Q+recon)", "",       "encoder/macroblock.c", "void x264_macroblock_encode( x264_t *h )"),
    ("MC",        "inter-mc",            "ENCODE",  "common/macroblock.c",  "void x264_mb_mc( x264_t *h )"),
    ("CABAC",     "entropy-cabac",       "",        "encoder/cabac.c",      "void x264_macroblock_write_cabac( x264_t *h, x264_cabac_t *cb )"),
    ("CAVLC",     "entropy-cavlc",       "",        "encoder/cavlc.c",      "void x264_macroblock_write_cavlc( x264_t *h )"),
    ("DEBLOCK",   "deblock",             "",        "common/deblock.c",     "void x264_frame_deblock_row( x264_t *h, int mb_y )"),
    # x264 derives boundary strengths in its MB-ENCODE loop (encoder.c calls this
    # per macroblock), not inside deblock_row — so without this tap the cost lands
    # in x264's residue and its `deblock` stage looks far cheaper than ours, which
    # derives bS inside the filter pass. Tapping it makes the comparison honest.
    ("BS",        "deblock-strength",    "",        "common/macroblock.c",  "void x264_macroblock_deblock_strength( x264_t *h )"),
    ("HPEL",      "hpel-filter",         "",        "common/mc.c",          "void x264_frame_filter( x264_t *h, x264_frame_t *frame, int mb_y, int b_end )"),
    ("TOTAL",     "TOTAL",               "",        "encoder/encoder.c",    "int     x264_encoder_encode( x264_t *h,"),
]

IDS = [t[0] for t in TAPS]

HEADER = """/* x264prof.h — rs_h264 measurement instrument (NOT part of upstream x264).
 * Stage profiler mirroring rusty_h264-common/src/prof.rs: one RAII scope per
 * stage function, rdtsc cycles + call counts into static buckets, dumped at exit.
 * Single-threaded only (plain counters) — profile with --threads 1. */
#ifndef X264PROF_H
#define X264PROF_H
#include <stdint.h>

enum {
%(enum)s
    X264P_N
};

extern uint64_t x264p_cyc[X264P_N];
extern uint64_t x264p_cnt[X264P_N];
extern const char * const x264p_name[X264P_N];
extern const char * const x264p_nest[X264P_N];
void x264p_init( void );

static inline uint64_t x264p_rdtsc( void )
{
#if defined(__i386__) || defined(__x86_64__)
    uint32_t lo, hi;
    __asm__ volatile( "rdtsc" : "=a"(lo), "=d"(hi) );
    return ((uint64_t)hi << 32) | lo;
#else
    return 0;
#endif
}

typedef struct { uint64_t t0; int id; } x264p_scope_t;

static inline void x264p_scope_end( x264p_scope_t *s )
{
    x264p_cyc[s->id] += x264p_rdtsc() - s->t0;
    x264p_cnt[s->id]++;
}

/* __attribute__((cleanup)) gives us RAII in C, so early returns still close
 * the scope — the same guarantee Rust's Drop gives our own profiler.
 *
 * Gated on -DX264_PROF so the STOCK binary carries zero instrumentation: the
 * throughput arm must not pay a tax our own profiler-off build doesn't pay.
 * build.sh builds both (x264.exe stock, x264_prof.exe instrumented). */
#ifdef X264_PROF
#define X264P_SCOPE(ID) \\
    x264p_init(); \\
    x264p_scope_t x264p_s_ __attribute__((cleanup(x264p_scope_end))) = { x264p_rdtsc(), (ID) }
#else
#define X264P_SCOPE(ID) ((void)0)
#endif

#endif
"""

BODY = """/* x264prof.c — rs_h264 measurement instrument (NOT part of upstream x264). */
#include <stdio.h>
#include <stdlib.h>
#include "common/x264prof.h"

/* Declared rather than pulled in via common.h: this file is compiled once,
 * bit-depth independent, and common.h requires BIT_DEPTH to be defined. */
int64_t x264_mdate( void );

uint64_t x264p_cyc[X264P_N];
uint64_t x264p_cnt[X264P_N];
const char * const x264p_name[X264P_N] = {
%(names)s
};
const char * const x264p_nest[X264P_N] = {
%(nests)s
};

static int x264p_ready;
static int64_t x264p_t0_us;
static uint64_t x264p_c0;

/* Cycles are wall-proportional on an invariant TSC; recover ns/cycle from the
 * whole-run wall/cycle ratio, exactly as our Rust profiler's anchor does. */
static void x264p_dump( void )
{
    double us = (double)(x264_mdate() - x264p_t0_us);
    double cyc = (double)(x264p_rdtsc() - x264p_c0);
    double ns_per_cyc = cyc > 0 ? (us * 1000.0) / cyc : 0.0;

    const char *path = getenv( "X264_PROF_OUT" );
    FILE *f = path ? fopen( path, "w" ) : stderr;
    if( !f ) f = stderr;

    fprintf( f, "#x264prof\\tstage\\tnested_in\\tcycles\\tcalls\\tms\\tcyc_per_call\\n" );
    for( int i = 0; i < X264P_N; i++ )
        fprintf( f, "x264prof\\t%%s\\t%%s\\t%%llu\\t%%llu\\t%%.3f\\t%%.1f\\n",
                 x264p_name[i], x264p_nest[i],
                 (unsigned long long)x264p_cyc[i], (unsigned long long)x264p_cnt[i],
                 x264p_cyc[i] * ns_per_cyc / 1e6,
                 x264p_cnt[i] ? (double)x264p_cyc[i] / x264p_cnt[i] : 0.0 );
    fprintf( f, "x264prof\\t_WALL\\t\\t0\\t0\\t%%.3f\\t0\\n", us / 1000.0 );
    if( path && f != stderr ) fclose( f );
}

void x264p_init( void )
{
    if( x264p_ready ) return;
    x264p_ready = 1;
    x264p_t0_us = x264_mdate();
    x264p_c0 = x264p_rdtsc();
    atexit( x264p_dump );
}
"""


def gen_files():
    enum = "\n".join("    X264P_%s," % i for i in IDS)
    names = "\n".join('    "%s",' % t[1] for t in TAPS)
    nests = "\n".join('    "%s",' % t[2] for t in TAPS)
    open(os.path.join(ROOT, "common", "x264prof.h"), "w").write(HEADER % {"enum": enum})
    open(os.path.join(ROOT, "common", "x264prof.c"), "w").write(
        BODY % {"names": names, "nests": nests})
    print("  wrote common/x264prof.{h,c}")


def insert_taps():
    by_file = {}
    for tid, _, _, path, sig in TAPS:
        by_file.setdefault(path, []).append((tid, sig))

    for path, taps in by_file.items():
        full = os.path.join(ROOT, path)
        src = open(full, encoding="utf-8", errors="surrogateescape").read()
        orig = src
        # Normalise first: several of these files carry arch-specific #includes
        # inside `#if HAVE_*` blocks (some near the END of the file), and landing
        # the include in one of those means it is never compiled.
        src = src.replace('#include "common/x264prof.h"\n', '')
        # The FIRST #include of every x264 .c is the unconditional common.h, so
        # inserting straight after it is always outside a conditional block.
        m = re.search(r'^#include .*$', src, re.M)
        assert m, path
        src = src[:m.end()] + '\n#include "common/x264prof.h"' + src[m.end():]

        for tid, sig in taps:
            if "X264P_%s )" % tid in src or "X264P_%s);" % tid in src:
                continue
            i = src.find(sig)
            assert i >= 0, "anchor not found in %s: %s" % (path, sig[:60])
            # the function's opening brace is the next line that is exactly '{'
            j = src.find("\n{\n", i)
            assert j >= 0 and j - i < 600, "opening brace not found for %s" % tid
            at = j + 3
            src = src[:at] + "    X264P_SCOPE( X264P_%s );\n" % tid + src[at:]
            print("  tap %-10s -> %s" % (tid, path))

        if src != orig:
            open(full, "w", encoding="utf-8", errors="surrogateescape").write(src)


def wire_build():
    """Add x264prof.c to the build driver's bit-depth-independent source list."""
    bs = os.path.join(ROOT, "build.sh")
    s = open(bs, encoding="utf-8").read()
    if "x264prof.c" in s:
        print("  build.sh already wires x264prof.c")
        return
    s = s.replace("      common/win32thread.c\"", "      common/win32thread.c\n      common/x264prof.c\"")
    open(bs, "w", encoding="utf-8").write(s)
    print("  build.sh: added common/x264prof.c")


if __name__ == "__main__":
    if not os.path.isdir(os.path.join(ROOT, "encoder")):
        sys.exit("not an x264 checkout: %s" % ROOT)
    print("instrumenting %s" % ROOT)
    gen_files()
    insert_taps()
    wire_build()
    print("done — rebuild with: bash build.sh")
