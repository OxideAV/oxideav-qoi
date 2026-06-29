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
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, PixelFormat, VideoFrame, VideoPlane};

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
    let mut pixels = Vec::new();
    let hdr = parse_qoi_into(input, &mut pixels)?;
    Ok(QoiImage {
        width: hdr.width,
        height: hdr.height,
        channels: hdr.channels,
        colorspace: hdr.colorspace,
        pixels,
        pts: None,
    })
}

/// Decode into a caller-owned pixel `Vec<u8>`, reusing its existing
/// allocation when large enough, and return the parsed
/// [`QoiHeader`].
///
/// Identical decode contract to [`parse_qoi`] — same byte-for-byte
/// pixel output, same error set — but the decoded pixel buffer is
/// written into `pixels` (cleared first) instead of allocated fresh
/// per call. Designed for tight decode-in-a-loop callers — image
/// pipelines, thumbnail batches, decoder-side benches — that want
/// to amortise the `width * height * channels` allocation across
/// many images of similar dimensions. After a few iterations the
/// buffer has grown to the largest image's pixel size, and every
/// subsequent decode reuses that capacity without a fresh
/// allocation. On return, `pixels.len()` is the exact decoded byte
/// count (`width * height * channels`) and `pixels.capacity()` is
/// whatever the previous worst case was (kept, not shrunk).
///
/// The returned [`QoiHeader`] reports the same `(width, height,
/// channels, colorspace)` tuple [`parse_qoi`] would have produced
/// — useful for callers that want to size further downstream
/// scratch buffers without keeping the full [`QoiImage`] around.
///
/// Both `parse_qoi` and `parse_qoi_into` go through this function,
/// so the decoder hot path is shared in one place. Errors are
/// reported via the same [`QoiError`] variants documented on
/// [`parse_qoi`]; on error, the caller's buffer is left in an
/// unspecified state (it was cleared on entry, then possibly
/// resized to `width * height * channels` zero bytes before the
/// failing chunk arm) and callers should not read from it. The
/// retained `capacity()` is still valid as scratch for the next
/// call.
pub fn parse_qoi_into(input: &[u8], pixels: &mut Vec<u8>) -> Result<QoiHeader> {
    pixels.clear();
    if input.len() < HEADER_SIZE + END_MARKER.len() {
        return Err(Error::invalid(
            "QOI: input shorter than header + end marker",
        ));
    }
    let hdr = parse_header_only(input)?;
    let QoiHeader {
        width,
        height,
        channels,
        colorspace: _,
    } = hdr;

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

    // Pre-size the caller's buffer to its EXACT final length (one
    // length-update, zero re-grows when the existing capacity is
    // large enough — see reuse contract below) and write through a
    // moving cursor. The capacity reservation can't trust the
    // header's `width * height * channels` directly — a small
    // (≈30-byte) file may claim e.g. 65536×65536 (≈1 TB) and
    // `total_bytes_usize` fits `usize` while vastly exceeding
    // available memory, so a naive `resize(total_bytes_usize, 0)`
    // aborts the process. The chunk stream physically can't decode
    // to more pixels than `chunks.len() * 62` (one RUN byte emits
    // at most 62 copies; every other op consumes ≥1 byte per
    // pixel), so when the header's claimed pixel count exceeds
    // that cap we reject the stream as truncated up-front rather
    // than over-allocating. Once past the guard the exact-size
    // resize gives every write a known in-bounds slot — no
    // per-pixel `push` bounds checks, no mid-loop reallocation. The
    // RUN arm copies a 3-or-4-byte template into a contiguous slice
    // in one `copy_from_slice` per channel layout instead of N
    // per-byte `Vec::push` calls.
    //
    // Reuse contract: when called on a previously-decoded buffer
    // whose `capacity()` already covers `total_out_bytes`, the
    // `resize` below is a length-update with no allocator traffic
    // — that's the headline benefit of `parse_qoi_into` over
    // `parse_qoi`.
    let max_decodable_pixels = chunks.len().saturating_mul(62);
    if pixel_count_usize > max_decodable_pixels {
        return Err(Error::invalid("QOI: chunk stream truncated mid-image"));
    }
    let bpp = channels as usize;
    let total_out_bytes = pixel_count_usize * bpp;
    pixels.resize(total_out_bytes, 0u8);

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
                write_pixel(pixels, out_pos, channels, prev);
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
                write_pixel(pixels, out_pos, channels, prev);
                out_pos += bpp;
                index[hash(prev) as usize] = prev;
            }
            Chunk::Index => {
                let idx = (tag & 0x3F) as usize;
                prev = index[idx];
                write_pixel(pixels, out_pos, channels, prev);
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
                write_pixel(pixels, out_pos, channels, prev);
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
                write_pixel(pixels, out_pos, channels, prev);
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

    Ok(hdr)
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
    // Single source of truth lives in `crate::ops::qoi_hash` (the
    // public typed primitive). This crate-internal alias keeps the
    // hot decode/encode call sites terse and stable while delegating
    // the arithmetic — `#[inline]` collapses the indirection so the
    // codegen is identical to an open-coded multiply here.
    crate::ops::qoi_hash(p)
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

/// A decoded QOI image buffered between `send_packet` and the matching
/// `receive_frame` / `receive_arena_frame`. We keep the full metadata
/// (not just a `VideoFrame`) so the arena path can emit a *correct*
/// `FrameHeader` — the generic default `receive_arena_frame` would
/// mislabel a packed RGB(A) plane as `Gray8` and report `width =
/// stride` (= width × channels). Storing the true `(width, height,
/// pixel_format)` lets us override the arena path with accurate values.
#[cfg(feature = "registry")]
struct PendingFrame {
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    /// `width * channels` — the byte stride of the single packed plane.
    stride: usize,
    pts: Option<i64>,
    pixels: Vec<u8>,
}

#[cfg(feature = "registry")]
struct QoiDecoder {
    codec_id: CodecId,
    pending: Option<PendingFrame>,
    eof: bool,
}

#[cfg(feature = "registry")]
impl Decoder for QoiDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }
    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        let image = parse_qoi(&packet.data)?;
        let pixel_format = match image.channels {
            QoiChannels::Rgb => PixelFormat::Rgb24,
            QoiChannels::Rgba => PixelFormat::Rgba,
        };
        let stride = image.width as usize * image.channels as usize;
        // QOI carries no timestamp of its own (the standalone
        // `parse_qoi` always yields `pts: None`). Thread the surrounding
        // `Packet`'s `pts` onto the produced frame so a muxer/player
        // downstream sees the presentation time the container assigned —
        // a `Packet` without a `pts` (`None`) still produces a frame with
        // `pts: None`, unchanged.
        self.pending = Some(PendingFrame {
            width: image.width,
            height: image.height,
            pixel_format,
            stride,
            pts: packet.pts,
            pixels: image.pixels,
        });
        Ok(())
    }
    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        match self.pending.take() {
            Some(f) => Ok(Frame::Video(VideoFrame {
                pts: f.pts,
                planes: vec![VideoPlane {
                    stride: f.stride,
                    data: f.pixels,
                }],
            })),
            None => {
                if self.eof {
                    Err(oxideav_core::Error::Eof)
                } else {
                    Err(oxideav_core::Error::NeedMore)
                }
            }
        }
    }
    fn receive_arena_frame(&mut self) -> oxideav_core::Result<oxideav_core::arena::sync::Frame> {
        use oxideav_core::arena::sync::{ArenaPool, FrameHeader, FrameInner};
        let f = match self.pending.take() {
            Some(f) => f,
            None => {
                return if self.eof {
                    Err(oxideav_core::Error::Eof)
                } else {
                    Err(oxideav_core::Error::NeedMore)
                };
            }
        };
        // QOI is a single packed plane. Build a one-shot arena sized to
        // the pixels and emit a FrameHeader with the TRUE width / height
        // / pixel format — unlike the trait-default `receive_arena_frame`
        // which would label the packed RGB(A) plane `Gray8` and report
        // `width = stride` (width × channels). The one-shot pool drops at
        // end of scope; the returned Frame keeps its leased buffer alive
        // via the Arc<FrameInner>.
        let total_bytes = f.pixels.len();
        let pool = ArenaPool::with_alloc_count_cap(1, total_bytes.max(1), 4);
        let arena = pool.lease()?;
        let dst = arena.alloc::<u8>(total_bytes)?;
        dst.copy_from_slice(&f.pixels);
        let header = FrameHeader::new(f.width, f.height, f.pixel_format, f.pts);
        FrameInner::new(arena, &[(0, total_bytes)], header)
    }
    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
    fn reset(&mut self) -> oxideav_core::Result<()> {
        // Called by the player after a container seek: the decoder must
        // be reusable as if freshly constructed. The trait default
        // implementation routes through `flush()` (which sets `eof`) and
        // never clears it again — a decoder reset that way would report
        // `Eof` on the next `receive_frame` even after a fresh
        // `send_packet`, since the drain loop leaves `eof == true`.
        //
        // QOI carries no cross-packet predictor / overlap state (each
        // packet is a self-contained image), so a correct reset is just
        // "forget the buffered frame and the end-of-stream latch".
        self.pending = None;
        self.eof = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trait-side Decoder behavioural tests.
//
// Every other decoder suite in this crate (`tests/decoder_boundary.rs`,
// `tests/decoder_rejects.rs`, `tests/property_sweep.rs`, …) drives the
// standalone `parse_qoi` function directly. None of them exercises the
// `oxideav_core::Decoder` trait impl — the `send_packet` / `receive_frame`
// state machine, the `NeedMore` / `Eof` protocol, packet-`pts` threading,
// or the `flush` transition. These tests pin that surface. `oxideav_core`
// is already a (feature-gated) dependency of this crate, so they live
// in-crate rather than in `tests/` (which would need a dev-dep on the
// framework crate, banned by workspace policy).
// ---------------------------------------------------------------------------
#[cfg(all(test, feature = "registry"))]
mod registry_decoder_tests {
    use super::*;
    use oxideav_core::{Error, Frame, Packet, TimeBase};

    /// Build a complete 2×2 RGBA QOI byte stream for the decoder tests.
    /// Uses the standalone encoder so the bytes are known-good.
    fn sample_rgba_qoi() -> (Vec<u8>, Vec<u8>) {
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255, // row 0
            0, 0, 255, 255, 255, 255, 255, 255, // row 1
        ];
        let bytes = crate::encode_qoi(2, 2, 4, &pixels);
        (bytes, pixels)
    }

    fn packet_with(data: Vec<u8>, pts: Option<i64>) -> Packet {
        let mut pkt = Packet::new(0, TimeBase::new(1, 1), data);
        pkt.pts = pts;
        pkt
    }

    #[test]
    fn receive_before_packet_is_need_more() {
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        // No packet sent yet, not flushed: the contract is NeedMore.
        match dec.receive_frame() {
            Err(Error::NeedMore) => {}
            other => panic!("expected NeedMore before any packet, got {other:?}"),
        }
    }

    #[test]
    fn send_packet_then_receive_yields_decoded_pixels() {
        let (bytes, pixels) = sample_rgba_qoi();
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, None))
            .expect("send_packet");
        let frame = dec.receive_frame().expect("receive_frame");
        let Frame::Video(vf) = frame else {
            panic!("expected a video frame");
        };
        assert_eq!(vf.planes.len(), 1, "QOI decodes to a single packed plane");
        assert_eq!(vf.planes[0].stride, 2 * 4, "stride = width * channels");
        assert_eq!(vf.planes[0].data, pixels, "decoded pixels are lossless");
        // Draining again with no further packet is NeedMore (the single
        // pending frame was taken).
        match dec.receive_frame() {
            Err(Error::NeedMore) => {}
            other => panic!("expected NeedMore after the only frame drained, got {other:?}"),
        }
    }

    #[test]
    fn packet_pts_is_threaded_onto_the_frame() {
        // Regression: the standalone `parse_qoi` always yields `pts:
        // None`; the trait-side decoder must thread the surrounding
        // packet's pts onto the produced frame so a player/muxer sees
        // the container-assigned presentation time.
        let (bytes, _) = sample_rgba_qoi();
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, Some(4242)))
            .expect("send_packet");
        let Frame::Video(vf) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        assert_eq!(vf.pts, Some(4242), "packet pts must reach the frame");
    }

    #[test]
    fn packet_without_pts_yields_frame_without_pts() {
        let (bytes, _) = sample_rgba_qoi();
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, None))
            .expect("send_packet");
        let Frame::Video(vf) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        assert_eq!(vf.pts, None, "a pts-less packet keeps the frame pts None");
    }

    #[test]
    fn flush_then_receive_is_eof_not_need_more() {
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.flush().expect("flush");
        match dec.receive_frame() {
            Err(Error::Eof) => {}
            other => panic!("expected Eof after flush with no pending frame, got {other:?}"),
        }
    }

    #[test]
    fn pending_frame_drains_even_after_flush() {
        // flush() sets eof, but a frame already decoded and pending must
        // still drain before Eof is reported (flush is "no more input",
        // not "discard buffered output").
        let (bytes, _) = sample_rgba_qoi();
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, Some(7)))
            .expect("send_packet");
        dec.flush().expect("flush");
        // The buffered frame comes out first...
        let Frame::Video(vf) = dec.receive_frame().expect("buffered frame drains") else {
            panic!("expected a video frame");
        };
        assert_eq!(vf.pts, Some(7));
        // ...then Eof.
        match dec.receive_frame() {
            Err(Error::Eof) => {}
            other => panic!("expected Eof after the buffered frame drained, got {other:?}"),
        }
    }

    #[test]
    fn malformed_packet_surfaces_invalid_data() {
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        // Not a QOI stream — bad magic.
        let err = dec
            .send_packet(&packet_with(b"not a qoi file at all".to_vec(), None))
            .expect_err("malformed packet must error");
        assert!(
            matches!(err, Error::InvalidData(_)),
            "expected InvalidData, got {err:?}"
        );
    }

    #[test]
    fn second_packet_replaces_the_pending_frame() {
        // The decoder buffers exactly one pending frame; a second
        // send_packet before draining replaces it (QOI packets are
        // self-contained single images — there is no multi-frame queue).
        let (bytes_a, pixels_a) = sample_rgba_qoi();
        // A distinct second image (solid green RGB).
        let pixels_b: Vec<u8> = vec![0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0];
        let bytes_b = crate::encode_qoi(2, 2, 3, &pixels_b);

        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes_a, Some(1)))
            .expect("send a");
        dec.send_packet(&packet_with(bytes_b, Some(2)))
            .expect("send b");
        let Frame::Video(vf) = dec.receive_frame().expect("receive") else {
            panic!("expected a video frame");
        };
        assert_eq!(
            vf.pts,
            Some(2),
            "the second packet's frame is what's pending"
        );
        assert_eq!(vf.planes[0].data, pixels_b, "second image's pixels");
        assert_ne!(
            vf.planes[0].data, pixels_a,
            "first image was replaced, not queued"
        );
    }

    #[test]
    fn rgb_packet_decodes_to_three_channel_plane() {
        let pixels: Vec<u8> = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let bytes = crate::encode_qoi(2, 2, 3, &pixels);
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, None))
            .expect("send_packet");
        let Frame::Video(vf) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        assert_eq!(vf.planes[0].stride, 2 * 3, "RGB stride = width * 3");
        assert_eq!(vf.planes[0].data, pixels);
    }

    #[test]
    fn reset_after_flush_makes_the_decoder_reusable() {
        // Regression: the trait-default reset() routes through flush()
        // (which sets eof) and never clears it, so a decoder reset that
        // way would report Eof forever. QOI's explicit reset() must
        // restore the "fresh, awaiting input" state (NeedMore, not Eof).
        let (bytes, pixels) = sample_rgba_qoi();
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        // Decode one, flush, drain to Eof — the natural end-of-stream.
        dec.send_packet(&packet_with(bytes.clone(), Some(1)))
            .expect("send 1");
        let _ = dec.receive_frame().expect("frame 1");
        dec.flush().expect("flush");
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)));

        // After a seek the player calls reset(); the decoder must come
        // back to a NeedMore (awaiting input) state, NOT a stuck Eof.
        dec.reset().expect("reset");
        match dec.receive_frame() {
            Err(Error::NeedMore) => {}
            other => panic!("expected NeedMore after reset, got {other:?}"),
        }

        // ...and it must decode subsequent packets normally.
        dec.send_packet(&packet_with(bytes, Some(2)))
            .expect("send 2");
        let Frame::Video(vf) = dec.receive_frame().expect("frame 2 after reset") else {
            panic!("expected a video frame");
        };
        assert_eq!(vf.pts, Some(2));
        assert_eq!(vf.planes[0].data, pixels);
    }

    #[test]
    fn reset_discards_a_buffered_frame() {
        // reset() is "forget everything", including a frame decoded but
        // not yet drained — a seek invalidates pending output.
        let (bytes, _) = sample_rgba_qoi();
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, Some(9)))
            .expect("send_packet");
        // Do NOT drain — reset while a frame is pending.
        dec.reset().expect("reset");
        match dec.receive_frame() {
            Err(Error::NeedMore) => {}
            other => panic!("expected NeedMore after reset drops the pending frame, got {other:?}"),
        }
    }

    #[test]
    fn arena_frame_carries_true_width_height_and_pixel_format_rgba() {
        // The QOI override of receive_arena_frame must emit a correct
        // FrameHeader. The trait DEFAULT would label the single packed
        // plane Gray8 and report width = stride (= width*4 = 8). We pin
        // the corrected values.
        use oxideav_core::PixelFormat;
        let (bytes, pixels) = sample_rgba_qoi(); // 2x2 RGBA
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, Some(11)))
            .expect("send_packet");
        let frame = match dec.receive_arena_frame() {
            Ok(f) => f,
            Err(e) => panic!("receive_arena_frame failed: {e:?}"),
        };
        let hdr = frame.header();
        assert_eq!(hdr.width, 2, "true pixel width, NOT the byte stride");
        assert_eq!(hdr.height, 2);
        assert_eq!(
            hdr.pixel_format,
            PixelFormat::Rgba,
            "packed RGBA, not Gray8"
        );
        assert_eq!(hdr.presentation_timestamp, Some(11), "pts threaded");
        assert_eq!(frame.plane_count(), 1, "QOI is one packed plane");
        assert_eq!(frame.plane(0), Some(pixels.as_slice()), "lossless pixels");
    }

    #[test]
    fn arena_frame_rgb_pixel_format() {
        use oxideav_core::PixelFormat;
        let pixels: Vec<u8> = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let bytes = crate::encode_qoi(2, 2, 3, &pixels);
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.send_packet(&packet_with(bytes, None))
            .expect("send_packet");
        let frame = match dec.receive_arena_frame() {
            Ok(f) => f,
            Err(e) => panic!("receive_arena_frame failed: {e:?}"),
        };
        let hdr = frame.header();
        assert_eq!(hdr.width, 2);
        assert_eq!(hdr.height, 2);
        assert_eq!(hdr.pixel_format, PixelFormat::Rgb24, "packed RGB24");
        assert_eq!(frame.plane(0), Some(pixels.as_slice()));
    }

    #[test]
    fn arena_frame_before_packet_is_need_more() {
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        match dec.receive_arena_frame() {
            Err(Error::NeedMore) => {}
            Err(e) => panic!("expected NeedMore before any packet, got Err({e:?})"),
            Ok(_) => panic!("expected NeedMore before any packet, got Ok(frame)"),
        }
    }

    #[test]
    fn arena_frame_after_flush_is_eof() {
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(crate::CODEC_ID_STR)))
            .expect("make_decoder");
        dec.flush().expect("flush");
        match dec.receive_arena_frame() {
            Err(Error::Eof) => {}
            Err(e) => panic!("expected Eof after flush, got Err({e:?})"),
            Ok(_) => panic!("expected Eof after flush, got Ok(frame)"),
        }
    }
}
