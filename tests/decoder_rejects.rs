//! Hand-built decoder *rejection* tests for the QOI structural rules.
//!
//! The existing decoder-facing suites all assert the *happy* path:
//! `decoder_boundary.rs` pins the wraparound / tag-precedence / index
//! worked examples, `property_sweep.rs` and `canonical_encoding.rs`
//! drive the encode→decode round-trip, and `reference_fixtures.rs`
//! checks byte-exact fixtures. None of them feed the decoder a
//! *malformed* stream and assert it is rejected — yet the one-page
//! specification mandates a precise set of structural well-formedness
//! conditions, and the decoder documents a matching set of
//! [`QoiError`] rejections on `parse_qoi`. A regression that silently
//! accepted a truncated stream, a missing end marker, or an illegal
//! `channels` byte would pass every other test in the crate.
//!
//! This module fills that gap. Each test assembles a minimal QOI byte
//! sequence that violates exactly one structural rule and asserts the
//! decoder returns an `Err` (with the right [`QoiError`] class). No
//! encoder is on the assertion path, so these catch a decoder
//! regression even when the encoder shares the same blind spot.
//!
//! Spec source (read-only, clean room):
//! `docs/image/qoi/qoi-specification.pdf` — *The Quite OK Image Format:
//! Specification Version 1.0, 2022-01-05*. The structural mandates
//! pinned here:
//!
//! * Header: 14 bytes — `char magic[4]` (`"qoif"`), `uint32_t width`
//!   (BE), `uint32_t height` (BE), `uint8_t channels` ("3 = RGB,
//!   4 = RGBA"), `uint8_t colorspace` ("0 = sRGB with linear alpha /
//!   1 = all channels linear"). A `channels` or `colorspace` value
//!   outside its stated enumeration is not a valid header.
//! * "An image is complete when all pixels specified by `width * height`
//!   have been covered." A stream that runs out of chunk bytes before
//!   `width * height` pixels are produced is incomplete; one that keeps
//!   going after the image is complete has trailing data the spec does
//!   not allow before the end marker.
//! * "The byte stream's end is marked with 7 `0x00` bytes followed by a
//!   single `0x01` byte." A stream missing that exact 8-byte trailer is
//!   malformed.
//! * Chunk widths are fixed (`QOI_OP_RGB` 4 bytes, `QOI_OP_RGBA`
//!   5 bytes, `QOI_OP_LUMA` 2 bytes). A chunk whose body is cut short by
//!   the end of the stream cannot be decoded.

use oxideav_qoi::{
    parse_qoi, parse_qoi_header, QoiError, END_MARKER, MAGIC, OP_LUMA, OP_RGB, OP_RGBA, OP_RUN,
};

/// Assemble a QOI file with a caller-chosen header and chunk body,
/// appending the canonical 8-byte end marker. `channels` / `colorspace`
/// are written raw so a test can inject illegal enum values.
fn stream(width: u32, height: u32, channels: u8, colorspace: u8, chunks: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(14 + chunks.len() + 8);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.push(channels);
    bytes.push(colorspace);
    bytes.extend_from_slice(chunks);
    bytes.extend_from_slice(END_MARKER);
    bytes
}

/// A well-formed single-pixel RGBA stream — the control case. Every
/// rejection test below mutates exactly one structural property of an
/// otherwise-valid stream; this confirms the unmutated baseline decodes.
fn valid_one_pixel() -> Vec<u8> {
    stream(1, 1, 4, 0, &[OP_RGBA, 10, 20, 30, 40])
}

fn assert_invalid(bytes: &[u8], what: &str) {
    match parse_qoi(bytes) {
        Err(QoiError::InvalidData(_)) => {}
        Err(QoiError::Unsupported(msg)) => {
            panic!("{what}: expected InvalidData, got Unsupported({msg})")
        }
        Ok(_) => panic!("{what}: malformed stream was accepted"),
    }
}

// ---------------------------------------------------------------------------
// Baseline — the control case must decode cleanly.
// ---------------------------------------------------------------------------

#[test]
fn baseline_valid_stream_decodes() {
    let img = parse_qoi(&valid_one_pixel()).expect("control stream must decode");
    assert_eq!(&img.pixels[0..4], &[10, 20, 30, 40]);
}

// ---------------------------------------------------------------------------
// Header: magic, length, channels, colorspace, dimensions.
// ---------------------------------------------------------------------------

#[test]
fn bad_magic_is_rejected() {
    let mut bytes = valid_one_pixel();
    bytes[0] = b'X'; // "Xoif" — not the mandated "qoif"
    assert_invalid(&bytes, "bad magic");
}

#[test]
fn each_magic_byte_position_matters() {
    // Mutating ANY of the four magic bytes must reject — pins that the
    // decoder compares the full 4-byte magic, not a prefix.
    for i in 0..4 {
        let mut bytes = valid_one_pixel();
        bytes[i] ^= 0x01;
        assert_invalid(&bytes, &format!("magic byte {i} flipped"));
    }
}

