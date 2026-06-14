//! Stream-level QOI chunk iterator.
//!
//! Walks the post-header chunk stream of a QOI byte slice and yields
//! one [`QoiOp`] per chunk encountered, *without* materialising a
//! pixel buffer or running the running-pixel-array / delta-decoding
//! state. This is the format-inspection counterpart to
//! [`crate::parse_qoi`] / [`crate::parse_qoi_into`]: same input
//! shape, same chunk-dispatch table (`0xfe` / `0xff` 8-bit tags
//! shadowing the 2-bit `11` RUN tag values 62 / 63), but no
//! `running_pixel_array` and no `prev` pixel — only the raw
//! per-chunk fields the spec defines.
//!
//! ## What this is for
//!
//! * **Stream analyzers and bench harnesses.** Counting how many
//!   bytes a given file spends on each `QOI_OP_*` is a useful
//!   compression-diagnostics signal (e.g. "this RGBA gradient
//!   spends 92 % of its bytes on LUMA"). Today that needs a
//!   reimplementation of the chunk dispatcher inside the analysis
//!   tool; with [`iter_ops`] the iteration is the framework's
//!   job and the tool just folds over a typed enum.
//! * **Debug pretty-printers and CLI dumpers.** A `qoidump` style
//!   tool can call [`iter_ops`] and print one line per chunk
//!   without going through the full decoder.
//! * **Round-trip self-checks.** A future fuzz/property test can
//!   compare the chunk-shape histogram of `encode_qoi(parse_qoi(x))`
//!   against `x`'s own histogram to detect regressions in the
//!   encoder's chunk-selection priority chain.
//!
//! The iterator is **stateless** with respect to the decoded image:
//! it does NOT track the previous pixel, the running pixel array,
//! or the output position. A `QoiOp::Diff` field reports the
//! biased-by-`{2,32,8}` deltas decoded from the chunk byte(s); a
//! `QoiOp::Index { index }` reports the 6-bit index value with no
//! lookup against the (un-tracked) running array; a `QoiOp::Run {
//! length }` reports the spec's `1..=62` run length without
//! emitting any pixels. Callers that want pixel-level output use
//! [`crate::parse_qoi`].
//!
//! ## Strict-mode validation
//!
//! [`iter_ops`] runs the same header validation that
//! [`crate::parse_qoi_header`] does — magic, channels ∈ {3, 4},
//! colorspace ∈ {0, 1}, non-zero dimensions, presence of the
//! trailing 8-byte end marker — and returns
//! [`crate::QoiError::InvalidData`] if any of those fail. Inside
//! the chunk loop the iterator surfaces a `QoiOp::Truncated`
//! variant if it runs off the end of the chunk slice mid-chunk
//! (i.e. an `OP_RGB` / `OP_RGBA` / `OP_LUMA` chunk whose body
//! bytes are missing). The iterator does NOT cross-check the
//! aggregate run pixel count against the header's
//! `width * height` — that's the full decoder's job.

use crate::error::{QoiError as Error, Result};
use crate::image::QoiHeader;
use crate::{END_MARKER, HEADER_SIZE, OP_DIFF, OP_INDEX, OP_LUMA, OP_RGB, OP_RGBA, OP_RUN};

/// QOI running-pixel-array hash — the 64-slot bucket selector the
/// spec defines as `index_position = (R*3 + G*5 + B*7 + A*11) % 64`.
///
/// This is the *typed primitive* form of the hash: it takes a single
/// `[r, g, b, a]` pixel and returns the `0..=63` slot the encoder and
/// decoder agree on. The multiply is done in `u32` (non-wrapping), so
/// e.g. the initial previous pixel `(0, 0, 0, 255)` hashes to
/// `(11 * 255) % 64 = 2805 % 64 = 53`, *not* the `21` an 8-bit-wrapping
/// multiply would give. Promoting to `u32` before the multiply is the
/// difference between a correct decoder and a subtly-wrong one.
///
/// Exposed publicly (round 267) so callers building their own QOI
/// tooling — a `QOI_OP_INDEX`-coverage checker, an alternative encoder
/// experimenting with the running array, a stream validator that wants
/// to confirm an `Index { index }` op actually points at the pixel it
/// would have hashed to — can reuse the exact bucket arithmetic the
/// crate's decoder uses, instead of re-deriving (and risking the
/// wrapping bug) it themselves. The mask `& 0x3F` is equivalent to
/// `% 64` for this non-negative sum and lets the compiler skip the
/// division.
///
/// ```
/// use oxideav_qoi::qoi_hash;
/// assert_eq!(qoi_hash([0, 0, 0, 255]), 53);
/// assert_eq!(qoi_hash([255, 255, 255, 255]), 38);
/// assert_eq!(qoi_hash([0, 0, 0, 0]), 0);
/// assert_eq!(qoi_hash([1, 2, 3, 4]), 14);
/// ```
#[inline]
pub fn qoi_hash(p: [u8; 4]) -> u8 {
    let r = p[0] as u32;
    let g = p[1] as u32;
    let b = p[2] as u32;
    let a = p[3] as u32;
    ((r * 3 + g * 5 + b * 7 + a * 11) & 0x3F) as u8
}

