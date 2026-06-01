# oxideav-qoi round-175 profile baseline (refreshed rounds 183 + 205)

This directory captures the profiling-baseline numbers produced by the
`examples/profile_qoi.rs` driver that round 175 introduces. The driver
is the durable artefact: any future round (or local A/B run) can
reproduce these numbers + capture per-symbol flame-graphs against it
without re-discovering the harness recipe.

Round 183 refreshed the decode column after replacing the decoder's
per-pixel `Vec::push` output writer with an exact-size `vec![0; n]`
buffer + cursor-write (`decoder::write_pixel`, `decoder::fill_run`).
Decode throughput moved from `0.5–1.5 GiB/s` to `0.6 GiB/s–37 GiB/s`
depending on chunk mix.

Round 205 refreshed the encode column by taking the same cursor-write
idea to the encoder: pre-allocate `vec![0u8; 14 + n*5 + 8]` once,
write every chunk through `buf[out_pos] = …` / `copy_from_slice`,
truncate down to the actual produced length before return. Encode
throughput moved by 1.08× on the solid-RUN row up to 1.85× on the
alpha-changing row — the chunks where the per-pixel emit was paying
the most in `Vec::push` capacity-growth + length-update cost.

The Criterion harnesses under `benches/` (added in round 156)
measure steady-state throughput in a sampling framework — great for
A/B regression detection, poor for `samply` / `perf record` /
`cargo flamegraph` because Criterion's warm-up + sampling layers +
estimator math show up in the captured stack and bury the real codec
hot paths. The profile driver is a flat measure-this-thing loop with
a single `Instant::now()` / `elapsed()` pair around a fixed iteration
count — clean stacks, comparable wall-clock cost.

## Headline numbers (round 205 refresh, Apple-silicon dev box, release build)

Each scenario is self-contained (deterministic xorshift32-seeded
synthetic input, no external fixtures, no `tests/fixtures/` reads).
The five scenarios mirror the five Criterion benches byte-for-byte
so profile output and bench numbers correspond.

```
== encode ==
  encode    rgba/320x240/gradient              iters=  200    0.329 ms/iter    891.22 MiB/s (raw)  out=122515B/iter (0.399 of input)
  encode    rgb24/640x480/gradient             iters=   80    1.482 ms/iter    593.02 MiB/s (raw)  out=479751B/iter (0.521 of input)
  encode    rgba/512x512/solid-run             iters=  200    0.443 ms/iter   2255.72 MiB/s (raw)  out=4255B/iter (0.004 of input)
  encode    rgba/320x240/alpha-changing        iters=  200    0.149 ms/iter   1968.21 MiB/s (raw)  out=383716B/iter (1.249 of input)
  encode    rgba/320x240/index-cycle           iters=  300    0.124 ms/iter   2368.72 MiB/s (raw)  out=76846B/iter (0.250 of input)

== decode ==
  decode    rgba/320x240/gradient              iters=  200    0.316 ms/iter    926.52 MiB/s (raw)
  decode    rgb24/640x480/gradient             iters=   80    1.399 ms/iter    628.39 MiB/s (raw)
  decode    rgba/512x512/solid-run             iters=  200    0.027 ms/iter  37662.72 MiB/s (raw)
  decode    rgba/320x240/alpha-changing        iters=  200    0.094 ms/iter   3126.75 MiB/s (raw)
  decode    rgba/320x240/index-cycle           iters=  300    0.107 ms/iter   2749.45 MiB/s (raw)

== roundtrip ==
  roundtrip rgba/320x240/gradient              iters=  200    0.636 ms/iter    460.47 MiB/s (raw)
  roundtrip rgb24/640x480/gradient             iters=   80    3.008 ms/iter    292.23 MiB/s (raw)
  roundtrip rgba/512x512/solid-run             iters=  200    0.473 ms/iter   2115.70 MiB/s (raw)
  roundtrip rgba/320x240/alpha-changing        iters=  200    0.246 ms/iter   1192.81 MiB/s (raw)
  roundtrip rgba/320x240/index-cycle           iters=  300    0.232 ms/iter   1260.82 MiB/s (raw)
```

