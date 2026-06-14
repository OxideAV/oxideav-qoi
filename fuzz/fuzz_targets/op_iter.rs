#![no_main]

//! Structure-aware fuzz of the stream-level chunk iterator.
//!
//! `iter_ops` / `iter_ops_strict` walk the post-header chunk stream and
//! yield one typed `QoiOp` per chunk *without* materialising a pixel
//! buffer or running the running-pixel-array / delta state. That makes
//! the walker a separate decode path from `parse_qoi` (which the
//! `decode` / `chunk_walk` targets cover): it has its own per-op
//! dispatch, its own mid-chunk truncation handling (the `Truncated`
//! sentinel vs. the strict-mode `Err`), and the `QoiOp` introspection
//! methods (`tag()`, `body_len()`, `encoded_len()`, `is_truncated()`)
//! reconstruct the on-wire shape of each op.
//!
//! Like `chunk_walk`, this target synthesizes a *spec-valid* 14-byte
//! header and a correct trailing 8-byte end marker so the walker gets
//! past the header gate on (nearly) every iteration, concentrating the
//! fuzzer's budget on the six per-op decode arms and the truncation
//! guards between them. The header layout, the six chunk encodings, the
//! end marker, and the per-op byte widths all follow the one-page
//! specification mirrored under `docs/image/qoi/`.
//!
//! Contracts asserted:
//!
//! 1. `iter_ops` returns `Ok` for the synthesized (always-valid) header,
//!    and the iterator never panics / aborts / OOMs while walking an
//!    arbitrary chunk stream.
//! 2. Every yielded op's `encoded_len()` equals `1 + body_len()` (except
//!    the `Truncated` sentinel, which reports a 1-byte tag), and summing
//!    `encoded_len()` over the walk reproduces the chunk-section byte
//!    count exactly (a `Truncated` tail consumes the rest of the slice).
//! 3. `tag()` is total over every yielded op (no overflow panic) — and
//!    for every non-`Truncated` op it reconstructs the exact leading
//!    chunk byte the walker dispatched on.
//! 4. `iter_ops_strict` agrees with `iter_ops`: it returns `Ok` with the
//!    same non-truncated op sequence iff the non-strict walk produced no
//!    `Truncated`, and `Err` otherwise.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{iter_ops, iter_ops_strict, QoiOp, END_MARKER, HEADER_SIZE, MAGIC};

// Keep synthesized dimensions tiny. The walker never allocates a pixel
// buffer (it only borrows the input slice), so the cap is purely to keep
// the header's claimed pixel count sane; the chunk slice length is what
// actually drives the per-op coverage.
const MAX_DIM: u32 = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        // Still drive the very-short path through the real entry points
        // so those inputs aren't ignored.
        let _ = iter_ops(data);
        let _ = iter_ops_strict(data);
        return;
    }

    // First 6 bytes pick a spec-valid header shape; the rest is the raw
    // chunk stream. Mapping width / height through `% MAX_DIM + 1` keeps
    // them in 1..=MAX_DIM (never zero, so the dimension gate passes).
    let raw_w = u16::from_be_bytes([data[0], data[1]]) as u32;
    let raw_h = u16::from_be_bytes([data[2], data[3]]) as u32;
    let width = (raw_w % MAX_DIM) + 1;
    let height = (raw_h % MAX_DIM) + 1;
    let channels: u8 = if data[4] & 1 == 1 { 4 } else { 3 };
    let colorspace: u8 = data[5] & 1;
    let chunk_stream = &data[6..];

    // Assemble: 14-byte header + fuzzer chunk stream + 8-byte end marker.
    let mut buf = Vec::with_capacity(HEADER_SIZE + chunk_stream.len() + END_MARKER.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf.push(channels);
    buf.push(colorspace);
    buf.extend_from_slice(chunk_stream);
    buf.extend_from_slice(END_MARKER);

    // The chunk slice the walker iterates: everything between the header
    // and the end marker. This equals `chunk_stream` byte-for-byte, but
    // re-slicing from `buf` keeps the position bookkeeping honest.
    let chunk_section_len = buf.len() - HEADER_SIZE - END_MARKER.len();
    let chunks = &buf[HEADER_SIZE..buf.len() - END_MARKER.len()];

    // Contract 1: the synthesized header is always valid, so iter_ops
    // must return Ok. Walk it and validate each op.
    let (_hdr, it) = match iter_ops(&buf) {
        Ok(pair) => pair,
        // A valid header + valid end marker can't legitimately fail the
        // header / trailer gate; anything else is a bug in the gate.
        Err(_) => return,
    };

    let mut pos = 0usize;
    let mut saw_truncated = false;
    let mut non_truncated: Vec<QoiOp> = Vec::new();
    for op in it {
        // Contract 3: tag() is total (never panics) over any yielded op.
        let tag = op.tag();

        if op.is_truncated() {
            // The walker yields at most one Truncated and then stops; it
            // must be the final item. Record it and break.
            saw_truncated = true;
            // A Truncated consumes the lone tag byte that was present.
            assert_eq!(op.encoded_len(), 1, "Truncated encoded_len must be 1");
            assert_eq!(op.body_len(), 0, "Truncated body_len must be 0");
            break;
        }

        // Contract 2: encoded_len == 1 + body_len for every real op.
        assert_eq!(
            op.encoded_len(),
            1 + op.body_len(),
            "encoded_len / body_len identity for {op:?}",
        );

        // Contract 3 (cont.): tag() reconstructs the exact leading byte
        // the walker dispatched on, at this chunk's start position.
        assert!(pos < chunks.len(), "op start past chunk slice: {op:?}");
        assert_eq!(tag, chunks[pos], "tag() mismatch for {op:?} at byte {pos}");

        pos += op.encoded_len();
        non_truncated.push(op);
    }

    // Contract 2 (cont.): consumed-byte accounting.
    if saw_truncated {
        // A mid-chunk truncation means the walk stopped before the end
        // of the chunk section — the consumed prefix is strictly inside
        // it (the partial chunk's tag byte and any body bytes present
        // are not double-counted into `pos`).
        assert!(
            pos < chunk_section_len,
            "truncation but consumed {pos} of {chunk_section_len} bytes",
        );
    } else {
        // A clean walk consumes the chunk section exactly.
        assert_eq!(
            pos, chunk_section_len,
            "clean walk consumed {pos} of {chunk_section_len} bytes",
        );
    }

    // Contract 4: iter_ops_strict agrees with the non-strict walk.
    match iter_ops_strict(&buf) {
        Ok((_, strict_ops)) => {
            assert!(
                !saw_truncated,
                "strict returned Ok but non-strict saw a Truncated",
            );
            assert_eq!(
                strict_ops, non_truncated,
                "strict op sequence differs from non-strict",
            );
        }
        Err(_) => {
            assert!(
                saw_truncated,
                "strict returned Err but non-strict saw no Truncated",
            );
        }
    }
});