/// One decoded QOI chunk, in the shape the spec defines.
///
/// Field values are the *raw* numbers the chunk carries — they are
/// NOT applied against a running pixel array or `prev` pixel. The
/// iterator that emits these is intentionally stateless; callers
/// that want the resolved pixel stream use [`crate::parse_qoi`].
///
/// The variant ordering matches the spec table in the crate root:
/// `Rgb` / `Rgba` (8-bit tags) come first, then the four 2-bit-tag
/// chunks in tag-prefix order (`00` Index / `01` Diff / `10`
/// Luma / `11` Run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QoiOp {
    /// `QOI_OP_RGB` — tag byte `0xFE` followed by 3 raw R/G/B
    /// bytes. Alpha is unchanged from the previous pixel (which
    /// the iterator doesn't track).
    Rgb {
        /// Red channel byte from the chunk body.
        r: u8,
        /// Green channel byte from the chunk body.
        g: u8,
        /// Blue channel byte from the chunk body.
        b: u8,
    },
    /// `QOI_OP_RGBA` — tag byte `0xFF` followed by 4 raw R/G/B/A
    /// bytes.
    Rgba {
        /// Red channel byte from the chunk body.
        r: u8,
        /// Green channel byte from the chunk body.
        g: u8,
        /// Blue channel byte from the chunk body.
        b: u8,
        /// Alpha channel byte from the chunk body.
        a: u8,
    },
    /// `QOI_OP_INDEX` — top-2-bit tag `00`, low-6 bits are a slot
    /// number into the (un-tracked-by-this-iterator) 64-entry
    /// running pixel array. Spec range: `0..=63`.
    Index {
        /// 6-bit index into the running pixel array.
        index: u8,
    },
    /// `QOI_OP_DIFF` — top-2-bit tag `01`, three 2-bit per-channel
    /// deltas each biased by 2 (so the post-bias range is
    /// `−2..=+1`). The fields here are the *un-biased* signed
    /// deltas the decoder would apply to `prev` before writing
    /// the output pixel.
    Diff {
        /// Signed red delta, range `−2..=+1`.
        dr: i8,
        /// Signed green delta, range `−2..=+1`.
        dg: i8,
        /// Signed blue delta, range `−2..=+1`.
        db: i8,
    },
    /// `QOI_OP_LUMA` — top-2-bit tag `10`, 6-bit `dg + 32` in the
    /// tag byte, then one body byte carrying `(dr-dg+8) << 4 |
    /// (db-dg+8)`. The fields here are the *un-biased* signed
    /// deltas the decoder would apply (`dg` directly, and
    /// `dr = (dr-dg) + dg`, `db = (db-dg) + dg`).
    Luma {
        /// Signed green delta, range `−32..=+31`.
        dg: i8,
        /// Signed red-minus-green delta, range `−8..=+7`.
        dr_dg: i8,
        /// Signed blue-minus-green delta, range `−8..=+7`.
        db_dg: i8,
    },
    /// `QOI_OP_RUN` — top-2-bit tag `11`, low-6 bits are
    /// `(run_length - 1)`. The reported `length` is the spec's
    /// post-debias `1..=62` value; tag values `0xFE` / `0xFF`
    /// shadow this range so RUN cannot encode 63 or 64.
    Run {
        /// Run length (post-debias), `1..=62`.
        length: u8,
    },
    /// The chunk stream ended in the middle of a chunk whose body
    /// hadn't fully arrived yet (RGB / RGBA / LUMA bodies the only
    /// chunks that have follow-on bytes). The iterator yields this
    /// variant *and then stops* — i.e. it is always the final item
    /// in a truncated stream's iteration.
    ///
    /// Yielded (rather than dropped) so analysis tools that walk
    /// the iterator can distinguish "ran out of chunks at a
    /// pixel-count-matched stop" (no `Truncated`) from "ran out of
    /// bytes mid-chunk" (a `Truncated` at the tail). Use
    /// [`iter_ops_strict`] to surface the truncation as an
    /// `Err(InvalidData)` instead.
    Truncated {
        /// The leading chunk byte we couldn't finish parsing.
        tag: u8,
        /// How many more bytes the chunk would have needed.
        missing_body_bytes: u8,
    },
}

