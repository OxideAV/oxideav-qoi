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
    let mut buf = Vec::new();
    encode_qoi_full_into(&mut buf, width, height, channels, colorspace, pixels);
    buf
}

/// Encode into a caller-owned `Vec<u8>`, reusing its existing
/// allocation when large enough.
///
/// Identical to [`encode_qoi`] but writes the encoded bytes into
/// `buf` (which is cleared first) instead of returning a fresh
/// `Vec<u8>`. Designed for tight encode-in-a-loop callers — image
/// servers, batch converters, encoder-side benches — that want to
/// amortise the worst-case `14 + n*5 + 8` allocation across many
/// images of similar dimensions. After a few iterations the buffer
/// has grown to the worst-case capacity of the largest image seen,
/// and every subsequent encode reuses that capacity without a fresh
/// allocation. On return, `buf.len()` is the encoded size and
/// `buf.capacity()` is whatever the previous worst case was (kept,
/// not shrunk).
///
/// `colorspace` defaults to 0 (sRGB with linear alpha) — use
/// [`encode_qoi_full_into`] to set it explicitly.
///
/// # Panics
///
/// See [`encode_qoi`].
pub fn encode_qoi_into(buf: &mut Vec<u8>, width: u32, height: u32, channels: u8, pixels: &[u8]) {
    encode_qoi_full_into(
        buf, width, height, channels, /* colorspace */ 0, pixels,
    );
}

/// Encode into a caller-owned `Vec<u8>` with an explicit
/// `colorspace` header byte.
///
/// Like [`encode_qoi_into`] but exposes the `colorspace` field. The
/// buffer is cleared on entry and grown to the worst-case
/// `14 + width*height*5 + 8` upper bound, then truncated to the
/// actual encoded size before return — so the existing capacity is
/// preserved across repeated calls and only re-grown when a larger
/// image arrives.
///
/// # Panics
///
/// See [`encode_qoi`].
pub fn encode_qoi_full_into(
    buf: &mut Vec<u8>,
    width: u32,
    height: u32,
    channels: u8,
    colorspace: u8,
    pixels: &[u8],
) {
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

    // Pre-size the caller-provided buffer to its EXACT worst-case
    // upper bound — header (14) + 5 bytes per pixel (the
    // QOI_OP_RGBA chunk, the widest chunk in the spec) + 8-byte end
    // marker — and write through a moving byte cursor `out_pos`.
    // The hot-path emit sites then become plain indexed stores
    // instead of `Vec::push` / `extend_from_slice` calls; the
    // per-call capacity check + length update the optimiser cannot
    // prove unnecessary on `Vec` goes away. The buffer is truncated
    // to `out_pos` before return, so callers see a `Vec<u8>` whose
    // `len()` reflects the actual encoded size while its
    // `capacity()` retains the worst-case headroom for the next
    // call (the headline benefit of the `_into` variant).
    //
    // Worst case is realised by `encode_alpha_changing_rgba` (every
    // pixel becomes a 5-byte RGBA chunk); on the solid-fill / index
    // / DIFF paths the over-allocation never materialises because
    // the buffer is truncated to the actual `out_pos` at return.
    //
    // Reuse contract: when called on a previously-encoded buffer
    // whose `capacity()` already covers `cap`, the `resize` below
    // is a length-update with no allocator traffic — that's the
    // headline benefit of the `_into` variant over `encode_qoi`.
    let pixel_count = (width as usize) * (height as usize);
    let cap = 14 + pixel_count * 5 + END_MARKER.len();
    buf.clear();
    buf.resize(cap, 0u8);

    // Header — exactly 14 bytes into the head of the buffer. One
    // `copy_from_slice` per field avoids the `extend_from_slice`
    // capacity-growth probes the previous version paid.
    buf[0..4].copy_from_slice(MAGIC);
    buf[4..8].copy_from_slice(&width.to_be_bytes());
    buf[8..12].copy_from_slice(&height.to_be_bytes());
    buf[12] = channels;
    buf[13] = colorspace;
    let out_pos_start: usize = 14;

    // Round-231 encode-loop split: dispatch on the channel count
    // ONCE up-front instead of per-pixel. The previous version had a
    // `match qoi_channels { Rgb => …, Rgba => … }` inside the hot
    // loop to assemble `cur`; on the RGB path it also synthesised
    // `cur[3] = prev[3]` so the downstream alpha-equality test had
    // a uniform shape, which (a) cost a per-pixel branch and (b)
    // generated an alpha-compare whose result was provably always
    // `true` in RGB mode. Hoisting the channel decision out of the
    // loop produces two specialised loops — RGB-3 carries no alpha
    // state at all and skips both the alpha compare and the RGBA
    // emit arm — and lets the optimiser inline the per-channel
    // pixel-load shape without the `match` discriminant.
    let out_pos = if channels == 4 {
        encode_inner_rgba(buf, out_pos_start, pixels, pixel_count)
    } else {
        encode_inner_rgb(buf, out_pos_start, pixels, pixel_count)
    };

    // End marker.
    let mut out_pos = out_pos;
    buf[out_pos..out_pos + END_MARKER.len()].copy_from_slice(END_MARKER);
    out_pos += END_MARKER.len();

    // Truncate down to the actual produced length so callers see a
    // `Vec<u8>` whose `len()` is the encoded size. The retained
    // `capacity()` is the prior worst case, so a subsequent call on
    // a similar image reuses the same allocation.
    buf.truncate(out_pos);
}

