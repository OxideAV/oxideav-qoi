# oxideav-qoi

Pure-Rust **QOI** (Quite OK Image) reader and writer for the
[`oxideav`](https://github.com/OxideAV/oxideav) framework. Clean-room
implementation of the one-page specification published at
[qoiformat.org](https://qoiformat.org/qoi-specification.pdf).

## Format coverage

QOI is a small, lossless RGB(A) image format invented by Dominic
Szablewski. The whole specification fits on one printed page; this
crate covers all of it:

| Element            | What it is                                   |
| ------------------ | -------------------------------------------- |
| 14-byte header     | `qoif` magic, BE width/height u32, channels (3 or 4), colorspace (0 or 1) |
| `QOI_OP_RGB`       | Tag `0xfe` + 3 raw RGB bytes (alpha unchanged) |
| `QOI_OP_RGBA`      | Tag `0xff` + 4 raw RGBA bytes                |
| `QOI_OP_INDEX`     | 2-bit tag `00` + 6-bit index into a 64-entry running pixel array |
| `QOI_OP_DIFF`      | 2-bit tag `01` + three 2-bit channel deltas, each biased by 2  (range −2..+1) |
| `QOI_OP_LUMA`      | 2-bit tag `10` + 6-bit `dg` (biased 32) + 4-bit `dr-dg` / `db-dg` (biased 8) |
| `QOI_OP_RUN`       | 2-bit tag `11` + 6-bit `(run-1)` for runs of 1..62 (62/63 are reserved for the RGB/RGBA tags) |
| Index hash         | `(R*3 + G*5 + B*7 + A*11) % 64`              |
| End marker         | `00 00 00 00 00 00 00 01` (8 bytes)          |

Encoder always picks the smallest legal chunk for each pixel using the
priority order set out in the spec (RUN > INDEX > DIFF > LUMA > RGB /
RGBA), so output sizes match the reference encoder byte-for-byte on
files we have access to.

## API

```rust
use oxideav_qoi::{parse_qoi, encode_qoi, QoiImage, QoiChannels};

// Decode a complete QOI file.
let img: QoiImage = parse_qoi(&qoi_bytes)?;
assert!(matches!(img.channels, QoiChannels::Rgba | QoiChannels::Rgb));

// Re-encode (round-trip).
let bytes: Vec<u8> = encode_qoi(
    img.width,
    img.height,
    img.channels as u8,           // 3 or 4
    &img.pixels,                  // tightly packed RGB or RGBA
);
```

`QoiImage` carries `width`, `height`, `channels`, `colorspace`, and a
flat `pixels: Vec<u8>` (RGB or RGBA, no row padding). The encoder takes
plain `(w, h, channels, &[u8])` so callers don't need to construct an
`QoiImage` first.

## Benchmarks

Criterion benchmarks under `benches/` cover the encoder and decoder
hot paths plus the full encode→decode roundtrip. Each bench is
self-contained — inputs are synthesised on the fly with the public
encoder API, no committed fixture files. Five scenarios cover the
op-mix surface:

* a natural-image RGBA gradient with light xorshift noise (mixed
  DIFF / LUMA / RGB / INDEX),
* a larger RGB24 VGA gradient (alpha-unchanged path),
* a single-colour 512×512 RGBA fill (`QOI_OP_RUN` dominated),
* a per-pixel-changing-alpha worst case (`QOI_OP_RGBA` dominated),
* an 8-colour cycle (`QOI_OP_INDEX` hot path).

Round-183 baseline on an Apple-silicon dev box (post-decoder-exact-
size-buffer refactor — see `profile/README.md` "Round-183 delta" for
the side-by-side):

| Scenario                          | Decode      | Encode      | Roundtrip   |
| --------------------------------- | ----------- | ----------- | ----------- |
| RGBA 320×240 gradient             |   899 MiB/s |   628 MiB/s |   363 MiB/s |
| RGB24 640×480 gradient            |   616 MiB/s |   431 MiB/s |   256 MiB/s |
| RGBA 512×512 solid (RUN-heavy)    |  37.4 GiB/s |  2.10 GiB/s |  2.02 GiB/s |
| RGBA 320×240 alpha-changing       |  3.14 GiB/s |  1.03 GiB/s |   798 MiB/s |
| RGBA 320×240 8-colour INDEX cycle |  2.73 GiB/s |  1.98 GiB/s |  1.17 GiB/s |

Run with `cargo bench -p oxideav-qoi --bench <decode|encode|roundtrip>`.

## Profiling

A round-175 standalone profile driver lives at
`examples/profile_qoi.rs` with the baseline numbers + flamegraph
recipe written up in `profile/README.md`. The five scenarios mirror
the Criterion benches byte-for-byte; the driver runs a flat
`Instant::now()` / `elapsed()` loop (no Criterion warm-up / sampling
layers) so a `samply` or `cargo flamegraph` capture shows the codec
hot path without sampling-framework noise. Round 183 refreshed the
decode column after replacing the per-pixel `Vec::push` output writer
with an exact-size `vec![0; n]` buffer + slice-cursor write; the
solid-RUN row now hits ~37 GiB/s as the inner write lowers to a wide
memcpy. The worst encode scenario (RGBA 320×240 gradient at ~625
MiB/s) is where the spec's chunk priority chain (RUN > INDEX > DIFF
> LUMA > RGB / RGBA) burns the most time per pixel — the target for
any future encoder-side optimisation round.

```sh
cargo run --release --example profile_qoi -- all
cargo run --release --example profile_qoi -- encode 5000
```

## Fuzzing

Two [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) targets
live under `fuzz/`:

* `decode` — feeds arbitrary bytes to `parse_qoi` and asserts the
  decoder always returns a `Result` rather than panicking, aborting,
  or OOMing.
* `encode_roundtrip` — derives a small image header from the first 6
  fuzz bytes (width / height each clamped to 1..=256, channels
  ∈ {3, 4}, colorspace ∈ {0, 1}), repeats the remaining payload to
  fill `w * h * channels` pixel bytes, calls `encode_qoi_full`, and
  asserts `parse_qoi` returns the exact same `(width, height,
  channels, colorspace, pixels)`. The QOI spec is lossless, so this
  contract must hold for every well-formed input — any drift between
  the encoder's chunk-selection priority chain (RUN > INDEX > DIFF >
  LUMA > RGB / RGBA) and the decoder's chunk walker breaks it.