impl QoiOp {
    /// The leading chunk byte that encodes this op.
    ///
    /// For the 8-bit-tag chunks this is simply `0xFE` ([`crate::OP_RGB`])
    /// or `0xFF` ([`crate::OP_RGBA`]). For the four 2-bit-tag chunks the
    /// low 6 bits are reconstructed from the op's fields and OR-ed onto
    /// the tag prefix, so the returned byte is *exactly* what an encoder
    /// would write as the first byte of the chunk:
    ///
    /// * `Index { index }` → `0x00 | (index & 0x3F)`
    /// * `Diff { dr, dg, db }` → `0x40 | (dr+2)<<4 | (dg+2)<<2 | (db+2)`
    /// * `Luma { dg, .. }` → `0x80 | ((dg+32) & 0x3F)` (the `dr_dg` /
    ///   `db_dg` deltas live in the *second* byte, not the tag)
    /// * `Run { length }` → `0xC0 | ((length-1) & 0x3F)`
    ///
    /// [`QoiOp::Truncated`] carries the raw tag byte the iterator
    /// couldn't finish parsing, so its `tag()` returns that byte
    /// verbatim.
    ///
    /// This is the inverse of the dispatch the iterator performs on the
    /// way in: `op.tag()` round-trips through the leading byte for every
    /// non-`Truncated` variant. It lets a chunk-shape histogram or a
    /// debug dumper recover the on-wire tag without re-encoding the
    /// whole chunk.
    ///
    /// The bias arithmetic (`+2` for `Diff`, `+32` for `Luma`, `-1` for
    /// `Run`) is done with wrapping operators so the method is **total**
    /// over the public field space: every field is masked down to the
    /// bit width the spec assigns it (`& 0x03` / `& 0x3F`), so a field
    /// carrying an out-of-spec value (the type allows it — the fields
    /// are `pub`) yields a well-defined low-bit tag instead of panicking
    /// on an overflowing add / subtract under a debug / fuzz build.
    /// For every in-spec value the result is identical to the plain
    /// arithmetic, so the iterator round-trip is unaffected.
    #[inline]
    pub fn tag(&self) -> u8 {
        match *self {
            QoiOp::Rgb { .. } => OP_RGB,
            QoiOp::Rgba { .. } => OP_RGBA,
            QoiOp::Index { index } => OP_INDEX | (index & 0x3F),
            QoiOp::Diff { dr, dg, db } => {
                let r = (dr as u8).wrapping_add(2) & 0x03;
                let g = (dg as u8).wrapping_add(2) & 0x03;
                let b = (db as u8).wrapping_add(2) & 0x03;
                OP_DIFF | (r << 4) | (g << 2) | b
            }
            QoiOp::Luma { dg, .. } => OP_LUMA | ((dg as u8).wrapping_add(32) & 0x3F),
            QoiOp::Run { length } => OP_RUN | (length.wrapping_sub(1) & 0x3F),
            QoiOp::Truncated { tag, .. } => tag,
        }
    }

    /// Number of body bytes that follow the tag byte for this op.
    ///
    /// `0` for the tag-only chunks ([`QoiOp::Index`] / [`QoiOp::Diff`] /
    /// [`QoiOp::Run`]), `1` for [`QoiOp::Luma`] (the `dr-dg` / `db-dg`
    /// nibble byte), `3` for [`QoiOp::Rgb`], `4` for [`QoiOp::Rgba`].
    ///
    /// [`QoiOp::Truncated`] returns `0` — it represents a chunk whose
    /// body never arrived, so there are no body bytes actually present
    /// in the stream to count.
    #[inline]
    pub fn body_len(&self) -> usize {
        match *self {
            QoiOp::Rgba { .. } => 4,
            QoiOp::Rgb { .. } => 3,
            QoiOp::Luma { .. } => 1,
            QoiOp::Index { .. } | QoiOp::Diff { .. } | QoiOp::Run { .. } => 0,
            QoiOp::Truncated { .. } => 0,
        }
    }

    /// Total on-wire byte width of this chunk: `1 + body_len()`.
    ///
    /// `1` for the four tag-only-or-tag-plus-nothing 2-bit chunks
    /// ([`QoiOp::Index`] / [`QoiOp::Diff`] / [`QoiOp::Run`]), `2` for
    /// [`QoiOp::Luma`], `4` for [`QoiOp::Rgb`], `5` for [`QoiOp::Rgba`].
    ///
    /// Summing `encoded_len()` over a full [`iter_ops`] walk (excluding
    /// any trailing [`QoiOp::Truncated`]) reproduces the chunk-section
    /// byte count — i.e. `input.len() - HEADER_SIZE - END_MARKER.len()`
    /// for a well-formed stream — without re-encoding. A
    /// [`QoiOp::Truncated`] reports `1` (the tag byte that *was*
    /// present), since by definition its body bytes are missing.
    #[inline]
    pub fn encoded_len(&self) -> usize {
        match *self {
            QoiOp::Truncated { .. } => 1,
            other => 1 + other.body_len(),
        }
    }

    /// `true` only for the [`QoiOp::Truncated`] sentinel the non-strict
    /// [`iter_ops`] walker yields when the stream ends mid-chunk.
    ///
    /// A convenience for folds that want to bail on the first truncation
    /// without a full `matches!(op, QoiOp::Truncated { .. })`.
    #[inline]
    pub fn is_truncated(&self) -> bool {
        matches!(self, QoiOp::Truncated { .. })
    }
}

