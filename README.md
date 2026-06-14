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
use oxideav_qoi::{
    parse_qoi, parse_qoi_header, parse_qoi_into,
    encode_qoi, encode_qoi_into,
    iter_ops, iter_ops_strict, QoiOp, qoi_hash,
    QoiImage, QoiChannels,
};

// Cheap header-only probe — no chunk walk, no pixel buffer allocated.
// Useful for thumbnail-grid metadata + pre-size limit checks.
let hdr = parse_qoi_header(&qoi_bytes)?;
println!("{}x{} {:?}", hdr.width, hdr.height, hdr.channels);

// Decode the full file (allocates a fresh pixel buffer).
let img: QoiImage = parse_qoi(&qoi_bytes)?;
assert!(matches!(img.channels, QoiChannels::Rgba | QoiChannels::Rgb));

// Re-encode (round-trip; allocates a fresh output buffer).
let bytes: Vec<u8> = encode_qoi(
    img.width,
    img.height,
    img.channels as u8,           // 3 or 4
    &img.pixels,                  // tightly packed RGB or RGBA
);

// Buffer-reuse variants for tight encode/decode loops. The output
// `Vec<u8>` is cleared on entry, written to, and retained for the
// next call — so a batch converter or image server amortises the
// worst-case allocation across many images of similar dimensions.
let mut enc_buf: Vec<u8> = Vec::new();
let mut dec_buf: Vec<u8> = Vec::new();
encode_qoi_into(&mut enc_buf, img.width, img.height, 4, &img.pixels);
let hdr = parse_qoi_into(&enc_buf, &mut dec_buf)?;
assert_eq!(hdr.width, img.width);
```

`QoiImage` carries `width`, `height`, `channels`, `colorspace`, and a
flat `pixels: Vec<u8>` (RGB or RGBA, no row padding). The encoder takes
plain `(w, h, channels, &[u8])` so callers don't need to construct an
`QoiImage` first. `QoiHeader` is the same metadata tuple without the
pixel buffer — returned by `parse_qoi_header` for cases where the
caller only needs to know the on-disk dimensions / channel count, e.g.
to size a destination buffer or reject oversized inputs before
committing to a full decode. The probe inspects only the 14-byte
header; it accepts inputs as short as 14 bytes and does not walk the
chunk stream or check the end marker.

The `iter_ops` / `iter_ops_strict` entry points (round 237) walk the
post-header chunk stream and yield one typed `QoiOp` per chunk —
`Rgb { r, g, b }`, `Rgba { r, g, b, a }`, `Index { index }`,
`Diff { dr, dg, db }`, `Luma { dg, dr_dg, db_dg }`, `Run { length }` —
*without* materialising a pixel buffer. The iterator is stateless with
respect to the running pixel array and `prev` pixel; delta fields are
the un-biased signed values the decoder would apply. Useful for chunk-
shape histograms (compression diagnostics), debug pretty-printers, and
encoder-priority-chain regression checks. `iter_ops_strict` collects
into a `Vec<QoiOp>` and surfaces mid-chunk truncation as an
`Err(InvalidData)`; the non-strict variant yields a final
`QoiOp::Truncated { tag, missing_body_bytes }` and stops.

`QoiOp` carries (round 267) typed-introspection methods that round-
trip each op back to its on-wire shape without re-encoding the whole
chunk: `tag()` reconstructs the exact leading chunk byte (the
bit-packed `OP_INDEX|index` / `OP_DIFF|(deltas+2)` /
`OP_LUMA|(dg+32)` / `OP_RUN|(length-1)` tag, or `0xFE` / `0xFF` for
RGB / RGBA, or the stored raw byte for `Truncated`); `body_len()` and
`encoded_len()` give the post-tag body width (0/1/3/4) and total
chunk width (1/2/4/5), so summing `encoded_len()` over an `iter_ops`
walk reproduces the chunk-section byte count; `is_truncated()` is a
convenience predicate for the `Truncated` sentinel.

`qoi_hash([r, g, b, a]) -> u8` (round 267) is the public typed form
of the spec's running-pixel-array bucket selector
`(R*3 + G*5 + B*7 + A*11) % 64`, with the multiply done in `u32` so
`(0,0,0,255)` hashes to `53` (not the `21` an 8-bit-wrapping multiply
gives). The crate's own decoder uses the same primitive, so a caller
building a `QOI_OP_INDEX`-coverage checker or stream validator can
confirm an `Index { index }` op points at the slot the pixel would
have hashed to, against the exact arithmetic the decoder agrees on.

The four `_into` entry points — `encode_qoi_into`,
`encode_qoi_full_into`, `parse_qoi_into` — take a caller-owned
`&mut Vec<u8>` instead of returning a fresh `Vec`. The buffer is
cleared on entry, resized to the worst-case (encode) or exact
(decode) byte count, and truncated to the actual size before
return; the retained `capacity()` covers the worst case seen so
far, so a tight encode/decode loop over images of similar size
allocates once and reuses thereafter. `parse_qoi_into` returns the
parsed `QoiHeader` so callers can size further downstream scratch
buffers without keeping the full `QoiImage` around. Same chunk
priority chain, same error variants, same byte-for-byte output as
the allocating wrappers — the only visible difference is whether
the backing allocation is caller-owned.

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

Round-282 baseline on an Apple-silicon dev box (post-encoder
run-arm wide scan-ahead — see `profile/README.md` "Round-282
delta" for the side-by-side; decode column unchanged since r183):

| Scenario                          | Decode      | Encode      | Roundtrip   |
| --------------------------------- | ----------- | ----------- | ----------- |
| RGBA 320×240 gradient             |  1.15 GiB/s |   997 MiB/s |   519 MiB/s |
| RGB24 640×480 gradient            |   671 MiB/s |   643 MiB/s |   328 MiB/s |
| RGBA 512×512 solid (RUN-heavy)    |  37.3 GiB/s |  24.8 GiB/s |  13.2 GiB/s |
| RGBA 320×240 alpha-changing       |  3.15 GiB/s |  2.30 GiB/s |  1.28 GiB/s |
| RGBA 320×240 8-colour INDEX cycle |  2.74 GiB/s |  2.66 GiB/s |  1.35 GiB/s |

A round-225 `reuse` bench A/Bs the new `_into` buffer-reuse
surface against the allocating wrappers on a tight 256-call inner
loop over a 64×64 RGBA image. On the Apple-silicon dev box the two
paths come out at parity within criterion's noise floor (encode
~1.78 ms / 256 calls = 7 µs/call; decode ~4.70 ms / 256 calls =
18 µs/call), reflecting that the macOS allocator's small/medium
block path is essentially free for these sizes. The `_into`
surface still earns its keep on systems with more expensive
malloc/free (some embedded targets, debug allocators, tracing
allocators) and on the rare-but-real workload of millions of
small QOI thumbnails — neither expressible in this microbench.

Run with `cargo bench -p oxideav-qoi --bench <decode|encode|roundtrip|reuse>`.

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
memcpy. Round 205 took the same cursor-write idea to the encoder:
pre-allocate the upper-bound `vec![0; 14 + n*5 + 8]` once, write
every chunk through `buf[out_pos] = …` + `buf[…].copy_from_slice`,
truncate down to the actual produced length at return. The
gradient row (the worst encode scenario, where the chunk priority
chain RUN > INDEX > DIFF > LUMA > RGB / RGBA runs the most picker
work per pixel) moved from 624 MiB/s → 930 MiB/s (1.49×); the
alpha-changing row (almost-every-pixel RGBA) moved from 1.06 GiB/s
→ 1.96 GiB/s (1.85×) as the per-pixel emit collapsed from five
`Vec::push` calls to a single 4-byte `copy_from_slice` after the
tag store. Round 231 split the encoder hot loop into two
channel-specialised inner functions (`encode_inner_rgba` for
4-channel input, `encode_inner_rgb` for 3-channel input): the
per-pixel `match qoi_channels { Rgb => …, Rgba => … }` that
assembled the 4-byte `cur` tuple is now hoisted out, and the
3-channel path no longer carries the RGBA emit arm or the
alpha-equality test at all (alpha is provably `0xff` for the
entire stream). RGB24 gradient encode moved from 593 MiB/s →
665 MiB/s (1.12×) and RGBA gradient from 891 MiB/s → 970 MiB/s
(1.09×); the RUN-dominated row also picked up 1.21× as the
chunk-byte index-load shape simplifies under the split. Round 282
restructured the encoder's run arm: instead of re-entering the
per-pixel loop (load + compare + run-counter bookkeeping + flush
test + a redundant per-pixel index store, each re-deriving the
hash) once per matching pixel, the encoder consumes the whole run
with an outlined wide scan-ahead — 16-byte (RGBA) / 12-byte (RGB)
4-pixel block compares with a scalar tail — and emits all the
`QOI_OP_RUN` chunks at once, byte-identical to the old
flush-at-62 emission. The RUN-dominated encode row moved from
2.68 GiB/s → 24.8 GiB/s (~9.2×) and the RGBA gradient picked up
~7% from the run state (and its pending-flush test) dropping out
of the non-run fall-through path.

```sh
cargo run --release --example profile_qoi -- all
cargo run --release --example profile_qoi -- encode 5000
```

## Fuzzing

Four [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) targets
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
* `chunk_walk` — structure-aware decoder target. The first 6 fuzz
  bytes pick a spec-valid header shape (width / height clamped to
  1..=64, channels ∈ {3, 4}, colorspace ∈ {0, 1}); the rest become
  the chunk stream, wrapped with a correct 14-byte header and a
  trailing 8-byte end marker. Because the synthesized header always
  passes the field gate, the decoder reaches the chunk walk on
  (nearly) every iteration, concentrating coverage on the six per-op
  decode paths (`QOI_OP_RGB` / `RGBA` / `INDEX` / `DIFF` / `LUMA` /
  `RUN`) and the truncation / overrun guards between them. The only
  contract asserted is that `parse_qoi` returns — never a panic.
* `op_iter` — structure-aware harness for the *stream-level chunk
  iterator* (`iter_ops` / `iter_ops_strict`), a decode path distinct
  from `parse_qoi`: it walks the chunk stream into typed `QoiOp`s
  without materialising a pixel buffer. Same header-synthesis trick as
  `chunk_walk` (spec-valid 14-byte header + 8-byte end marker wrapped
  around the fuzzer's chunk bytes) so the walker clears the header gate
  on nearly every iteration. Beyond "never panics" it asserts the
  `encoded_len() == 1 + body_len()` width identity, that `tag()`
  reconstructs the exact leading chunk byte for every non-truncated op,
  exact consumed-byte accounting against the chunk-section length, and
  that `iter_ops_strict` agrees with the non-strict walk on the
  truncation boundary. Building this target surfaced — and the round
  fixed — a `QoiOp::tag()` overflow panic reachable via the type's
  `pub` fields (`Run { length: 0 }` / `Diff { dr: i8::MAX, .. }` /
  `Luma { dg: i8::MAX, .. }` overflowed the `-1` / `+2` / `+32` bias
  steps under overflow checks); the bias arithmetic is now wrapping
  and masked to the tag field, identical for every in-spec value.

```sh
cargo +nightly fuzz run decode
cargo +nightly fuzz run encode_roundtrip
cargo +nightly fuzz run chunk_walk
cargo +nightly fuzz run op_iter
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
max-dim gradient. The `chunk_walk` corpus carries one named seed per
chunk type (RGB, RGBA, RUN, DIFF, LUMA, INDEX) plus a truncated-stream
seed. The `op_iter` corpus mirrors that one-seed-per-chunk-type set
plus a mixed-op stream and two mid-chunk-truncation seeds (RGB body
short, LUMA nibble missing). A 90-second local run of `chunk_walk`
reached ~28.5M executions (~314k exec/s) with coverage saturated and
zero crashes; the daily
`fuzz.yml` workflow runs all targets through the org reusable
`crate-fuzz.yml` for a 30-minute budget each.

## Property tests

`tests/property_sweep.rs` (round 199, depth-mode) is a deterministic
property-style sweep that complements the fuzz harness with hundreds
of pseudo-random `(width, height, channels, colorspace, pixels)`
triples per scenario. Six semantic invariants are asserted per case:

1. lossless roundtrip `parse_qoi(encode_qoi_full(…)) == input`,
2. worst-case size bound `bytes.len() <= 14 + n*5 + 8`,
3. header bytes and end marker echo the input,
4. encoder determinism (same input → same bytes twice),
5. tighter solid-fill bound `14 + 5 + ceil(n/62) + 8`,
6. idempotent re-encode `encode(decode(encode(px))) == encode(px)`.

Five input generators (random, smooth-deltas, RUN-heavy, 8-colour
palette, alpha-churn) exercise different paths through the encoder's
chunk-priority chain. A self-contained xorshift32 PRNG is seeded
per scenario so any failure is reproducible from the seed printed in
the assertion message; no `proptest` / `quickcheck` dev-dep is
introduced. Roughly 4_000 distinct cases run in well under a second
on a release build:

```sh
cargo test -p oxideav-qoi --test property_sweep
```

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
