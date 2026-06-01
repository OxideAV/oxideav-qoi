//! Pure-Rust **QOI** (Quite OK Image) reader and writer.
//!
//! Clean-room implementation of the one-page specification published at
//! [qoiformat.org](https://qoiformat.org/qoi-specification.pdf). No
//! third-party source code was consulted; the spec PDF is the sole
//! source of truth.
//!
//! ## What QOI is
//!
//! A small, lossless RGB(A) image format. Files are made of a 14-byte
//! header, a stream of 1- to 5-byte chunks, and an 8-byte end marker.
//! Compression comes from three things:
//! * a **64-entry running pixel array** indexed by the hash function
//!   `(R*3 + G*5 + B*7 + A*11) % 64`,
//! * **delta encoding** against the previous pixel (DIFF chunks store
//!   each channel in 2 bits, LUMA chunks in a `dg + (dr-dg) + (db-dg)`
//!   layout),
//! * **runs** of the previous pixel (1..62 long).
//!
//! The format has no entropy coder, no quantisation, and no
//! optional/extension chunks — what's listed below is the entire spec.
//!
//! ## Chunks
//!
//! | Chunk          | Tag bits / byte | Body         |
//! |----------------|------------------|--------------|
//! | `QOI_OP_RGB`   | `11111110` (`0xfe`)         | 3 raw R/G/B bytes (alpha unchanged) |
//! | `QOI_OP_RGBA`  | `11111111` (`0xff`)         | 4 raw R/G/B/A bytes |
//! | `QOI_OP_INDEX` | top-2 `00`, low-6 = index   | (none)       |
//! | `QOI_OP_DIFF`  | top-2 `01`, low-6 = packed deltas | (none)  |
//! | `QOI_OP_LUMA`  | top-2 `10`, low-6 = `dg+32` | 1 byte: `(dr-dg+8) << 4 | (db-dg+8)` |
//! | `QOI_OP_RUN`   | top-2 `11`, low-6 = `(run-1)`, `run` ∈ 1..=62 | (none) |
//!
//! Note tags `0xfe` / `0xff` (8-bit) shadow the 2-bit `11` RUN values
//! 62 / 63, so RUN tops out at 62 instead of 64.
//!
//! ## API
//!
//! ```
//! use oxideav_qoi::{parse_qoi, encode_qoi, QoiChannels, QoiColorspace};
//!
//! // Round-trip a 2×2 RGBA image through encode → decode.
//! let pixels: Vec<u8> = vec![
//!     255,   0,   0, 255,    0, 255,   0, 255,
//!       0,   0, 255, 255,  255, 255, 255, 255,
//! ];
//! let bytes = encode_qoi(2, 2, /* channels */ 4, &pixels);
//! let back = parse_qoi(&bytes).unwrap();
//! assert_eq!(back.width, 2);
//! assert_eq!(back.height, 2);
//! assert_eq!(back.channels, QoiChannels::Rgba);
//! assert_eq!(back.colorspace, QoiColorspace::SrgbWithLinearAlpha);
//! assert_eq!(back.pixels, pixels);
//! ```
//!
//! ## Standalone vs registry-integrated
//!
//! The crate's default `registry` Cargo feature pulls in `oxideav-core`
//! and exposes the framework `Decoder` / `Encoder` trait surface plus
//! a [`registry::register`] entry point. The sibling
//! [`registry::register_containers`] call wires the `.qoi` file
//! extension into a `ContainerRegistry` so cli-convert / pipeline
//! output probing can resolve `.qoi` paths through the central
//! registry instead of a hard-coded list. Disable the feature
//! (`default-features = false`) for an `oxideav-core`-free build that
//! still exposes the standalone [`parse_qoi`] / [`encode_qoi`] API
//! plus crate-local [`QoiImage`] / [`QoiChannels`] / [`QoiColorspace`]
//! / [`QoiError`] types.

pub mod decoder;
pub mod encoder;
pub mod error;
pub mod image;
#[cfg(feature = "registry")]
pub mod registry;

/// Codec id for QOI image frames.
pub const CODEC_ID_STR: &str = "qoi";

/// Magic at the start of every QOI file (4 bytes, ASCII `qoif`).
pub const MAGIC: &[u8; 4] = b"qoif";

/// Total header length: magic (4) + width u32 BE (4) + height u32 BE
/// (4) + channels u8 (1) + colorspace u8 (1) = 14 bytes.
pub const HEADER_SIZE: usize = 14;

/// Trailing 8-byte end marker per the spec.
pub const END_MARKER: &[u8; 8] = &[0, 0, 0, 0, 0, 0, 0, 1];