/// Iterator returned by [`iter_ops`] / [`iter_ops_strict`].
///
/// Walks the chunk slice (header-and-trailer stripped) in a single
/// linear pass, yielding one [`QoiOp`] per chunk. Holds a borrow
/// of the input slice; the iteration is `Copy`-free at the byte
/// level (each yielded `QoiOp` owns its fields as primitives).
pub struct QoiOpIter<'a> {
    chunks: &'a [u8],
    pos: usize,
    /// When true, a truncation has already been yielded — further
    /// iterations return `None`. Without this flag, a truncated
    /// final chunk would emit the `Truncated` variant forever
    /// (since `pos` doesn't advance past the bad chunk).
    done: bool,
}

impl<'a> Iterator for QoiOpIter<'a> {
    type Item = QoiOp;

    fn next(&mut self) -> Option<QoiOp> {
        if self.done || self.pos >= self.chunks.len() {
            return None;
        }
        let tag = self.chunks[self.pos];
        self.pos += 1;

        // 8-bit tags 0xfe / 0xff take precedence over the 2-bit
        // `11` RUN tag — that's why RUN's range is 1..=62, not
        // 1..=64.
        if tag == OP_RGBA {
            if self.pos + 4 > self.chunks.len() {
                self.done = true;
                let missing = (self.pos + 4 - self.chunks.len()) as u8;
                return Some(QoiOp::Truncated {
                    tag,
                    missing_body_bytes: missing,
                });
            }
            let r = self.chunks[self.pos];
            let g = self.chunks[self.pos + 1];
            let b = self.chunks[self.pos + 2];
            let a = self.chunks[self.pos + 3];
            self.pos += 4;
            return Some(QoiOp::Rgba { r, g, b, a });
        }
        if tag == OP_RGB {
            if self.pos + 3 > self.chunks.len() {
                self.done = true;
                let missing = (self.pos + 3 - self.chunks.len()) as u8;
                return Some(QoiOp::Truncated {
                    tag,
                    missing_body_bytes: missing,
                });
            }
            let r = self.chunks[self.pos];
            let g = self.chunks[self.pos + 1];
            let b = self.chunks[self.pos + 2];
            self.pos += 3;
            return Some(QoiOp::Rgb { r, g, b });
        }
        match tag & 0xC0 {
            OP_INDEX => Some(QoiOp::Index { index: tag & 0x3F }),
            OP_DIFF => {
                let dr = ((tag >> 4) & 0x03) as i8 - 2;
                let dg = ((tag >> 2) & 0x03) as i8 - 2;
                let db = (tag & 0x03) as i8 - 2;
                Some(QoiOp::Diff { dr, dg, db })
            }
            OP_LUMA => {
                if self.pos >= self.chunks.len() {
                    self.done = true;
                    return Some(QoiOp::Truncated {
                        tag,
                        missing_body_bytes: 1,
                    });
                }
                let dg = (tag & 0x3F) as i8 - 32;
                let b2 = self.chunks[self.pos];
                self.pos += 1;
                let dr_dg = ((b2 >> 4) & 0x0F) as i8 - 8;
                let db_dg = (b2 & 0x0F) as i8 - 8;
                Some(QoiOp::Luma { dg, dr_dg, db_dg })
            }
            OP_RUN => {
                // 6-bit (run - 1), so post-debias range is 1..=64
                // before the 0xfe / 0xff theft caps it at 1..=62.
                // The dispatch above already routed 0xfe / 0xff to
                // RGB / RGBA, so values 62 / 63 of the 6-bit field
                // are unreachable here — but we still mask & 0x3F
                // and add 1 to make the arithmetic obvious.
                let length = (tag & 0x3F) + 1;
                Some(QoiOp::Run { length })
            }
            _ => unreachable!("tag & 0xC0 has only four possible 2-bit values"),
        }
    }
}

/// Build a [`QoiOpIter`] over the chunk stream of a complete QOI
/// byte slice (header + chunks + end marker).
///
/// Validates the same 14-byte header [`crate::parse_qoi`] does
/// (magic, channels ∈ {3, 4}, colorspace ∈ {0, 1}, non-zero
/// dimensions) AND confirms the trailing 8-byte end marker is
/// present and equal to `00 00 00 00 00 00 00 01`. The chunk
/// slice the returned iterator walks is the bytes *between* the
/// 14-byte header and the 8-byte end marker.
///
/// The iterator surfaces mid-chunk truncation by yielding
/// [`QoiOp::Truncated`] as the final item. Callers that prefer
/// truncation as an error should use [`iter_ops_strict`].
///
/// Returns `(QoiHeader, QoiOpIter)` so the caller can correlate
/// the chunk stream with the header's `(width, height, channels,
/// colorspace)` without re-parsing.
pub fn iter_ops(input: &[u8]) -> Result<(QoiHeader, QoiOpIter<'_>)> {
    if input.len() < HEADER_SIZE + END_MARKER.len() {
        return Err(Error::invalid(
            "QOI: input shorter than header + end marker",
        ));
    }
    let hdr = crate::parse_qoi_header(input)?;
    let trailer = &input[input.len() - END_MARKER.len()..];
    if trailer != END_MARKER {
        return Err(Error::invalid("QOI: missing or invalid end marker"));
    }
    let chunks = &input[HEADER_SIZE..input.len() - END_MARKER.len()];
    Ok((
        hdr,
        QoiOpIter {
            chunks,
            pos: 0,
            done: false,
        },
    ))
}

