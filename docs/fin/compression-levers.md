# Compression levers

How rusty_h264 gets smaller files. This is the *theory and direction* doc — not an
implementation plan. It explains where the bits go today, the framework for
attacking them, and the three highest-impact levers for the encoder.

## Where we are today

The encoder codes **every frame as a full intra IDR**, every macroblock as
`I_16x16` with **DC prediction only** (predict the whole 16×16 as one flat
average), nearest-rounding quantization, a single fixed QP, and CAVLC entropy
coding. The bitstream is correct and bit-exactly decodable by ffmpeg — but the
*decisions* on top of it are the simplest possible. Almost all of the gap to a
tuned encoder (x264 is ~1.55× smaller at matched QP) is missing **encoder
intelligence**, not missing bitstream capability.

## The framework: rate-distortion

Compression is minimizing **bits** for a given **distortion**. Every technique
below pulls one of three strings:

1. **Shrink the residual** before coding it (better prediction).
2. **Code the residual more cheaply** (smarter quantization / entropy).
3. **Spend bits where they matter** (rate control, RD-optimal decisions).

The residual — source minus prediction — is what we actually spend bits on. The
cheapest bit is the one you never code because your prediction was already
right.

---

## Lever 1 — Richer intra prediction

**The biggest still-image / I-frame win.** Today we use one mode (`I_16x16` DC).
H.264 offers far more:

- **`I_16x16` vertical / horizontal / plane.** *Plane* fits a linear gradient
  across the block, which collapses the residual on smooth content (skies,
  shading) that DC leaves with visible ramp error.
- **`I_4x4`, 9 directional modes per 4×4 block.** Each small block is predicted
  along an edge direction (horizontal, vertical, diagonal-down-left, etc.). This
  is what captures texture and fine structure — and it's the largest single
  chunk of the gap to x264.
- **Chroma: 4 modes** (DC / horizontal / vertical / plane) instead of DC only.

**Why it works:** better prediction → smaller residual → fewer and smaller
transform coefficients → fewer CAVLC bits. Pulls lever (1) directly.

**Cost / dependencies:** needs per-block neighbor reconstruction (we already
have this), mode signalling in the bitstream, and — to actually *use* the modes
— Lever 3 to choose among them.

**Expected impact:** large for intra/still content; closes most of the
intra-only gap to x264.

---

## Lever 2 — Inter prediction (P-frames)

**By far the largest win for video.** Today we throw away all temporal
redundancy by coding every frame from scratch. Real video is ~99% the same
frame-to-frame.

- **Motion-compensated prediction:** "this block is *that* block from the
  previous frame, shifted by (dx, dy)." Code only the motion vector plus a
  near-zero residual.
- **Motion estimation:** the search for the best-matching reference block (the
  expensive part; quality scales with search effort).
- **Quarter-pel interpolation:** sub-pixel motion for sharper matches.
- **MV prediction:** code motion-vector *deltas* against neighbors, since
  adjacent blocks tend to move together.
- **`P_Skip`:** when the predicted block is good enough, code *nothing at all* —
  not even a residual.

**Why it works:** temporal prediction makes the residual vanish for static or
smoothly-moving regions. Pulls lever (1) at a scale intra prediction can't —
typically 10–50× over all-intra on real sequences.

**Cost / dependencies:** a decoded-picture buffer (reference frame management),
the motion search, interpolation filters, and new slice/macroblock syntax
(P-slices, MV coding). The heaviest lift, and the step-change for video.

**Expected impact:** transformational for clips; irrelevant for single images.

---

## Lever 3 — Rate-distortion optimization (mode decision + trellis quant)

**The multiplier that makes Levers 1 and 2 worth anything.** Having modes and
motion vectors is useless without *choosing well*. Today the encoder makes no
rate-distortion decisions at all.

- **Mode decision:** for each macroblock, evaluate candidates (`I_16x16` modes,
  `I_4x4` modes, later P modes / MVs) by their true cost
  `J = Distortion + λ · Bits` and pick the minimum. This is where tuned encoders
  spend most of their cleverness.
- **Trellis / RD quantization:** we round each coefficient to the nearest level,
  but the *bit cost* of a coefficient in CAVLC is non-linear. Sometimes rounding
  a coefficient down — even to zero — costs far fewer bits for negligible
  distortion. Trellis quant solves for the RD-optimal level sequence per block
  (Viterbi over the coefficients).
- **Adaptive QP / perceptual weighting:** flat regions tolerate a higher QP for
  free; detail and edges want more bits. Moving bits to where the eye notices
  improves *perceived* efficiency, and proper rate control lets us hit a target
  bitrate instead of a fixed quality.

**Why it works:** pulls levers (2) and (3) — codes the same residual more
cheaply and spends the bit budget where it buys the most quality. Typically
10–20% from mode decision and another 5–10% from trellis, "free" at the same QP.

**Cost / dependencies:** needs the candidates to choose *between* (so it rides on
Levers 1 and 2), a bit-cost estimator for CAVLC, and a distortion metric. It is
also the main reason a serious encoder is "slower" — it is *thinking*; we are
not.

**Expected impact:** a consistent multiplier across all content; the difference
between "has the features" and "uses them well."

---

## Honorable mentions (context, not core levers)

- **In-loop deblocking filter** — doesn't shrink the bitstream directly, but
  raising reconstructed quality lets us run a higher QP for the same perceived
  quality (effective compression), and it improves reference-frame quality,
  which compounds through inter prediction. Currently signalled *disabled*.
- **CABAC** — ~10–15% better than CAVLC, but a *Main-profile* tool, outside
  Constrained Baseline. A ceiling, not a path, unless we expand scope.

## Priority summary

| Goal | Lever order |
|---|---|
| Still images / intra-only | **1 (intra modes)** → **3 (RDO)** |
| Video / sequences | **2 (inter)** → **3 (RDO)** → 1 |

The through-line: the bitstream substrate is correct and conformant; the
compression gap is the *encoder's choices*. Levers 1 and 2 add capability;
Lever 3 is what extracts value from it.
