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

## Fuzzing

The decoder has a [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)
harness under `fuzz/`. The `decode` target feeds arbitrary bytes to
`parse_qoi` and asserts it always returns a `Result` rather than
panicking, aborting, or OOMing:

```sh
cargo +nightly fuzz run decode
```

The corpus is seeded from the byte-exact reference fixtures in
`tests/fixtures/` plus a regression seed for a small file whose header
claims a ~1 TB image. That class of input is the one crash the harness
found: the old decoder reserved the output buffer from the header's
attacker-controlled `width * height * channels` and aborted on the
allocation. The reservation is now bounded by what the chunk stream can
physically decode (`chunks.len() * 62` pixels), so an oversized header
is rejected as a truncated stream instead of crashing the process. A
daily `fuzz.yml` workflow runs the target through the org reusable
`crate-fuzz.yml` for a 30-minute budget.

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