/// 8-bit chunk tag for `QOI_OP_RGB` (`11111110`).
pub const OP_RGB: u8 = 0xFE;
/// 8-bit chunk tag for `QOI_OP_RGBA` (`11111111`).
pub const OP_RGBA: u8 = 0xFF;
/// 2-bit chunk tag prefix for `QOI_OP_INDEX` (`00xxxxxx`).
pub const OP_INDEX: u8 = 0x00;
/// 2-bit chunk tag prefix for `QOI_OP_DIFF` (`01xxxxxx`).
pub const OP_DIFF: u8 = 0x40;
/// 2-bit chunk tag prefix for `QOI_OP_LUMA` (`10xxxxxx`).
pub const OP_LUMA: u8 = 0x80;
/// 2-bit chunk tag prefix for `QOI_OP_RUN` (`11xxxxxx`).
pub const OP_RUN: u8 = 0xC0;

pub use decoder::parse_qoi;
pub use encoder::{encode_qoi, encode_qoi_full};
pub use error::{QoiError, Result};
pub use image::{QoiChannels, QoiColorspace, QoiImage};

#[cfg(feature = "registry")]
pub use registry::{__oxideav_entry, register, register_codecs, register_containers};

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a vivid w×h checkerboard so every chunk type gets at
    /// least some exercise.
    fn rgba_checker(w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let q = ((x & 1) + 2 * (y & 1)) as usize;
                let rgba = [
                    [255, 0, 0, 255],
                    [0, 255, 0, 255],
                    [0, 0, 255, 200],
                    [255, 255, 255, 128],
                ][q];
                data.extend_from_slice(&rgba);
            }
        }
        data
    }

    #[test]
    fn header_layout_is_14_bytes() {
        let pixels = rgba_checker(4, 3);
        let bytes = encode_qoi(4, 3, 4, &pixels);
        assert_eq!(&bytes[0..4], MAGIC);
        assert_eq!(
            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            4
        );
        assert_eq!(
            u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            3
        );
        assert_eq!(bytes[12], 4); // channels
        assert_eq!(bytes[13], 0); // colorspace = sRGB+linear-alpha by default
    }

    #[test]
    fn end_marker_present() {
        let pixels = rgba_checker(4, 3);
        let bytes = encode_qoi(4, 3, 4, &pixels);
        assert_eq!(&bytes[bytes.len() - 8..], END_MARKER);
    }

    #[test]
    fn roundtrip_rgba() {
        let pixels = rgba_checker(16, 12);
        let bytes = encode_qoi(16, 12, 4, &pixels);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.width, 16);
        assert_eq!(back.height, 12);
        assert_eq!(back.channels, QoiChannels::Rgba);
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn roundtrip_rgb() {
        // Same checker but drop the alpha byte.
        let mut data = Vec::with_capacity(16 * 12 * 3);
        for y in 0..12 {
            for x in 0..16 {
                let q = ((x & 1) + 2 * (y & 1)) as usize;
                let rgb = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]][q];
                data.extend_from_slice(&rgb);
            }
        }
        let bytes = encode_qoi(16, 12, 3, &data);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.width, 16);
        assert_eq!(back.height, 12);
        assert_eq!(back.channels, QoiChannels::Rgb);
        assert_eq!(back.pixels, data);
    }

    #[test]
    fn solid_color_uses_runs() {
        // 200 pixels of solid (200,50,25,255). After the first chunk
        // we expect ceil(199/62) = 4 RUN chunks to cover the rest.
        let pixels = [200u8, 50, 25, 255].repeat(200);
        let bytes = encode_qoi(200, 1, 4, &pixels);
        // Header (14) + at most 5 (first chunk) + 4 RUN bytes (1 each)
        // + end marker (8) = 31 bytes upper bound.
        assert!(bytes.len() <= 31, "encoded length: {}", bytes.len());
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn diff_chunk_path() {
        // Walk through small per-channel deltas. The very first pixel
        // (10,10,10,255) jumps from prev (0,0,0,255) by dr=dg=db=10 —
        // dg fits ±32 and dr-dg = db-dg = 0 ∈ ±8, so the first chunk
        // picks LUMA (2 bytes) rather than DIFF. The remaining three
        // pixels stay within ±2 per channel and pick DIFF.
        let pixels = vec![
            10, 10, 10, 255, 11, 10, 9, 255, 12, 11, 10, 255, 11, 12, 11, 255,
        ];
        let bytes = encode_qoi(4, 1, 4, &pixels);
        // Header (14) + 1 LUMA (2) + 3 DIFF (1 each) + marker (8) = 27.
        assert_eq!(bytes.len(), 27);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn luma_chunk_path() {
        // dg outside ±2 (so DIFF doesn't fit) but inside ±32 (LUMA does).
        // Single jump of dg = 10; dr = 10, db = 10 → dr-dg = 0,
        // db-dg = 0, all in ±8. Both pixels pick LUMA: the first
        // because (10,10,10,255) − (0,0,0,255) is dg=10, the second
        // because (20,20,20,255) − (10,10,10,255) is dg=10.
        let pixels = vec![10, 10, 10, 255, 20, 20, 20, 255];
        let bytes = encode_qoi(2, 1, 4, &pixels);
        // Header + 2 LUMA (2 each) + marker (8) = 26.
        assert_eq!(bytes.len(), 26);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn rgb_chunk_path_when_alpha_unchanged() {
        // First pixel (10,10,10,255) picks LUMA (dg=10, dr-dg=db-dg=0).
        // Second pixel (200,50,25,255) jumps too far for LUMA — dg=40
        // is outside ±32 — so it falls through to RGB.
        let pixels = vec![10, 10, 10, 255, 200, 50, 25, 255];
        let bytes = encode_qoi(2, 1, 4, &pixels);
        // Header + LUMA (2) + RGB (4) + marker (8) = 28.
        assert_eq!(bytes.len(), 28);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn rgba_chunk_path_when_alpha_changes() {
        // First pixel (10,10,10,255) picks LUMA. Second pixel changes
        // alpha 255 → 100, which forces RGBA.
        let pixels = vec![10, 10, 10, 255, 10, 10, 10, 100];
        let bytes = encode_qoi(2, 1, 4, &pixels);
        // Header + LUMA (2) + RGBA (5) + marker (8) = 29.
        assert_eq!(bytes.len(), 29);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn index_chunk_path() {
        // Two pixels: A then B then A again. The third pixel should be
        // encoded as a 1-byte INDEX chunk (because A's slot in the
        // running array now equals A). Both A and B require RGB
        // (their channel deltas don't fit DIFF or LUMA).
        let pixels = vec![200, 50, 25, 255, 10, 200, 70, 255, 200, 50, 25, 255];
        let bytes = encode_qoi(3, 1, 4, &pixels);
        // Header + 2 RGB chunks (4 each = 8) + 1 INDEX (1) + marker (8) = 31.
        assert_eq!(bytes.len(), 31);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn run_caps_at_62_then_starts_new_run() {
        // 100 identical pixels at (5,6,7,255). First pixel encodes as
        // LUMA from prev (0,0,0,255): dg=6, dr-dg=-1, db-dg=1 — fits.
        // Remaining 99 pixels = ceil(99/62) = 2 RUN chunks (one of 62,
        // one of 37).
        let pixels = [5u8, 6, 7, 255].repeat(100);
        let bytes = encode_qoi(100, 1, 4, &pixels);
        // = 14 + 2 + 2 + 8 = 26.
        assert_eq!(bytes.len(), 26);
        let runs: Vec<u8> = bytes
            .iter()
            .copied()
            .filter(|b| (*b & 0xC0) == OP_RUN && *b != OP_RGB && *b != OP_RGBA)
            .collect();
        assert!(runs.contains(&(OP_RUN | (62 - 1))));
        assert!(runs.contains(&(OP_RUN | (37 - 1))));
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[0] = b'X';
        assert!(matches!(parse_qoi(&bytes), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn parse_rejects_bad_channels() {
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[12] = 5;
        assert!(matches!(parse_qoi(&bytes), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn parse_rejects_bad_colorspace() {
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[13] = 7;
        assert!(matches!(parse_qoi(&bytes), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn parse_rejects_missing_end_marker() {
        let bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        // Strip the end marker.
        let truncated = &bytes[..bytes.len() - 8];
        assert!(matches!(
            parse_qoi(truncated),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn parse_rejects_zero_dimension() {
        // Hand-craft a header with width=0.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(4);
        bytes.push(0);
        bytes.extend_from_slice(END_MARKER);
        assert!(matches!(parse_qoi(&bytes), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn hash_function_examples() {
        // Spec: index_position = (R*3 + G*5 + B*7 + A*11) % 64,
        // computed with full-width arithmetic (NOT u8 wrapping).
        // [0,0,0,255]: 11*255 = 2805. 2805 % 64 = 53.
        assert_eq!(decoder::hash([0, 0, 0, 255]), 53);
        // [255,255,255,255]: 255*(3+5+7+11) = 6630. 6630 % 64 = 38.
        assert_eq!(decoder::hash([255, 255, 255, 255]), 38);
        // [0,0,0,0]: every term is zero.
        assert_eq!(decoder::hash([0, 0, 0, 0]), 0);
        // [1,2,3,4]: 1*3 + 2*5 + 3*7 + 4*11 = 3 + 10 + 21 + 44 = 78.
        // 78 % 64 = 14.
        assert_eq!(decoder::hash([1, 2, 3, 4]), 14);
    }

    #[test]
    fn end_marker_constant_is_correct() {
        assert_eq!(END_MARKER, &[0, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn single_pixel_image() {
        // Just to make sure the i+1 == pixel_count run-flush kicks in.
        let pixels = vec![1, 2, 3, 4];
        let bytes = encode_qoi(1, 1, 4, &pixels);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn solid_image_first_pixel_starts_run() {
        // First pixel equals initial prev (0,0,0,255). The very first
        // chunk is a RUN — exercises the "run flushes at image end"
        // fallback.
        let pixels = [0u8, 0, 0, 255].repeat(5);
        let bytes = encode_qoi(5, 1, 4, &pixels);
        // Header + 1 RUN of 5 + marker = 23.
        assert_eq!(bytes.len(), 23);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn huge_header_does_not_over_allocate() {
        // Regression for a fuzz-discovered abort: a tiny (~30-byte)
        // file may declare a 65536×65536 RGBA image (≈1 TB of pixels).
        // `width*height*channels` fits `usize` on 64-bit targets, so
        // the old eager `Vec::with_capacity(total_bytes)` asked the
        // allocator for ~1 TB and aborted the process. The decoder
        // must instead reject the stream cleanly: the few chunk bytes
        // present can't possibly fill the claimed pixel count, so it
        // reports a truncated stream rather than allocating.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&65536u32.to_be_bytes()); // width
        bytes.extend_from_slice(&65536u32.to_be_bytes()); // height
        bytes.push(4); // channels = RGBA
        bytes.push(0); // colorspace = sRGB
                       // A single RGB chunk's worth of payload, then the end marker —
                       // nowhere near enough to fill 4.29e9 pixels.
        bytes.push(OP_RGB);
        bytes.extend_from_slice(&[1, 2, 3]);
        bytes.extend_from_slice(END_MARKER);
        // Must return Err (truncated), NOT abort/OOM.
        assert!(matches!(parse_qoi(&bytes), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn decoder_exact_size_buffer_run_only_stream() {
        // Round-183 regression: the decoder's RUN arm now writes
        // through a `chunks_exact_mut + copy_from_slice` filler into a
        // pre-allocated `vec![0; pixel_count * bpp]` buffer instead of
        // calling `push_pixel` N times. Exercise that path with a
        // pure-RUN stream of varying lengths (including the 62-pixel
        // chunk cap + the leftover modulo) to confirm the new filler
        // produces byte-exact output for every modular boundary.
        for w in [1usize, 61, 62, 63, 124, 125, 200] {
            // Solid stream: every pixel equals the seed (0,0,0,255),
            // so the entire image after the first pixel decodes to a
            // sequence of RUN chunks (one per 62-pixel block, plus a
            // tail).
            let pixels = [0u8, 0, 0, 255].repeat(w);
            let bytes = encode_qoi(w as u32, 1, 4, &pixels);
            let back = parse_qoi(&bytes).expect("decode");
            assert_eq!(
                back.pixels, pixels,
                "width={w}: round-trip mismatch on solid stream"
            );
        }
    }

    #[test]
    fn decoder_exact_size_buffer_mixed_stream() {
        // Round-183 regression: the non-RUN chunk arms now write
        // through `write_pixel(&mut [u8], out_pos, …)` instead of
        // appending to a `Vec<u8>`. Exercise every chunk arm with a
        // synthetic stream that drives DIFF / LUMA / RGB / RGBA /
        // INDEX through the new cursor-write path. The sequence is a
        // small palette that hits the INDEX hot path on repeats,
        // forces RGB on a large-delta jump, and forces RGBA on an
        // alpha-changing pixel.
        let pixels: Vec<u8> = vec![
            10, 10, 10, 255, //  LUMA from (0,0,0,255)
            11, 10, 9, 255, //   DIFF (small delta)
            200, 50, 25, 255, // RGB (large delta, alpha unchanged)
            10, 10, 10, 255, //  INDEX (matches the first pixel's slot)
            10, 10, 10, 100, //  RGBA (alpha changed)
        ];
        let bytes = encode_qoi(5, 1, 4, &pixels);
        let back = parse_qoi(&bytes).expect("decode");
        assert_eq!(back.pixels, pixels);
        // Also confirm width / height land back correctly through the
        // exact-size pre-allocation path.
        assert_eq!(back.width, 5);
        assert_eq!(back.height, 1);
    }

    #[test]
    fn encoder_exact_size_buffer_run_only_stream() {
        // Round-205 regression: the encoder's RUN arm now writes the
        // single-byte run tag via an indexed `buf[out_pos] = …` store
        // into a pre-allocated upper-bound buffer instead of `Vec::push`.
        // Exercise that path with a pure-RUN stream of varying lengths
        // (including the 62-pixel chunk cap + the leftover modulo) to
        // confirm the new cursor write produces byte-exact output for
        // every modular boundary that crosses the cap. Also covers the
        // first-pixel-equals-seed fast path where the very first chunk
        // is a RUN (no LUMA / DIFF preface).
        for w in [1usize, 61, 62, 63, 124, 125, 200] {
            let pixels = [0u8, 0, 0, 255].repeat(w);
            let bytes = encode_qoi(w as u32, 1, 4, &pixels);
            // Header (14) + ceil(w / 62) RUN bytes + end marker (8).
            let expected_len = 14 + w.div_ceil(62) + 8;
            assert_eq!(
                bytes.len(),
                expected_len,
                "width={w}: solid-RUN encoded length mismatch"
            );
            let back = parse_qoi(&bytes).expect("decode");
            assert_eq!(
                back.pixels, pixels,
                "width={w}: round-trip mismatch on solid stream"
            );
        }
    }

    #[test]
    fn encoder_exact_size_buffer_mixed_stream() {
        // Round-205 regression: the non-RUN chunk arms now write
        // through `buf[out_pos] = …` + `buf[out_pos..].copy_from_slice`
        // stores instead of `Vec::push` / `extend_from_slice`. Exercise
        // every chunk arm with a synthetic stream that drives DIFF /
        // LUMA / RGB / RGBA / INDEX through the new cursor path. The
        // sequence is a small palette that hits the INDEX hot path on
        // repeats, forces RGB on a large-delta jump, and forces RGBA
        // on an alpha-changing pixel.
        let pixels: Vec<u8> = vec![
            10, 10, 10, 255, //  LUMA from (0,0,0,255)
            11, 10, 9, 255, //   DIFF (small delta)
            200, 50, 25, 255, // RGB (large delta, alpha unchanged)
            10, 10, 10, 255, //  INDEX (matches the first pixel's slot)
            10, 10, 10, 100, //  RGBA (alpha changed)
        ];
        let bytes = encode_qoi(5, 1, 4, &pixels);
        // Header (14) + LUMA (2) + DIFF (1) + RGB (4) + INDEX (1) +
        // RGBA (5) + end marker (8) = 35.
        assert_eq!(bytes.len(), 35);
        let back = parse_qoi(&bytes).expect("decode");
        assert_eq!(back.pixels, pixels);
        assert_eq!(back.width, 5);
        assert_eq!(back.height, 1);
    }

    #[test]
    fn encoder_truncates_to_actual_len() {
        // Round-205 regression: the encoder pre-allocates an
        // upper-bound `vec![0; 14 + n*5 + 8]` and truncates down to
        // the actual produced length before return. For a heavily
        // compressed input (solid fill — runs cap at 62 so a 200-pixel
        // stream encodes to a tiny payload) the returned Vec's `len`
        // must equal the encoded size, NOT the upper-bound capacity.
        let pixels = [200u8, 50, 25, 255].repeat(200);
        let bytes = encode_qoi(200, 1, 4, &pixels);
        // Worst case would be 14 + 200*5 + 8 = 1022. Actual: first
        // pixel is an RGB chunk (4 bytes), then ceil(199/62) = 4 RUN
        // bytes, then end marker — well under the upper bound. The
        // returned Vec's len() must reflect the truncated size.
        assert!(
            bytes.len() < 14 + 200 * 5 + 8,
            "encoded length {} should be much less than worst-case upper bound",
            bytes.len()
        );
        // Sanity: every byte after position bytes.len() in the original
        // upper-bound allocation would have been the zero-fill from
        // `vec![0u8; cap]`. Truncation must drop those — otherwise the
        // decoder would see trailing zero bytes between the last chunk
        // and the end marker and reject the stream.
        let back = parse_qoi(&bytes).expect("decode");
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn colorspace_all_linear_roundtrips() {
        let pixels = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let bytes = encode_qoi_full(2, 1, 4, /* colorspace */ 1, &pixels);
        assert_eq!(bytes[13], 1);
        let back = parse_qoi(&bytes).unwrap();
        assert_eq!(back.colorspace, QoiColorspace::AllLinear);
        assert_eq!(back.pixels, pixels);
    }
}