// ---------------------------------------------------------------------------
// Round-231: channel-specialised inner encode loops.
//
// Both functions assume the caller has already written the 14-byte
// header into `buf[0..14]`, pre-sized `buf` to the worst-case bound
// `14 + pixel_count*5 + END_MARKER.len()`, and validated the
// `pixels.len()` invariant. They walk `pixels` once, write chunks
// into `buf` starting at `out_pos_start`, and return the byte cursor
// reached after the last chunk (i.e. the position where the caller
// should write the end marker). The end marker itself + the final
// truncate are the caller's responsibility — that boilerplate is
// identical between the two channel modes and stays in
// `encode_qoi_full_into`.
//
// Why two functions instead of a single generic body. The previous
// version had a `match QoiChannels { Rgb => …, Rgba => … }` inside
// the per-pixel loop to assemble the 4-byte `cur` from the input
// stride; on the RGB path it also stuffed `prev[3]` into `cur[3]`
// so the alpha-equality test downstream had a uniform shape. Both
// were per-pixel branches with provably-fixed outcomes for the
// duration of a given encode call. Hoisting the decision out lets
// the optimiser:
//   * inline the pixel-load shape (3-byte vs 4-byte) without the
//     match discriminant,
//   * elide the alpha-compare arm entirely from the RGB version
//     (alpha never changes — the input stream carries no alpha),
//   * elide the RGBA-emit arm entirely from the RGB version
//     (alpha never changes, so the path is unreachable),
//   * keep the RGBA version's chunk-priority chain identical to
//     the spec's wording (RUN > INDEX > DIFF > LUMA > RGB / RGBA).
// ---------------------------------------------------------------------------

