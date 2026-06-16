//! Hand-built decoder boundary tests for the QOI wraparound and
//! tag-shadowing rules.
//!
//! The existing `property_sweep` / `canonical_encoding` suites drive the
//! codec encode→decode and only reach the decoder's channel-wraparound
//! arms through randomly-generated (clamped) pixels — they never pin the
//! *named worked examples* the one-page specification spells out for the
//! `QOI_OP_DIFF` and `QOI_OP_LUMA` wraparound arithmetic, nor do they
//! feed those values to the decoder directly. This module fills that gap
//! with minimal, hand-assembled chunk streams: each test constructs a
//! decoder-side byte sequence by hand (header, a single boundary chunk,
//! then the end marker) and asserts the decoded pixel matches the exact
//! value the spec states.
//!
//! Spec source (read-only, clean room):
//! `docs/image/qoi/qoi-specification.pdf` — *The Quite OK Image Format:
//! Specification Version 1.0, 2022-01-05*. The relevant clauses:
//!
//! * `QOI_OP_DIFF`: "The difference to the current channel values are
//!   using a wraparound operation, so `1 - 2` will result in `255`,
//!   while `255 + 1` will result in `0`." Stored with a bias of `2`
//!   (`-2` → `0` / `b00`, `1` → `3` / `b11`).
//! * `QOI_OP_LUMA`: "The difference to the current channel values are
//!   using a wraparound operation, so `10 - 13` will result in `253`,
//!   while `250 + 7` will result in `1`." `dg` biased by `32`,
//!   `dr-dg` / `db-dg` biased by `8`.
//! * `QOI_OP_RUN`: run-length stored with bias `-1`; lengths `63`
//!   (`b111110`) and `64` (`b111111`) are illegal because those bytes
//!   are the `QOI_OP_RGB` / `QOI_OP_RGBA` 8-bit tags. "The 8-bit tags
//!   have precedence over the 2-bit tags. A decoder must check for the
//!   presence of an 8-bit tag first."
//! * `QOI_OP_INDEX`: "A running `array[64]` (zero-initialized) of
//!   previously seen pixel values is maintained." The crucial subtlety
//!   the worked examples below pin: the index is zero-initialized to
//!   `(0,0,0,0)` — alpha `0` — which is *distinct* from the spec's
//!   initial previous pixel `(0,0,0,255)`. A decoder that mistakenly
//!   seeds the 64-slot array with the initial `prev` would return alpha
//!   `255` for an `INDEX` into an unwritten slot; the spec mandates
//!   alpha `0`. Each emitted pixel "is put into this array at the
//!   position formed by [the] hash function", so a slot only acquires a
//!   non-zero value once a pixel has been emitted into it.
//!
//! No encoder is involved on the assertion path: these tests would
//! catch a decoder regression even if the encoder shared the same bug.

use oxideav_qoi::{
    parse_qoi, qoi_hash, QoiChannels, END_MARKER, MAGIC, OP_DIFF, OP_INDEX, OP_LUMA, OP_RGB,
    OP_RGBA, OP_RUN,
};

/// Assemble a minimal single-row RGBA QOI file: `qoif` header for a
/// `width × 1` RGBA image, the caller's raw chunk bytes, then the
/// 8-byte end marker. The decoder starts from `prev = (0,0,0,255)` and
/// a zeroed 64-slot index, exactly as the spec mandates.
fn rgba_stream(width: u32, chunks: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(14 + chunks.len() + 8);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&width.to_be_bytes()); // width
    bytes.extend_from_slice(&1u32.to_be_bytes()); // height = 1
    bytes.push(4); // channels = RGBA
    bytes.push(0); // colorspace = sRGB + linear alpha
    bytes.extend_from_slice(chunks);
    bytes.extend_from_slice(END_MARKER);
    bytes
}

/// Build a `QOI_OP_DIFF` chunk byte from three signed channel deltas in
/// the spec's range `-2..=1`. Each is stored with a bias of `2`, two
/// bits per channel, MSB-first (`dr`, `dg`, `db`).
fn diff_chunk(dr: i32, dg: i32, db: i32) -> u8 {
    let r = (dr + 2) as u8 & 0x03;
    let g = (dg + 2) as u8 & 0x03;
    let b = (db + 2) as u8 & 0x03;
    OP_DIFF | (r << 4) | (g << 2) | b
}

/// Build the two `QOI_OP_LUMA` bytes from `dg` (range `-32..=31`) and the
/// channel-relative diffs `dr-dg` / `db-dg` (range `-8..=7`). `dg` is
/// biased by `32` into the 6-bit tag byte; `dr-dg` and `db-dg` are
/// biased by `8` into the high / low nibble of the second byte.
fn luma_chunk(dg: i32, dr_dg: i32, db_dg: i32) -> [u8; 2] {
    let b0 = OP_LUMA | ((dg + 32) as u8 & 0x3F);
    let b1 = (((dr_dg + 8) as u8 & 0x0F) << 4) | ((db_dg + 8) as u8 & 0x0F);
    [b0, b1]
}