```sh
cargo +nightly fuzz run decode
cargo +nightly fuzz run encode_roundtrip
```

The `decode` corpus is seeded from the byte-exact reference fixtures
in `tests/fixtures/` plus a regression seed for a small file whose
header claims a ~1 TB image. That class of input is the one crash the
harness found: the old decoder reserved the output buffer from the
header's attacker-controlled `width * height * channels` and aborted
on the allocation. The reservation is now bounded by what the chunk
stream can physically decode (`chunks.len() * 62` pixels), so an
oversized header is rejected as a truncated stream instead of
crashing the process. The `encode_roundtrip` corpus is seeded with
five small inputs covering RUN-heavy (solid 4×4 RGB), DIFF / LUMA
(2×2 RGBA), INDEX (8×8 RGBA cycle), single-pixel, and a clamped
max-dim gradient. A 30-second local smoke run reaches ~1,000
exec/s with no crashes; the daily `fuzz.yml` workflow runs both
targets through the org reusable `crate-fuzz.yml` for a 30-minute
budget each.

## Standalone vs registry-integrated

The crate's default `registry` Cargo feature pulls in `oxideav-core`
and exposes the framework `Decoder` / `Encoder` trait surface plus a
`registry::register` entry point. The sibling
`registry::register_containers` call wires the `.qoi` file extension
into a `ContainerRegistry` so cli-convert / pipeline output probing
can resolve `.qoi` paths through the central registry instead of a
hard-coded list. Disable the feature (`default-features = false`) for
an `oxideav-core`-free build that still exposes the standalone
`parse_qoi` / `encode_qoi` API plus crate-local `QoiImage` /
`QoiChannels` / `QoiColorspace` / `QoiError` types.

## License

MIT — see [LICENSE](LICENSE).