/// RGBA (`channels == 4`) inner loop. Walks `pixels` 4 bytes at a
/// time, emits chunks per the QOI priority chain
/// (RUN > INDEX > DIFF > LUMA > RGB / RGBA), returns the cursor
/// position past the last chunk.
#[inline]
fn encode_inner_rgba(
    buf: &mut [u8],
    out_pos_start: usize,
    pixels: &[u8],
    pixel_count: usize,
) -> usize {
    let mut out_pos = out_pos_start;
    let mut prev: [u8; 4] = [0, 0, 0, 255];
    let mut index: [[u8; 4]; 64] = [[0, 0, 0, 0]; 64];

    let mut i = 0usize;
    while i < pixel_count {
        let off = i * 4;
        let cur: [u8; 4] = [
            pixels[off],
            pixels[off + 1],
            pixels[off + 2],
            pixels[off + 3],
        ];

        if cur == prev {
            // Round-282 run-arm restructure: consume the WHOLE run in
            // one outlined scan-ahead call instead of re-entering the
            // per-pixel loop (load + compare + run-counter bookkeeping
            // + flush test) once per matching pixel. See
            // [`run_scan_emit_rgba`] for the scan + emission +
            // index-store equivalence details.
            //
            // Per spec, every pixel seen by the encoder is put into
            // the index. For a RUN that's N copies of `prev`, all
            // landing in the same slot — equivalent to a SINGLE store
            // of `prev` at `hash(prev)`. The previous per-pixel store
            // was a no-op repeat that still re-derived `hash(cur)`
            // (three multiplies + adds) per pixel; the index state
            // observed by every later INDEX-arm lookup is unchanged
            // (lookups only happen after the run breaks).
            index[hash(cur) as usize] = cur;
            // `prev` already equals `cur`; resume after the run.
            (i, out_pos) = run_scan_emit_rgba(buf, out_pos, pixels, pixel_count, i, cur);
        } else {
            let h = hash(cur) as usize;
            if index[h] == cur {
                buf[out_pos] = OP_INDEX | h as u8;
                out_pos += 1;
            } else {
                index[h] = cur;

                if cur[3] == prev[3] {
                    // Alpha unchanged → DIFF, LUMA, or RGB.
                    let dr = cur[0].wrapping_sub(prev[0]) as i8 as i32;
                    let dg = cur[1].wrapping_sub(prev[1]) as i8 as i32;
                    let db = cur[2].wrapping_sub(prev[2]) as i8 as i32;

                    if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                        buf[out_pos] = OP_DIFF
                            | (((dr + 2) as u8) << 4)
                            | (((dg + 2) as u8) << 2)
                            | ((db + 2) as u8);
                        out_pos += 1;
                    } else {
                        let dr_dg = dr - dg;
                        let db_dg = db - dg;
                        if (-32..=31).contains(&dg)
                            && (-8..=7).contains(&dr_dg)
                            && (-8..=7).contains(&db_dg)
                        {
                            buf[out_pos] = OP_LUMA | ((dg + 32) as u8);
                            buf[out_pos + 1] = (((dr_dg + 8) as u8) << 4) | ((db_dg + 8) as u8);
                            out_pos += 2;
                        } else {
                            buf[out_pos] = OP_RGB;
                            buf[out_pos + 1..out_pos + 4].copy_from_slice(&cur[..3]);
                            out_pos += 4;
                        }
                    }
                } else {
                    // Alpha changed → must be RGBA. Tag + 4 pixel
                    // bytes; the 4-byte `copy_from_slice` is the
                    // fast straight-line memcpy of the full pixel.
                    buf[out_pos] = OP_RGBA;
                    buf[out_pos + 1..out_pos + 5].copy_from_slice(&cur);
                    out_pos += 5;
                }
            }
            prev = cur;
            i += 1;
        }
    }

    out_pos
}

