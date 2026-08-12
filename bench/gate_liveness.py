#!/usr/bin/env python3
"""LIVENESS — was the gate's decision site actually REACHED by this encode?

Every gate harness here answers "is the line in the right place". None of them
could answer the question that comes first: did the gate RUN? Three states are
indistinguishable in a BD table, and they have opposite fixes:

    seen > 0, fired == 0   the corpus lacks the content      -> extend the corpus
    seen == 0 (or ~0)      the path is dead / barely alive    -> fix the config
    fired > 0, no output   the arm is a no-op                 -> delete the gate

The counters have existed all along (`signals::census`, (fired, seen) per gate).
What did not exist was any harness READING them: `gate_audit.py` carries a
hand-curated "clips this gate acts on" table, and every census number in these
files is a comment transcribing a `gatecheck` run somebody did separately. That
is the same species of defect as a stale threshold — a constant that drifts
against the encoder while looking authoritative.

Worse, `gatecheck` builds its OWN `EncoderConfig`, so its counts describe a
DIFFERENT encode than the one an audit judges. That gap is exactly how the
`--refs 3` problem survived:

    sub8_grain   seen 1213 at --refs 1   ->   seen 19 at --refs 3   (akiyo_cif)
    sub8_split   seen 4852               ->   seen 76

The split arm is alive only on the first P frame of each GOP once num_refs > 1.
Note the number that matters: 19, NOT 0. A zero-check would have called that run
healthy. Liveness is a FLOOR, not a null test — `min_seen` is the count below
which you would not believe the gate was really under test.

The tap (`RFF_CENSUS_CSV=<path>`, `rusty_h264::gate_census_dump_csv`) writes from
the same process, the same flags, and the same encode the harness is measuring.

Usage as a module:
    import gate_liveness as live
    counts = live.run(["target/release/rusty_h264.exe", "encode", ...], env)
    status, msg = live.verdict(counts, chain=["sub8_grain"], min_seen=200)

Usage as a tool — diff two configurations, which is the check that would have
caught the above by itself:
    python bench/gate_liveness.py --diff refs1.csv refs3.csv
"""
import os
import subprocess
import sys
import tempfile

CENSUS_ENV = "RFF_CENSUS_CSV"


def read(path):
    """Parse a census CSV into {gate: (fired, seen)}."""
    out = {}
    with open(path) as f:
        rows = [l.strip() for l in f if l.strip()]
    if not rows:
        return out
    hdr = [h.strip().lower() for h in rows[0].split(",")]
    gi, fi, si = hdr.index("gate"), hdr.index("fired"), hdr.index("seen")
    for line in rows[1:]:
        c = [x.strip() for x in line.split(",")]
        out[c[gi]] = (int(c[fi]), int(c[si]))
    return out


def run(argv, env=None):
    """Run one encode with the census tap on; return {gate: (fired, seen)}.

    The census rides the encode the caller was going to run anyway — measuring
    it in a separate run would be measuring a different configuration, which is
    the entire failure this module exists to close.
    """
    env = dict(env or os.environ)
    fd, path = tempfile.mkstemp(suffix=".census.csv")
    os.close(fd)
    try:
        env[CENSUS_ENV] = path
        subprocess.run(argv, capture_output=True, env=env)
        if not os.path.exists(path) or os.path.getsize(path) == 0:
            return {}
        return read(path)
    finally:
        try:
            os.remove(path)
        except OSError:
            pass


