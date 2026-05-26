# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