/// RGB (`channels == 3`) inner loop. Walks `pixels` 3 bytes at a
/// time, tracks a 3-byte previous pixel + 3-byte index entries (the
/// alpha never changes from the spec's initial 0xff, so the alpha
/// compare arm and the RGBA emit arm are unreachable and don't need
/// to exist). Returns the cursor position past the last chunk.
///
/// Hash uses the spec formula with `A = 0xff` substituted in: the
/// running pixel array is shared between RGB and RGBA streams in
/// the spec definition, but a decoder reading an RGB-channels
/// stream observes the same alpha=0xff invariant, so the index hits
/// agree with the unified-array version under the substitution.
#[inline]
fn encode_inner_rgb(
    buf: &mut [u8],
    out_pos_start: usize,
    pixels: &[u8],
    pixel_count: usize,
) -> usize {
    let mut out_pos = out_pos_start;
    // Spec initial pixel is (0,0,0,255). In RGB mode the alpha
    // channel is fixed at 0xff for the whole stream — we keep it
    // inside the local `cur` / `prev` arrays so the hash function
    // (which mixes all four channels) produces the same value the
    // RGBA path would.
    let mut prev: [u8; 4] = [0, 0, 0, 255];
    let mut index: [[u8; 4]; 64] = [[0, 0, 0, 0]; 64];

    let mut i = 0usize;
    while i < pixel_count {
        let off = i * 3;
        // Alpha stays 0xff for the entire RGB stream — no per-pixel
        // load, no per-pixel `cur[3] = prev[3]` synthesis, no
        // alpha-compare arm downstream.
        let cur: [u8; 4] = [pixels[off], pixels[off + 1], pixels[off + 2], 0xff];

        if cur == prev {
            // Round-282 run-arm restructure — see the RGBA loop's run
            // arm for the single-index-store equivalence argument and
            // [`run_scan_emit_rgb`] for the wide-scan details.
            index[hash(cur) as usize] = cur;
            (i, out_pos) = run_scan_emit_rgb(buf, out_pos, pixels, pixel_count, i, cur);
        } else {
            let h = hash(cur) as usize;
            if index[h] == cur {
                buf[out_pos] = OP_INDEX | h as u8;
                out_pos += 1;
            } else {
                index[h] = cur;

                // Alpha is provably unchanged for the entire RGB
                // stream, so the alpha-compare arm collapses to
                // its "alpha-unchanged" branch — DIFF / LUMA / RGB.
                let dr = cur[0].wrapping_sub(prev[0]) as i8 as i32;
                let dg = cur[1].wrapping_sub(prev[1]) as i8 as i32;
                let db = cur[2].wrapping_sub(prev[2]) as i8 as i32;

                if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                    buf[out_pos] = OP_DIFF
                        | (((dr + 2) as u8) << 4)
                        | (((dg + 2) as u8) << 2)
                        | ((db + 2) as u8);
                    out_pos += 1;
                } else {
                    let dr_dg = dr - dg;
                    let db_dg = db - dg;
                    if (-32..=31).contains(&dg)
                        && (-8..=7).contains(&dr_dg)
                        && (-8..=7).contains(&db_dg)
                    {
                        buf[out_pos] = OP_LUMA | ((dg + 32) as u8);
                        buf[out_pos + 1] = (((dr_dg + 8) as u8) << 4) | ((db_dg + 8) as u8);
                        out_pos += 2;
                    } else {
                        buf[out_pos] = OP_RGB;
                        buf[out_pos + 1..out_pos + 4].copy_from_slice(&cur[..3]);
                        out_pos += 4;
                    }
                }
            }
            prev = cur;
            i += 1;
        }
    }

    out_pos
}

/// Round-282 run scan + emit (RGBA layout). The caller has already
/// observed `pixels[i] == prev` and stored the run's pixel into the
/// running index; this helper finds where the run ends and emits the
/// corresponding `QOI_OP_RUN` chunks.
///
/// Scans forward from pixel `i + 1` over pixels equal to `cur`, then
/// emits the whole run (length `j - i`) as ⌊N/62⌋ max-length chunks
/// followed by one remainder chunk — byte-identical to the previous
/// per-pixel flush-at-62 / flush-at-image-end emission (the
/// `QOI_OP_RUN` field stores `run-1` in 6 bits, so the legal max per
/// chunk is 62; tags 62 / 63 are stolen by the 8-bit RGB / RGBA
/// tags). Returns `(next_pixel_index, out_pos)`.
///
/// The 16-byte (4-pixel) fixed-size block compare lowers to two
/// word-width loads + compares — no per-pixel branching until the
/// block straddling the run's end, which the scalar tail loop
/// resolves. `#[inline(never)]` keeps this body out of the caller's
/// per-pixel loop: runs amortise the call overhead over their whole
/// length, while the caller's non-run fall-through path stays small
/// enough to keep its chunk-priority chain in registers.
#[inline(never)]
fn run_scan_emit_rgba(
    buf: &mut [u8],
    mut out_pos: usize,
    pixels: &[u8],
    pixel_count: usize,
    i: usize,
    cur: [u8; 4],
) -> (usize, usize) {
    let pat: [u8; 16] = {
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&cur);
        p[4..8].copy_from_slice(&cur);
        p[8..12].copy_from_slice(&cur);
        p[12..16].copy_from_slice(&cur);
        p
    };
    let mut j = i + 1;
    while j + 4 <= pixel_count && pixels[j * 4..j * 4 + 16] == pat {
        j += 4;
    }
    while j < pixel_count && pixels[j * 4..j * 4 + 4] == cur {
        j += 1;
    }

    let mut n = j - i;
    while n >= 62 {
        buf[out_pos] = OP_RUN | 61;
        out_pos += 1;
        n -= 62;
    }
    if n > 0 {
        buf[out_pos] = OP_RUN | (n as u8 - 1);
        out_pos += 1;
    }
    (j, out_pos)
}