#[test]
fn input_shorter_than_header_is_rejected() {
    // 14-byte header is the minimum a header probe needs; anything
    // shorter cannot even be inspected.
    for len in 0..14 {
        let truncated = &valid_one_pixel()[..len];
        assert_invalid(truncated, &format!("input of {len} bytes"));
        // The cheap header probe must agree on the same rejection.
        assert!(
            parse_qoi_header(truncated).is_err(),
            "header probe must reject {len}-byte input"
        );
    }
}

#[test]
fn input_with_header_but_no_end_marker_is_rejected() {
    // `parse_qoi` requires header + 8-byte end marker = 22 bytes minimum.
    // A bare 14-byte header (which the *header probe* accepts) is too
    // short for the full decode.
    let header_only = &valid_one_pixel()[..14];
    assert_invalid(header_only, "header-only (14 bytes), no end marker");
    // ...yet the header probe accepts exactly the 14-byte header.
    assert!(
        parse_qoi_header(header_only).is_ok(),
        "header probe must accept a bare 14-byte header"
    );
}

#[test]
fn illegal_channels_value_is_rejected() {
    // Spec enumerates only 3 (RGB) and 4 (RGBA). Every other byte value
    // is an invalid header — sweep the whole u8 range.
    for ch in 0u16..=255 {
        let ch = ch as u8;
        let bytes = stream(1, 1, ch, 0, &[OP_RGBA, 1, 2, 3, 4]);
        if ch == 3 || ch == 4 {
            // 3/4 are legal; the (RGBA) chunk body decodes fine for both.
            assert!(parse_qoi(&bytes).is_ok(), "channels={ch} must be legal");
        } else {
            assert_invalid(&bytes, &format!("channels={ch}"));
            assert!(
                parse_qoi_header(&bytes).is_err(),
                "header probe must reject channels={ch}"
            );
        }
    }
}

#[test]
fn illegal_colorspace_value_is_rejected() {
    // Spec enumerates only 0 (sRGB+linear-alpha) and 1 (all linear).
    for cs in 0u16..=255 {
        let cs = cs as u8;
        let bytes = stream(1, 1, 4, cs, &[OP_RGBA, 1, 2, 3, 4]);
        if cs == 0 || cs == 1 {
            assert!(parse_qoi(&bytes).is_ok(), "colorspace={cs} must be legal");
        } else {
            assert_invalid(&bytes, &format!("colorspace={cs}"));
            assert!(
                parse_qoi_header(&bytes).is_err(),
                "header probe must reject colorspace={cs}"
            );
        }
    }
}

#[test]
fn zero_width_is_rejected() {
    let bytes = stream(0, 1, 4, 0, &[OP_RGBA, 1, 2, 3, 4]);
    assert_invalid(&bytes, "width = 0");
    assert!(
        parse_qoi_header(&bytes).is_err(),
        "header probe rejects w=0"
    );
}

#[test]
fn zero_height_is_rejected() {
    let bytes = stream(1, 0, 4, 0, &[OP_RGBA, 1, 2, 3, 4]);
    assert_invalid(&bytes, "height = 0");
    assert!(
        parse_qoi_header(&bytes).is_err(),
        "header probe rejects h=0"
    );
}

#[test]
fn zero_by_zero_is_rejected() {
    let bytes = stream(0, 0, 4, 0, &[]);
    assert_invalid(&bytes, "0x0 image");
}

// ---------------------------------------------------------------------------
// End marker: 7 × 0x00 then 0x01.
// ---------------------------------------------------------------------------

#[test]
fn wrong_end_marker_is_rejected() {
    let mut bytes = valid_one_pixel();
    let last = bytes.len() - 1;
    bytes[last] = 0x00; // trailer becomes all-zero — not the mandated ...01
    assert_invalid(&bytes, "all-zero end marker");
}

#[test]
fn each_end_marker_byte_position_matters() {
    // Flip each of the 8 trailer bytes in turn; every position is
    // load-bearing (7 zeros then a 1).
    let base = valid_one_pixel();
    let start = base.len() - 8;
    for i in 0..8 {
        let mut bytes = base.clone();
        bytes[start + i] ^= 0x01;
        assert_invalid(&bytes, &format!("end-marker byte {i} flipped"));
    }
}

#[test]
fn truncated_end_marker_is_rejected() {
    // Drop trailing bytes one at a time so the 8-byte marker is partial.
    // The decoder slices the last 8 bytes as the trailer; a stream that
    // is shorter (or whose tail no longer equals the marker) is rejected.
    let base = valid_one_pixel();
    for drop in 1..=8 {
        let bytes = &base[..base.len() - drop];
        assert_invalid(bytes, &format!("end marker short by {drop}"));
    }
}

// ---------------------------------------------------------------------------
// Chunk-body truncation: a chunk whose body is cut off by the stream end.
// ---------------------------------------------------------------------------

