# oxideav-qoi round-175 profile baseline

This directory captures the profiling-baseline numbers produced by the
`examples/profile_qoi.rs` driver that round 175 introduces. The driver
is the durable artefact: any future round (or local A/B run) can
reproduce these numbers + capture per-symbol flame-graphs against it
without re-discovering the harness recipe.

The Criterion harnesses under `benches/` (added in round 156)
measure steady-state throughput in a sampling framework — great for
A/B regression detection, poor for `samply` / `perf record` /
`cargo flamegraph` because Criterion's warm-up + sampling layers +
estimator math show up in the captured stack and bury the real codec
hot paths. The profile driver is a flat measure-this-thing loop with
a single `Instant::now()` / `elapsed()` pair around a fixed iteration
count — clean stacks, comparable wall-clock cost.

## Headline numbers (round 175, Apple-silicon dev box, release build)

Each scenario is self-contained (deterministic xorshift32-seeded
synthetic input, no external fixtures, no `tests/fixtures/` reads).
The five scenarios mirror the five Criterion benches byte-for-byte
so profile output and bench numbers correspond.

```
== encode ==
  encode    rgba/320x240/gradient              iters=  200    0.479 ms/iter    611.98 MiB/s (raw)  out=122515B/iter (0.399 of input)
  encode    rgb24/640x480/gradient             iters=   80    2.023 ms/iter    434.44 MiB/s (raw)  out=479751B/iter (0.521 of input)
  encode    rgba/512x512/solid-run             iters=  200    0.464 ms/iter   2155.27 MiB/s (raw)  out=4255B/iter (0.004 of input)
  encode    rgba/320x240/alpha-changing        iters=  200    0.283 ms/iter   1036.52 MiB/s (raw)  out=383716B/iter (1.249 of input)
  encode    rgba/320x240/index-cycle           iters=  300    0.141 ms/iter   2074.78 MiB/s (raw)  out=76846B/iter (0.250 of input)

== decode ==
  decode    rgba/320x240/gradient              iters=  200    0.413 ms/iter    708.53 MiB/s (raw)
  decode    rgb24/640x480/gradient             iters=   80    1.687 ms/iter    520.97 MiB/s (raw)
  decode    rgba/512x512/solid-run             iters=  200    0.651 ms/iter   1535.84 MiB/s (raw)
  decode    rgba/320x240/alpha-changing        iters=  200    0.216 ms/iter   1355.07 MiB/s (raw)
  decode    rgba/320x240/index-cycle           iters=  300    0.201 ms/iter   1457.01 MiB/s (raw)

== roundtrip ==
  roundtrip rgba/320x240/gradient              iters=  200    0.876 ms/iter    334.53 MiB/s (raw)
  roundtrip rgb24/640x480/gradient             iters=   80    3.738 ms/iter    235.10 MiB/s (raw)
  roundtrip rgba/512x512/solid-run             iters=  200    1.160 ms/iter    861.79 MiB/s (raw)
  roundtrip rgba/320x240/alpha-changing        iters=  200    0.527 ms/iter    556.23 MiB/s (raw)
  roundtrip rgba/320x240/index-cycle           iters=  300    0.363 ms/iter    807.59 MiB/s (raw)
```

These numbers track the round-156 Criterion baseline in the
crate-level `README.md` (decode 540 MiB/s gradient → 1.55 GiB/s solid,
encode 640 MiB/s gradient → 2.13 GiB/s solid). The bench-vs-profile
delta is within noise; the wall-clock ordering of scenarios is the
same in both harnesses.

## Reading the numbers

### Decode

- Decode runs at **0.5–1.5 GiB/s of raw output** depending on the
  chunk mix. The `solid-run` scenario decodes 1 GiB/s slower than
  `alpha-changing` despite being the simpler input because the
  fully-RUN-dominated stream still hits the per-pixel `push_pixel`
  output writer 256 Ki times; that inner write loop is the dominant
  cost once chunk dispatch collapses to a single hot arm. The
  `alpha-changing` scenario is faster per output byte because each
  RGBA chunk emits 4 output bytes in one branch and the chunk-walk
  pointer advances 5 input bytes per pixel — the throughput numerator
  doesn't have a "wasted RUN expansion" tail.
- `gradient` (mixed DIFF / LUMA / RGB / INDEX) is the cheapest per
  output byte after `index-cycle` — chunk dispatch is well-balanced
  and the running pixel array sees enough hits to keep the INDEX arm
  warm.
- The `huge_header_does_not_over_allocate` fuzz-discovered bound on
  the eager `Vec::with_capacity` reservation (capped at
  `chunks.len() * 62` pixels rather than the header's claimed
  `width * height`) carries no measurable cost on real images — the
  cap only kicks in when the header is attacker-claimed-impossible.

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