/// Round-282 run scan + emit (RGB layout). Same contract as
/// [`run_scan_emit_rgba`]; RGB pixels are 3 bytes, so the block
/// pattern is 12 bytes = 4 pixels (one word-width + one half-word
/// compare per block). `cur[3]` is the fixed 0xff alpha and is not
/// part of the input comparison.
#[inline(never)]
fn run_scan_emit_rgb(
    buf: &mut [u8],
    mut out_pos: usize,
    pixels: &[u8],
    pixel_count: usize,
    i: usize,
    cur: [u8; 4],
) -> (usize, usize) {
    let pat: [u8; 12] = {
        let mut p = [0u8; 12];
        p[0..3].copy_from_slice(&cur[..3]);
        p[3..6].copy_from_slice(&cur[..3]);
        p[6..9].copy_from_slice(&cur[..3]);
        p[9..12].copy_from_slice(&cur[..3]);
        p
    };
    let mut j = i + 1;
    while j + 4 <= pixel_count && pixels[j * 3..j * 3 + 12] == pat {
        j += 4;
    }
    while j < pixel_count && pixels[j * 3..j * 3 + 3] == cur[..3] {
        j += 1;
    }

    let mut n = j - i;
    while n >= 62 {
        buf[out_pos] = OP_RUN | 61;
        out_pos += 1;
        n -= 62;
    }
    if n > 0 {
        buf[out_pos] = OP_RUN | (n as u8 - 1);
        out_pos += 1;
    }
    (j, out_pos)
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
    let colorspace = resolve_colorspace_option(params)?;
    // Echo the resolved colorspace back through the output params'
    // option map so a consumer querying `output_params()` sees the
    // exact header byte the encoder will write, and a re-construction
    // from those params reproduces the same stream.
    out_params
        .options
        .insert("colorspace", colorspace.to_string());
    Ok(Box::new(QoiEncoder {
        codec_id: CodecId::new(crate::CODEC_ID_STR),
        out_params,
        colorspace,
        pending: None,
        eof: false,
    }))
}

/// Typed encoder options for QOI.
///
/// The framework's [`CodecOptionsStruct`] surface — declaring a static
/// `SCHEMA` and registering it via `CodecInfo::encoder_options` — is
/// what makes a codec's tuning knobs discoverable to `oxideav list`,
/// validatable by the pipeline's JSON-options checker, and parsed with
/// uniform error messages. QOI's only knob is the informational
/// `colorspace` header byte.
///
/// The `colorspace` option accepts the numeric forms `"0"` / `"1"` and
/// the symbolic names `"srgb"` (= 0, sRGB with linear alpha) and
/// `"linear"` (= 1, all channels linear). Unknown keys and out-of-set
/// values are rejected by [`parse_options`] before this struct's
/// `apply` runs; absent → the default `0`.
#[cfg(feature = "registry")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QoiEncoderOptions {
    /// Resolved QOI colorspace header byte (0 or 1). Defaults to 0
    /// (sRGB with linear alpha), matching the standalone [`encode_qoi`].
    pub colorspace: u8,
}

