# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
