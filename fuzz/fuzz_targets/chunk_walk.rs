#![no_main]

//! Structure-aware chunk-walker target. Where the `decode` target
//! throws fully-arbitrary bytes at [`parse_qoi`] (so most inputs die
//! in header / magic / end-marker validation long before a single
//! chunk is dispatched), this target *synthesises a valid 14-byte
//! header + 8-byte end marker* from the first few fuzz bytes and
//! hands the remainder to the decoder as a raw QOI_OP chunk stream.
//! That keeps the fuzzer's coverage feedback focused on the chunk
//! decoder itself — the `QOI_OP_RGB / RGBA / INDEX / DIFF / LUMA /
//! RUN` dispatch and the mid-chunk truncation paths — rather than on
//! the header guard rails the `decode` target already saturates.
//!
//! Contracts exercised (none may panic / abort / OOM / index out of
//! bounds / integer-overflow in a debug build, for any input):
//!
//! * [`parse_qoi_header`] on the synthesised header.
//! * [`parse_qoi_into`] with a *reused* caller buffer — the
//!   amortised-allocation path the `decode` target never touches
//!   (it calls `parse_qoi`, which allocates fresh each time). Two
//!   back-to-back decodes into the same `Vec` check the `clear()` +
//!   regrow contract.
//! * [`iter_ops`] — the lazy chunk walker, driven to exhaustion. The
//!   per-op invariants `encoded_len() == 1 + body_len()` and
//!   `tag()`-round-trips-the-leading-byte are asserted for every
//!   non-`Truncated` op, and a `Truncated` is verified to be the
//!   *final* item (the walker's `done` latch).
//! * [`iter_ops_strict`] — the eager variant; a successful return is
//!   cross-checked to contain no `Truncated` and to agree op-for-op
//!   with the lazy walk's non-truncated prefix.
//! * [`qoi_hash`] over the op-derived pixel bytes, exercising the
//!   running-array hash on attacker-chosen channel values.
//!
//! No external oracle is consulted — the clean-room wall bars the
//! reference `qoi.h`; every expectation here comes from the QOI spec
//! (`docs/image/qoi/`) and this crate's own documented op contracts.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{
    iter_ops, iter_ops_strict, parse_qoi_header, parse_qoi_into, qoi_hash, QoiOp, END_MARKER,
    HEADER_SIZE, MAGIC,
};

// Cap the synthesised dimensions so the decoder's
// `width * height * channels` pixel budget can't make a single
// iteration allocate (and then walk) an enormous buffer. 4096 total
// pixels (e.g. 64x64) is plenty to reach any chunk-dispatch arm while
// staying fast enough for libfuzzer to keep throughput high. The
// chunk stream itself is whatever the fuzzer supplies, so truncation
// / over-supply relative to the pixel count is still fully explored.
const MAX_DIM: u32 = 64;