#[cfg(feature = "registry")]
impl oxideav_core::CodecOptionsStruct for QoiEncoderOptions {
    const SCHEMA: &'static [oxideav_core::OptionField] = &[oxideav_core::OptionField {
        name: "colorspace",
        // Accept both the numeric and the symbolic spellings; the Enum
        // kind validates the value against this exact set in
        // `parse_options` before `apply` is called.
        kind: oxideav_core::OptionKind::Enum(&["0", "srgb", "1", "linear"]),
        default: oxideav_core::OptionValue::String(String::new()),
        help: "QOI colorspace header byte: 0/\"srgb\" (sRGB with linear \
               alpha) or 1/\"linear\" (all channels linear). Informational \
               only — does not change pixel bytes.",
    }];

    fn apply(&mut self, key: &str, value: &oxideav_core::OptionValue) -> oxideav_core::Result<()> {
        match key {
            "colorspace" => {
                self.colorspace = match value.as_str()? {
                    "0" | "srgb" => 0,
                    "1" | "linear" => 1,
                    // Unreachable in practice: the Enum schema already
                    // restricts the value set. Kept as a defensive arm.
                    other => {
                        return Err(oxideav_core::Error::invalid(format!(
                            "QOI encoder: invalid colorspace {other:?}"
                        )))
                    }
                };
                Ok(())
            }
            // Unreachable: parse_options rejects unknown keys against
            // SCHEMA before apply runs.
            other => Err(oxideav_core::Error::invalid(format!(
                "QOI encoder: unknown option {other:?}"
            ))),
        }
    }
}

/// Read the optional `colorspace` tuning knob from
/// [`CodecParameters::options`] and resolve it to the on-wire QOI
/// header byte (0 = sRGB with linear alpha, 1 = all channels linear).
///
/// Goes through the framework's schema-validated [`parse_options`] path
/// against [`QoiEncoderOptions`], so an unknown option key or an
/// out-of-set value is rejected with a uniform `InvalidData` error at
/// encoder construction rather than silently ignored. Absent option →
/// default 0, matching the standalone [`encode_qoi`].
#[cfg(feature = "registry")]
fn resolve_colorspace_option(params: &CodecParameters) -> oxideav_core::Result<u8> {
    let opts: QoiEncoderOptions = oxideav_core::parse_options(&params.options)?;
    Ok(opts.colorspace)
}

#[cfg(feature = "registry")]
struct QoiEncoder {
    codec_id: CodecId,
    out_params: CodecParameters,
    /// Resolved QOI colorspace header byte (0 or 1), parsed once at
    /// construction from the `colorspace` option.
    colorspace: u8,
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

        let bytes = encode_qoi_full(width, height, channels, self.colorspace, &pixels);
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

// ---------------------------------------------------------------------------
// Trait-side Encoder behavioural tests.
//
// The crate's encoder suites (`tests/canonical_encoding.rs`,
// `tests/property_sweep.rs`, …) drive the standalone `encode_qoi`
// function. None of them exercises the `oxideav_core::Encoder` trait
// impl — the `send_frame` / `receive_packet` state machine, the stride
// repacking path, the colorspace option, the pixel-format validation,
// or the keyframe flag on the produced packet. These pin that surface.
// In-crate (not `tests/`) so they can name `oxideav_core` types without
// a dev-dep on the framework crate.
// ---------------------------------------------------------------------------
#[cfg(all(test, feature = "registry"))]
mod registry_encoder_tests {
    use super::*;
    use oxideav_core::{CodecOptions, Error, VideoFrame, VideoPlane};

    fn params(width: u32, height: u32, format: PixelFormat) -> CodecParameters {
        let mut p = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
        p.width = Some(width);
        p.height = Some(height);
        p.pixel_format = Some(format);
        p
    }

    fn video_frame(stride: usize, data: Vec<u8>) -> Frame {
        Frame::Video(VideoFrame {
            pts: None,
            planes: vec![VideoPlane { stride, data }],
        })
    }

