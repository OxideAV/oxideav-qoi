# oxideav-qoi

Pure-Rust **QOI** (Quite OK Image) reader and writer for the
[`oxideav`](https://github.com/OxideAV/oxideav) framework. Clean-room
implementation of the one-page specification published at
[qoiformat.org](https://qoiformat.org/qoi-specification.pdf).

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

## API

```rust
use oxideav_qoi::{
    parse_qoi, parse_qoi_header, parse_qoi_into,
    encode_qoi, encode_qoi_into,
    iter_ops, iter_ops_strict, QoiOp, qoi_hash,
    QoiImage, QoiChannels,
};

// Cheap header-only probe — no chunk walk, no pixel buffer allocated.
let hdr = parse_qoi_header(&qoi_bytes)?;
println!("{}x{} {:?}", hdr.width, hdr.height, hdr.channels);

// Decode the full file (allocates a fresh pixel buffer).
let img: QoiImage = parse_qoi(&qoi_bytes)?;

// Re-encode (round-trip; allocates a fresh output buffer).
let bytes: Vec<u8> = encode_qoi(
    img.width,
    img.height,
    img.channels as u8,           // 3 or 4
    &img.pixels,                  // tightly packed RGB or RGBA
);

// Buffer-reuse variants for tight encode/decode loops.
let mut enc_buf: Vec<u8> = Vec::new();
let mut dec_buf: Vec<u8> = Vec::new();
encode_qoi_into(&mut enc_buf, img.width, img.height, 4, &img.pixels);
let hdr = parse_qoi_into(&enc_buf, &mut dec_buf)?;
```

`QoiImage` carries `width`, `height`, `channels`, `colorspace`, and a
flat `pixels: Vec<u8>` (RGB or RGBA, no row padding). The encoder takes
plain `(w, h, channels, &[u8])` so callers need not construct a
`QoiImage` first. `QoiHeader` is the same metadata without the pixel
buffer — returned by `parse_qoi_header`, which inspects only the
14-byte header (accepts inputs as short as 14 bytes; does not walk the
chunk stream or check the end marker).

### Chunk-stream iteration

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
`parse_qoi_into` returns the parsed `QoiHeader`. Same chunk priority
chain, same error variants, same byte-for-byte output as the
allocating wrappers.

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

```sh
cargo bench -p oxideav-qoi --bench <decode|encode|roundtrip|reuse|op_walk>
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

Five [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) targets
live under `fuzz/`:

* `decode` — feeds arbitrary bytes to `parse_qoi`, asserting the
  decoder always returns a `Result` rather than panicking or OOMing.
* `encode_roundtrip` — derives a small image from the fuzz bytes,
  encodes, and asserts `parse_qoi` recovers the exact input (QOI is
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

```sh
cargo +nightly fuzz run decode
cargo +nightly fuzz run encode_roundtrip
cargo +nightly fuzz run chunk_walk
cargo +nightly fuzz run op_iter
cargo +nightly fuzz run op_write
```

The `decode` corpus is seeded from the byte-exact fixtures in
`tests/fixtures/` plus a regression seed for a header claiming a ~1 TB
image — the decoder bounds its output reservation by what the chunk
stream can physically decode, so an oversized header is rejected as a
truncated stream rather than crashing the process. The daily `fuzz.yml`
workflow runs all targets through the org reusable `crate-fuzz.yml`.

## Property tests

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

## Standalone vs registry-integrated

The default `registry` Cargo feature pulls in `oxideav-core` and
exposes the framework `Decoder` / `Encoder` trait surface plus a
`registry::register` entry point. `registry::register_containers` wires
the `.qoi` file extension into a `ContainerRegistry`. Disable the
feature (`default-features = false`) for an `oxideav-core`-free build
that still exposes the standalone `parse_qoi` / `encode_qoi` API plus
crate-local `QoiImage` / `QoiChannels` / `QoiColorspace` / `QoiError`
types.

## License

MIT — see [LICENSE](LICENSE).
