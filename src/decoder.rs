//! QOI byte-stream decoder.
//!
//! Implements the "Decoder" half of the one-page qoiformat.org
//! specification: 14-byte header, six chunk encodings, 64-entry
//! running pixel array indexed by `(R*3 + G*5 + B*7 + A*11) % 64`,
//! 8-byte end marker.
//!
//! The decoder runs over the input in a single linear pass and never
//! seeks. Every chunk is dispatched on the leading byte (8-bit tags
//! `0xfe` / `0xff` shadow the 2-bit `11` RUN tag values 62 / 63 — see
//! [`Chunk::from_tag`]). On success it returns one [`QoiImage`] with a
//! tightly-packed `width * height * channels` pixel buffer.
//!
//! Decoded output is *always* lossless; QOI carries no quantisation
//! parameters and never throws bits away. The `colorspace` byte at
//! header offset 13 is informational — both values yield identical
//! pixel bytes.

use crate::error::{QoiError as Error, Result};
use crate::image::{QoiChannels, QoiColorspace, QoiHeader, QoiImage};
use crate::{END_MARKER, HEADER_SIZE, MAGIC, OP_DIFF, OP_INDEX, OP_LUMA, OP_RGB, OP_RGBA, OP_RUN};

#[cfg(feature = "registry")]
use oxideav_core::Decoder;
#[cfg(feature = "registry")]
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, VideoFrame, VideoPlane};

// ---------------------------------------------------------------------------
// Public standalone API
// ---------------------------------------------------------------------------

/// Cheap header-only probe of a QOI byte slice.
///
/// Validates the same 14-byte header [`parse_qoi`] would and returns
/// the parsed metadata — width, height, channels, colorspace — without
/// touching the chunk stream or allocating a pixel buffer. The post-
/// header body is *not* inspected: a file whose header parses
/// successfully here can still fail [`parse_qoi`] later if the chunk
/// stream is truncated or the trailing end marker is missing/wrong.
///
/// Intended for thumbnail-grid probing, pixel-buffer pre-sizing, and
/// per-application limit checks (e.g. "reject any `.qoi` larger than
/// 8K × 8K before allocating a decode buffer") where decoding the full
/// pixel stream would be wasteful.
///
/// Returns [`QoiError::InvalidData`] for the same header-level errors
/// `parse_qoi` does:
/// * input shorter than the 14-byte header,
/// * leading bytes ≠ `qoif`,
/// * `channels` field ≠ 3 and ≠ 4,
/// * `colorspace` field ≠ 0 and ≠ 1,
/// * width or height = 0.
///
/// Note `parse_qoi_header` accepts inputs as short as 14 bytes (the
/// header alone). `parse_qoi` rejects anything shorter than
/// `14 + 8 = 22` bytes because it also requires the trailing end
/// marker; the header probe does not.
pub fn parse_qoi_header(input: &[u8]) -> Result<QoiHeader> {
    parse_header_only(input)
}

/// Internal header-only parser shared by [`parse_qoi`] and
/// [`parse_qoi_header`]. Single source of truth for the per-field
/// validity tests so a future spec clarification (e.g. a new colorspace
/// value) propagates to both entry points in one edit.
fn parse_header_only(input: &[u8]) -> Result<QoiHeader> {
    if input.len() < HEADER_SIZE {
        return Err(Error::invalid("QOI: input shorter than 14-byte header"));
    }
    if &input[0..4] != MAGIC {
        return Err(Error::invalid("QOI: missing 'qoif' magic"));
    }
    let width = u32::from_be_bytes([input[4], input[5], input[6], input[7]]);
    let height = u32::from_be_bytes([input[8], input[9], input[10], input[11]]);
    let channels_byte = input[12];
    let colorspace_byte = input[13];

    let channels = match channels_byte {
        3 => QoiChannels::Rgb,
        4 => QoiChannels::Rgba,
        other => {
            return Err(Error::invalid(format!(
                "QOI: header.channels = {other} (must be 3 or 4)"
            )))
        }
    };
    let colorspace = match colorspace_byte {
        0 => QoiColorspace::SrgbWithLinearAlpha,
        1 => QoiColorspace::AllLinear,
        other => {
            return Err(Error::invalid(format!(
                "QOI: header.colorspace = {other} (must be 0 or 1)"
            )))
        }
    };
    if width == 0 || height == 0 {
        return Err(Error::invalid("QOI: zero dimension in header"));
    }

    Ok(QoiHeader {
        width,
        height,
        channels,
        colorspace,
    })
}

