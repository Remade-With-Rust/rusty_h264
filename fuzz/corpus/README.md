# Fuzz corpora

The committed seeds are deliberately SMALL (24-48 KB truncations of real x264
streams, ~400 KB total). A corpus is an input to the fuzzer, not an artifact of
it, and a 33 MB directory of full streams -- which is what seeding from the whole
bench corpus produced -- belongs in a fuzzing service's storage rather than in
git history.

A truncated stream is still a good seed: it exercises the parser and libFuzzer
grows it from there.

## Growing the corpus locally

Seed from the full x264 corpus the decode benchmark builds:

```sh
bash bench/decode_x264_speedtest.sh 1        # builds _xbench/*.264
cp _xbench/*.264 fuzz/corpus/decode_stream/
cargo +nightly fuzz run decode_stream -- -max_total_time=3600
```

`cargo fuzz` writes newly-interesting inputs back into the corpus directory, so
a long local run leaves a better corpus behind. **Do not commit what it adds** --
minimise it first (`cargo +nightly fuzz cmin <target>`) and keep the committed
set small.

## The three targets

| target | entry point | why it is separate |
|---|---|---|
| `decode` | `Decoder::decode` | one call; the narrowest reproduction of a crash |
| `decode_stream` | `Decoder::decode_stream` | multi-picture state: reference lists, MMCO, POC across an IDR, multi-slice pictures with differing ref lists |
| `nal_split` | `split_annex_b` + `emulation_unprevent` | the byte layer below the decoder; far more executions per second than a full decode |

A crash found here is a **security** bug, not a robustness one -- see
[`SECURITY.md`](../../SECURITY.md). The reproducer libFuzzer writes into
`fuzz/artifacts/` becomes a regression test.