def verdict(counts, chain, min_seen=0):
    """Judge one clip's liveness. Returns (status, message).

    status: "ok" | "absent" | "dead" | "thin" | "inconsistent"

    `chain` is the precondition chain, LEAF LAST (e.g.
    ["sub8_grain", "sub8_split"] when the second sits behind the first). It is
    declared rather than threaded through the encoder's call frames: threading
    makes every intermediate function carry an argument it does not use, to
    express what is really a global property. The declaration checks itself —
    `seen` cannot GROW as the chain narrows, so a wrong order (or an entry point
    the chain omits) shows up instead of being trusted.
    """
    if not counts:
        return "absent", "no census: build with the RFF_CENSUS_CSV tap, or the run cannot say"
    missing = [g for g in chain if g not in counts]
    if missing:
        return "absent", "census does not report %s (has: %s)" % (
            ", ".join(missing), ", ".join(sorted(counts)))

    seen = [(g, counts[g][1]) for g in chain]
    # CAUSALITY is the invariant that actually holds, and it holds whatever UNIT
    # each stage counts: a stage that was never consulted cannot have a live
    # downstream. Do NOT check "seen must not grow" — that is only true for a
    # same-unit chain, and real chains cross granularity: sub8_grain is consulted
    # per macroblock (1213 on akiyo) while sub8_split is consulted per quad
    # (4852 = 4x1213). Growth there is arithmetic. An earlier version of this
    # function refused that chain outright and the real counts refuted it
    # immediately; `narrowing()` below reports it without refusing.
    for (og, ov), (ig, iv) in zip(seen, seen[1:]):
        if ov == 0 and iv > 0:
            return "inconsistent", (
                "`%s` was never consulted yet `%s` seen %d — a dead stage cannot have a "
                "live downstream, so the declared order is wrong or `%s` has an entry "
                "point the chain omits" % (og, ig, iv, ig))

    leaf, lseen = seen[-1]
    dead = next((g for g, v in seen if v == 0), None)
    if lseen == 0:
        where = ("; the path dies upstream at `%s`" % dead) if dead and dead != leaf else ""
        return "dead", (
            "`%s` was NEVER CONSULTED%s — the gate did not run in this configuration. "
            "This is not a corpus gap: adding content changes nothing when the decision "
            "site is not reached." % (leaf, where))
    if lseen <= min_seen:
        return "thin", (
            "`%s` consulted only %d time(s), at or below the min_seen=%d floor — reached, "
            "but nowhere near exercised enough to carry a verdict (sub8_grain reads 19 at "
            "--refs 3 and 1213 at --refs 1; 19 is not zero and is not a test)"
            % (leaf, lseen, min_seen))
    return "ok", "`%s` seen %d, fired %d" % (leaf, lseen, counts[leaf][0])


def narrowing(counts, chain):
    """ADVISORY: stages where consultations GREW along the chain.

    Only a defect when every stage counts the same unit. Returns a list of
    (outer, outer_seen, inner, inner_seen) for the caller to print — never a
    refusal, because per-MB feeding per-quad grows by construction.
    """
    seen = [(g, counts[g][1]) for g in chain if g in counts]
    return [(og, ov, ig, iv) for (og, ov), (ig, iv) in zip(seen, seen[1:])
            if ov > 0 and iv > ov]


def diff(a_path, b_path, collapse=4.0):
    """Compare two census files and flag every gate whose consultations COLLAPSE.

    This is the standalone form of the check that would have caught the
    `--refs 3` problem with no other tooling: encode the same clip under the
    configuration the census was taken in and the one the audit runs in, diff
    them, and any gate that loses most of its consultations is being measured
    where it cannot run.
    """
    a, b = read(a_path), read(b_path)
    rows, bad = [], 0
    for g in sorted(set(a) | set(b)):
        af, asn = a.get(g, (0, 0))
        bf, bsn = b.get(g, (0, 0))
        flag = ""
        if asn > 0 and bsn == 0:
            flag, bad = "  <- DEAD in B", bad + 1
        elif asn > 0 and bsn > 0 and asn / bsn >= collapse:
            flag, bad = "  <- COLLAPSES %.0fx" % (asn / bsn), bad + 1
        rows.append("  %-18s seen %8d -> %-8d   fired %8d -> %-8d%s"
                    % (g, asn, bsn, af, bf, flag))
    print("LIVENESS DIFF  A=%s  B=%s" % (a_path, b_path))
    print("\n".join(rows))
    print("---")
    if bad:
        print("%d gate(s) lose their decision site between these configurations." % bad)
        print("An audit run under B cannot say anything about them: it would be measuring")
        print("a gate that is switched off, and reporting the fallback as a neutral result.")
    else:
        print("PASS: every gate keeps its decision site across both configurations.")
    return 1 if bad else 0


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--diff":
        sys.exit(diff(sys.argv[2], sys.argv[3]))
    print(__doc__)
    sys.exit(2)
