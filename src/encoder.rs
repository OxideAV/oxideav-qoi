//! QOI byte-stream encoder.
//!
//! Implements the "Encoder" half of the one-page qoiformat.org
//! specification. For each input pixel the encoder picks the
//! smallest legal chunk in the spec's priority order:
//!
//! 1. **`QOI_OP_RUN`** — extend an in-flight run when the current
//!    pixel equals the previous one (cap at 62, since tags 0xfe /
//!    0xff are stolen by RGB / RGBA).
//! 2. **`QOI_OP_INDEX`** — when the running pixel array's slot at
//!    `hash(cur)` already equals `cur`.
//! 3. **`QOI_OP_DIFF`** — alpha unchanged AND each per-channel delta
//!    is in `−2..=+1`.
//! 4. **`QOI_OP_LUMA`** — alpha unchanged AND `dg ∈ −32..=31` AND
//!    both `dr-dg` and `db-dg` ∈ `−8..=7`.
//! 5. **`QOI_OP_RGB`** — alpha unchanged but the deltas don't fit
//!    DIFF / LUMA.
//! 6. **`QOI_OP_RGBA`** — alpha changed.
//!
//! Followed by the 8-byte end marker `00 00 00 00 00 00 00 01`.
//!
//! Inputs of `channels == 3` carry alpha implicitly as `0xFF`. The
//! encoder writes the same `channels` byte back into the header, so a
//! 3-channel input round-trips byte-for-byte through the encoder.

use crate::decoder::hash;
use crate::image::QoiChannels;
use crate::{END_MARKER, MAGIC, OP_DIFF, OP_INDEX, OP_LUMA, OP_RGB, OP_RGBA, OP_RUN};

#[cfg(feature = "registry")]
use oxideav_core::Encoder;
#[cfg(feature = "registry")]
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase};

// ---------------------------------------------------------------------------
// Public standalone API
// ---------------------------------------------------------------------------

/// Encode raw RGB or RGBA pixel bytes into a complete QOI file
/// (`qoif` header + chunks + end marker).
///
/// `channels` must be 3 or 4. `pixels` must be tightly packed at
/// `width * height * channels` bytes (no row stride padding).
/// `colorspace` defaults to 0 (sRGB with linear alpha) — use
/// [`encode_qoi_full`] to set it explicitly.
///
/// # Panics
///
/// Panics if `channels` is not 3 or 4, or if `pixels.len() !=
/// width * height * channels`. (These are programmer errors at the
/// encode boundary; QOI itself has no error path here — every valid
/// pixel input encodes successfully.)
pub fn encode_qoi(width: u32, height: u32, channels: u8, pixels: &[u8]) -> Vec<u8> {
    encode_qoi_full(width, height, channels, /* colorspace */ 0, pixels)
}

/// Encode raw RGB or RGBA pixel bytes with an explicit `colorspace`
/// header byte (0 = sRGB with linear alpha, 1 = all linear).
///
/// `colorspace` is purely informational — it doesn't affect the
/// pixel bytes the decoder produces. Use [`encode_qoi`] for the
/// common case where you don't care.
///
/// # Panics
///
/// See [`encode_qoi`].
pub fn encode_qoi_full(
    width: u32,
    height: u32,
    channels: u8,
    colorspace: u8,
    pixels: &[u8],
) -> Vec<u8> {
    assert!(
        channels == 3 || channels == 4,
        "QOI: channels must be 3 or 4, got {channels}"
    );
    assert!(
        colorspace <= 1,
        "QOI: colorspace must be 0 or 1, got {colorspace}"
    );
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(channels as usize))
        .expect("QOI: width*height*channels overflows usize");
    assert_eq!(
        pixels.len(),
        expected,
        "QOI: pixels.len() = {}, expected width*height*channels = {expected}",
        pixels.len()
    );

    let qoi_channels = if channels == 4 {
        QoiChannels::Rgba
    } else {
        QoiChannels::Rgb
    };

    // Reserve a generous upper-bound for the output: header (14) +
    // worst-case 5 bytes per pixel (QOI_OP_RGBA) + end marker (8).
    let cap = 14 + (width as usize) * (height as usize) * 5 + END_MARKER.len();
    let mut out = Vec::with_capacity(cap);

    // Header.
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.push(channels);
    out.push(colorspace);

    // Per-spec initial state: previous pixel = RGBA(0,0,0,255), index
    // array zero-filled.
    let mut prev: [u8; 4] = [0, 0, 0, 255];
    let mut index: [[u8; 4]; 64] = [[0, 0, 0, 0]; 64];
    let mut run: u8 = 0;

    let pixel_count = (width as usize) * (height as usize);
    let bpp = channels as usize;

    for i in 0..pixel_count {
        let off = i * bpp;
        let cur = match qoi_channels {
            QoiChannels::Rgb => [pixels[off], pixels[off + 1], pixels[off + 2], prev[3]],
            QoiChannels::Rgba => [
                pixels[off],
                pixels[off + 1],
                pixels[off + 2],
                pixels[off + 3],
            ],
        };

        if cur == prev {
            run += 1;
            // Per spec, every pixel seen by the encoder is put into
            // the index. For a RUN that's `run` copies of `prev`, all
            // landing in the same slot — equivalent to a single store
            // of `prev` at `hash(prev)`. We do it once per matching
            // pixel; the redundant repeats are no-ops since the slot
            // already holds `prev`.
            index[hash(cur) as usize] = cur;
            // The QOI_OP_RUN field stores `run-1` in 6 bits, so the
            // legal max for a single chunk is 62 (tags 62 / 63 are
            // stolen by the 8-bit RGB / RGBA tags). At 62 we have
            // to flush even if the next pixel matches.
            if run == 62 || i + 1 == pixel_count {
                // Encode the QOI_OP_RUN with bias of −1.
                out.push(OP_RUN | (run - 1));
                run = 0;
            }
        } else {
            // Any pending run must be flushed before we emit a new
            // chunk for `cur`.
            if run > 0 {
                out.push(OP_RUN | (run - 1));
                run = 0;
            }

            let h = hash(cur) as usize;
            if index[h] == cur {
                out.push(OP_INDEX | h as u8);
            } else {
                index[h] = cur;

                if cur[3] == prev[3] {
                    // Alpha unchanged → DIFF, LUMA, or RGB.
                    let dr = cur[0].wrapping_sub(prev[0]) as i8 as i32;
                    let dg = cur[1].wrapping_sub(prev[1]) as i8 as i32;
                    let db = cur[2].wrapping_sub(prev[2]) as i8 as i32;

                    if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                        let byte = OP_DIFF
                            | (((dr + 2) as u8) << 4)
                            | (((dg + 2) as u8) << 2)
                            | ((db + 2) as u8);
                        out.push(byte);
                    } else {
                        let dr_dg = dr - dg;
                        let db_dg = db - dg;
                        if (-32..=31).contains(&dg)
                            && (-8..=7).contains(&dr_dg)
                            && (-8..=7).contains(&db_dg)
                        {
                            out.push(OP_LUMA | ((dg + 32) as u8));
                            out.push((((dr_dg + 8) as u8) << 4) | ((db_dg + 8) as u8));
                        } else {
                            out.push(OP_RGB);
                            out.push(cur[0]);
                            out.push(cur[1]);
                            out.push(cur[2]);
                        }
                    }
                } else {
                    // Alpha changed → must be RGBA.
                    out.push(OP_RGBA);
                    out.push(cur[0]);
                    out.push(cur[1]);
                    out.push(cur[2]);
                    out.push(cur[3]);
                }
            }
        }
        prev = cur;
    }

    // End marker.
    out.extend_from_slice(END_MARKER);
    out
}