/// Decode a complete QOI file (`qoif` header + chunks + end marker)
/// into a [`QoiImage`].
///
/// Returns [`QoiError::InvalidData`] for any of:
/// * input shorter than the 14-byte header,
/// * leading bytes ≠ `qoif`,
/// * `channels` field ≠ 3 and ≠ 4,
/// * `colorspace` field ≠ 0 and ≠ 1,
/// * width or height = 0,
/// * any chunk runs past the end of the stream,
/// * the trailing 8-byte end marker is missing or wrong.
pub fn parse_qoi(input: &[u8]) -> Result<QoiImage> {
    if input.len() < HEADER_SIZE + END_MARKER.len() {
        return Err(Error::invalid(
            "QOI: input shorter than header + end marker",
        ));
    }
    let QoiHeader {
        width,
        height,
        channels,
        colorspace,
    } = parse_header_only(input)?;

    // Guard against width * height * channels overflowing usize on
    // unusual targets (the spec permits up to u32::MAX * u32::MAX,
    // which clearly exceeds any realistic memory limit). We reject
    // the request before allocating. Note this only rejects values
    // that don't *fit* `usize`; a value that fits usize but exceeds
    // available RAM (e.g. 65536×65536 ≈ 1 TB) is handled by the
    // bounded pre-allocation below, not here.
    let pixel_count = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| Error::unsupported("QOI: width*height overflows u64"))?;
    let bytes_per_pixel = channels as u64;
    let total_bytes = pixel_count
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| Error::unsupported("QOI: width*height*channels overflows u64"))?;
    // The success of this conversion is the guard; the value itself is
    // not used for sizing (see the bounded reservation below).
    let _total_bytes_usize: usize = total_bytes
        .try_into()
        .map_err(|_| Error::unsupported("QOI: total pixel bytes overflows usize"))?;
    let pixel_count_usize: usize = pixel_count
        .try_into()
        .map_err(|_| Error::unsupported("QOI: pixel count overflows usize"))?;

    let chunks = &input[HEADER_SIZE..input.len() - END_MARKER.len()];
    let trailer = &input[input.len() - END_MARKER.len()..];
    if trailer != END_MARKER {
        return Err(Error::invalid("QOI: missing or invalid end marker"));
    }

    // Pre-allocate the output buffer to its EXACT final size (one
    // allocation, zero re-grows) and write through a moving cursor.
    // The capacity reservation can't trust the header's
    // `width * height * channels` directly — a small (≈30-byte) file
    // may claim e.g. 65536×65536 (≈1 TB) and `total_bytes_usize` fits
    // `usize` while vastly exceeding available memory, so a naive
    // `Vec::with_capacity(total_bytes_usize)` aborts the process. The
    // chunk stream physically can't decode to more pixels than
    // `chunks.len() * 62` (one RUN byte emits at most 62 copies; every
    // other op consumes ≥1 byte per pixel), so when the header's
    // claimed pixel count exceeds that cap we reject the stream as
    // truncated up-front rather than over-allocating. Once past the
    // guard the exact-size `vec![0; bytes_per_pixel * pixel_count]`
    // gives every write a known in-bounds slot — no per-pixel
    // `push` bounds checks, no mid-loop reallocation. The RUN arm
    // copies a 3-or-4-byte template into a contiguous slice in one
    // `copy_from_slice` per channel layout instead of N per-byte
    // `Vec::push` calls.
    let max_decodable_pixels = chunks.len().saturating_mul(62);
    if pixel_count_usize > max_decodable_pixels {
        return Err(Error::invalid("QOI: chunk stream truncated mid-image"));
    }
    let bpp = channels as usize;
    let total_out_bytes = pixel_count_usize * bpp;
    let mut pixels = vec![0u8; total_out_bytes];

    // Per-spec initial state: previous pixel = RGBA(0,0,0,255), index
    // array zero-filled.
    let mut prev: [u8; 4] = [0, 0, 0, 255];
    let mut index: [[u8; 4]; 64] = [[0, 0, 0, 0]; 64];

    let mut pos = 0usize;
    // Byte-cursor into `pixels`; advances by `bpp` per emitted pixel.
    let mut out_pos: usize = 0;

    while out_pos < total_out_bytes {
        if pos >= chunks.len() {
            return Err(Error::invalid("QOI: chunk stream truncated mid-image"));
        }
        let tag = chunks[pos];
        pos += 1;

        match Chunk::from_tag(tag) {
            Chunk::Rgb => {
                if pos + 3 > chunks.len() {
                    return Err(Error::invalid("QOI: QOI_OP_RGB truncated"));
                }
                prev[0] = chunks[pos];
                prev[1] = chunks[pos + 1];
                prev[2] = chunks[pos + 2];
                // Alpha unchanged.
                pos += 3;
                write_pixel(&mut pixels, out_pos, channels, prev);
                out_pos += bpp;
                index[hash(prev) as usize] = prev;
            }
            Chunk::Rgba => {
                if pos + 4 > chunks.len() {
                    return Err(Error::invalid("QOI: QOI_OP_RGBA truncated"));
                }
                prev[0] = chunks[pos];
                prev[1] = chunks[pos + 1];
                prev[2] = chunks[pos + 2];
                prev[3] = chunks[pos + 3];
                pos += 4;
                write_pixel(&mut pixels, out_pos, channels, prev);
                out_pos += bpp;
                index[hash(prev) as usize] = prev;
            }
            Chunk::Index => {
                let idx = (tag & 0x3F) as usize;
                prev = index[idx];
                write_pixel(&mut pixels, out_pos, channels, prev);
                out_pos += bpp;
                // Index already holds prev — re-storing is a no-op but
                // keeps the loop body symmetric with the other arms.
                index[hash(prev) as usize] = prev;
            }
            Chunk::Diff => {
                // 2 bits each, biased by 2 → range −2..+1.
                let dr = ((tag >> 4) & 0x03) as i32 - 2;
                let dg = ((tag >> 2) & 0x03) as i32 - 2;
                let db = (tag & 0x03) as i32 - 2;
                prev[0] = prev[0].wrapping_add(dr as u8);
                prev[1] = prev[1].wrapping_add(dg as u8);
                prev[2] = prev[2].wrapping_add(db as u8);
                // Alpha unchanged.
                write_pixel(&mut pixels, out_pos, channels, prev);
                out_pos += bpp;
                index[hash(prev) as usize] = prev;
            }
            Chunk::Luma => {
                if pos >= chunks.len() {
                    return Err(Error::invalid("QOI: QOI_OP_LUMA truncated"));
                }
                let dg = (tag & 0x3F) as i32 - 32;
                let b2 = chunks[pos];
                pos += 1;
                let dr_dg = ((b2 >> 4) & 0x0F) as i32 - 8;
                let db_dg = (b2 & 0x0F) as i32 - 8;
                let dr = dr_dg + dg;
                let db = db_dg + dg;
                prev[0] = prev[0].wrapping_add(dr as u8);
                prev[1] = prev[1].wrapping_add(dg as u8);
                prev[2] = prev[2].wrapping_add(db as u8);
                // Alpha unchanged.
                write_pixel(&mut pixels, out_pos, channels, prev);
                out_pos += bpp;
                index[hash(prev) as usize] = prev;
            }
            Chunk::Run => {
                // 6-bit (run - 1) → real run length 1..=62. Tag values
                // 0xfe / 0xff are stolen by the 8-bit RGB / RGBA tags.
                let run = (tag & 0x3F) as usize + 1;
                let run_bytes = run * bpp;
                if out_pos + run_bytes > total_out_bytes {
                    return Err(Error::invalid("QOI: run overshoots image size"));
                }
                // Fast path: fill the contiguous run slice with the
                // 3-or-4-byte pixel template. One bounds check per
                // pixel inside `copy_from_slice` collapses to one
                // bounds check per run for the outer slice index.
                fill_run(&mut pixels[out_pos..out_pos + run_bytes], channels, prev);
                out_pos += run_bytes;
                // Per spec: "Each pixel that is seen by the encoder
                // and decoder is put into this array at the position
                // formed by [the] hash function." A RUN is *N* copies
                // of `prev` — write it into its hashed slot. This is
                // load-bearing for the RUN that opens a fresh stream
                // before any non-RUN chunk has a chance to populate
                // index[hash(prev)] for the initial (0,0,0,255) pixel.
                index[hash(prev) as usize] = prev;
            }
        }
    }

    if pos != chunks.len() {
        return Err(Error::invalid(
            "QOI: trailing bytes between last chunk and end marker",
        ));
    }

    Ok(QoiImage {
        width,
        height,
        channels,
        colorspace,
        pixels,
        pts: None,
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Dispatch the leading chunk byte to one of the six QOI ops.
///
/// The 8-bit tags `0xfe` (RGB) and `0xff` (RGBA) take precedence over
/// the 2-bit `11` RUN tag — that's why RUN can only encode lengths
/// 1..=62 instead of 1..=64.
enum Chunk {
    Rgb,
    Rgba,
    Index,
    Diff,
    Luma,
    Run,
}

impl Chunk {
    #[inline]
    fn from_tag(tag: u8) -> Self {
        if tag == OP_RGBA {
            Self::Rgba
        } else if tag == OP_RGB {
            Self::Rgb
        } else {
            match tag & 0xC0 {
                OP_INDEX => Self::Index,
                OP_DIFF => Self::Diff,
                OP_LUMA => Self::Luma,
                OP_RUN => Self::Run,
                _ => unreachable!("tag & 0xC0 has only four possible values"),
            }
        }
    }
}

/// QOI hash function — the 64-slot running pixel array's bucket
/// selector. Defined by the spec as `(R*3 + G*5 + B*7 + A*11) % 64`,
/// computed with full-width (non-wrapping) arithmetic.
///
/// E.g. the initial previous pixel `(0,0,0,255)` hashes to
/// `(11 * 255) % 64 = 2805 % 64 = 53`, NOT 21 (which would be the
/// wrapping-u8 answer). Doing the multiply in u8 silently wraps and
/// scrambles the index distribution, so we promote everything to u32.
#[inline]
pub(crate) fn hash(p: [u8; 4]) -> u8 {
    let r = p[0] as u32;
    let g = p[1] as u32;
    let b = p[2] as u32;
    let a = p[3] as u32;
    ((r * 3 + g * 5 + b * 7 + a * 11) & 0x3F) as u8
}

/// Write a pixel into the pre-allocated output buffer at byte offset
/// `out_pos`, in the requested channel layout. Caller is responsible
/// for keeping `out_pos + channels as usize` in bounds.
///
/// Replaces an earlier `push_pixel(&mut Vec<u8>, ...)` helper that did
/// 3 or 4 individual `Vec::push` calls per pixel; the per-push capacity
/// check + len-update was visible cost on the RUN-dominated decode
/// path. The single `copy_from_slice` here folds both arms into one
/// bounds check per pixel.
#[inline]
fn write_pixel(out: &mut [u8], out_pos: usize, channels: QoiChannels, p: [u8; 4]) {
    match channels {
        QoiChannels::Rgb => {
            out[out_pos..out_pos + 3].copy_from_slice(&p[..3]);
        }
        QoiChannels::Rgba => {
            out[out_pos..out_pos + 4].copy_from_slice(&p);
        }
    }
}

/// Fill an exact-length output slice with `count = slice.len() / bpp`
/// copies of the pixel template `p`, in the requested channel layout.
///
/// Used by `QOI_OP_RUN`. A RUN encodes 1..=62 identical pixels — the
/// previous implementation called `push_pixel` in a loop, paying a
/// per-iteration `Vec::push`-bounds-check cost on what should be a
/// straight-line memcpy. This helper writes the template once per
/// pixel through a single slice index per pixel; the inner per-byte
/// stores collapse to a tight unrolled loop the optimiser is happy
/// to vectorise.
#[inline]
fn fill_run(out: &mut [u8], channels: QoiChannels, p: [u8; 4]) {
    match channels {
        QoiChannels::Rgb => {
            for px in out.chunks_exact_mut(3) {
                px.copy_from_slice(&p[..3]);
            }
        }
        QoiChannels::Rgba => {
            for px in out.chunks_exact_mut(4) {
                px.copy_from_slice(&p);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Registry-side Decoder trait impl
// ---------------------------------------------------------------------------

#[cfg(feature = "registry")]
pub fn make_decoder(_params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    Ok(Box::new(QoiDecoder {
        codec_id: CodecId::new(crate::CODEC_ID_STR),
        pending: None,
        eof: false,
    }))
}

#[cfg(feature = "registry")]
struct QoiDecoder {
    codec_id: CodecId,
    pending: Option<VideoFrame>,
    eof: bool,
}

#[cfg(feature = "registry")]
impl Decoder for QoiDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }
    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        let image = parse_qoi(&packet.data)?;
        self.pending = Some(image_to_video_frame(image));
        Ok(())
    }
    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        match self.pending.take() {
            Some(f) => Ok(Frame::Video(f)),
            None => {
                if self.eof {
                    Err(oxideav_core::Error::Eof)
                } else {
                    Err(oxideav_core::Error::NeedMore)
                }
            }
        }
    }
    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

#[cfg(feature = "registry")]
fn image_to_video_frame(image: QoiImage) -> VideoFrame {
    let stride = image.width as usize * image.channels as usize;
    VideoFrame {
        pts: image.pts,
        planes: vec![VideoPlane {
            stride,
            data: image.pixels,
        }],
    }
}
