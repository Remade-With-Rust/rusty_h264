#!/bin/sh
# Synthesize the corpus-gap content classes (great-gate.md §2): screen content
# and grain. These classes were MISSING from the natural-video corpus, which is
# exactly why the synthetic/grain axis signals could not be validated before
# P1. Deterministic ffmpeg recipes — regenerate rather than commit binaries.
set -e
cd "$(dirname "$0")/clips"

# SCREEN: terminal-style text (static code lines + one scrolling line).
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=c=black:s=352x288:r=30:d=2,\
drawtext=fontfile='C\:/Windows/Fonts/consola.ttf':text='fn main() { let sig = FrameSignals new(sy) }':fontcolor=white:fontsize=13:x=8:y=20,\
drawtext=fontfile='C\:/Windows/Fonts/consola.ttf':text='let bs = derive_bs_row(r); harvest(sig, qp);':fontcolor=lime:fontsize=13:x=8:y=40,\
drawtext=fontfile='C\:/Windows/Fonts/consola.ttf':text='cargo test -p rusty_h264-encoder --release':fontcolor=white:fontsize=13:x=8:y=mod(288-30*t\,288),\
format=yuv420p" -frames:v 60 screen_text.y4m

# SCREEN: synthetic UI/graphics (testsrc2: gradients, text, moving elements).
ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "testsrc2=s=352x288:r=30:d=2,format=yuv420p" -frames:v 60 screen_ui.y4m

# GRAIN: natural static content + temporal noise (film-grain analog; allf=t
# regenerates the noise every frame — noise never predicts).
ffmpeg -hide_banner -loglevel error -y -i akiyo_cif.y4m \
  -vf "noise=alls=14:allf=t,format=yuv420p" -frames:v 60 grain_akiyo.y4m

# GRAIN: pure grain on flat luma — the degenerate case (no texture, no motion,
# ONLY the noise floor).
ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "color=c=gray:s=352x288:r=30:d=2,noise=alls=14:allf=t,format=yuv420p" \
  -frames:v 60 grain_flat.y4m

echo "synth clips written to $(pwd)"