// ---------------------------------------------------------------------------
// Registry-side Encoder trait impl
// ---------------------------------------------------------------------------

#[cfg(feature = "registry")]
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    let mut out_params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
    out_params.width = params.width;
    out_params.height = params.height;
    out_params.pixel_format = params.pixel_format;
    Ok(Box::new(QoiEncoder {
        codec_id: CodecId::new(crate::CODEC_ID_STR),
        out_params,
        pending: None,
        eof: false,
    }))
}

#[cfg(feature = "registry")]
struct QoiEncoder {
    codec_id: CodecId,
    out_params: CodecParameters,
    pending: Option<Vec<u8>>,
    eof: bool,
}

#[cfg(feature = "registry")]
impl Encoder for QoiEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.out_params
    }
    fn send_frame(&mut self, frame: &Frame) -> oxideav_core::Result<()> {
        let vf = match frame {
            Frame::Video(v) => v,
            _ => {
                return Err(oxideav_core::Error::invalid(
                    "QOI encoder: expected video frame",
                ))
            }
        };
        let format = self.out_params.pixel_format.ok_or_else(|| {
            oxideav_core::Error::invalid("QOI encoder: pixel_format missing in CodecParameters")
        })?;
        let width = self.out_params.width.ok_or_else(|| {
            oxideav_core::Error::invalid("QOI encoder: width missing in CodecParameters")
        })?;
        let height = self.out_params.height.ok_or_else(|| {
            oxideav_core::Error::invalid("QOI encoder: height missing in CodecParameters")
        })?;
        let channels: u8 = match format {
            PixelFormat::Rgba => 4,
            PixelFormat::Rgb24 => 3,
            other => {
                return Err(oxideav_core::Error::invalid(format!(
                    "QOI encoder: unsupported pixel format {other:?}"
                )))
            }
        };
        if vf.planes.is_empty() {
            return Err(oxideav_core::Error::invalid(
                "QOI encoder: empty frame plane",
            ));
        }

        // QOI requires tightly packed pixels (no row padding). Repack
        // if the source plane has stride > width * channels.
        let plane = &vf.planes[0];
        let row_bytes = width as usize * channels as usize;
        let pixels: Vec<u8> = if plane.stride == row_bytes {
            plane.data.clone()
        } else {
            let mut v = Vec::with_capacity(row_bytes * height as usize);
            for y in 0..height as usize {
                let start = y * plane.stride;
                let end = start + row_bytes;
                if end > plane.data.len() {
                    return Err(oxideav_core::Error::invalid(
                        "QOI encoder: frame plane truncated",
                    ));
                }
                v.extend_from_slice(&plane.data[start..end]);
            }
            v
        };

        let bytes = encode_qoi(width, height, channels, &pixels);
        self.pending = Some(bytes);
        Ok(())
    }
    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        match self.pending.take() {
            Some(bytes) => {
                let mut pkt = Packet::new(0, TimeBase::new(1, 1), bytes);
                pkt.flags.keyframe = true;
                Ok(pkt)
            }
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