fuzz_target!(|data: &[u8]| {
    // 6 control bytes pick the header shape; the rest is the raw
    // chunk stream that sits between header and end marker.
    if data.len() < 6 {
        return;
    }

    let raw_w = u16::from_be_bytes([data[0], data[1]]) as u32;
    let raw_h = u16::from_be_bytes([data[2], data[3]]) as u32;
    // Clamp to 1..=MAX_DIM so the header always validates (the
    // decoder rejects zero dimensions) and the pixel budget stays
    // bounded.
    let width = raw_w.clamp(1, MAX_DIM);
    let height = raw_h.clamp(1, MAX_DIM);
    let channels: u8 = if data[4] & 1 == 1 { 4 } else { 3 };
    let colorspace: u8 = if data[5] & 1 == 1 { 1 } else { 0 };
    let chunk_stream = &data[6..];

    // Assemble: 14-byte header (magic + dims + channels + colorspace)
    // + raw chunk stream + canonical 8-byte end marker.
    let mut bytes = Vec::with_capacity(HEADER_SIZE + chunk_stream.len() + END_MARKER.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.push(channels);
    bytes.push(colorspace);
    bytes.extend_from_slice(chunk_stream);
    bytes.extend_from_slice(END_MARKER);

    // Header must parse — we built it from validated fields.
    let hdr = parse_qoi_header(&bytes).expect("synthesised header should parse");
    assert_eq!(hdr.width, width);
    assert_eq!(hdr.height, height);

    // Decode-into a reused buffer twice. The first call grows it; the
    // second must clear + refill without leaking the prior contents.
    // The chunk stream is attacker-controlled, so either call may
    // legitimately return Err (truncated / pixel-count mismatch / bad
    // index) — the contract under test is "returns, never panics".
    let mut buf = Vec::new();
    let _ = parse_qoi_into(&bytes, &mut buf);
    let _ = parse_qoi_into(&bytes, &mut buf);

    // Walk the chunk stream lazily and validate per-op invariants.
    if let Ok((_h, it)) = iter_ops(&bytes) {
        let mut lazy_ops: Vec<QoiOp> = Vec::new();
        let mut saw_truncated = false;
        for op in it {
            // A Truncated sentinel must be the final yielded item —
            // the walker latches `done` after emitting it.
            assert!(
                !saw_truncated,
                "iter_ops yielded an op after Truncated: {op:?}"
            );
            match op {
                QoiOp::Truncated {
                    missing_body_bytes, ..
                } => {
                    saw_truncated = true;
                    assert!(op.is_truncated());
                    // RGB needs ≤3, RGBA ≤4, LUMA 1 trailing byte; the
                    // missing count is always within that envelope.
                    assert!(
                        (1..=4).contains(&missing_body_bytes),
                        "implausible missing_body_bytes={missing_body_bytes}"
                    );
                    assert_eq!(op.encoded_len(), 1);
                }
                other => {
                    // encoded_len() == 1 + body_len() for every real op.
                    assert_eq!(other.encoded_len(), 1 + other.body_len());
                    // tag() reconstructs the leading on-wire byte. For
                    // the four 2-bit chunks this round-trips exactly;
                    // exercise the dispatch by feeding tag() back.
                    let t = other.tag();
                    match other {
                        QoiOp::Rgb { .. } => assert_eq!(t, 0xFE),
                        QoiOp::Rgba { .. } => assert_eq!(t, 0xFF),
                        QoiOp::Index { .. } => assert_eq!(t & 0xC0, 0x00),
                        QoiOp::Diff { .. } => assert_eq!(t & 0xC0, 0x40),
                        QoiOp::Luma { .. } => assert_eq!(t & 0xC0, 0x80),
                        QoiOp::Run { length } => {
                            assert_eq!(t & 0xC0, 0xC0);
                            assert!((1..=62).contains(&length));
                        }
                        QoiOp::Truncated { .. } => unreachable!(),
                    }
                    // Feed any concrete pixel-ish bytes through the
                    // running-array hash to exercise it on fuzzer data.
                    if let QoiOp::Rgba { r, g, b, a } = other {
                        let _ = qoi_hash([r, g, b, a]);
                    }
                    if let QoiOp::Rgb { r, g, b } = other {
                        let _ = qoi_hash([r, g, b, 255]);
                    }
                    lazy_ops.push(other);
                }
            }
        }

        // The eager walker must agree: on success it yields exactly
        // the non-truncated prefix; if the lazy walk hit a Truncated,
        // the strict walk must surface it as an Err.
        match iter_ops_strict(&bytes) {
            Ok((_h2, strict_ops)) => {
                assert!(
                    !saw_truncated,
                    "iter_ops_strict succeeded but iter_ops saw a truncation"
                );
                assert_eq!(strict_ops, lazy_ops);
            }
            Err(_) => {
                assert!(
                    saw_truncated,
                    "iter_ops_strict errored but iter_ops saw no truncation"
                );
            }
        }
    }
});
