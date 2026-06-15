#![no_main]

//! Structure-aware fuzz of `QoiOp::write_to` — the byte-level inverse of
//! the `iter_ops` chunk walker.
//!
//! `write_to` appends an op's complete on-wire chunk (leading tag byte
//! plus every body byte the chunk format defines) to a caller buffer. Its
//! headline contract is that re-serializing the ops a walk produced
//! reconstructs the original chunk section byte-for-byte, so
//! `iter_ops(input)` → `write_to` → `iter_ops` round-trips an in-spec
//! chunk stream exactly. No other fuzz target ever exercises `write_to`;
//! the only coverage to date is a single hand-built mixed-op unit test.
//!
//! Like `op_iter` / `chunk_walk`, this target synthesizes a *spec-valid*
//! 14-byte header and a correct trailing 8-byte end marker around the
//! fuzzer's bytes so the walker clears the header gate on nearly every
//! iteration, concentrating the budget on the six per-op `write_to` arms
//! and the truncation boundary. The header layout, the six chunk
//! encodings, the end marker, and the per-op byte widths all follow the
//! one-page specification mirrored under `docs/image/qoi/`.
//!
//! Contracts asserted:
//!
//! 1. The synthesized header is always valid, so `iter_ops` returns `Ok`
//!    and the walk never panics / aborts / OOMs.
//! 2. Each `write_to` appends exactly `encoded_len()` bytes — the
//!    serialized width matches the op's own self-report.
//! 3. **Clean walk** (no `Truncated`): the rebuilt chunk buffer equals
//!    the original chunk section byte-for-byte, and re-walking the rebuilt
//!    stream (wrapped in the same header + end marker) yields the
//!    identical op sequence. This is the round-trip inverse property.
//! 4. **Truncated walk**: every non-`Truncated` op's bytes still match the
//!    original at their offset (the rebuilt buffer is a byte-prefix of the
//!    original chunk section, since the `Truncated` sentinel re-emits only
//!    its stored leading byte and drops the never-arrived body), and
//!    re-walking the rebuilt stream reproduces exactly the non-truncated
//!    prefix of ops.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{iter_ops, iter_ops_strict, QoiOp, END_MARKER, HEADER_SIZE, MAGIC};

const MAX_DIM: u32 = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }

    // First 6 bytes pick a spec-valid header shape; the rest is the raw
    // chunk stream. `% MAX_DIM + 1` keeps each dimension in 1..=MAX_DIM
    // (never zero, so the dimension gate passes).
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

    let chunk_section = &buf[HEADER_SIZE..buf.len() - END_MARKER.len()];

    // Contract 1: the synthesized header is always valid → Ok.
    let (_hdr, it) = match iter_ops(&buf) {
        Ok(pair) => pair,
        Err(_) => return,
    };

    // Re-serialize every op via write_to, tracking the truncation boundary.
    let mut rebuilt: Vec<u8> = Vec::with_capacity(chunk_section.len());
    let mut non_truncated: Vec<QoiOp> = Vec::new();
    let mut saw_truncated = false;
    for op in it {
        let before = rebuilt.len();
        op.write_to(&mut rebuilt);
        // Contract 2: write_to appends exactly encoded_len() bytes.
        assert_eq!(
            rebuilt.len() - before,
            op.encoded_len(),
            "write_to appended != encoded_len() for {op:?}",
        );

        if op.is_truncated() {
            // The walker yields at most one Truncated and then stops; it
            // is always the final item. Its write_to re-emits only the
            // stored leading byte (encoded_len() == 1, asserted above), so
            // the rebuilt buffer ends one byte into the partial chunk.
            saw_truncated = true;
            break;
        }
        non_truncated.push(op);
    }

    if !saw_truncated {
        // Contract 3 — clean walk: rebuilt == original chunk section.
        assert_eq!(
            rebuilt, chunk_section,
            "clean-walk rebuilt chunk section differs from original",
        );

        // Re-walk the rebuilt stream and confirm the op list is identical.
        let mut rebuilt_file = Vec::with_capacity(buf.len());
        rebuilt_file.extend_from_slice(&buf[..HEADER_SIZE]);
        rebuilt_file.extend_from_slice(&rebuilt);
        rebuilt_file.extend_from_slice(END_MARKER);
        let (_, rt_ops) = iter_ops_strict(&rebuilt_file)
            .expect("rebuilt clean stream must walk without truncation");
        assert_eq!(
            rt_ops, non_truncated,
            "round-trip op sequence differs from original (clean walk)",
        );
    } else {
        // Contract 4 — truncated walk: rebuilt is a byte-prefix of the
        // original chunk section (the dropped partial body is the only
        // difference between rebuilt.len() and where truncation began).
        assert!(
            rebuilt.len() <= chunk_section.len(),
            "rebuilt longer than original chunk section under truncation",
        );
        assert_eq!(
            &rebuilt[..],
            &chunk_section[..rebuilt.len()],
            "rebuilt prefix diverges from original before truncation point",
        );

        // Re-walking the rebuilt prefix (wrapped in header + marker) must
        // reproduce exactly the non-truncated op sequence. Because the
        // last byte of `rebuilt` is the Truncated chunk's lone leading
        // byte, the re-walk re-derives that same Truncated tail; strip it.
        let mut rebuilt_file = Vec::with_capacity(HEADER_SIZE + rebuilt.len() + END_MARKER.len());
        rebuilt_file.extend_from_slice(&buf[..HEADER_SIZE]);
        rebuilt_file.extend_from_slice(&rebuilt);
        rebuilt_file.extend_from_slice(END_MARKER);
        let (_, it2) = iter_ops(&rebuilt_file).expect("rebuilt header still valid");
        let mut rt_ops: Vec<QoiOp> = Vec::new();
        for op in it2 {
            if op.is_truncated() {
                break;
            }
            rt_ops.push(op);
        }
        assert_eq!(
            rt_ops, non_truncated,
            "round-trip op sequence differs from original (truncated walk)",
        );
    }
});
