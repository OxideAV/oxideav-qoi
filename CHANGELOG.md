# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4](https://github.com/OxideAV/oxideav-qoi/compare/v0.1.3...v0.1.4) - 2026-06-15

### Other

- add op_write target + write_to round-trip contract tests (r311)
- add QoiOp::write_to — byte-level inverse of iter_ops chunk walker
- qoi r298: op_iter fuzz target + fix QoiOp::tag() overflow panic
- structure-aware chunk_walk decoder target (r291 depth/fuzz)
- run-arm wide scan-ahead (r282 profile round)

### Added

- Round-311 `op_write` fuzz target plus three in-tree contract tests for
  `QoiOp::write_to`. The method is the byte-level inverse of the
  `iter_ops` chunk walker (round 304) but had no fuzz coverage and only a
  single hand-built mixed-op round-trip unit test. The structure-aware
  `op_write` target reuses the `op_iter` header-synthesis trick (spec-valid
  14-byte header + 8-byte end marker wrapped around the fuzzer's chunk
  bytes), walks the stream with `iter_ops`, re-serializes every yielded op
  via `write_to`, and asserts: each `write_to` appends exactly
  `encoded_len()` bytes; a clean walk's rebuilt buffer equals the original
  chunk section byte-for-byte and re-walks to the identical op sequence
  (the `iter_ops` → `write_to` → `iter_ops` inverse property); and a
  truncated walk's rebuilt buffer is a byte-prefix of the original (the
  `Truncated` sentinel re-emits only its lone leading byte, dropping the
  never-arrived body) that re-walks to the non-truncated op prefix. Seeded
  with the same one-per-chunk-type + mixed + truncated corpus as `op_iter`.
  Because the local ASAN fuzz toolchain isn't always available, the same
  contract is also pinned by three deterministic unit tests
  (`op_write_to_fuzz_contract_clean_streams`,
  `op_write_to_fuzz_contract_truncated_streams`, and
  `op_write_to_fuzz_contract_every_single_tag_byte`, the last sweeping all
  256 possible leading tag bytes through the round-trip contract).
