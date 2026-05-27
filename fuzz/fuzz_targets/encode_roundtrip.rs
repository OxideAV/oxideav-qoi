#![no_main]

//! Encode-then-decode roundtrip target. QOI is a lossless format, so
//! `parse_qoi(encode_qoi(w, h, ch, px)) == (w, h, ch, px)` must hold
//! for every well-formed pixel input. This target derives a small
//! image header from the first few fuzz bytes, feeds the rest as raw
//! pixel data, calls [`encode_qoi_full`], then [`parse_qoi`], and
//! asserts the decoded pixel buffer is byte-identical to the input.
//!
//! Beyond catching a roundtrip-breaking encoder mistake, this target
//! exercises:
//!
//! * the encoder's chunk-selection priority chain
//!   (`RUN, INDEX, DIFF, LUMA, RGB, RGBA` in descending priority)
//!   against attacker-chosen pixel streams — in particular the LUMA
//!   `(dr-dg, db-dg)` range check and the RUN cap at 62 + image-end
//!   flush;
//! * the encoder's running-pixel-array maintenance (the spec puts
//!   *every* pixel into the index, including the ones inside a RUN),
//!   which is the easiest place to drift from the decoder's own
//!   indexing and silently break roundtrip;
//! * the decoder's pixel-count exhaustion check against an
//!   encoder-produced stream, distinct from the `decode` target's
//!   fully attacker-controlled byte stream.
//!
//! Dimensions are capped (≤256×256 = 64 KiB pixels) to keep each
//! iteration fast enough that libfuzzer can drive coverage instead
//! of waiting on encoder loops.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{encode_qoi_full, parse_qoi, QoiChannels, QoiColorspace};

// Hard cap on per-iteration work. 256×256 RGBA = 256 KiB pixel bytes
// in, ≤1.25 MiB encoded, and ≤256 KiB decoded back out — bounded
// enough that libfuzzer can keep throughput high while still
// exercising every chunk-selection path. Larger headers are clamped
// rather than rejected so the fuzzer doesn't burn budget regenerating
// the high bytes of `width` / `height` to land below the cap.
const MAX_DIM: u32 = 256;

fuzz_target!(|data: &[u8]| {
    // Need at least the 6 header bytes plus one pixel's worth of
    // payload. Anything shorter can't drive a meaningful roundtrip,
    // and a 0-pixel image would just be the bare 14-byte header + 8
    // end marker, which the encoder also rejects (via the
    // `pixels.len() == w*h*channels` assert when both are zero only
    // trivially — but the decoder rejects zero dimensions, so we
    // require width >= 1 and height >= 1 below).
    if data.len() < 7 {
        return;
    }

    // First 6 bytes pick the image shape; the rest is pixel data.
    // u16 BE × 2 for width / height (clamped 1..=MAX_DIM), one byte
    // selects channels (low bit → 3 or 4), one byte selects
    // colorspace (low bit → 0 or 1).
    let raw_w = u16::from_be_bytes([data[0], data[1]]) as u32;
    let raw_h = u16::from_be_bytes([data[2], data[3]]) as u32;
    // Map 0 → 1 (avoid zero-dimension which encode_qoi accepts but
    // decode rejects, breaking the roundtrip assertion) and clamp to
    // MAX_DIM.
    let width = raw_w.clamp(1, MAX_DIM);
    let height = raw_h.clamp(1, MAX_DIM);
    let channels: u8 = if data[4] & 1 == 1 { 4 } else { 3 };
    let colorspace: u8 = if data[5] & 1 == 1 { 1 } else { 0 };
    let payload = &data[6..];

    let needed = (width as usize) * (height as usize) * (channels as usize);
    // Build the pixel buffer from the payload, repeating it as a
    // cycle if too short. Repeating instead of zero-padding gives
    // libfuzzer's coverage feedback something to bite on for any
    // pixel offset, not just the first `payload.len()` bytes.
    let mut pixels = Vec::with_capacity(needed);
    if !payload.is_empty() {
        while pixels.len() < needed {
            let take = (needed - pixels.len()).min(payload.len());
            pixels.extend_from_slice(&payload[..take]);
        }
    } else {
        pixels.resize(needed, 0);
    }
    debug_assert_eq!(pixels.len(), needed);

    let bytes = encode_qoi_full(width, height, channels, colorspace, &pixels);

    // Sanity-check the encoder's output before handing it to the
    // decoder: the size must fit the worst-case bound the encoder
    // pre-reserves (header + 5 bytes / pixel + 8-byte end marker).
    let worst_case = 14 + needed / channels as usize * 5 + 8;
    assert!(
        bytes.len() <= worst_case,
        "encoder emitted {} bytes, worst-case bound was {worst_case} \
         ({}x{} ch={} colorspace={})",
        bytes.len(),
        width,
        height,
        channels,
        colorspace
    );

    // The lossless roundtrip contract.
    let back = parse_qoi(&bytes).unwrap_or_else(|e| {
        panic!(
            "parse_qoi rejected encoder output: {e:?} \
             ({}x{} ch={} colorspace={}, encoded={} bytes, pixels_in={})",
            width,
            height,
            channels,
            colorspace,
            bytes.len(),
            pixels.len()
        );
    });

    assert_eq!(back.width, width);
    assert_eq!(back.height, height);
    assert_eq!(
        back.channels,
        if channels == 4 {
            QoiChannels::Rgba
        } else {
            QoiChannels::Rgb
        }
    );
    assert_eq!(
        back.colorspace,
        if colorspace == 1 {
            QoiColorspace::AllLinear
        } else {
            QoiColorspace::SrgbWithLinearAlpha
        }
    );
    assert_eq!(
        back.pixels, pixels,
        "lossless roundtrip broke: encoder/decoder mismatch \
         ({}x{} ch={} colorspace={})",
        width, height, channels, colorspace,
    );
});