### Round-183 delta vs the original round-175 baseline (decode)

| Scenario                          | Decode r175    | Decode r183     | Speedup |
| --------------------------------- | -------------- | --------------- | ------- |
| RGBA 320×240 gradient             | 708 MiB/s      |   899 MiB/s     | 1.27×   |
| RGB24 640×480 gradient            | 521 MiB/s      |   616 MiB/s     | 1.18×   |
| RGBA 512×512 solid-RUN            | 1.54 GiB/s     | 37.4 GiB/s      | 24.4×   |
| RGBA 320×240 alpha-changing       | 1.36 GiB/s     |  3.14 GiB/s     | 2.31×   |
| RGBA 320×240 8-colour INDEX cycle | 1.46 GiB/s     |  2.73 GiB/s     | 1.87×   |

The solid-RUN row is the dramatic case: every chunk is a RUN, every
RUN now lowers to a single `[u8]::chunks_exact_mut + copy_from_slice`
hot loop the autovectoriser turns into a wide-store memcpy.

### Round-205 delta vs the round-183 baseline (encode)

| Scenario                          | Encode r183    | Encode r205     | Speedup |
| --------------------------------- | -------------- | --------------- | ------- |
| RGBA 320×240 gradient             |   628 MiB/s    |   930 MiB/s     | 1.49×   |
| RGB24 640×480 gradient            |   431 MiB/s    |   569 MiB/s     | 1.32×   |
| RGBA 512×512 solid-RUN            |  2.15 GiB/s    |  2.29 GiB/s     | 1.08×   |
| RGBA 320×240 alpha-changing       |  1.06 GiB/s    |  1.96 GiB/s     | 1.85×   |
| RGBA 320×240 8-colour INDEX cycle |  2.03 GiB/s    |  2.44 GiB/s     | 1.18×   |

The alpha-changing row is the dramatic case: every pixel emits an
RGBA chunk, which the previous code did as five `Vec::push` calls
(tag + R + G + B + A); after r205 it's `buf[out_pos] = OP_RGBA` +
one 4-byte `copy_from_slice(&cur)`, a single tag store + a single
memcpy. The gradient row's gain is the second-largest because every
pixel actually walks the priority chain (RUN > INDEX > DIFF > LUMA >
RGB / RGBA) and each arm now lowers to indexed stores instead of
`Vec::push` capacity-check sequences. The solid-RUN row's gain is
the smallest because the RUN chunk is only a single byte — the
per-chunk emit was already cheap so the cursor-write saves less.

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
  `solid-run` scenario hits ~2.3 GiB/s because the `cur == prev`
  fast path skips the per-pixel hash + DIFF / LUMA arithmetic; the
  `alpha-changing` scenario hits ~1.96 GiB/s because every pixel
  bypasses the DIFF / LUMA / RGB tests and emits the unconditional
  5-byte RGBA chunk, which after r205 lowers to a tag store + 4-byte
  `copy_from_slice` (vs five `Vec::push` calls before).
- `gradient` (RGBA 320×240) is the worst encode case at 930 MiB/s
  (post r205) because every pixel actually walks the full priority
  chain — the DIFF range check fails on most pixels (the xorshift
  noise pushes deltas outside ±1), the LUMA range check then runs,
  and only then does the per-pixel hash + index store happen. The
  per-chunk emit is now a single indexed store + at most one short
  `copy_from_slice`, but the priority-chain walk itself is the
  remaining hot path; SIMD or branch-restructured fast paths in the
  picker would be the next encode-side improvement.
- `index-cycle` is the cheapest at 2.44 GiB/s because the 8-colour
  palette puts a hit at `index[hash(cur)]` on every cycle pass; the
  short-circuit at the INDEX arm skips the DIFF / LUMA / RGB checks
  for ~85 % of pixels — the INDEX-arm emit is a single tag-byte
  indexed store.

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
