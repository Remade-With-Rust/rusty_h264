"""One home for the campaign-gating BD arithmetic (fast-transcendentals plan, A6).

Before this module, ssim_db / polyfit3 / bd lived as EIGHT copies across the
bench scripts, already drifted into FOUR AST-distinct variants (docstrings,
truthiness-vs-`!= 0.0` pivot guards, lambda-vs-def integrators, `bd` vs
`bd_rate`) — all still semantically identical, which is exactly the moment to
consolidate: a difference you can read beats one you discover mid-campaign.

The canonical bodies below are the majority variant (gate_refit's), verbatim.
`python bdmath.py` runs the equivalence selftest: every function is compared
against a frozen copy of the original implementation on random sweeps, exact
float equality (same ops, same order).
"""
import math


def ssim_db(s):
    """SSIM -> dB-like scale (-10*log10(1-s), floored 1e-9 = 90 dB cap)."""
    return -10.0 * math.log10(max(1.0 - s, 1e-9))


def polyfit3(xs, ys):
    """Least-squares cubic fit via normal equations (4x4, partial pivot)."""
    a = [[0.0] * 4 for _ in range(4)]
    b = [0.0] * 4
    for x, y in zip(xs, ys):
        xp = [1.0]
        for p in range(1, 7):
            xp.append(xp[p - 1] * x)
        for j in range(4):
            for k in range(4):
                a[j][k] += xp[j + k]
            b[j] += y * xp[j]
    for c in range(4):
        piv = max(range(c, 4), key=lambda r: abs(a[r][c]))
        a[c], a[piv] = a[piv], a[c]
        b[c], b[piv] = b[piv], b[c]
        for r in range(4):
            if r != c and a[c][c]:
                f = a[r][c] / a[c][c]
                for k in range(c, 4):
                    a[r][k] -= f * a[c][k]
                b[r] -= f * b[c]
    return [b[i] / a[i][i] if a[i][i] else 0.0 for i in range(4)]


def bd_rate(anchor, test):
    """BD-rate (%) of test vs anchor; each a list of (rate, quality_db).

    Negative = test needs FEWER bits at equal quality. None when either curve
    has < 4 points or the quality ranges do not overlap.
    """
    def prep(p):
        v = sorted((d, math.log10(r)) for r, d in p)
        return [x[0] for x in v], [x[1] for x in v]
    if len(anchor) < 4 or len(test) < 4:
        return None
    da, la = prep(anchor)
    dt, lt = prep(test)
    ca, ct = polyfit3(da, la), polyfit3(dt, lt)
    lo, hi = max(da[0], dt[0]), min(da[-1], dt[-1])
    if hi <= lo:
        return None
    I = lambda c, x: c[0]*x + c[1]*x*x/2 + c[2]*x**3/3 + c[3]*x**4/4
    return (10.0 ** (((I(ct, hi) - I(ct, lo)) - (I(ca, hi) - I(ca, lo))) / (hi - lo)) - 1) * 100


# Five scripts used this name for the same function.
bd = bd_rate


def _selftest():
    import random
    # Frozen verbatim copy of the ORIGINAL (gate_refit) implementation — the
    # oracle. If a "cleanup" of the canonical bodies above changes one bit of
    # any campaign verdict, this fails.
    def _o_ssim_db(s):
        return -10.0 * math.log10(max(1.0 - s, 1e-9))

    def _o_polyfit3(xs, ys):
        a = [[0.0] * 4 for _ in range(4)]
        b = [0.0] * 4
        for x, y in zip(xs, ys):
            xp = [1.0]
            for p in range(1, 7):
                xp.append(xp[p - 1] * x)
        # (loop split exactly as the original: accumulate per point)
            for j in range(4):
                for k in range(4):
                    a[j][k] += xp[j + k]
                b[j] += y * xp[j]
        for c in range(4):
            piv = max(range(c, 4), key=lambda r: abs(a[r][c]))
            a[c], a[piv] = a[piv], a[c]
            b[c], b[piv] = b[piv], b[c]
            for r in range(4):
                if r != c and a[c][c]:
                    f = a[r][c] / a[c][c]
                    for k in range(c, 4):
                        a[r][k] -= f * a[c][k]
                    b[r] -= f * b[c]
        return [b[i] / a[i][i] if a[i][i] else 0.0 for i in range(4)]

    def _o_bd(anchor, test):
        def prep(p):
            v = sorted((d, math.log10(r)) for r, d in p)
            return [x[0] for x in v], [x[1] for x in v]
        if len(anchor) < 4 or len(test) < 4:
            return None
        da, la = prep(anchor)
        dt, lt = prep(test)
        ca, ct = _o_polyfit3(da, la), _o_polyfit3(dt, lt)
        lo, hi = max(da[0], dt[0]), min(da[-1], dt[-1])
        if hi <= lo:
            return None
        I = lambda c, x: c[0]*x + c[1]*x*x/2 + c[2]*x**3/3 + c[3]*x**4/4
        return (10.0 ** (((I(ct, hi) - I(ct, lo)) - (I(ca, hi) - I(ca, lo))) / (hi - lo)) - 1) * 100

    rng = random.Random(0x5EED)
    for i in range(4097):
        s = i / 4096.0
        assert ssim_db(s) == _o_ssim_db(s), s
    for _ in range(2000):
        n = rng.randint(4, 8)
        pts_a = [(rng.uniform(100, 1e6), rng.uniform(28, 44)) for _ in range(n)]
        pts_t = [(r * rng.uniform(0.5, 1.5), d) for r, d in pts_a]
        assert bd_rate(pts_a, pts_t) == _o_bd(pts_a, pts_t)
        xs = [d for _, d in pts_a]
        ys = [math.log10(r) for r, _ in pts_a]
        assert polyfit3(xs, ys) == _o_polyfit3(xs, ys)
    # Landmark: halved rate at every quality reads exactly -50%.
    a = [(1000.0 * 2**i, 30.0 + 2*i) for i in range(4)]
    t = [(r / 2, d) for r, d in a]
    assert abs(bd_rate(a, t) - -50.0) < 1e-6
    assert abs(bd_rate(a, a)) < 1e-9
    print("bdmath selftest OK")


if __name__ == "__main__":
    _selftest()
