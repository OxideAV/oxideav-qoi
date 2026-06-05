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
}
