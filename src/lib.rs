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
pub mod ops;
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

pub use decoder::{parse_qoi, parse_qoi_header, parse_qoi_into};
pub use encoder::{encode_qoi, encode_qoi_full, encode_qoi_full_into, encode_qoi_into};
pub use error::{QoiError, Result};
pub use image::{QoiChannels, QoiColorspace, QoiHeader, QoiImage};
pub use ops::{iter_ops, iter_ops_strict, qoi_hash, QoiOp, QoiOpIter};

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
    fn encoder_rgb_path_never_emits_rgba_chunk() {
        // Round-231 regression: with the encoder split into two
        // channel-specialised inner loops, the 3-channel path no
        // longer carries the RGBA emit arm at all. We can't observe
        // unreachable code from outside, but we can observe the
        // contract — for ANY 3-channel input, the encoded byte
        // stream never contains the 0xff OP_RGBA tag in a chunk
        // position. The header bytes 0..14 and end marker bytes
        // last-8..last are excluded since neither contains a
        // chunk-byte 0xff in a well-formed file.
        //
        // Exercise this across all five property-style input
        // generators so the assertion holds regardless of which
        // chunk arms the input would prefer.
        for w in [1u32, 7, 16, 64] {
            for h in [1u32, 5, 16] {
                let n = (w as usize) * (h as usize);
                // Smooth-ish RGB input — covers DIFF + LUMA arms.
                let mut pixels = vec![0u8; n * 3];
                for (i, slot) in pixels.iter_mut().enumerate() {
                    *slot = ((i as u32).wrapping_mul(37) ^ 0x5a) as u8;
                }
                let bytes = encode_qoi(w, h, 3, &pixels);
                let chunks = &bytes[14..bytes.len() - 8];
                // Walk the chunk stream and confirm no leading tag
                // is OP_RGBA. We don't try to fully decode chunks
                // here — we only check that the first byte of each
                // dispatched chunk is not 0xff. A bare scan over
                // the slice would over-count (0xff bytes can occur
                // inside RGB / LUMA payloads); instead we walk the
                // exact chunk shapes the spec defines.
                let mut pos = 0;
                while pos < chunks.len() {
                    let tag = chunks[pos];
                    assert_ne!(
                        tag, OP_RGBA,
                        "w={w} h={h}: 3-channel encode emitted an \
                         OP_RGBA chunk at offset {pos}"
                    );
                    pos += match tag {
                        OP_RGB => 4, // tag + 3 body bytes
                        // No RGBA in this stream — defensively
                        // panic if we somehow see one.
                        OP_RGBA => unreachable!(),
                        other => match other & 0xC0 {
                            OP_INDEX | OP_DIFF | OP_RUN => 1,
                            OP_LUMA => 2,
                            _ => unreachable!(),
                        },
                    };
                }
                // Round-trip still holds — sanity.
                let back = parse_qoi(&bytes).expect("decode");
                assert_eq!(back.pixels, pixels, "w={w} h={h}: round-trip drift");
            }
        }
    }

    #[test]
    fn encoder_channel_split_preserves_alpha_changing_rgba_path() {
        // Round-231 regression: the channel-split must not have
        // accidentally moved the RGBA emit arm out of the 4-channel
        // path. Encode a stream that forces RGBA on every pixel
        // (alpha changes every pixel + RGB triple stays out of
        // INDEX / DIFF / LUMA range) and confirm the encoder
        // produces n * 5 + 14 + 8 bytes exactly — the worst-case
        // shape only the RGBA emit arm can produce.
        let mut pixels = Vec::with_capacity(64 * 4);
        for i in 0..64u8 {
            // r,g,b cycle through wide deltas; alpha changes every
            // pixel to defeat both INDEX hits AND the alpha-equality
            // fast path.
            pixels.push(i.wrapping_mul(91));
            pixels.push(i.wrapping_mul(43));
            pixels.push(i.wrapping_mul(17));
            pixels.push(i ^ 0x5a);
        }
        let bytes = encode_qoi(64, 1, 4, &pixels);
        // We can't assert exactly n*5 + 22 because the encoder may
        // still find some pixel landing in an INDEX slot from a
        // prior cycle. But the dominant chunk MUST be OP_RGBA for
        // the contract to hold — assert at least 80% of chunk
        // dispatches are 5-byte RGBA chunks.
        let chunks = &bytes[14..bytes.len() - 8];
        let mut total = 0usize;
        let mut rgba = 0usize;
        let mut pos = 0;
        while pos < chunks.len() {
            let tag = chunks[pos];
            total += 1;
            if tag == OP_RGBA {
                rgba += 1;
                pos += 5;
            } else if tag == OP_RGB {
                pos += 4;
            } else {
                pos += match tag & 0xC0 {
                    OP_INDEX | OP_DIFF | OP_RUN => 1,
                    OP_LUMA => 2,
                    _ => unreachable!(),
                };
            }
        }
        assert!(
            rgba * 100 / total >= 80,
            "expected the RGBA emit arm to dominate, got {rgba}/{total}"
        );
        // Round-trip still holds.
        let back = parse_qoi(&bytes).expect("decode");
        assert_eq!(back.pixels, pixels);
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
    fn parse_header_extracts_metadata_without_body_walk() {
        // Round-210 depth-mode: header-only probe agrees with the full
        // decode on (width, height, channels, colorspace). The header
        // probe doesn't walk the chunk stream, so this also confirms
        // the four fields live in the same byte offsets the spec lays
        // out (0..4 magic, 4..8 width BE, 8..12 height BE, 12 channels,
        // 13 colorspace).
        let pixels = rgba_checker(16, 12);
        let bytes = encode_qoi(16, 12, 4, &pixels);
        let hdr = decoder::parse_qoi_header(&bytes).expect("header parse");
        assert_eq!(hdr.width, 16);
        assert_eq!(hdr.height, 12);
        assert_eq!(hdr.channels, QoiChannels::Rgba);
        assert_eq!(hdr.colorspace, QoiColorspace::SrgbWithLinearAlpha);

        // And the full decode agrees byte-for-byte on the same fields.
        let img = parse_qoi(&bytes).unwrap();
        assert_eq!(img.width, hdr.width);
        assert_eq!(img.height, hdr.height);
        assert_eq!(img.channels, hdr.channels);
        assert_eq!(img.colorspace, hdr.colorspace);
    }

    #[test]
    fn parse_header_accepts_14_byte_input() {
        // Header probe must accept a 14-byte slice (the bare header)
        // even though `parse_qoi` rejects anything shorter than
        // 14 + 8 = 22 bytes (header + end marker). This is the headline
        // use case: probe metadata without committing to a full decode.
        let mut bytes = Vec::with_capacity(HEADER_SIZE);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&3u32.to_be_bytes()); // width
        bytes.extend_from_slice(&5u32.to_be_bytes()); // height
        bytes.push(3); // channels = RGB
        bytes.push(1); // colorspace = all linear
        assert_eq!(bytes.len(), HEADER_SIZE);
        let hdr = decoder::parse_qoi_header(&bytes).expect("14B header probe");
        assert_eq!(hdr.width, 3);
        assert_eq!(hdr.height, 5);
        assert_eq!(hdr.channels, QoiChannels::Rgb);
        assert_eq!(hdr.colorspace, QoiColorspace::AllLinear);

        // Same bytes rejected by `parse_qoi` because no end marker.
        assert!(matches!(parse_qoi(&bytes), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn parse_header_rejects_short_input() {
        // Anything shorter than 14 bytes is rejected up-front — no
        // partial header parses, no panics on bounds-checked slicing.
        for n in 0..HEADER_SIZE {
            let buf = vec![b'q'; n];
            assert!(
                matches!(
                    decoder::parse_qoi_header(&buf),
                    Err(QoiError::InvalidData(_))
                ),
                "len={n} should be rejected"
            );
        }
    }

    #[test]
    fn parse_header_rejects_bad_magic() {
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[0] = b'X';
        assert!(matches!(
            decoder::parse_qoi_header(&bytes),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn parse_header_rejects_bad_channels() {
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[12] = 5;
        assert!(matches!(
            decoder::parse_qoi_header(&bytes),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn parse_header_rejects_bad_colorspace() {
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[13] = 7;
        assert!(matches!(
            decoder::parse_qoi_header(&bytes),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn parse_header_rejects_zero_dimension() {
        // Header probe enforces the same zero-dimension reject as the
        // full decoder so consumers using the probe to pre-size a
        // buffer never get a `Some((0, 0))` they have to special-case.
        let mut bytes = Vec::with_capacity(HEADER_SIZE);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&0u32.to_be_bytes()); // width = 0
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(4);
        bytes.push(0);
        assert!(matches!(
            decoder::parse_qoi_header(&bytes),
            Err(QoiError::InvalidData(_))
        ));

        bytes[4..8].copy_from_slice(&1u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&0u32.to_be_bytes()); // height = 0
        assert!(matches!(
            decoder::parse_qoi_header(&bytes),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn parse_header_does_not_inspect_body_or_end_marker() {
        // Documented contract: header probe ignores everything past
        // byte 14. A file with a valid header followed by entirely
        // garbage body — including a missing/wrong end marker — must
        // still parse the header successfully. Callers that need the
        // body's well-formedness call `parse_qoi`.
        let mut bytes = Vec::with_capacity(HEADER_SIZE + 16);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&4u32.to_be_bytes());
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.push(4);
        bytes.push(0);
        // Garbage tail — not a valid chunk stream + end marker.
        bytes.extend_from_slice(&[0xab; 16]);
        let hdr = decoder::parse_qoi_header(&bytes).expect("header still parses");
        assert_eq!(hdr.width, 4);
        assert_eq!(hdr.height, 3);
        // `parse_qoi` on the same bytes fails: the trailing 8 bytes
        // aren't the spec's `00 00 00 00 00 00 00 01` end marker.
        assert!(parse_qoi(&bytes).is_err());
    }

    #[test]
    fn parse_header_qoi_header_is_copy() {
        // QoiHeader carries only POD fields, so it implements `Copy`.
        // This lets consumers stash it into per-thread scratch state
        // (e.g. a thumbnail grid's per-cell metadata cache) without
        // worrying about move semantics or `Clone`-call overhead. We
        // assert the trait bound here so a future field addition that
        // silently breaks `Copy` (e.g. adding a `String`) gets caught
        // by the build, not by a downstream consumer.
        fn _is_copy<T: Copy>() {}
        _is_copy::<QoiHeader>();
    }

    #[test]
    fn parse_header_on_reference_fixture_agrees_with_full_decode() {
        // Round-210 regression: every byte-exact reference fixture
        // (decoded by `tests/reference_fixtures.rs`) reports the same
        // header metadata through the probe as through the full decode.
        // We don't include the fixture bytes here (they live in
        // `tests/fixtures/`); this asserts the contract on hand-rolled
        // headers that mirror them.
        // (i)  edgecase-like:  RGBA, sRGB, tiny dims.
        // (ii) testcard-like:  RGB,  sRGB, modest dims.
        // (iii) all-linear:    RGBA, linear, square dims.
        for (w, h, ch, cs) in [
            (256u32, 64u32, 4u8, 0u8),
            (256, 256, 3, 0),
            (128, 128, 4, 1),
        ] {
            let mut bytes = Vec::with_capacity(HEADER_SIZE);
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&w.to_be_bytes());
            bytes.extend_from_slice(&h.to_be_bytes());
            bytes.push(ch);
            bytes.push(cs);
            let hdr = decoder::parse_qoi_header(&bytes).expect("synthetic header should parse");
            assert_eq!(hdr.width, w);
            assert_eq!(hdr.height, h);
            assert_eq!(
                hdr.channels,
                if ch == 4 {
                    QoiChannels::Rgba
                } else {
                    QoiChannels::Rgb
                }
            );
            assert_eq!(
                hdr.colorspace,
                if cs == 1 {
                    QoiColorspace::AllLinear
                } else {
                    QoiColorspace::SrgbWithLinearAlpha
                }
            );
        }
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

    // -----------------------------------------------------------------
    // Round-225 depth-mode: caller-owned-buffer `_into` API surface.
    // -----------------------------------------------------------------

    #[test]
    fn encode_qoi_into_matches_encode_qoi_byte_for_byte() {
        // The `_into` variant must produce identical bytes to the
        // allocating wrapper — same chunk priority chain, same end
        // marker, same header. The only difference is whether the
        // backing allocation was caller-owned or fresh.
        let pixels = rgba_checker(16, 12);
        let owned = encode_qoi(16, 12, 4, &pixels);
        let mut buf = Vec::new();
        encode_qoi_into(&mut buf, 16, 12, 4, &pixels);
        assert_eq!(owned, buf);
    }

    #[test]
    fn encode_qoi_full_into_matches_encode_qoi_full_byte_for_byte() {
        // Same byte-equivalence contract for the colorspace-explicit
        // variant. The all-linear (`colorspace=1`) header byte must
        // propagate through the `_into` path unchanged.
        let pixels = vec![10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 100];
        let owned = encode_qoi_full(3, 1, 4, /* colorspace */ 1, &pixels);
        let mut buf = Vec::new();
        encode_qoi_full_into(&mut buf, 3, 1, 4, /* colorspace */ 1, &pixels);
        assert_eq!(owned, buf);
        assert_eq!(buf[13], 1);
    }

    #[test]
    fn encode_qoi_into_reuses_buffer_across_calls() {
        // The headline benefit of `_into`: a buffer whose capacity
        // already covers the worst-case bound for the next image
        // doesn't trigger a fresh allocation. We can't observe
        // "did the allocator run" directly, but we can observe the
        // contract: after a large encode, capacity sticks; after a
        // small encode using the same buffer, capacity is still at
        // least the prior worst case, and the encoded bytes match
        // the fresh-allocation encoder.
        let big_pixels = rgba_checker(32, 32);
        let small_pixels = rgba_checker(4, 4);

        let mut buf = Vec::new();
        encode_qoi_into(&mut buf, 32, 32, 4, &big_pixels);
        let big_cap = buf.capacity();
        let big_bytes = buf.clone();

        // Now encode a smaller image into the same buffer. The
        // capacity must be at least the prior `big_cap` (i.e. the
        // allocation is retained, not shrunk) and the output must
        // match a fresh-allocation encode of the small input.
        encode_qoi_into(&mut buf, 4, 4, 4, &small_pixels);
        assert!(
            buf.capacity() >= big_cap,
            "encode_qoi_into shrank capacity from {big_cap} to {} between \
             calls — defeats the buffer-reuse contract",
            buf.capacity()
        );
        let small_owned = encode_qoi(4, 4, 4, &small_pixels);
        assert_eq!(buf, small_owned);

        // And the big encode is unaffected by the reuse path.
        let big_owned = encode_qoi(32, 32, 4, &big_pixels);
        assert_eq!(big_bytes, big_owned);
    }

    #[test]
    fn encode_qoi_into_clears_existing_contents() {
        // A pre-populated buffer must be cleared before the new
        // encode is written — otherwise stale bytes between the
        // (former) end marker and the new output would surface as
        // garbage prefixed to the new image.
        let mut buf = vec![0xAB; 1000];
        let pixels = rgba_checker(4, 4);
        encode_qoi_into(&mut buf, 4, 4, 4, &pixels);
        // First four bytes are the QOI magic, not the leftover 0xAB.
        assert_eq!(&buf[0..4], MAGIC);
        // Last 8 bytes are the spec end marker.
        assert_eq!(&buf[buf.len() - 8..], END_MARKER);
        // Round-trip recovers the input pixels.
        let back = parse_qoi(&buf).unwrap();
        assert_eq!(back.pixels, pixels);
    }

    #[test]
    fn parse_qoi_into_matches_parse_qoi_byte_for_byte() {
        // Same pixel bytes, same header metadata. The only
        // difference is that the `_into` path returns the header
        // separately and writes pixels into a caller-owned `Vec`.
        let pixels = rgba_checker(16, 12);
        let bytes = encode_qoi(16, 12, 4, &pixels);

        let owned = parse_qoi(&bytes).expect("decode");
        let mut pix_buf = Vec::new();
        let hdr = parse_qoi_into(&bytes, &mut pix_buf).expect("decode into");

        assert_eq!(hdr.width, owned.width);
        assert_eq!(hdr.height, owned.height);
        assert_eq!(hdr.channels, owned.channels);
        assert_eq!(hdr.colorspace, owned.colorspace);
        assert_eq!(pix_buf, owned.pixels);
    }

    #[test]
    fn parse_qoi_into_reuses_buffer_across_calls() {
        // Decode-side counterpart of the encoder reuse test. After
        // decoding a 32×32 image, the pixel-buffer capacity must
        // stick across a subsequent decode of a 4×4 image — so the
        // allocator is touched once for the largest image seen, not
        // once per call.
        let big_pixels = rgba_checker(32, 32);
        let small_pixels = rgba_checker(4, 4);
        let big_bytes = encode_qoi(32, 32, 4, &big_pixels);
        let small_bytes = encode_qoi(4, 4, 4, &small_pixels);

        let mut pix_buf = Vec::new();
        let _ = parse_qoi_into(&big_bytes, &mut pix_buf).expect("big decode");
        let big_cap = pix_buf.capacity();
        assert_eq!(pix_buf, big_pixels);

        let hdr = parse_qoi_into(&small_bytes, &mut pix_buf).expect("small decode");
        assert!(
            pix_buf.capacity() >= big_cap,
            "parse_qoi_into shrank capacity from {big_cap} to {} between \
             calls — defeats the buffer-reuse contract",
            pix_buf.capacity()
        );
        assert_eq!(hdr.width, 4);
        assert_eq!(hdr.height, 4);
        assert_eq!(hdr.channels, QoiChannels::Rgba);
        assert_eq!(pix_buf, small_pixels);
    }

    #[test]
    fn parse_qoi_into_clears_existing_contents() {
        // A pre-populated pixel buffer must be cleared on entry —
        // otherwise stale bytes past the new image's
        // `width * height * channels` would surface to callers that
        // compute their own row strides off `pix_buf.len()` rather
        // than `width * channels`.
        let mut pix_buf = vec![0xAB; 8192];
        let pixels = rgba_checker(8, 6);
        let bytes = encode_qoi(8, 6, 4, &pixels);
        let hdr = parse_qoi_into(&bytes, &mut pix_buf).expect("decode");
        assert_eq!(hdr.width, 8);
        assert_eq!(hdr.height, 6);
        // Length is exactly `width * height * channels` — no
        // trailing 0xAB bytes from the prior allocation.
        assert_eq!(pix_buf.len(), 8 * 6 * 4);
        assert_eq!(pix_buf, pixels);
    }

    #[test]
    fn parse_qoi_into_propagates_decoder_errors() {
        // Every error path the standard `parse_qoi` reports must
        // also surface through `parse_qoi_into` — same `QoiError`
        // variants, same message shape.
        let mut pix_buf = Vec::new();
        // Bad magic.
        let mut bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        bytes[0] = b'X';
        assert!(matches!(
            parse_qoi_into(&bytes, &mut pix_buf),
            Err(QoiError::InvalidData(_))
        ));
        // Truncated input (shorter than header + end marker).
        let short = vec![b'q', b'o', b'i', b'f'];
        assert!(matches!(
            parse_qoi_into(&short, &mut pix_buf),
            Err(QoiError::InvalidData(_))
        ));
        // Missing end marker.
        let bytes = encode_qoi(2, 1, 4, &[0, 0, 0, 255, 0, 0, 0, 255]);
        let truncated = &bytes[..bytes.len() - 8];
        assert!(matches!(
            parse_qoi_into(truncated, &mut pix_buf),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn into_apis_roundtrip_under_buffer_reuse() {
        // End-to-end: encode into a reusable buffer, decode into
        // another reusable buffer, both reused across two distinct
        // images. The round-trip contract — same pixel bytes out as
        // in — must hold per-image regardless of what the buffers
        // happened to contain from prior calls.
        let img_a = rgba_checker(7, 5);
        let img_b = rgba_checker(13, 9);

        let mut enc_buf = Vec::new();
        let mut dec_buf = Vec::new();

        encode_qoi_into(&mut enc_buf, 7, 5, 4, &img_a);
        let hdr = parse_qoi_into(&enc_buf, &mut dec_buf).expect("decode A");
        assert_eq!(hdr.width, 7);
        assert_eq!(hdr.height, 5);
        assert_eq!(dec_buf, img_a);

        encode_qoi_into(&mut enc_buf, 13, 9, 4, &img_b);
        let hdr = parse_qoi_into(&enc_buf, &mut dec_buf).expect("decode B");
        assert_eq!(hdr.width, 13);
        assert_eq!(hdr.height, 9);
        assert_eq!(dec_buf, img_b);

        // And a third image smaller than the second: dec_buf shrinks
        // by `len`, not by `capacity`, so the buffer retains the
        // worst-case allocation seen so far.
        let img_c = rgba_checker(4, 4);
        encode_qoi_into(&mut enc_buf, 4, 4, 4, &img_c);
        let _ = parse_qoi_into(&enc_buf, &mut dec_buf).expect("decode C");
        assert_eq!(dec_buf.len(), 4 * 4 * 4);
        assert_eq!(dec_buf, img_c);
    }
}