#[test]
fn truncated_rgb_chunk_is_rejected() {
    // QOI_OP_RGB is a 4-byte chunk (tag + r,g,b). Provide the tag and
    // only two of the three colour bytes, then the end marker. The
    // decoder reaches the RGB arm, needs 3 body bytes, finds 2.
    for body in 0..3 {
        let mut chunks = vec![OP_RGB];
        chunks.extend(std::iter::repeat(0u8).take(body));
        let bytes = stream(1, 1, 4, 0, &chunks);
        assert_invalid(&bytes, &format!("RGB with {body}/3 body bytes"));
    }
}

#[test]
fn truncated_rgba_chunk_is_rejected() {
    // QOI_OP_RGBA is a 5-byte chunk (tag + r,g,b,a).
    for body in 0..4 {
        let mut chunks = vec![OP_RGBA];
        chunks.extend(std::iter::repeat(0u8).take(body));
        let bytes = stream(1, 1, 4, 0, &chunks);
        assert_invalid(&bytes, &format!("RGBA with {body}/4 body bytes"));
    }
}

#[test]
fn truncated_luma_chunk_is_rejected() {
    // QOI_OP_LUMA is a 2-byte chunk (tag + second diff byte). The lone
    // tag byte with no second byte must be rejected. `OP_LUMA | 0` is a
    // valid LUMA tag; the missing second byte makes the chunk truncated.
    let bytes = stream(1, 1, 4, 0, &[OP_LUMA]);
    assert_invalid(&bytes, "LUMA with no second byte");
}

// ---------------------------------------------------------------------------
// Image-completion mismatches: too few / too many pixels for width*height.
// ---------------------------------------------------------------------------

#[test]
fn stream_with_no_chunks_for_nonzero_image_is_rejected() {
    // Header claims a 1×1 image but the chunk region is empty: the
    // decoder cannot cover width*height = 1 pixel, so the stream is
    // incomplete (truncated mid-image).
    let bytes = stream(1, 1, 4, 0, &[]);
    assert_invalid(&bytes, "1x1 image with zero chunk bytes");
}

#[test]
fn stream_ending_before_image_complete_is_rejected() {
    // Header claims 4 pixels; only one RGBA chunk is provided. The
    // decoder runs out of chunk bytes after pixel 0, before reaching
    // width*height = 4.
    let bytes = stream(4, 1, 4, 0, &[OP_RGBA, 1, 2, 3, 4]);
    assert_invalid(&bytes, "4-pixel image, 1 pixel of chunks");
}

#[test]
fn run_overshooting_image_size_is_rejected() {
    // A 1×1 image with a RUN of length 5 would emit 5 pixels — 4 past
    // the image. The decoder must reject the run as overshooting the
    // declared size rather than writing out of bounds.
    let run5 = OP_RUN | 4; // (run - 1) = 4 → length 5
    let bytes = stream(1, 1, 4, 0, &[run5]);
    assert_invalid(&bytes, "RUN of 5 in a 1-pixel image");
}

#[test]
fn trailing_chunk_after_image_complete_is_rejected() {
    // Header claims 1 pixel; the chunk region supplies TWO pixels. After
    // the first pixel the image is complete, leaving a whole extra chunk
    // sitting between the last consumed byte and the end marker. The spec
    // has no slack here — the decoder rejects the trailing data.
    let bytes = stream(1, 1, 4, 0, &[OP_RGBA, 1, 2, 3, 4, OP_RGB, 9, 9, 9]);
    assert_invalid(&bytes, "extra chunk after the image is complete");
}

#[test]
fn stray_byte_before_end_marker_is_rejected() {
    // One garbage byte after the (complete) single pixel but before the
    // end marker. The decoder consumes exactly the one pixel, then finds
    // the cursor is not at the chunk-region end: trailing bytes.
    let bytes = stream(1, 1, 4, 0, &[OP_RGBA, 1, 2, 3, 4, 0x00]);
    assert_invalid(&bytes, "stray byte before end marker");
}

// ---------------------------------------------------------------------------
// Oversized-header guard: a tiny file claiming a huge image.
// ---------------------------------------------------------------------------

#[test]
fn oversized_header_with_tiny_body_is_rejected_not_oom() {
    // A header claiming a 65536×65536 image (≈ 4.3 G pixels, ≈ 17 GB at
    // RGBA) with an essentially empty chunk stream must be rejected as a
    // truncated stream, NOT trigger a multi-gigabyte allocation. The
    // chunk region can decode at most chunks.len()*62 pixels, far fewer
    // than the header's claim, so the decoder bails before allocating.
    let bytes = stream(65536, 65536, 4, 0, &[OP_RGBA, 1, 2, 3, 4]);
    assert_invalid(&bytes, "65536x65536 header, 1 pixel of chunks");
    // The cheap header probe, by contrast, only inspects the 14-byte
    // header — it does NOT walk the chunk stream, so the oversized claim
    // is not its concern and it returns the metadata successfully.
    let hdr = parse_qoi_header(&bytes).expect("header probe reads metadata only");
    assert_eq!(hdr.width, 65536);
    assert_eq!(hdr.height, 65536);
}
