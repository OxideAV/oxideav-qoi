# oxideav-qoi round-175 profile baseline (refreshed round 183)

This directory captures the profiling-baseline numbers produced by the
`examples/profile_qoi.rs` driver that round 175 introduces. The driver
is the durable artefact: any future round (or local A/B run) can
reproduce these numbers + capture per-symbol flame-graphs against it
without re-discovering the harness recipe.

Round 183 refreshed the decode column after replacing the decoder's
per-pixel `Vec::push` output writer with an exact-size `vec![0; n]`
buffer + cursor-write (`decoder::write_pixel`, `decoder::fill_run`).
Decode throughput moved from `0.5–1.5 GiB/s` to `0.6 GiB/s–37 GiB/s`
depending on chunk mix; the encode numbers are unchanged because the
encoder's `Vec::push`-heavy chunk emitter wasn't touched. See the
"Round-183 delta" note under "Reading the numbers" for which chunks
moved by what proportion.

The Criterion harnesses under `benches/` (added in round 156)
measure steady-state throughput in a sampling framework — great for
A/B regression detection, poor for `samply` / `perf record` /
`cargo flamegraph` because Criterion's warm-up + sampling layers +
estimator math show up in the captured stack and bury the real codec
hot paths. The profile driver is a flat measure-this-thing loop with
a single `Instant::now()` / `elapsed()` pair around a fixed iteration
count — clean stacks, comparable wall-clock cost.

## Headline numbers (round 183 refresh, Apple-silicon dev box, release build)

Each scenario is self-contained (deterministic xorshift32-seeded
synthetic input, no external fixtures, no `tests/fixtures/` reads).
The five scenarios mirror the five Criterion benches byte-for-byte
so profile output and bench numbers correspond.

```
== encode ==
  encode    rgba/320x240/gradient              iters=  200    0.466 ms/iter    628.02 MiB/s (raw)  out=122515B/iter (0.399 of input)
  encode    rgb24/640x480/gradient             iters=   80    2.038 ms/iter    431.18 MiB/s (raw)  out=479751B/iter (0.521 of input)
  encode    rgba/512x512/solid-run             iters=  200    0.465 ms/iter   2149.88 MiB/s (raw)  out=4255B/iter (0.004 of input)
  encode    rgba/320x240/alpha-changing        iters=  200    0.277 ms/iter   1058.91 MiB/s (raw)  out=383716B/iter (1.249 of input)
  encode    rgba/320x240/index-cycle           iters=  300    0.144 ms/iter   2033.75 MiB/s (raw)  out=76846B/iter (0.250 of input)

== decode ==
  decode    rgba/320x240/gradient              iters=  200    0.326 ms/iter    898.78 MiB/s (raw)
  decode    rgb24/640x480/gradient             iters=   80    1.427 ms/iter    615.97 MiB/s (raw)
  decode    rgba/512x512/solid-run             iters=  200    0.027 ms/iter  37405.03 MiB/s (raw)
  decode    rgba/320x240/alpha-changing        iters=  200    0.093 ms/iter   3143.62 MiB/s (raw)
  decode    rgba/320x240/index-cycle           iters=  300    0.107 ms/iter   2728.65 MiB/s (raw)

== roundtrip ==
  roundtrip rgba/320x240/gradient              iters=  200    0.807 ms/iter    363.06 MiB/s (raw)
  roundtrip rgb24/640x480/gradient             iters=   80    3.432 ms/iter    256.06 MiB/s (raw)
  roundtrip rgba/512x512/solid-run             iters=  200    0.484 ms/iter   2064.44 MiB/s (raw)
  roundtrip rgba/320x240/alpha-changing        iters=  200    0.367 ms/iter    797.58 MiB/s (raw)
  roundtrip rgba/320x240/index-cycle           iters=  300    0.245 ms/iter   1195.66 MiB/s (raw)
```

### Round-183 delta vs the original round-175 baseline

| Scenario                          | Decode r175    | Decode r183     | Speedup |
| --------------------------------- | -------------- | --------------- | ------- |
| RGBA 320×240 gradient             | 708 MiB/s      |   899 MiB/s     | 1.27×   |
| RGB24 640×480 gradient            | 521 MiB/s      |   616 MiB/s     | 1.18×   |
| RGBA 512×512 solid-RUN            | 1.54 GiB/s     | 37.4 GiB/s      | 24.4×   |
| RGBA 320×240 alpha-changing       | 1.36 GiB/s     |  3.14 GiB/s     | 2.31×   |
| RGBA 320×240 8-colour INDEX cycle | 1.46 GiB/s     |  2.73 GiB/s     | 1.87×   |

Encode numbers are unchanged within run-to-run noise (round 183 only
touched the decoder). Roundtrip rows pick up the decoder's
improvement on the read half — the smaller-than-decode gain reflects
the still-unchanged encoder share of the wall-clock loop.

The solid-RUN row is the dramatic case: every chunk is a RUN, every
RUN now lowers to a single `[u8]::chunks_exact_mut + copy_from_slice`
hot loop the autovectoriser turns into a wide-store memcpy. The
gradient row's gain is more modest because every output byte still
goes through chunk-dispatch + index lookup, but the per-pixel
`Vec::push`-bounds-check is gone.

## Reading the numbers

### Decode