- Round-304 `QoiOp::write_to(&mut Vec<u8>)` — the byte-level inverse of
  the `iter_ops` chunk walker. Where `tag()` reconstructs only the
  leading chunk byte, `write_to` appends the complete on-wire chunk:
  leading byte plus every body byte the spec defines (`Luma`'s
  `(dr_dg+8)<<4 | (db_dg+8)` nibble byte per QOI_OP_LUMA Byte[1], `Rgb`'s
  raw `r/g/b`, `Rgba`'s raw `r/g/b/a`). The appended byte count equals
  `encoded_len()`, the leading byte equals `tag()`, and the bytes are
  exactly what the crate's own encoder would write — so
  `iter_ops(input)` → `write_to` → `iter_ops` round-trips an in-spec
  chunk stream byte-for-byte (asserted on a mixed-op image exercising
  all six chunk types). A `Truncated` sentinel re-emits only its stored
  leading byte, consistent with its `encoded_len()` of 1. Like `tag()`,
  the `+2` / `+32` / `-1` / `+8` bias steps use wrapping operators and
  mask to each field's spec bit width, so the method is total over the
  `pub` field space and never panics under overflow checks. Lets a stream
  rewriter, an alternative encoder, or a synthesis test emit chunks
  without re-deriving the spec bit-packing.
- Round-298 `op_iter` fuzz target — a structure-aware harness for the
  stream-level chunk iterator (`iter_ops` / `iter_ops_strict`), a decode
  path distinct from `parse_qoi`: it walks the chunk stream into typed
  `QoiOp`s without materialising a pixel buffer. Like `chunk_walk` it
  synthesizes a spec-valid 14-byte header + trailing 8-byte end marker
  around the fuzzer's bytes so the walker gets past the header gate on
  nearly every iteration. Beyond "never panics", it asserts the
  `encoded_len() == 1 + body_len()` width identity, that `tag()`
  reconstructs the exact leading chunk byte for every non-truncated op,
  exact consumed-byte accounting against the chunk-section length, and
  that `iter_ops_strict` agrees with the non-strict walk on the
  truncation boundary (`Ok` ⇔ no `Truncated`, else `Err`). Seeded with
  one named input per chunk type plus mixed-op and truncated (RGB / LUMA)
  seeds.

### Fixed

- `QoiOp::tag()` is now **total** over the public field space. The
  variants carry `pub` fields, so a caller can construct an op whose
  field falls outside the bit width the spec assigns it —
  `Run { length: 0 }` (the `-1` run-length debias underflows),
  `Diff { dr: i8::MAX, .. }` (the `+2` delta bias overflows `i8`), or
  `Luma { dg: i8::MAX, .. }` (the `+32` bias overflows). Under a debug /
  fuzz build (overflow checks on) the old plain arithmetic panicked
  (`attempt to subtract / add with overflow`); the bias steps now use
  `wrapping_add` / `wrapping_sub` and mask the result down to the tag's
  field, so the method returns a well-defined byte instead of panicking.
  Every in-spec value yields the identical tag as before, so the
  iterator round-trip is unchanged. Found while building the `op_iter`
  fuzz target; covered by the new `op_tag_is_total_over_extreme_fields`
  unit test.

- Round-291 `chunk_walk` fuzz target — a structure-aware decoder
  harness that synthesizes a spec-valid 14-byte header + trailing
  8-byte end marker around the fuzzer's bytes and hands the result to
  `parse_qoi` as the chunk stream. Because the synthesized header
  always passes the field gate (magic, channels ∈ {3,4}, colorspace
  ∈ {0,1}, non-zero dimensions clamped to 1..=64), the decoder reaches
  the per-op walk on nearly every iteration, concentrating coverage on
  the six chunk decode paths (`QOI_OP_RGB` / `RGBA` / `INDEX` / `DIFF`
  / `LUMA` / `RUN`) and the truncation / overrun guards rather than
  rediscovering the header. The only asserted contract is that the
  decode call returns (`Ok` or `Err`, never a panic / abort / OOM).
  Seeded with one named input per chunk type plus a truncated-stream
  seed. A 90-second local run reached ~28.5M executions (~314k
  exec/s), coverage saturated, zero crashes.

### Changed

- Round-282 encoder run-arm restructure (profile-guided): when a
  pixel equals the previous one, the encoder now consumes the WHOLE
  run with a wide scan-ahead (16-byte / 4-pixel block compares for
  RGBA, 12-byte / 4-pixel for RGB, scalar tail at the run boundary)
  and emits all the `QOI_OP_RUN` chunks at once (⌊N/62⌋ max-length
  chunks + remainder) instead of re-entering the per-pixel loop —
  load + compare + run-counter bookkeeping + flush test — once per
  matching pixel. The running-pixel-array store for the run also
  collapses from one no-op-repeat store (each re-deriving the
  `(R*3+G*5+B*7+A*11) % 64` hash) per pixel to a single store when
  the run opens; index state observed by later INDEX lookups is
  unchanged because lookups only happen after the run breaks.
  Removing the `run` counter also drops the pending-run flush test
  from the non-run fall-through path. Output is byte-identical
  (reference-fixture byte-exact round-trip tests + a 5½-minute
  `encode_roundtrip` fuzz run confirm). Criterion, three interleaved
  pre/post pairs on the Apple-silicon dev box: RUN-dominated solid
  512×512 RGBA 2.66–2.70 GiB/s → 24.6–24.8 GiB/s (~9.2×); RGBA
  320×240 gradient 896–952 MiB/s → 999–1068 MiB/s (~+7%); RGB24
  gradient, alpha-changing, and INDEX-cycle rows unchanged within
  noise.

## [0.1.3](https://github.com/OxideAV/oxideav-qoi/compare/v0.1.2...v0.1.3) - 2026-06-10

### Other

- r267 typed-primitive surface — qoi_hash + QoiOp introspection
- drop release-plz.toml — use release-plz defaults across the workspace
- stream-level QoiOp chunk iterator (r237)
- channel-specialised inner loops (r231)
- caller-owned-buffer _into variants for encode + decode (r225)
- add parse_qoi_header cheap header probe + QoiHeader (r210)
- exact-size buffer + cursor-write hot path (r205)
- property-style sweep across QOI encode/decode invariants (r199)

### Added

- Round-267 typed-primitive surface on the `ops` module:
  - `qoi_hash([u8; 4]) -> u8`, the public typed form of the spec's
    running-pixel-array bucket selector
    `(R*3 + G*5 + B*7 + A*11) % 64`. The multiply runs in `u32`
    (non-wrapping), so `(0,0,0,255)` hashes to `53`, not the `21`
    an 8-bit-wrapping multiply gives. Re-exported at the crate root.
    The crate-internal `decoder::hash` now delegates to it, so there
    is a single source of truth for the hash arithmetic.
  - `QoiOp::tag()` reconstructs the exact leading chunk byte each op
    would encode to — `0xFE` / `0xFF` for `Rgb` / `Rgba`, and the
    bit-packed `OP_INDEX|index` / `OP_DIFF|(deltas+2)` /
    `OP_LUMA|(dg+32)` / `OP_RUN|(length-1)` tags for the four 2-bit
    chunks. `Truncated` returns its stored raw tag byte. This is the
    inverse of the iterator's leading-byte dispatch.
  - `QoiOp::body_len()` / `QoiOp::encoded_len()` give the post-tag
    body width (`0`/`1`/`3`/`4`) and total on-wire chunk width
    (`1`/`2`/`4`/`5`). Summing `encoded_len()` over an `iter_ops`
    walk reproduces the chunk-section byte count without re-encoding.
  - `QoiOp::is_truncated()` convenience predicate for the
    `Truncated` sentinel.
- Round-237 stream-level QOI chunk iterator: new `ops` module with
  a typed `QoiOp` enum (`Rgb`, `Rgba`, `Index`, `Diff`, `Luma`,
  `Run`, `Truncated`), a `QoiOpIter<'a>` `Iterator` impl, and two
  constructors — `iter_ops(&[u8]) -> Result<(QoiHeader,
  QoiOpIter<'_>)>` and `iter_ops_strict(&[u8]) -> Result<(QoiHeader,
  Vec<QoiOp>)>`. Walks the post-header chunk stream once linearly
  and yields one `QoiOp` per chunk *without* materialising a pixel
  buffer or running the running-pixel-array / `prev`-pixel state.
  Delta fields on `Diff` / `Luma` carry the un-biased signed values
  the spec defines (`-2..=+1` for `Diff`, `-32..=+31` for `Luma.dg`,
  `-8..=+7` for `Luma.{dr_dg, db_dg}`); `Run.length` carries the
  post-debias `1..=62` value; `Index.index` carries the raw 6-bit
  field. Both constructors run the same 14-byte header validation
  the full decoder does (magic, channels ∈ {3, 4}, colorspace ∈
  {0, 1}, non-zero dimensions, 8-byte end-marker present). The
  iterator surfaces mid-chunk truncation as a final
  `QoiOp::Truncated { tag, missing_body_bytes }`; the strict
  variant elevates the same condition to an `Err(InvalidData)`.
  Intended for chunk-shape histograms (compression diagnostics),
  debug pretty-printers / dumpers, and future encoder-priority-
  chain regression checks. Module-level docs spell out the
  "stateless w.r.t. decoded image" contract.

### Changed

- Round-231 encoder hot-path channel split: replaced the per-pixel
  `match qoi_channels { Rgb => …, Rgba => … }` that assembled the
  4-byte `cur` tuple inside the encode loop with a single up-front
  dispatch into one of two channel-specialised inner functions —
  `encode_inner_rgb` for 3-channel input and `encode_inner_rgba`
  for 4-channel input. Both are `#[inline]` and share an identical
  chunk-priority chain (RUN > INDEX > DIFF > LUMA > RGB / RGBA);
  the differences are confined to (a) the pixel-load shape (3-byte
  array literal vs 4-byte array literal), (b) the 3-channel path
  no longer carries the alpha-equality test or the RGBA emit arm
  at all — alpha is provably `0xff` for the entire stream, so
  both are unreachable, and (c) the 3-channel path no longer
  synthesises `cur[3] = prev[3]` on every pixel just to keep the
  downstream alpha-compare shape uniform.

  Encode throughput on the round-205 profile baseline (Apple-
  silicon dev box, release build, 3000-iter `examples/profile_qoi.rs`
  run):

  | Scenario                          | Encode r205 | Encode r231 | Speedup |
  | --------------------------------- | ----------- | ----------- | ------- |
  | RGBA 320×240 gradient             |   891 MiB/s |   974 MiB/s | 1.09×   |
  | RGB24 640×480 gradient            |   593 MiB/s |   652 MiB/s | 1.10×   |
  | RGBA 512×512 solid-RUN            |  2.26 GiB/s |  2.70 GiB/s | 1.19×   |
  | RGBA 320×240 alpha-changing       |  1.97 GiB/s |  2.05 GiB/s | 1.04×   |
  | RGBA 320×240 8-colour INDEX cycle |  2.37 GiB/s |  2.54 GiB/s | 1.07×   |

  The solid-RUN row is the biggest relative win because the inner
  loop body shrinks the most when the discriminant load goes away
  — every pixel takes the `cur == prev` fast path, and the
  `OP_RUN | (run - 1)` flush is the only chunk arm exercised, so
  shedding the match has nowhere to hide. The RGB24 gradient is
  the structurally cleanest win: the 3-channel inner loop no
  longer has the alpha-compare branch at all, and the optimiser
  keeps the LUMA / DIFF range checks tighter without the
  discriminant-load pressure. The alpha-changing row picks up
  the least relatively because its hot path was already the
  unconditional 5-byte RGBA emit — a tag store + 4-byte
  `copy_from_slice` that the r205 cursor-write had already taken
  to the limit of what the chunk-emit shape can do; the saving
  is the per-pixel discriminant load only.

  Public API (`encode_qoi`, `encode_qoi_full`, `encode_qoi_into`,
  `encode_qoi_full_into`) is unchanged byte-for-byte — the new
  inner functions are crate-private and produce identical chunk
  streams to the previous single-loop encoder. All 48 unit tests
  + 8 property-sweep tests + 6 reference-fixture byte-exact tests
  + the doctest pass under both `--features registry` and
  `--no-default-features`; in particular the four reference
  fixtures (`edgecase.qoi`, `qoi_logo.qoi`, `testcard.qoi`,
  `testcard_rgba.qoi`) still re-encode byte-for-byte against
  themselves, confirming the channel split preserves the chunk-
  selection chain exactly.

### Added

- Round-225 depth-mode public-API surface: `encode_qoi_into`,
  `encode_qoi_full_into`, and `parse_qoi_into` — caller-owned
  `&mut Vec<u8>` variants of the existing allocating wrappers.
  Buffer is cleared on entry, resized to the worst-case
  (`14 + n*5 + 8` for encode, `width*height*channels` for decode),
  written through the same cursor-store hot path, then truncated
  to the actual produced length before return. The retained
  `capacity()` covers the largest image seen so far, so tight
  encode/decode loops over similarly-sized images allocate once
  and reuse thereafter (image servers, thumbnail batches, encode-
  loop converters). `parse_qoi_into` returns the parsed
  `QoiHeader` so callers can size further downstream scratch
  without keeping the full `QoiImage` around.

  Implementation: both `encode_qoi` / `encode_qoi_full` and
  `parse_qoi` now delegate to the `_into` variants — the encoder
  and decoder hot paths are each defined exactly once, so a
  future bit-exactness change propagates to every entry point in
  one edit. Same chunk priority chain (RUN > INDEX > DIFF > LUMA
  > RGB / RGBA on encode), same `QoiError` variants on decode,
  same byte-for-byte output as the allocating wrappers — the
  only visible difference is buffer ownership.

  Nine new unit tests cover byte-equivalence against the
  allocating wrappers (encode + decode), capacity retention
  across calls (the headline reuse contract), buffer-clear on
  entry (no stale-byte leaks), end-to-end roundtrip under reuse,
  and full `QoiError` propagation through the decode `_into`
  path. A new `benches/reuse.rs` bench A/Bs alloc-per-call
  against the reuse path on a 64×64 RGBA encode/decode inner
  loop of 256 calls per criterion iteration. Apple-silicon dev
  box: both paths come out at parity within criterion's noise
  floor (encode ~7 µs/call, decode ~18 µs/call), reflecting that
  the macOS allocator's small/medium-block path is essentially
  free at these sizes — the surface still earns its keep on
  systems with more expensive malloc/free and on
  millions-of-small-thumbnails batch workloads. All 48 unit
  tests + 8 property-sweep tests + 6 reference-fixture tests +
  the doctest pass under both `--features registry` and
  `--no-default-features`.

- Round-210 depth-mode public-API surface: `parse_qoi_header` plus
  the supporting `QoiHeader` struct (`width`, `height`, `channels`,
  `colorspace`, all `Copy`). Cheap header-only probe that validates
  the 14-byte QOI header (`qoif` magic, channels ∈ {3,4}, colorspace
  ∈ {0,1}, non-zero dims) and returns the metadata tuple without
  walking the chunk stream or allocating a pixel buffer. Accepts
  inputs as short as 14 bytes (`parse_qoi` still requires 14 + 8 =
  22 to also cover the end marker). Intended for thumbnail-grid
  metadata probing, output-size estimation before allocating a
  decode buffer, and per-application size-limit rejection where
  decoding the full pixel stream would be wasteful.

  Implementation: shared `parse_header_only` helper in
  `decoder.rs` so the validity tests are a single source of truth —
  any future spec clarification on header-field validity (e.g. a
  new colorspace value) lands once and propagates to both
  `parse_qoi_header` and the prologue of `parse_qoi`. Nine new
  tests cover field extraction, 14-byte minimum input, short-input
  rejection, bad magic / channels / colorspace / zero-dimension
  rejection, body-tail ignored (probe parses a file whose body is
  garbage as long as the header is well-formed), `QoiHeader: Copy`
  trait bound, and synthetic-header parity. A
  `header_probe_agrees_with_full_decode_on_every_fixture` integration
  test loops over all four reference fixtures + their 14-byte
  prefixes and asserts the probe's `(width, height, channels,
  colorspace)` tuple matches the full decode byte-for-byte. Public
  API is purely additive (existing `parse_qoi` / `encode_qoi` /
  `encode_qoi_full` signatures unchanged); standalone (no
  `oxideav-core`) and registry-feature builds both expose the new
  entry point.

### Changed

- Round-205 encoder hot-path refactor: replaced the per-chunk
  `Vec::push` / `Vec::extend_from_slice` emit pattern with an
  exact-size `vec![0u8; 14 + pixel_count * 5 + END_MARKER.len()]`
  pre-allocation + moving `out_pos` byte cursor that writes every
  chunk through indexed `buf[out_pos] = …` stores (single-byte tags)
  or `buf[out_pos..].copy_from_slice` (multi-byte chunks). The
  buffer is truncated down to the actual produced length at return,
  so the public `encode_qoi` / `encode_qoi_full` signature is
  unchanged. Worst-case allocation is realised only on the
  alpha-changing-every-pixel path; on the solid-fill / index / DIFF
  paths the over-allocation never materialises because the buffer
  is truncated to the actual `out_pos` at return. Mirrors the
  round-183 decoder refactor that replaced per-pixel `Vec::push`
  writes with `&mut [u8]` cursor stores. Encode throughput on the
  round-175 profile baseline (Apple-silicon dev box, release build,
  1000-iter `examples/profile_qoi.rs` run):

  | Scenario                          | Encode r183 | Encode r205 | Speedup |
  | --------------------------------- | ----------- | ----------- | ------- |
  | RGBA 320×240 gradient             |   624 MiB/s |   930 MiB/s | 1.49×   |
  | RGB24 640×480 gradient            |   431 MiB/s |   569 MiB/s | 1.32×   |
  | RGBA 512×512 solid-RUN            |  2.12 GiB/s |  2.29 GiB/s | 1.08×   |
  | RGBA 320×240 alpha-changing       |  1.06 GiB/s |  1.96 GiB/s | 1.85×   |
  | RGBA 320×240 8-colour INDEX cycle |  2.07 GiB/s |  2.44 GiB/s | 1.18×   |

  The gradient and alpha-changing rows are the dramatic cases: the
  per-chunk emit collapses to a tag store + at most one 4-byte
  `copy_from_slice` instead of five separate `Vec::push` calls. All
  29 pre-existing unit tests + 8 property-sweep tests + 5 fixture
  byte-exact tests + the doctest pass under both `--features
  registry` and `--no-default-features`; three new regression tests
  (`encoder_exact_size_buffer_run_only_stream` covering the RUN-arm
  tag-store path across the 62-pixel cap boundary,
  `encoder_exact_size_buffer_mixed_stream` covering DIFF / LUMA /
  RGB / RGBA / INDEX via the indexed-store and `copy_from_slice`
  paths, and `encoder_truncates_to_actual_len` confirming the
  post-emit `Vec::truncate` drops trailing zero bytes from the
  worst-case allocation so the decoder doesn't see them between
  the last chunk and the end marker) lock in the new cursor-write
  contract.

### Added

- Round-199 deterministic property-test sweep under
  `tests/property_sweep.rs`. Eight test functions run roughly 4_000
  pseudo-random `(width, height, channels, colorspace, pixels)`
  triples through `encode_qoi_full → parse_qoi` and assert six
  semantic invariants per case: lossless roundtrip, the worst-case
  size bound `14 + n*5 + 8`, header bytes echoing the input,
  encoder determinism, a tighter solid-fill compact bound of
  `14 + 5 + ceil(n/62) + 8`, and idempotent re-encode
  (`encode(decode(encode(px))) == encode(px)`). Five distinct input
  generators (random, smooth-deltas, RUN-heavy, 8-colour palette,
  alpha-churn) each exercise a different path through the encoder's
  chunk-priority chain (RUN > INDEX > DIFF > LUMA > RGB / RGBA).
  A separate sweep hammers the solid-fill bound at the 62-pixel
  chunk-cap modular boundaries (widths 1, 30, 61, 62, 63, 124, 125,
  187, 200, 512, 1024) and another targets skewed shapes (1×N,
  N×1, prime×prime). The harness uses a self-contained xorshift32
  PRNG seeded per scenario so any failure is reproducible from the
  seed printed in the assertion message; no new dev-dep is
  introduced (`proptest` / `quickcheck` deliberately avoided). All
  40 tests in the crate now pass under both `--features registry`
  and `--no-default-features`.

## [0.1.2](https://github.com/OxideAV/oxideav-qoi/compare/v0.1.1...v0.1.2) - 2026-05-29

### Other

- exact-size output buffer + cursor-write hot path (r183)
- flat samply-friendly driver + r175 baseline numbers (r175)
- add encode_roundtrip cargo-fuzz target (r162)
- criterion harnesses for decode / encode / roundtrip (r156)
- add cargo-fuzz decode harness; fix huge-header allocation abort

### Changed

- Round-183 decoder hot-path refactor: replaced the per-pixel
  `Vec::push`-based output writer (`decoder::push_pixel`) with an
  exact-size `vec![0; pixel_count * bpp]` buffer + slice-cursor write
  (`decoder::write_pixel`) plus a contiguous `chunks_exact_mut +
  copy_from_slice` filler for RUN chunks (`decoder::fill_run`). The
  capacity-truncation guard the round-162 fuzz harness added moves
  from a runtime cap on the eager `Vec::with_capacity` reservation
  to a pre-allocation check that rejects the request before
  allocating; legitimate-input behaviour is unchanged. Decode
  throughput on the round-175 profile baseline (Apple-silicon dev
  box, release build):
  | Scenario                          | Decode r175 | Decode r183 | Speedup |
  | --------------------------------- | ----------- | ----------- | ------- |
  | RGBA 320×240 gradient             | 708 MiB/s   |   899 MiB/s | 1.27×   |
  | RGB24 640×480 gradient            | 521 MiB/s   |   616 MiB/s | 1.18×   |
  | RGBA 512×512 solid-RUN            | 1.54 GiB/s  | 37.4 GiB/s  | 24.4×   |
  | RGBA 320×240 alpha-changing       | 1.36 GiB/s  | 3.14 GiB/s  | 2.31×   |
  | RGBA 320×240 8-colour INDEX cycle | 1.46 GiB/s  | 2.73 GiB/s  | 1.87×   |

  The solid-RUN row is the dramatic case: every chunk is a RUN, every
  RUN now lowers to a single `chunks_exact_mut + copy_from_slice`
  loop the autovectoriser turns into a wide-store memcpy. The encoder
  was not touched (its `Vec::push`-heavy chunk emitter is the target
  for a future encoder-side optimisation round). All 24 pre-existing
  unit tests + 5 fixture roundtrip tests + the doctest + the existing
  fuzz harness corpora pass unchanged; two new regression tests
  (`decoder_exact_size_buffer_run_only_stream` covering the RUN-only
  `fill_run` path across the 62-pixel cap boundary, and
  `decoder_exact_size_buffer_mixed_stream` covering DIFF / LUMA /
  RGB / RGBA / INDEX via `write_pixel`) lock in the new cursor-write
  contract.

### Added

- Round-175 profile driver under `examples/profile_qoi.rs` plus a
  baseline numbers document in `profile/README.md`. The driver is a
  flat measure-this-thing harness (single `Instant::now()` /
  `elapsed()` pair around a fixed iteration loop) covering the same
  five chunk-mix scenarios as the Criterion benches: RGBA 320×240
  gradient, RGB24 640×480 gradient, RGBA 512×512 solid-RUN, RGBA
  320×240 alpha-changing, and RGBA 320×240 8-colour INDEX cycle.
  Designed for `samply` / `cargo flamegraph` / `perf record` capture
  where Criterion's warm-up + sampling layers would otherwise bury
  the codec hot path. Apple-silicon baseline: decode 0.5–1.5 GiB/s,
  encode 0.4–2.2 GiB/s, roundtrip 0.2–0.9 GiB/s; the worst encode
  case (RGBA gradient with mixed DIFF/LUMA/RGB chunks at 612 MiB/s)
  identifies the priority-chain walk in `encode_qoi_full` as the
  target for any future encoder optimisation round. Run with
  `cargo run --release --example profile_qoi -- <mode> [<iters>]`;
  modes: `encode` / `decode` / `roundtrip` / `all`.
- Criterion benchmarks under `benches/` (`decode`, `encode`,
  `roundtrip`), five scenarios each: natural-image gradient (RGBA
  320×240 and RGB24 640×480), single-colour fill that's dominated by
  `QOI_OP_RUN`, alpha-changing worst case that's almost-all
  `QOI_OP_RGBA`, and an 8-colour cycle that exercises the
  `QOI_OP_INDEX` hot path. All inputs are synthesised on the fly —
  no committed fixture files. Round-156 baseline (Apple-silicon dev
  box): decode ~540 MiB/s gradient → ~1.5 GiB/s solid; encode ~640
  MiB/s gradient → ~2.1 GiB/s solid; full roundtrip ~335 MiB/s
  gradient → ~915 MiB/s solid. Run with
  `cargo bench -p oxideav-qoi --bench <name>`.
- `cargo-fuzz` harness under `fuzz/` with a `decode` target that feeds
  arbitrary bytes to `parse_qoi` and asserts it never panics / aborts /
  OOMs. Corpus seeded from the reference fixtures plus a huge-header
  regression seed. Daily `fuzz.yml` workflow runs it via the org
  reusable `crate-fuzz.yml` (30-minute budget).
- Second `cargo-fuzz` target `encode_roundtrip`: derives a small image
  header from the first 6 fuzz bytes (w / h clamped 1..=256, channels
  ∈ {3, 4}, colorspace ∈ {0, 1}), repeats the payload to fill
  `w * h * channels` pixel bytes, encodes via `encode_qoi_full`, and
  asserts `parse_qoi` returns the exact same `(w, h, channels,
  colorspace, pixels)`. The QOI spec is lossless — any drift between
  encoder chunk selection (RUN > INDEX > DIFF > LUMA > RGB / RGBA)
  and the decoder breaks this contract. 30-second local smoke run:
  33,637 iterations, ~1,000 exec/s, no crashes. Corpus seeded with
  five small inputs covering RUN-heavy, DIFF/LUMA, INDEX, single-pixel,
  and a clamped max-dim gradient.

### Fixed

- Decoder no longer aborts on a small file whose header claims a huge
  image (e.g. 65536×65536 ≈ 1 TB). The output buffer reservation was
  computed from the attacker-controlled `width * height * channels` and
  handed straight to `Vec::with_capacity`, aborting the process. The
  reservation is now bounded by the maximum number of pixels the chunk
  stream can physically decode (`chunks.len() * 62`); an oversized
  header is rejected as a truncated stream. Found by the new fuzz
  harness. Regression test: `huge_header_does_not_over_allocate`.

## [0.1.1](https://github.com/OxideAV/oxideav-qoi/compare/v0.1.0...v0.1.1) - 2026-05-06

### Other

- drop stale REGISTRARS / with_all_features intra-doc links
- drop dead `linkme` dep
- re-export __oxideav_entry from registry sub-module
- auto-register via oxideav_core::register! macro (linkme distributed slice)
- unify entry point on register(&mut RuntimeContext) ([#502](https://github.com/OxideAV/oxideav-qoi/pull/502))
- add register_containers for .qoi extension lookup

## [0.0.2](https://github.com/OxideAV/oxideav-qoi/compare/v0.0.1...v0.0.2) - 2026-05-05

### Other

- clippy 1.95 — useless_vec → array literal in three RUN-chunk fixtures

### Added

- Initial release: pure-Rust QOI (Quite OK Image) reader and writer,
  clean-room from the one-page qoiformat.org specification.
- 14-byte header (`qoif` magic, BE width/height, channels, colorspace).
- All eight chunk encodings: `QOI_OP_RGB`, `QOI_OP_RGBA`,
  `QOI_OP_INDEX`, `QOI_OP_DIFF`, `QOI_OP_LUMA`, `QOI_OP_RUN`.
- 64-entry running pixel array indexed by
  `(R*3 + G*5 + B*7 + A*11) % 64`.
- 8-byte end marker `00 00 00 00 00 00 00 01`.
- Standalone `parse_qoi` / `encode_qoi` API plus crate-local
  `QoiImage` / `QoiChannels` / `QoiColorspace` / `QoiError` types.
- Default-on `registry` cargo feature wires up `Decoder` / `Encoder`
  trait impls against `oxideav-core`. Image-library consumers can
  build with `--no-default-features` for an `oxideav-core`-free build.
- `registry::register_containers(&mut ContainerRegistry)` registers
  the `.qoi` file extension against the container name `"qoi"` so
  cli-convert / pipeline output probing can resolve `.qoi` paths
  through the central registry instead of a hard-coded list.
