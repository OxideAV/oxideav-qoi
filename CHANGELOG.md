# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