- Decode runs at **0.6 GiB/s of mixed-chunk output → 37 GiB/s of solid
  RUN-stream output** depending on the chunk mix. The round-183
  `fill_run` helper turns a `solid-run` decode into a tight
  `chunks_exact_mut` + `copy_from_slice` loop that the optimiser
  vectorises to wide stores; that's the row that jumps 24× over the
  round-175 baseline. Mixed-chunk rows still pay the per-pixel
  chunk-dispatch + index-store cost, but the per-pixel
  `Vec::push`-bounds-check is gone.
- `gradient` (mixed DIFF / LUMA / RGB / INDEX) is the most balanced
  per-chunk cost — chunk dispatch sees every arm and the running
  pixel array sees enough hits to keep the INDEX arm warm, so it's
  the floor on what the decoder hot path can hit before the
  chunk-walker itself becomes the bottleneck.
- The `huge_header_does_not_over_allocate` fuzz-discovered bound on
  the eager `Vec::with_capacity` reservation (capped at
  `chunks.len() * 62` pixels rather than the header's claimed
  `width * height`) carries no measurable cost on real images — the
  cap only kicks in when the header is attacker-claimed-impossible.
  Round 183 moved this from a runtime-only check on the eager
  reservation to a pre-allocation check that rejects the request
  before allocating; the legitimate-input path always satisfies the
  bound trivially (every chunk emits ≥1 pixel and consumes ≥1 byte).

### Encode

- Encode is dominated by the **chunk-selection priority chain**
  (RUN > INDEX > DIFF > LUMA > RGB / RGBA) the spec mandates and the
  `encoder.rs::encode_qoi_full` loop walks every pixel. The
  `solid-run` scenario hits ~2.1 GiB/s because the `cur == prev`
  fast path skips the per-pixel hash + DIFF / LUMA arithmetic; the
  `alpha-changing` scenario hits ~1.0 GiB/s because every pixel
  bypasses the DIFF / LUMA / RGB tests and emits the unconditional
  5-byte RGBA chunk, which is the second-cheapest exit because no
  arithmetic is needed past the alpha-changed check.
- `gradient` (RGBA 320×240) is the worst encode case at 612 MiB/s
  because every pixel actually walks the full priority chain — the
  DIFF range check fails on most pixels (the xorshift noise pushes
  deltas outside ±1), the LUMA range check then runs, and only then
  does the per-pixel hash + index store happen. This is the encode
  hot path we'd target if a future round wanted to add a SIMD or
  branch-restructured fast path.
- `index-cycle` is the cheapest at 2.07 GiB/s because the 8-colour
  palette puts a hit at `index[hash(cur)]` on every cycle pass; the
  short-circuit at the INDEX arm skips the DIFF / LUMA / RGB checks
  for ~85 % of pixels.

### Roundtrip

- Roundtrip is roughly `encode + decode` for each scenario, within
  the few-percent overhead of the intermediate `Vec<u8>` allocation
  the timed loop doesn't elide. There's no cross-frame state to
  amortise — QOI is a stateless single-image codec — so the
  roundtrip row reads as a sanity check on the encode/decode pair
  rather than a separate measurement.

## Reproducing

```bash
# 1. Build the profile driver in release with debug info.
cargo build --release --example profile_qoi -p oxideav-qoi

# 2. Run all five scenarios across encode / decode / roundtrip.
./target/release/examples/profile_qoi all

# Per-mode subsets are useful for sampler runs (samply / perf):
./target/release/examples/profile_qoi encode    5000
./target/release/examples/profile_qoi decode    5000
./target/release/examples/profile_qoi roundtrip 2000
```

### Capturing flamegraphs (samply, no root on macOS)

`samply` is the recommended sampler on macOS — it uses
`task_for_pid` after self-signing, no DTrace / `perf` / elevated
privileges. On Linux substitute `perf record` (root or
`perf_event_paranoid <= 1`) or `samply record` directly.

```bash
cargo install samply
cargo install inferno

# Sample. --unstable-presymbolicate writes a sidecar syms file so
# the JSON profile resolves to source symbols even after the
# binary's debug-info is gone.
samply record --unstable-presymbolicate --save-only \
    -o encode.json.gz \
    -r 1997 \
    -- target/release/examples/profile_qoi encode 5000

# Convert samply's processed-profile JSON to Brendan-Gregg folded
# stacks, then SVG. (The folded-stacks format is the stable
# interchange artefact — drop the JSON afterwards.)
samply export --output encode.folded encode.json.gz
inferno-flamegraph \
    --title "oxideav-qoi encode (round 175)" \
    --subtitle "samply 1997Hz, 5000 iters x 5 scenarios" \
    < encode.folded > encode.svg

# Repeat for decode / roundtrip.
```

The intermediate `*.json.gz` files are NOT committed — they're a
samply implementation detail. The folded-stack files (`*.folded`)
and SVGs (`*.svg`) are the stable interchange format; future rounds
that capture profiles should commit those alongside this README
baseline.

## Wall

Captured without consulting any external library source. `samply` is
a sampling profiler that only observes the OxideAV binary at runtime;
the captured stacks reference only the project's own modules + stdlib
+ macOS runtime (`libsystem_*`, `dyld`). No third-party QOI
implementation participated in this baseline.
