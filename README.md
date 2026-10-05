# oxideav-qoi

[![CI](https://github.com/OxideAV/oxideav-qoi/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-qoi/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-qoi.svg)](https://crates.io/crates/oxideav-qoi) [![docs.rs](https://docs.rs/oxideav-qoi/badge.svg)](https://docs.rs/oxideav-qoi) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust **QOI** (Quite OK Image) reader and writer for the
[`oxideav`](https://github.com/OxideAV/oxideav) framework. Clean-room
implementation of the one-page specification published at
[qoiformat.org](https://qoiformat.org/qoi-specification.pdf). Follows
the OxideAV image-crate API contract (`IMAGE_CRATE_API`), so it reads
like `oxideav-png` / `oxideav-webp` / every other OxideAV image crate.

## Standalone use

```toml
[dependencies]
oxideav-qoi = { version = "0.2", default-features = false }   # no oxideav-core
```

```rust
let bytes = std::fs::read("in.qoi")?;
if oxideav_qoi::probe(&bytes) {
    let info = oxideav_qoi::info(&bytes)?;          // header only: width, height, format, colour
    let img  = oxideav_qoi::decode(&bytes)?;        // QoiImage, native layout (Rgb24 or Rgba)
    let rgba: Vec<u8> = img.to_rgba8();             // tightly packed RGBA, 4 * width bytes per row
    let (w, h) = (img.width(), img.height());

    let opts = oxideav_qoi::EncodeOptions::default();
    let out: Vec<u8> = oxideav_qoi::encode_rgba8(w, h, &rgba, &opts)?;
    std::fs::write("out.qoi", out)?;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

| Item | Signature |
|---|---|
| `probe` | `fn(&[u8]) -> bool` — `qoif` magic sniff; total, allocation-free |
| `info` | `fn(&[u8]) -> Result<ImageInfo>` — 14-byte header only: `width`, `height`, `format`, `frames` (1), `has_alpha`, `color`, `has_icc` / `has_exif` / `has_xmp` (false), plus `channels` / `colorspace` |
| `decode` / `decode_with` | `fn(&[u8]) -> Result<QoiImage>`, `fn(&[u8], &DecodeOptions) -> Result<QoiImage>` — native layout, one tightly packed plane |
| `decode_rgb8` / `decode_rgba8` | `-> Result<RgbImage>` / `-> Result<RgbaImage>` (`{ width, height, data }`, 3 / 4 bytes per pixel) |
| `decode_from` | `fn<R: Read>(R) -> Result<QoiImage>` — reads to end (QOI needs the trailing end marker) |
| `encode` | `fn(&QoiImage, &EncodeOptions) -> Result<Vec<u8>>` |
| `encode_rgb8` / `encode_rgba8` | `fn(w, h, &[u8], &EncodeOptions) -> Result<Vec<u8>>` — `channels` 3 / 4 |
| `encode_to` | `fn<W: Write>(&QoiImage, &EncodeOptions, W) -> Result<()>` |

`QoiImage { width, height, format: PixelFormat, planes: Vec<Plane>,
color: ColorInfo, metadata: Metadata }` with `new` / `packed` /
`from_rgb8` / `from_rgba8` (geometry-validated, `Result`), `width()` /
`height()` / `format()` / `stride()` / `as_bytes()` / `into_raw()` /
`to_rgb8()` / `to_rgba8()` / `channels()` / `colorspace()`. QOI has no
palette, so there is no `palette` field. `Error` = `QoiError {
InvalidData, Unsupported, LimitExceeded, Io(std::io::Error) }`.

The pre-contract entry points (`parse_qoi`, `parse_qoi_header`,
`encode_qoi`, `encode_qoi_full`, `QoiEncoderOptions`) remain for one
release as `#[deprecated]` wrappers with byte-identical output.

## Framework use

The default `registry` feature pulls in `oxideav-core`:

```rust
let mut ctx = oxideav_core::RuntimeContext::new();
oxideav_qoi::register(&mut ctx);          // codec "qoi" + the "qoi" container (demuxer, muxer, probe, .qoi)
// or: register_codecs(&mut ctx.codecs) / register_containers(&mut ctx.containers)
# let params = oxideav_core::CodecParameters::video(oxideav_core::CodecId::new("qoi"));
let dec = oxideav_qoi::make_decoder(&params)?;   // oxideav_core::Decoder
let enc = oxideav_qoi::make_encoder(&params)?;   // oxideav_core::Encoder
# Ok::<(), oxideav_core::Error>(())
```

`From<QoiImage> for VideoFrame` (one packed plane plus the colour-signal
side-channel) and `QoiImage::from_video_frame(&VideoFrame,
&CodecParameters)` / `TryFrom<(&VideoFrame, &CodecParameters)>` bridge
the two layers; `QoiPixelFormat` ↔ `oxideav_core::PixelFormat` map 1:1
by name. The framework `Decoder` / `Encoder` are thin adapters over the
standalone `decode` / `encode`: the decoder threads the `Packet`'s `pts`
onto the frame, attaches the colour signal, overrides `reset()` (a
reused decoder returns to `NeedMore`, not `Eof`) and the zero-copy
`receive_arena_frame` (true `(width, height, pixel_format)` in the
`FrameHeader`); the encoder requires `width` / `height` /
`pixel_format` (`Rgb24` / `Rgba`) on `CodecParameters`, repacks padded
planes, marks every packet a keyframe, and honours the `colorspace`
option (`"0"` / `"srgb"` / `"1"` / `"linear"`, discoverable through
`CodecRegistry::encoder_options_schema`; absent → follows the frame's
colour signal). Unsupported pixel formats are `Error::Unsupported`.

The `qoi` container (`oxideav_qoi::container`) lets the framework — and
`oxideav-image` — open and write QOI files through the registry: the
probe matches the `qoif` magic (or the `.qoi` hint), the demuxer reads
the 14-byte header and declares one video stream with `width` /
`height`, the native `pixel_format` (`Rgb24` / `Rgba`) and the colour
signal the `colorspace` byte defines (always stamped — the format always
signals it), then emits the whole file as one packet (`pts 0`, time base
`1/1`); the muxer writes the encoder's single packet through and refuses
a second one with `Error::Unsupported` (a QOI file holds one image).

## Supported layouts

| Native layout | Header `channels` | Decode | Encode | `to_rgb8` / `to_rgba8` |
|---|---|---|---|---|
| `Rgb24` (3 bytes/pixel) | 3 | ✅ | ✅ | copy / alpha `255` |
| `Rgba` (4 bytes/pixel) | 4 | ✅ | ✅ | alpha dropped / copy |

That is the whole format: 8-bit RGB(A), one packed plane, no palette,
no 16-bit, no grayscale, no animation. `encode` writes the image as
given (a plane with row padding is repacked; QOI streams are tightly
packed); `encode_rgb8` → `channels = 3`, `encode_rgba8` → `channels =
4`. There is no 8-bit RGB(A) input QOI cannot represent, so
`Error::Unsupported` is reserved for geometry overflowing `usize`.

## Options

`DecodeOptions { max_width, max_height, max_pixels, max_bytes: Option<_>,
strict }` — `None` lifts a limit; the default caps the decoded plane at
1 GiB and leaves dimensions unlimited; `unlimited()` lifts everything.
`strict` is accepted for contract uniformity and has no effect: QOI has
no advisory rules, every check (magic, field ranges, exact pixel count,
chunk bounds, end marker) is mandatory in both modes.

`EncodeOptions { colorspace: Option<QoiColorspace> }` — QOI's only knob,
the informational header byte. `None` (default) derives it from the
image's `color` (transfer 8 → `AllLinear`, anything else →
`SrgbWithLinearAlpha`; the raw `encode_rgb8` / `encode_rgba8` paths
carry no colour and write sRGB); `Some(cs)` forces it. The byte never
changes pixel bytes.

## Metadata and colour

QOI carries no ICC profile, Exif, XMP or gamma: `Metadata` is always
empty on a decoded image and the encoder ignores (cannot carry) whatever
a caller sets. The one piece of colour signalling is the header's
colorspace byte, mapped two ways by `QoiColorspace::color_info` /
`from_color_info`:

| Byte | Meaning (spec) | `ColorInfo { range, primaries, transfer, matrix }` |
|---|---|---|
| 0 | sRGB with linear alpha | `Full`, 1 (BT.709 / sRGB), 13 (sRGB), 0 (identity / RGB) |
| 1 | all channels linear | `Full`, 1, 8 (linear), 0 |

The spec says nothing about primaries; BT.709 / sRGB is the convention
this crate documents for both bytes (the byte only distinguishes the
transfer). `decode(encode(img)) == img` — planes, colour, metadata —
is pinned for both bytes and both layouts.

## Limits

Every `DecodeOptions` limit is checked against the 14-byte header before
the pixel buffer is allocated (`Error::LimitExceeded`). Independently of
the options, the decoder bounds its allocation by what the chunk stream
can physically produce (at most 62 pixels per chunk byte), so a 30-byte
file claiming a 65536×65536 image is rejected as truncated instead of
asking the allocator for 16 GiB. `probe` and `info` never allocate.

**Decode speed** (release build, Apple M4 Max, single thread,
`cargo run --release --example profile_qoi -- 12mp`): one photo-like
4000×3000 RGBA frame (12 MP, 45.8 MiB raw, 13.4 MiB encoded) decodes
through `decode` in **53.5–54.5 ms** — about **223 MP/s**, ~850 MiB/s of
raw RGBA produced. The `profile/` baseline has the per-chunk-mix rows.

## Format coverage

QOI is a small, lossless RGB(A) image format. The whole specification
fits on one printed page; this crate covers all of it:

| Element            | What it is                                   |
| ------------------ | -------------------------------------------- |
| 14-byte header     | `qoif` magic, BE width/height u32, channels (3 or 4), colorspace (0 or 1) |
| `QOI_OP_RGB`       | Tag `0xfe` + 3 raw RGB bytes (alpha unchanged) |
| `QOI_OP_RGBA`      | Tag `0xff` + 4 raw RGBA bytes                |
| `QOI_OP_INDEX`     | 2-bit tag `00` + 6-bit index into a 64-entry running pixel array |
| `QOI_OP_DIFF`      | 2-bit tag `01` + three 2-bit channel deltas, each biased by 2 (range −2..+1) |
| `QOI_OP_LUMA`      | 2-bit tag `10` + 6-bit `dg` (biased 32) + 4-bit `dr-dg` / `db-dg` (biased 8) |
| `QOI_OP_RUN`       | 2-bit tag `11` + 6-bit `(run-1)` for runs of 1..62 (62/63 reserved for the RGB/RGBA tags) |
| Index hash         | `(R*3 + G*5 + B*7 + A*11) % 64`              |
| End marker         | `00 00 00 00 00 00 00 01` (8 bytes)          |

The encoder always picks the smallest legal chunk for each pixel using
the spec priority order (RUN > INDEX > DIFF > LUMA > RGB / RGBA), so
output sizes match the canonical encoder byte-for-byte.

## Depth APIs

Beyond the contract floor the crate keeps its QOI-specific surface: the
raw header view `QoiHeader` / `QoiChannels` / `QoiColorspace`, the
chunk-level walker below, and the buffer-reuse `_into` entry points.

### Chunk-stream iteration (depth API)

`iter_ops` / `iter_ops_strict` walk the post-header chunk stream and
yield one typed `QoiOp` per chunk — `Rgb`, `Rgba`, `Index`, `Diff`,
`Luma`, `Run` — without materialising a pixel buffer. The iterator is
stateless with respect to the running pixel array and `prev` pixel;
delta fields are the un-biased signed values the decoder would apply.
Useful for chunk-shape histograms, debug pretty-printers, and
encoder-priority regression checks. `iter_ops_strict` collects into a
`Vec<QoiOp>` and surfaces mid-chunk truncation as `Err(InvalidData)`;
the non-strict variant yields a final
`QoiOp::Truncated { tag, missing_body_bytes }` and stops.

`QoiOp` carries typed-introspection methods: `tag()` reconstructs the
exact leading chunk byte, `body_len()` / `encoded_len()` give the
post-tag body width (0/1/3/4) and total chunk width (1/2/4/5), and
`is_truncated()` tests the `Truncated` sentinel.
`QoiOp::write_to(&mut Vec<u8>)` is the byte-level inverse of the
`iter_ops` walker — it appends the full on-wire chunk, so
`iter_ops(input)` → `write_to` → `iter_ops` round-trips an in-spec
chunk stream byte-for-byte. The bias arithmetic is total over the
`pub` field space, so out-of-spec field values yield a well-defined
byte sequence rather than panicking.

`qoi_hash([r, g, b, a]) -> u8` is the public typed form of the spec's
running-pixel-array bucket selector `(R*3 + G*5 + B*7 + A*11) % 64`,
with the multiply done in `u32` (so `(0,0,0,255)` hashes to `53`).

### Buffer-reuse `_into` entry points

`encode_qoi_into`, `encode_qoi_full_into`, and `parse_qoi_into` take a
caller-owned `&mut Vec<u8>` instead of returning a fresh `Vec`. The
buffer is cleared on entry, resized to the worst-case (encode) or exact
(decode) byte count, and truncated to the actual size before return;
the retained capacity covers the worst case seen so far, so a tight
loop over images of similar size allocates once and reuses thereafter.
`parse_qoi_into` returns the parsed `QoiHeader` and applies no
`DecodeOptions` limits (only the physical chunk-stream guard). Same
chunk priority chain, same error variants, same byte-for-byte output as
the contract `encode*` / `decode` entry points; the raw `(w, h,
channels, colorspace, &[u8])` arguments are asserted (panic on misuse),
unlike the `Result`-returning contract paths.

## Benchmarks

Criterion benchmarks under `benches/` cover the encoder and decoder
hot paths plus the full encode→decode roundtrip. Inputs are
synthesised on the fly with the public encoder API (no committed
fixtures). Five scenarios cover the op-mix surface (natural-image RGBA
gradient, RGB24 VGA gradient, single-colour RUN-dominated fill,
per-pixel alpha-changing RGBA worst case, 8-colour INDEX cycle). A
`reuse` bench A/Bs the `_into` surface against the allocating wrappers.
An `op_walk` bench measures the streaming chunk-walk decode path
(`iter_ops` / `iter_ops_strict`) — typed-`QoiOp` dispatch without
materialising a pixel buffer — across the same five shapes, pairing the
allocation-free lazy walk against the eager `Vec`-materialising variant.
An `op_write` bench measures the inverse `QoiOp::write_to`
re-serialization path (pre-collected ops re-emitted to bytes) on those
same five shapes, pairing a reused output buffer against a fresh one.

```sh
cargo bench -p oxideav-qoi --bench <decode|encode|roundtrip|reuse|op_walk|op_write>
```

## Profiling

A standalone profile driver lives at `examples/profile_qoi.rs` with
baseline numbers + flamegraph recipe in `profile/README.md`. The five
scenarios mirror the Criterion benches; the driver runs a flat
`Instant::now()` / `elapsed()` loop so a `samply` or `cargo flamegraph`
capture shows the codec hot path without sampling-framework noise.

```sh
cargo run --release --example profile_qoi -- all
cargo run --release --example profile_qoi -- encode 5000
```

## Fuzzing

Eight [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) targets
live under `fuzz/`:

* `demux` — the framework path: the bytes as a file into the container
  demuxer, the packet through the registered decoder (under a 1 Mpx
  `DecoderLimits` budget), then back through the muxer.
* `decode` — feeds arbitrary bytes to the whole contract decode
  surface (`probe`, `info`, `decode`, `decode_with` with a tight
  `max_pixels`, `decode_rgb8`, `decode_rgba8`, `decode_from`),
  asserting every call returns rather than panicking or OOMing, that
  the paths agree with each other whenever they accept the input, and
  that every accepted image survives `decode(encode(img)) == img`.
* `encode_roundtrip` — derives a small image from the fuzz bytes,
  encodes, and asserts `decode` recovers the exact input (QOI is
  lossless).
* `chunk_walk` — structure-aware decoder target: a spec-valid header is
  synthesised around the fuzzer's chunk bytes so the decoder reaches
  the per-op decode paths on nearly every iteration.
* `op_iter` — structure-aware harness for the stream-level chunk
  iterator (`iter_ops` / `iter_ops_strict`), asserting the
  `encoded_len() == 1 + body_len()` width identity, the `tag()`
  reconstruction, exact consumed-byte accounting, and strict/non-strict
  agreement at the truncation boundary.
* `op_write` — structure-aware harness for `QoiOp::write_to`, asserting
  each `write_to` appends exactly `encoded_len()` bytes and the
  `iter_ops` → `write_to` → `iter_ops` round-trip identity.
* `trait_decode` — structure-aware harness for the framework-side
  `oxideav_core::Decoder` trait path (distinct from the standalone
  `decode` the other decode targets hit): a spec-valid header is
  synthesised around the fuzzer's chunk bytes, fed to the decoder as a
  `Packet`, and both `receive_frame` (heap `VideoFrame`) and
  `receive_arena_frame` (zero-copy arena `Frame`) are driven — asserting
  neither panics and that, when both succeed, they agree on the decoded
  pixel bytes, the plane stride, and the arena `FrameHeader`'s true
  `(width, height, pixel_format)`.
* `into_equiv` — differential harness for the caller-owned
  buffer-reuse `_into` API: the same synthesised header + chunk stream
  is decoded through both `decode` and `parse_qoi_into` (into a
  persistent, pre-dirtied, reused buffer) and asserted to agree on
  accept/reject, pixels, and header; when the decode succeeds the
  recovered pixels are re-encoded through both `encode_rgb8` /
  `encode_rgba8` and `encode_qoi_full_into` and asserted byte-identical. The reuse buffers
  persist across iterations (`thread_local`), so the `_into` path
  continuously sees a previous, differently-sized image — the
  shrinking-reuse stale-tail surface, driven by attacker-chosen sizes
  and op mixes.

```sh
cargo +nightly fuzz run decode
cargo +nightly fuzz run encode_roundtrip
cargo +nightly fuzz run chunk_walk
cargo +nightly fuzz run op_iter
cargo +nightly fuzz run op_write
cargo +nightly fuzz run trait_decode
cargo +nightly fuzz run into_equiv
```

The `decode` corpus is seeded from the byte-exact fixtures in
`tests/fixtures/` plus a regression seed for a header claiming a ~1 TB
image — the decoder bounds its output reservation by what the chunk
stream can physically decode, so an oversized header is rejected as a
truncated stream rather than crashing the process. The daily `fuzz.yml`
workflow runs all targets through the org reusable `crate-fuzz.yml`.

## Property tests

`tests/contract_roundtrip.rs` sweeps the contract surface itself: 600
pseudo-random images (five content generators × `Rgb24` / `Rgba` ×
both colorspace bytes × 1..=64 geometry, one in four with row padding)
pin the exact identity `decode(encode(img)) == img` — planes, colour,
metadata — and that `info`, `decode_rgb8` / `decode_rgba8`,
`encode_rgb8` / `encode_rgba8`, `encode_to` / `decode_from` and `probe`
all agree with it; a second sweep pins the deprecated pre-contract
wrappers byte-identical to the contract path.

`tests/property_sweep.rs` is a deterministic property-style sweep that
complements the fuzz harness with hundreds of pseudo-random
`(width, height, channels, colorspace, pixels)` triples per scenario.
Six semantic invariants are asserted per case: lossless roundtrip,
worst-case size bound, header/end-marker echo, encoder determinism,
the tighter solid-fill bound, and idempotent re-encode. Five input
generators exercise different paths through the chunk-priority chain. A
self-contained xorshift32 PRNG is seeded per scenario so any failure is
reproducible (no `proptest` / `quickcheck` dev-dep).

```sh
cargo test -p oxideav-qoi --test property_sweep
```

`tests/canonical_encoding.rs` adds the complementary
*chunk-minimality* class of invariant: the `property_sweep` checks all
hold for any decodable stream, so they cannot catch the encoder picking
a legal-but-oversized chunk (an `RGB` where a `DIFF` fit, an `INDEX`
where a `RUN` applied) — that output still decodes pixel-exact. The
canonical sweep walks the encoder's bytes with `iter_ops`, re-derives
the decoder running state (`prev` pixel + 64-slot index) in lockstep,
and asserts every emitted chunk is the highest-priority legal choice on
the spec ladder (`RUN > INDEX > DIFF > LUMA > RGB / RGBA`). It also pins
the spec's two named canonical-form rules: intermediate runs are maxed
at 62, and no two consecutive `QOI_OP_INDEX` chunks resolve to the same
slot. Same five generators × 200 seeds plus hand-built edge cases.

```sh
cargo test -p oxideav-qoi --test canonical_encoding
```

`tests/decoder_boundary.rs` pins the spec's *named worked examples* and
init-state subtleties directly against the decoder — no encoder on the
assertion path, so a shared encoder/decoder bug can't mask a regression.
Hand-assembled single-chunk streams pin: the `QOI_OP_DIFF` wraparound
(`1 - 2 = 255`, `255 + 1 = 0`) and full `-2..=1` delta sweep; the
`QOI_OP_LUMA` wraparound (`10 - 13 = 253`, `250 + 7 = 1`) and `dg` /
`dr-dg` / `db-dg` endpoint sweep; 8-bit-tag precedence (`0xfe` / `0xff`
are never decoded as a `RUN`) and the `RUN` length ceiling of 62; and
the running-array zero-initialisation — an `INDEX` into an unwritten
slot decodes `(0,0,0,0)` (alpha 0), *distinct* from the initial previous
pixel `(0,0,0,255)`, including the slot-53 trap where the initial prev's
hash slot is still empty until a pixel is actually emitted into it.

```sh
cargo test -p oxideav-qoi --test decoder_boundary
```

`tests/decoder_rejects.rs` is the complementary *negative* class: every
other suite asserts that well-formed streams decode, but none feed the
decoder a malformed stream and assert it is rejected. The spec mandates
a precise set of structural well-formedness conditions; these tests pin
each one directly against `decode` (no encoder on the assertion path).
Covered rejections: bad / partial `qoif` magic (every byte position),
input shorter than the 14-byte header, header-only input with no end
marker, illegal `channels` (full u8 sweep — only 3 / 4 accepted),
illegal `colorspace` (full u8 sweep — only 0 / 1 accepted), zero
width / height / 0×0, wrong or truncated 8-byte end marker (every byte
position), truncated `RGB` / `RGBA` / `LUMA` chunk bodies, a stream that
ends before `width * height` pixels are covered, a `RUN` that overshoots
the declared image size, a trailing chunk or stray byte after the image
is complete, and the oversized-header guard (a 65536×65536 header with a
one-pixel body is rejected as truncated rather than triggering a
multi-gigabyte allocation — the default 1 GiB `max_bytes` cap refuses
it first, and with the limits lifted the physical guard still does).
Each test also cross-checks that the cheap `info` probe agrees on the
header-level rejections while ignoring chunk-stream-level ones.

```sh
cargo test -p oxideav-qoi --test decoder_rejects
```

`tests/into_equivalence.rs` pins the caller-owned buffer-reuse `_into`
API (`encode_qoi_into`, `encode_qoi_full_into`, `parse_qoi_into`) —
until now covered only by the `reuse` bench, which measures throughput
rather than bytes. Across the same five op-mix generators it asserts:
byte-for-byte equivalence with the allocating contract entry points
into a fresh buffer; dirty-buffer reuse (a buffer pre-filled with `0xAA` at an
unrelated capacity yields the identical result); shrinking reuse (a
large image then a small one into the *same* buffer leaves no stale
tail past the truncation point); reuse after a rejected decode (a
malformed stream must not poison the next successful decode); and the
capacity-amortisation promise (after the worst-case call, a run of
same-or-smaller images never re-grows the allocation). The `into_equiv`
fuzz target drives the same differential on attacker-chosen inputs.

```sh
cargo test -p oxideav-qoi --test into_equivalence
```

## License

MIT — see [LICENSE](LICENSE).