    #[test]
    fn send_frame_then_receive_yields_a_qoi_packet() {
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Rgba)).expect("make_encoder");
        enc.send_frame(&video_frame(2 * 4, pixels.clone()))
            .expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        // The packet is a complete QOI file the standalone decoder reads.
        let img = crate::parse_qoi(&pkt.data).expect("packet is a valid QOI stream");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.channels, crate::QoiChannels::Rgba);
        assert_eq!(img.pixels, pixels, "round-trip is lossless");
        assert!(pkt.flags.keyframe, "every QOI frame is an intra keyframe");
    }

    #[test]
    fn receive_before_send_is_need_more() {
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Rgba)).expect("make_encoder");
        match enc.receive_packet() {
            Err(Error::NeedMore) => {}
            other => panic!("expected NeedMore before any frame, got {other:?}"),
        }
    }

    #[test]
    fn flush_then_receive_is_eof() {
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Rgba)).expect("make_encoder");
        enc.flush().expect("flush");
        match enc.receive_packet() {
            Err(Error::Eof) => {}
            other => panic!("expected Eof after flush with no pending packet, got {other:?}"),
        }
    }

    #[test]
    fn colorspace_option_is_written_into_the_header() {
        // The trait-side encoder must thread a `colorspace` option from
        // CodecParameters into the QOI header byte — previously it always
        // wrote 0 regardless of the requested colorspace.
        let pixels: Vec<u8> = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let mut p = params(2, 2, PixelFormat::Rgb24);
        p.options = CodecOptions::new().set("colorspace", "1");
        let mut enc = make_encoder(&p).expect("make_encoder");
        enc.send_frame(&video_frame(2 * 3, pixels))
            .expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        // Header byte 13 is the colorspace.
        assert_eq!(pkt.data[13], 1, "colorspace=1 reaches the header");
        let img = crate::parse_qoi(&pkt.data).expect("valid stream");
        assert_eq!(img.colorspace, crate::QoiColorspace::AllLinear);
    }

    #[test]
    fn colorspace_symbolic_names_resolve() {
        for (name, expect) in [("srgb", 0u8), ("linear", 1u8)] {
            let pixels: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
            let mut p = params(2, 2, PixelFormat::Rgb24);
            p.options = CodecOptions::new().set("colorspace", name);
            let mut enc = make_encoder(&p).expect("make_encoder");
            enc.send_frame(&video_frame(2 * 3, pixels))
                .expect("send_frame");
            let pkt = enc.receive_packet().expect("receive_packet");
            assert_eq!(pkt.data[13], expect, "colorspace name {name:?} -> {expect}");
        }
    }

    #[test]
    fn colorspace_defaults_to_zero_when_unset() {
        let pixels: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Rgb24)).expect("make_encoder");
        enc.send_frame(&video_frame(2 * 3, pixels))
            .expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        assert_eq!(pkt.data[13], 0, "no option -> default colorspace 0");
    }

    #[test]
    fn output_params_echo_the_resolved_colorspace() {
        let mut p = params(4, 4, PixelFormat::Rgba);
        p.options = CodecOptions::new().set("colorspace", "1");
        let enc = make_encoder(&p).expect("make_encoder");
        assert_eq!(
            enc.output_params().options.get("colorspace"),
            Some("1"),
            "output_params reflect the resolved colorspace"
        );
    }

    #[test]
    fn invalid_colorspace_option_is_rejected_at_construction() {
        let mut p = params(2, 2, PixelFormat::Rgba);
        p.options = CodecOptions::new().set("colorspace", "banana");
        match make_encoder(&p) {
            Err(Error::InvalidData(_)) => {}
            Ok(_) => panic!("bogus colorspace must be rejected"),
            Err(other) => panic!("expected InvalidData, got {other:?}"),
        }
    }

    #[test]
    fn unknown_option_key_is_rejected_at_construction() {
        // Going through the schema-validated parse_options path means an
        // unrecognised key is a hard error, not silently ignored.
        let mut p = params(2, 2, PixelFormat::Rgba);
        p.options = CodecOptions::new().set("quality", "9");
        match make_encoder(&p) {
            Err(Error::InvalidData(_)) => {}
            Ok(_) => panic!("unknown option key must be rejected"),
            Err(other) => panic!("expected InvalidData, got {other:?}"),
        }
    }

    #[test]
    fn options_schema_lists_colorspace() {
        use oxideav_core::{CodecOptionsStruct, OptionKind};
        let schema = QoiEncoderOptions::SCHEMA;
        assert_eq!(schema.len(), 1, "QOI has exactly one encoder option");
        let f = &schema[0];
        assert_eq!(f.name, "colorspace");
        match f.kind {
            OptionKind::Enum(vals) => {
                assert!(vals.contains(&"srgb"));
                assert!(vals.contains(&"linear"));
                assert!(vals.contains(&"0"));
                assert!(vals.contains(&"1"));
            }
            other => panic!("colorspace should be an Enum, got {other:?}"),
        }
    }

    #[test]
    fn typed_options_default_is_colorspace_zero() {
        use oxideav_core::parse_options;
        let opts: QoiEncoderOptions = parse_options(&CodecOptions::new()).expect("empty parses");
        assert_eq!(opts.colorspace, 0);
        assert_eq!(opts, QoiEncoderOptions::default());
    }

    #[test]
    fn padded_stride_is_repacked_tight() {
        // A source plane with stride > width*channels (row padding) must
        // be repacked to QOI's tightly-packed layout before encoding.
        let w = 3u32;
        let h = 2u32;
        let row = (w * 4) as usize; // 12 bytes of real pixels per row
        let stride = row + 4; // 4 padding bytes per row
        let mut data = Vec::new();
        let mut tight = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let p = [(y * w + x) as u8, 1, 2, 255];
                data.extend_from_slice(&p);
                tight.extend_from_slice(&p);
            }
            data.extend_from_slice(&[0xAA; 4]); // padding (must be ignored)
        }
        assert_eq!(data.len(), stride * h as usize);
        let mut enc = make_encoder(&params(w, h, PixelFormat::Rgba)).expect("make_encoder");
        enc.send_frame(&video_frame(stride, data))
            .expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        let img = crate::parse_qoi(&pkt.data).expect("valid stream");
        assert_eq!(
            img.pixels, tight,
            "padding bytes are stripped, pixels are tight"
        );
    }

    #[test]
    fn unsupported_pixel_format_is_rejected() {
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Yuv420P)).expect("make_encoder");
        let pixels = vec![0u8; 2 * 2 * 4];
        let err = enc
            .send_frame(&video_frame(2 * 4, pixels))
            .expect_err("YUV is not a QOI pixel layout");
        assert!(matches!(err, Error::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn missing_pixel_format_is_rejected() {
        let mut p = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
        p.width = Some(2);
        p.height = Some(2);
        // pixel_format left None.
        let mut enc = make_encoder(&p).expect("make_encoder");
        let err = enc
            .send_frame(&video_frame(2 * 4, vec![0u8; 16]))
            .expect_err("missing pixel_format must error");
        assert!(matches!(err, Error::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn empty_plane_is_rejected() {
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Rgba)).expect("make_encoder");
        let err = enc
            .send_frame(&Frame::Video(VideoFrame {
                pts: None,
                planes: vec![],
            }))
            .expect_err("empty frame must error");
        assert!(matches!(err, Error::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn truncated_plane_is_rejected() {
        // A plane whose data is shorter than stride*height in the
        // repack path must be rejected, not read out of bounds.
        let mut enc = make_encoder(&params(4, 4, PixelFormat::Rgba)).expect("make_encoder");
        // Declare padded stride but supply far too few bytes.
        let err = enc
            .send_frame(&video_frame(4 * 4 + 8, vec![0u8; 10]))
            .expect_err("short plane must error");
        assert!(matches!(err, Error::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn non_video_frame_is_rejected() {
        use oxideav_core::AudioFrame;
        let mut enc = make_encoder(&params(2, 2, PixelFormat::Rgba)).expect("make_encoder");
        let audio = Frame::Audio(AudioFrame {
            samples: 1,
            pts: None,
            data: vec![vec![0u8; 4]],
        });
        let err = enc
            .send_frame(&audio)
            .expect_err("audio is not a QOI frame");
        assert!(matches!(err, Error::InvalidData(_)), "got {err:?}");
    }
}