// ---------------------------------------------------------------------------
// QOI_OP_DIFF wraparound — spec worked examples "1 - 2 = 255", "255 + 1 = 0"
// ---------------------------------------------------------------------------

#[test]
fn diff_wraparound_low_underflows_to_255() {
    // prev red = 1 (set by an RGB chunk), then a DIFF of dr = -2 takes
    // it to 1 - 2 = 255 (wraparound), with dg = db = 0 unchanged.
    // This pins the spec's literal "1 - 2 will result in 255".
    let mut chunks = vec![OP_RGB, 1, 0, 0]; // pixel 0 = (1,0,0,255)
    chunks.push(diff_chunk(-2, 0, 0)); // pixel 1 = (255,0,0,255)
    let bytes = rgba_stream(2, &chunks);

    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(img.channels, QoiChannels::Rgba);
    assert_eq!(&img.pixels[0..4], &[1, 0, 0, 255], "pixel 0");
    assert_eq!(
        &img.pixels[4..8],
        &[255, 0, 0, 255],
        "DIFF dr=-2 from red=1 must wrap to 255 (spec: 1 - 2 = 255)"
    );
}

#[test]
fn diff_wraparound_high_overflows_to_0() {
    // prev red = 255, then DIFF of dr = +1 takes it to 255 + 1 = 0.
    // Pins the spec's literal "255 + 1 will result in 0".
    let mut chunks = vec![OP_RGB, 255, 0, 0]; // pixel 0 = (255,0,0,255)
    chunks.push(diff_chunk(1, 0, 0)); // pixel 1 = (0,0,0,255)
    let bytes = rgba_stream(2, &chunks);

    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(&img.pixels[0..4], &[255, 0, 0, 255], "pixel 0");
    assert_eq!(
        &img.pixels[4..8],
        &[0, 0, 0, 255],
        "DIFF dr=+1 from red=255 must wrap to 0 (spec: 255 + 1 = 0)"
    );
}