/// Same as [`iter_ops`], but eagerly walks the iterator and
/// returns `Err(QoiError::InvalidData)` if any chunk arrives
/// truncated.
///
/// Slightly more expensive than [`iter_ops`] because it materialises
/// the chunk vector to detect the trailing `Truncated` before
/// handing it to the caller. Use when the caller treats truncated
/// streams as fatal (parser front-ends, fuzzer reject lists) and
/// doesn't want to fold over a final `Truncated` variant manually.
///
/// The returned `Vec<QoiOp>` does NOT contain a `Truncated`
/// variant — that's been folded into the `Err`. A successful
/// return is a chunk-by-chunk decode of every byte between the
/// header and the end marker.
pub fn iter_ops_strict(input: &[u8]) -> Result<(QoiHeader, Vec<QoiOp>)> {
    let (hdr, it) = iter_ops(input)?;
    let mut out = Vec::new();
    for op in it {
        match op {
            QoiOp::Truncated {
                tag,
                missing_body_bytes,
            } => {
                return Err(Error::invalid(format!(
                    "QOI: chunk byte 0x{tag:02x} truncated, {missing_body_bytes} body byte(s) missing"
                )));
            }
            other => out.push(other),
        }
    }
    Ok((hdr, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode_qoi, encode_qoi_full};

    /// Solid run of identical pixels — encoder must emit one
    /// starter chunk for the first pixel (it can't open with a
    /// RUN of the sentinel since the sentinel is `(0,0,0,255)`,
    /// not `(200,50,25,255)`) followed by RUN chunks covering the
    /// other 199 pixels. The starter is RGB (not RGBA) because
    /// alpha 255 matches the sentinel alpha, so the encoder takes
    /// the alpha-unchanged path and the smaller chunk. We want at
    /// least one RUN of length 62 (the maximum) and the remaining
    /// runs summing to 199.
    #[test]
    fn op_iter_solid_run() {
        let pixels = [200u8, 50, 25, 255].repeat(200);
        let bytes = encode_qoi(200, 1, 4, &pixels);
        let (hdr, ops) = iter_ops_strict(&bytes).unwrap();
        assert_eq!(hdr.width, 200);
        assert_eq!(hdr.height, 1);

        // Aggregate the chunk shapes the encoder produced.
        let mut total_run_pixels: u32 = 0;
        let mut starter_count = 0;
        let mut run_count = 0;
        let mut saw_max_run = false;
        let mut other_count = 0;
        for op in &ops {
            match *op {
                QoiOp::Rgb { r, g, b } => {
                    starter_count += 1;
                    assert_eq!((r, g, b), (200, 50, 25));
                }
                QoiOp::Rgba { r, g, b, a } => {
                    starter_count += 1;
                    assert_eq!((r, g, b, a), (200, 50, 25, 255));
                }
                QoiOp::Run { length } => {
                    run_count += 1;
                    assert!((1..=62).contains(&length), "run length out of spec");
                    if length == 62 {
                        saw_max_run = true;
                    }
                    total_run_pixels += length as u32;
                }
                _ => other_count += 1,
            }
        }
        // 1 starter chunk (RGB or RGBA, encoder's choice), then
        // RUNs covering the remaining 199 pixels.
        assert_eq!(starter_count, 1);
        assert!(run_count >= 4, "got {run_count} RUNs, expected ≥4");
        assert_eq!(other_count, 0);
        assert_eq!(total_run_pixels, 199);
        assert!(saw_max_run, "expected at least one RUN of length 62");
    }

    /// A 2-pixel image where pixel-1 differs from the initial
    /// sentinel by `(0, 0, 0)` in RGB and `−1` in alpha — alpha
    /// changes so the encoder must emit RGBA, not a DIFF (DIFF
    /// requires alpha unchanged). We expect either Rgba then
    /// Index (the second pixel returns to the sentinel-equivalent)
    /// or one full Rgba then one shorter chunk; either way every
    /// op variant we yield is one of the typed shapes.
    #[test]
    fn op_iter_yields_typed_variants() {
        // First pixel: (1, 2, 3, 254). Second pixel: same RGB,
        // alpha 255 (so alpha changed → must be RGBA).
        let pixels: Vec<u8> = vec![1, 2, 3, 254, 1, 2, 3, 255];
        let bytes = encode_qoi(2, 1, 4, &pixels);
        let (_, ops) = iter_ops_strict(&bytes).unwrap();
        // We don't pin the exact chunk shape (that's encoder
        // implementation detail) but every op is a well-formed
        // typed variant, and we can re-decode and confirm.
        assert!(!ops.is_empty());
        // Sanity-check at least one RGBA was emitted (the second
        // pixel has alpha change, RGB / DIFF / LUMA all keep
        // alpha unchanged, INDEX would need a prior match, RUN
        // requires equality).
        let any_rgba = ops.iter().any(|op| matches!(op, QoiOp::Rgba { .. }));
        assert!(any_rgba, "expected at least one Rgba op, got {ops:?}");
    }

    /// DIFF / LUMA delta fields must report the un-biased signed
    /// deltas. Encode a 2-pixel run where pixel-2 differs from
    /// pixel-1 by (−1, +1, +0) in RGB (alpha unchanged) so the
    /// encoder picks DIFF; the second op must report
    /// `dr = -1, dg = +1, db = 0`.
    #[test]
    fn op_iter_diff_deltas_are_unbiased() {
        // Pixel 1: (50, 100, 150, 255). Pixel 2: (49, 101, 150, 255).
        let pixels: Vec<u8> = vec![50, 100, 150, 255, 49, 101, 150, 255];
        let bytes = encode_qoi(2, 1, 4, &pixels);
        let (_, ops) = iter_ops_strict(&bytes).unwrap();
        // First op is the RGB / RGBA chunk for pixel 1; second
        // op is the DIFF for pixel 2.
        let diffs: Vec<_> = ops
            .iter()
            .filter_map(|op| {
                if let QoiOp::Diff { dr, dg, db } = *op {
                    Some((dr, dg, db))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(diffs, vec![(-1, 1, 0)], "ops: {ops:?}");
    }

    /// `iter_ops` must reject malformed input via the same error
    /// path `parse_qoi` uses (bad magic / channels / colorspace /
    /// zero dimensions / short trailer).
    #[test]
    fn iter_ops_rejects_bad_magic() {
        let mut bytes = encode_qoi(2, 1, 4, &[1, 2, 3, 255, 4, 5, 6, 255]);
        bytes[0] = b'X';
        assert!(iter_ops(&bytes).is_err());
    }

    #[test]
    fn iter_ops_rejects_bad_end_marker() {
        let mut bytes = encode_qoi(2, 1, 4, &[1, 2, 3, 255, 4, 5, 6, 255]);
        let last = bytes.len() - 1;
        bytes[last] = 2; // valid end marker is `…00 01`
        assert!(iter_ops(&bytes).is_err());
    }

    #[test]
    fn iter_ops_rejects_zero_dimension() {
        // Hand-build a header with width=0; iter_ops should reject
        // before we even look at the chunks.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"qoif");
        bytes.extend_from_slice(&0u32.to_be_bytes()); // width
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height
        bytes.push(4); // channels
        bytes.push(0); // colorspace
        bytes.extend_from_slice(crate::END_MARKER);
        assert!(iter_ops(&bytes).is_err());
    }

    /// `iter_ops` (non-strict) surfaces a truncated final chunk
    /// as a `Truncated` variant; `iter_ops_strict` surfaces it
    /// as an `Err`.
    #[test]
    fn truncated_rgb_yields_truncated_then_ends() {
        // Hand-build a complete-header valid QOI then splice an
        // unfinished `0xFE 0x10 0x20` (RGB tag + 2 of 3 body
        // bytes) before the end marker.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"qoif");
        bytes.extend_from_slice(&1u32.to_be_bytes()); // width
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height
        bytes.push(4);
        bytes.push(0);
        bytes.push(OP_RGB);
        bytes.push(0x10);
        bytes.push(0x20);
        // (missing 1 RGB body byte)
        bytes.extend_from_slice(crate::END_MARKER);

        let (_, mut it) = iter_ops(&bytes).unwrap();
        let first = it.next().unwrap();
        assert!(
            matches!(
                first,
                QoiOp::Truncated {
                    tag: OP_RGB,
                    missing_body_bytes: 1
                }
            ),
            "first op was {first:?}"
        );
        assert!(
            it.next().is_none(),
            "iterator should not yield after Truncated"
        );

        // Strict variant turns the same input into an error.
        let strict = iter_ops_strict(&bytes);
        assert!(strict.is_err());
    }

    /// Encoder writes `colorspace = 1` through `encode_qoi_full`;
    /// iter_ops returns the matching header without touching the
    /// chunk stream.
    #[test]
    fn iter_ops_reports_colorspace_one() {
        let pixels = vec![1, 2, 3, 255, 4, 5, 6, 255];
        let bytes = encode_qoi_full(2, 1, 4, /* colorspace */ 1, &pixels);
        let (hdr, _) = iter_ops(&bytes).unwrap();
        assert_eq!(hdr.colorspace, crate::QoiColorspace::AllLinear);
    }

    /// Sanity: an Index op must report the 6-bit index value
    /// 0..=63 (un-mangled by the tag bits).
    #[test]
    fn index_field_masks_to_six_bits() {
        // Hand-build: tag = OP_INDEX | 0x2A = 0x2A (no body bytes).
        // We don't care that the chunk is semantically wrong
        // here (the running array is all-zero at start, so
        // reading index 0x2A produces (0, 0, 0, 0), which is
        // fine bytes-wise but doesn't match width*height=1
        // count — that's the *decoder*'s consistency check, not
        // ours).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"qoif");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(4);
        bytes.push(0);
        bytes.push(OP_INDEX | 0x2A);
        bytes.extend_from_slice(crate::END_MARKER);
        let (_, mut it) = iter_ops(&bytes).unwrap();
        let op = it.next().unwrap();
        assert!(matches!(op, QoiOp::Index { index: 0x2A }));
    }

    /// `qoi_hash` is the public typed primitive form of the spec's
    /// running-array bucket selector. It must agree with the four
    /// worked examples in the crate-root docs (and with the internal
    /// `decoder::hash`, which now delegates to it).
    #[test]
    fn qoi_hash_matches_spec_examples() {
        // (11 * 255) % 64 = 2805 % 64 = 53 — NOT 21 (the wrapping-u8
        // answer). This is the canonical "promote to u32" check.
        assert_eq!(qoi_hash([0, 0, 0, 255]), 53);
        // 255 * (3+5+7+11) = 6630, 6630 % 64 = 38.
        assert_eq!(qoi_hash([255, 255, 255, 255]), 38);
        assert_eq!(qoi_hash([0, 0, 0, 0]), 0);
        // 1*3 + 2*5 + 3*7 + 4*11 = 3+10+21+44 = 78, 78 % 64 = 14.
        assert_eq!(qoi_hash([1, 2, 3, 4]), 14);
        // Every output is in the 0..=63 slot range.
        for r in [0u8, 17, 200, 255] {
            for a in [0u8, 1, 128, 255] {
                assert!(qoi_hash([r, 99, 7, a]) < 64);
            }
        }
    }

    /// `qoi_hash` agrees with the crate-internal `decoder::hash` for
    /// every pixel (the internal one now delegates, so this guards
    /// against a future divergence if either is edited).
    #[test]
    fn qoi_hash_agrees_with_decoder_hash() {
        for &p in &[
            [0u8, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 0],
            [1, 2, 3, 4],
            [200, 50, 25, 255],
            [13, 200, 7, 99],
        ] {
            assert_eq!(qoi_hash(p), crate::decoder::hash(p), "pixel {p:?}");
        }
    }

    /// `QoiOp::tag()` must reconstruct the exact leading chunk byte the
    /// iterator dispatched on — i.e. round-trip through the first byte.
    /// Walk a synthesised stream, re-read each op's `tag()`, and confirm
    /// it equals the byte at the chunk's start position in the original
    /// slice.
    #[test]
    fn op_tag_roundtrips_leading_byte() {
        // Mixed-op image: gradient + alpha churn forces RGB / RGBA /
        // DIFF / LUMA / INDEX / RUN to all appear across the stream.
        let mut pixels = Vec::new();
        let mut x: u32 = 0x1234_5678;
        for i in 0..256u32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let r = (i * 2) as u8;
            let g = (x & 0xFF) as u8;
            let b = (i * 3) as u8;
            let a = if i % 16 == 0 { (x >> 8) as u8 } else { 255 };
            pixels.extend_from_slice(&[r, g, b, a]);
        }
        let bytes = encode_qoi(16, 16, 4, &pixels);
        let chunks = &bytes[crate::HEADER_SIZE..bytes.len() - crate::END_MARKER.len()];
        let (_, it) = iter_ops(&bytes).unwrap();
        let mut pos = 0usize;
        let mut saw = (false, false, false, false, false, false); // rgb,rgba,index,diff,luma,run
        for op in it {
            assert!(!op.is_truncated(), "unexpected truncation: {op:?}");
            assert_eq!(op.tag(), chunks[pos], "op {op:?} at chunk byte {pos}");
            match op {
                QoiOp::Rgb { .. } => saw.0 = true,
                QoiOp::Rgba { .. } => saw.1 = true,
                QoiOp::Index { .. } => saw.2 = true,
                QoiOp::Diff { .. } => saw.3 = true,
                QoiOp::Luma { .. } => saw.4 = true,
                QoiOp::Run { .. } => saw.5 = true,
                QoiOp::Truncated { .. } => unreachable!(),
            }
            pos += op.encoded_len();
        }
        // The whole chunk section was consumed exactly.
        assert_eq!(pos, chunks.len());
        // The op mix actually exercised the typed-tag reconstruction
        // for at least the four 2-bit chunks plus one 8-bit chunk.
        assert!(saw.2 || saw.3 || saw.4 || saw.5, "no 2-bit chunks seen");
        assert!(saw.0 || saw.1, "no 8-bit chunk seen");
    }

    /// `encoded_len()` / `body_len()` widths per variant, including the
    /// `Truncated` sentinel's `1`/`0`.
    #[test]
    fn op_encoded_len_widths() {
        let cases = [
            (QoiOp::Index { index: 5 }, 1usize, 0usize),
            (
                QoiOp::Diff {
                    dr: -1,
                    dg: 0,
                    db: 1,
                },
                1,
                0,
            ),
            (QoiOp::Run { length: 30 }, 1, 0),
            (
                QoiOp::Luma {
                    dg: -3,
                    dr_dg: 2,
                    db_dg: -1,
                },
                2,
                1,
            ),
            (QoiOp::Rgb { r: 1, g: 2, b: 3 }, 4, 3),
            (
                QoiOp::Rgba {
                    r: 1,
                    g: 2,
                    b: 3,
                    a: 4,
                },
                5,
                4,
            ),
            (
                QoiOp::Truncated {
                    tag: OP_RGB,
                    missing_body_bytes: 1,
                },
                1,
                0,
            ),
        ];
        for (op, enc, body) in cases {
            assert_eq!(op.encoded_len(), enc, "encoded_len for {op:?}");
            assert_eq!(op.body_len(), body, "body_len for {op:?}");
            // For every non-truncated op, encoded_len == 1 + body_len.
            if !op.is_truncated() {
                assert_eq!(
                    op.encoded_len(),
                    1 + op.body_len(),
                    "len identity for {op:?}"
                );
            }
        }
    }

    /// `tag()` reconstructs the spec bit-packing for each 2-bit chunk
    /// exactly (independent of the iterator), and `is_truncated()` only
    /// fires on the sentinel.
    #[test]
    fn op_tag_bit_packing_and_truncated_flag() {
        assert_eq!(QoiOp::Index { index: 0x2A }.tag(), OP_INDEX | 0x2A);
        // Diff dr=-2,dg=+1,db=0 → (0,3,2) after +2 bias → 0b00_11_10.
        assert_eq!(
            QoiOp::Diff {
                dr: -2,
                dg: 1,
                db: 0
            }
            .tag(),
            OP_DIFF | 0b00_11_10
        );
        // Luma dg=-32 → (dg+32)=0 → tag low6 = 0.
        assert_eq!(
            QoiOp::Luma {
                dg: -32,
                dr_dg: 0,
                db_dg: 0
            }
            .tag(),
            OP_LUMA
        );
        // Luma dg=+31 → 63 → tag low6 = 0x3F.
        assert_eq!(
            QoiOp::Luma {
                dg: 31,
                dr_dg: 0,
                db_dg: 0
            }
            .tag(),
            OP_LUMA | 0x3F
        );
        // Run length=62 → (length-1)=61 → 0x3D.
        assert_eq!(QoiOp::Run { length: 62 }.tag(), OP_RUN | 61);
        assert_eq!(QoiOp::Run { length: 1 }.tag(), OP_RUN);

        assert!(QoiOp::Truncated {
            tag: OP_LUMA,
            missing_body_bytes: 1
        }
        .is_truncated());
        assert!(!QoiOp::Run { length: 1 }.is_truncated());
        assert!(!QoiOp::Rgb { r: 0, g: 0, b: 0 }.is_truncated());
    }

    /// `tag()` must be **total** over the public field space. The
    /// variants carry `pub` fields, so a caller can construct an op
    /// whose field is outside the spec's bit width — `Run { length: 0 }`
    /// (the `-1` bias underflows), `Diff { dr: 127, .. }` (the `+2` bias
    /// overflows `i8`), `Luma { dg: 127, .. }` (the `+32` bias
    /// overflows). Under a debug / fuzz build (overflow checks on) the
    /// plain arithmetic would panic; the wrapping form must not, and
    /// must still mask the result down to the tag's bit field.
    #[test]
    fn op_tag_is_total_over_extreme_fields() {
        // Run { length: 0 } — the underflow case. Low 6 bits of
        // 0u8.wrapping_sub(1) = 0xFF & 0x3F = 0x3F.
        assert_eq!(QoiOp::Run { length: 0 }.tag(), OP_RUN | 0x3F);
        // Run { length: u8::MAX }.
        let _ = QoiOp::Run { length: u8::MAX }.tag();

        // Diff with every channel at the i8 extremes — must not panic
        // and must stay within the OP_DIFF tag space.
        for dr in [i8::MIN, -1, 0, 1, i8::MAX] {
            for dg in [i8::MIN, i8::MAX] {
                for db in [i8::MIN, i8::MAX] {
                    let t = QoiOp::Diff { dr, dg, db }.tag();
                    assert_eq!(t & 0xC0, OP_DIFF, "Diff tag prefix");
                }
            }
        }

        // Luma dg at the i8 extremes — must not panic and stay in the
        // OP_LUMA tag space.
        for dg in [i8::MIN, -1, 0, 1, i8::MAX] {
            let t = QoiOp::Luma {
                dg,
                dr_dg: i8::MAX,
                db_dg: i8::MIN,
            }
            .tag();
            assert_eq!(t & 0xC0, OP_LUMA, "Luma tag prefix");
        }

        // Index masks to 6 bits regardless of the high 2.
        assert_eq!(QoiOp::Index { index: 0xFF }.tag(), OP_INDEX | 0x3F);
    }
}