#[test]
fn diff_full_delta_range_all_channels() {
    // Sweep every legal DIFF delta (-2..=1) on each channel from a
    // mid-range prev, confirming the bias-2 decode is exact and the
    // alpha stays unchanged (DIFF never touches alpha).
    for dr in -2..=1 {
        for dg in -2..=1 {
            for db in -2..=1 {
                let base = [100u8, 110, 120, 255];
                let mut chunks = vec![OP_RGB, base[0], base[1], base[2]];
                chunks.push(diff_chunk(dr, dg, db));
                let bytes = rgba_stream(2, &chunks);
                let img = parse_qoi(&bytes).expect("decode");
                let exp = [
                    base[0].wrapping_add(dr as u8),
                    base[1].wrapping_add(dg as u8),
                    base[2].wrapping_add(db as u8),
                    255,
                ];
                assert_eq!(
                    &img.pixels[4..8],
                    &exp,
                    "DIFF dr={dr} dg={dg} db={db} from {base:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// QOI_OP_LUMA wraparound — spec worked examples "10 - 13 = 253", "250 + 7 = 1"
// ---------------------------------------------------------------------------

#[test]
fn luma_wraparound_underflows_to_253() {
    // The spec's "10 - 13 will result in 253": set red = 10 via RGB,
    // then apply a LUMA whose effective red delta is -13.
    // Decoder: dr = (dr-dg) + dg. Pick dg = -5, dr-dg = -8 → dr = -13.
    // 10 + (-13) = 253 (wraparound).
    let mut chunks = vec![OP_RGB, 10, 50, 0]; // prev = (10,50,0,255)
    chunks.extend_from_slice(&luma_chunk(-5, -8, 0));
    let bytes = rgba_stream(2, &chunks);

    let img = parse_qoi(&bytes).expect("decode");
    // green: 50 + dg(-5) = 45; red: 10 + dr(-13) = 253; blue: 0 + db(-5) = 251
    assert_eq!(
        img.pixels[4], 253,
        "LUMA red delta -13 from red=10 must wrap to 253 (spec: 10 - 13 = 253)"
    );
    assert_eq!(img.pixels[5], 45, "green: 50 + dg(-5)");
    assert_eq!(img.pixels[6], 251, "blue: 0 + db(-5) wraps");
    assert_eq!(img.pixels[7], 255, "alpha unchanged by LUMA");
}

#[test]
fn luma_wraparound_overflows_to_1() {
    // The spec's "250 + 7 will result in 1": set red = 250 via RGB,
    // then apply a LUMA whose effective red delta is +7.
    // dg = +5, dr-dg = +2 → dr = +7. 250 + 7 = 257 → 1 (wraparound).
    let mut chunks = vec![OP_RGB, 250, 100, 0]; // prev = (250,100,0,255)
    chunks.extend_from_slice(&luma_chunk(5, 2, 0));
    let bytes = rgba_stream(2, &chunks);

    let img = parse_qoi(&bytes).expect("decode");
    // red: 250 + dr(7) = 1; green: 100 + dg(5) = 105; blue: 0 + db(5) = 5
    assert_eq!(
        img.pixels[4], 1,
        "LUMA red delta +7 from red=250 must wrap to 1 (spec: 250 + 7 = 1)"
    );
    assert_eq!(img.pixels[5], 105, "green: 100 + dg(5)");
    assert_eq!(img.pixels[6], 5, "blue: 0 + db(5)");
    assert_eq!(img.pixels[7], 255, "alpha unchanged by LUMA");
}

#[test]
fn luma_extreme_dg_endpoints() {
    // dg endpoints -32 and +31, with dr-dg / db-dg at their endpoints
    // -8 / +7. Confirms the bias-32 / bias-8 decode is exact at the
    // edges of the representable LUMA range.
    let cases: &[(i32, i32, i32)] = &[(-32, -8, 7), (31, 7, -8), (-32, 7, 7), (31, -8, -8)];
    for &(dg, dr_dg, db_dg) in cases {
        let base = [128u8, 128, 128, 255];
        let mut chunks = vec![OP_RGB, base[0], base[1], base[2]];
        chunks.extend_from_slice(&luma_chunk(dg, dr_dg, db_dg));
        let bytes = rgba_stream(2, &chunks);
        let img = parse_qoi(&bytes).expect("decode");
        let dr = dr_dg + dg;
        let db = db_dg + dg;
        let exp = [
            base[0].wrapping_add(dr as u8),
            base[1].wrapping_add(dg as u8),
            base[2].wrapping_add(db as u8),
            255,
        ];
        assert_eq!(
            &img.pixels[4..8],
            &exp,
            "LUMA dg={dg} dr-dg={dr_dg} db-dg={db_dg}"
        );
    }
}

// ---------------------------------------------------------------------------
// 8-bit tag precedence — 0xfe / 0xff are NEVER decoded as RUN
// ---------------------------------------------------------------------------

#[test]
fn rgb_tag_0xfe_is_not_a_run() {
    // 0xFE has top two bits 0b11 — naively that's a RUN. The spec says
    // the 8-bit RGB tag takes precedence: a decoder must check the full
    // byte first. Decode a single RGB chunk and assert exactly ONE
    // pixel was produced (a RUN of 0xFE & 0x3F = 62 + 1 = 63 would emit
    // 63 pixels and overshoot a 1-pixel image).
    let chunks = vec![OP_RGB, 17, 34, 51];
    let bytes = rgba_stream(1, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(img.pixels.len(), 4, "0xFE must decode as ONE RGB pixel");
    assert_eq!(&img.pixels[0..4], &[17, 34, 51, 255]);
}

#[test]
fn rgba_tag_0xff_is_not_a_run() {
    // 0xFF likewise has top two bits 0b11. The 8-bit RGBA tag wins.
    let chunks = vec![OP_RGBA, 17, 34, 51, 68];
    let bytes = rgba_stream(1, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(img.pixels.len(), 4, "0xFF must decode as ONE RGBA pixel");
    assert_eq!(&img.pixels[0..4], &[17, 34, 51, 68]);
}

#[test]
fn run_max_length_62_decodes_to_62_copies() {
    // Largest legal RUN: stored value 61 (b111101) → run length 62.
    // The very next legal value 62 (b111110) is 0xFE = RGB, and 63
    // (b111111) is 0xFF = RGBA, so 62 is the ceiling. First pixel is a
    // RUN of the initial prev (0,0,0,255).
    let run_byte = OP_RUN | 61; // 0xC0 | 0x3D = 0xFD, length = 62
    assert_eq!(
        run_byte, 0xFD,
        "max legal RUN byte is 0xFD, just below 0xFE"
    );
    let chunks = vec![run_byte];
    let bytes = rgba_stream(62, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(img.pixels.len(), 62 * 4, "RUN of 62 emits 62 pixels");
    for px in img.pixels.chunks_exact(4) {
        assert_eq!(px, &[0, 0, 0, 255], "every RUN pixel is the initial prev");
    }
}

#[test]
fn run_length_one_decodes_single_pixel() {
    // Minimal RUN: stored value 0 → length 1.
    let chunks = vec![OP_RUN];
    let bytes = rgba_stream(1, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(img.pixels.len(), 4, "RUN of 1 emits 1 pixel");
    assert_eq!(&img.pixels[0..4], &[0, 0, 0, 255]);
}

// ---------------------------------------------------------------------------
// QOI_OP_INDEX — the running array is zero-initialized to (0,0,0,0),
// which is NOT the initial previous pixel (0,0,0,255)
// ---------------------------------------------------------------------------

/// Build a `QOI_OP_INDEX` chunk byte referencing slot `idx` (0..=63).
/// The 2-bit tag is `b00`, so the chunk byte is just the 6-bit index.
fn index_chunk(idx: u8) -> u8 {
    debug_assert!(idx < 64);
    OP_INDEX | (idx & 0x3F)
}

#[test]
fn index_into_unwritten_slot_yields_zero_alpha() {
    // First chunk of the stream is an INDEX into slot 5, which has never
    // been written. The spec's running array is "zero-initialized", so
    // slot 5 holds (0,0,0,0) — alpha 0. A decoder that seeded the array
    // with the initial prev (0,0,0,255) would wrongly return alpha 255.
    let chunks = vec![index_chunk(5)];
    let bytes = rgba_stream(1, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(img.channels, QoiChannels::Rgba);
    assert_eq!(
        &img.pixels[0..4],
        &[0, 0, 0, 0],
        "INDEX into an unwritten slot must decode the zero-initialized \
         (0,0,0,0) entry, NOT the initial prev (0,0,0,255)"
    );
}

#[test]
fn index_slot_zero_at_stream_start_is_zero_alpha() {
    // The sharpest form of the trap: slot 0 is referenced by the chunk
    // byte 0x00, and nothing has been emitted yet. It must decode to
    // (0,0,0,0). This is exactly where an off-by-one initial-prev seed
    // would surface alpha 255.
    let chunks = vec![index_chunk(0)];
    assert_eq!(chunks[0], 0x00, "INDEX slot 0 is the all-zero chunk byte");
    let bytes = rgba_stream(1, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(
        &img.pixels[0..4],
        &[0, 0, 0, 0],
        "INDEX slot 0 at stream start is the zero-initialized entry"
    );
}

#[test]
fn index_slot_53_at_start_is_not_the_initial_prev() {
    // The initial previous pixel (0,0,0,255) hashes to slot 53
    // ((11 * 255) % 64 = 53). But the array is zero-initialized and the
    // initial prev is NEVER pre-stored — a slot only acquires a value
    // after a pixel is emitted into it. So an INDEX into slot 53 BEFORE
    // any pixel has been emitted must still decode (0,0,0,0), not the
    // initial prev. This pins that the decoder does not pre-seed
    // index[hash(initial_prev)].
    assert_eq!(
        qoi_hash([0, 0, 0, 255]),
        53,
        "initial prev hashes to slot 53 (sanity for the trap below)"
    );
    let chunks = vec![index_chunk(53)];
    let bytes = rgba_stream(1, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(
        &img.pixels[0..4],
        &[0, 0, 0, 0],
        "slot 53 is empty until the initial prev is actually emitted; \
         an INDEX into it at stream start must yield (0,0,0,0)"
    );
}

#[test]
fn run_then_index_53_recalls_the_emitted_initial_prev() {
    // Once the initial prev (0,0,0,255) is actually emitted — here via a
    // 1-pixel RUN — the decoder stores it at index[hash] = index[53].
    // A following INDEX into slot 53 then recalls the full (0,0,0,255),
    // alpha included. Contrast with the previous test where slot 53 was
    // still zero. This pins the store-on-emit half of the index contract
    // through the decoder directly.
    let chunks = vec![OP_RUN, index_chunk(53)]; // pixel0 = RUN(prev), pixel1 = INDEX 53
    let bytes = rgba_stream(2, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(&img.pixels[0..4], &[0, 0, 0, 255], "pixel 0 (RUN of prev)");
    assert_eq!(
        &img.pixels[4..8],
        &[0, 0, 0, 255],
        "pixel 1: INDEX 53 recalls the now-stored initial prev"
    );
}

#[test]
fn rgba_then_index_recalls_exact_stored_pixel() {
    // Emit a fully-specified RGBA pixel, which the decoder stores at
    // index[hash(pixel)]. A following INDEX into that exact slot must
    // recall all four channels byte-exact. This drives the index
    // store→recall path through the decoder with a non-trivial colour
    // (so a wrong hash or a dropped channel would surface here).
    let px = [17u8, 34, 51, 68];
    let slot = qoi_hash(px);
    let mut chunks = vec![OP_RGBA, px[0], px[1], px[2], px[3]];
    chunks.push(index_chunk(slot)); // recall the pixel just stored
    let bytes = rgba_stream(2, &chunks);
    let img = parse_qoi(&bytes).expect("decode");
    assert_eq!(&img.pixels[0..4], &px, "pixel 0 (RGBA)");
    assert_eq!(
        &img.pixels[4..8],
        &px,
        "pixel 1: INDEX into hash slot recalls the exact stored RGBA pixel"
    );
}
