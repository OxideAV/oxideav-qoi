#![no_main]

//! Differential fuzz of the caller-owned buffer-reuse `_into` API
//! against the allocating wrappers.
//!
//! The crate exposes two parallel families of entry points: the
//! allocating `parse_qoi` / `encode_qoi_full` (fresh `Vec` per call)
//! and the buffer-reuse `parse_qoi_into` / `encode_qoi_full_into`
//! (caller-owned `Vec`, cleared + resized + truncated in place, the
//! allocation retained across calls). The two families must be
//! observationally identical: same accept / reject decision, same
//! decoded pixels + header, same encoded bytes. The `_into` path's
//! extra machinery — `clear()`, `resize` to the worst-case (encode) or
//! exact (decode) length, `truncate` to the produced size, all over a
//! buffer that may still hold a *previous, differently-sized* image —
//! is exactly where a stale tail byte or a missed clear would diverge.
//!
//! This target is structure-aware in the same way as `chunk_walk`: it
//! synthesizes a spec-valid 14-byte header + 8-byte end marker around
//! the fuzzer's chunk stream so the decoder reaches the per-op paths
//! on nearly every run. Then it drives the differential:
//!
//! 1. Decode the synthesized stream through BOTH `parse_qoi` and
//!    `parse_qoi_into` (into a persistent, reused, pre-dirtied buffer).
//!    Assert they agree on Ok/Err, and — when both succeed — on the
//!    decoded pixels and the `(width, height, channels, colorspace)`
//!    header.
//!
//! 2. When the decode succeeds, re-encode the recovered pixels through
//!    BOTH `encode_qoi_full` and `encode_qoi_full_into` (into a second
//!    persistent, reused buffer) and assert byte-for-byte identity.
//!
//! The decode / encode buffers persist across fuzz iterations via a
//! `thread_local`, so the reuse path sees a continuous stream of
//! differently-sized images — the shrinking-reuse stale-tail case the
//! in-crate `into_equivalence` test pins deterministically, driven here
//! by attacker-chosen sizes and op mixes.
//!
//! The header layout, chunk encodings, and end marker follow the
//! qoiformat.org specification mirrored under `docs/image/qoi/`.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{
    encode_qoi_full, encode_qoi_full_into, parse_qoi, parse_qoi_into, END_MARKER, MAGIC,
};
use std::cell::RefCell;

// Match `chunk_walk`'s cap: the decoder rejects any header claiming
// more pixels than `chunks.len() * 62` can decode, so dimensions stay
// bounded and each iteration's buffers stay small.
const MAX_DIM: u32 = 64;

thread_local! {
    // Reused across iterations so the `_into` reuse path genuinely sees
    // a previous, differently-sized image each call — the stale-tail
    // surface the allocating path can never exhibit.
    static DEC_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static ENC_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        // Still drive both decode entry points on the very-short path so
        // the differential covers sub-header inputs too.
        let mut scratch = Vec::new();
        let a = parse_qoi(data);
        let b = parse_qoi_into(data, &mut scratch);
        assert_eq!(
            a.is_ok(),
            b.is_ok(),
            "parse_qoi / parse_qoi_into disagree on Ok/Err for a short input"
        );
        return;
    }

    // Synthesize a spec-valid header shape (same scheme as chunk_walk).
    let raw_w = u16::from_be_bytes([data[0], data[1]]) as u32;
    let raw_h = u16::from_be_bytes([data[2], data[3]]) as u32;
    let width = (raw_w % MAX_DIM) + 1;
    let height = (raw_h % MAX_DIM) + 1;
    let channels: u8 = if data[4] & 1 == 1 { 4 } else { 3 };
    let colorspace: u8 = data[5] & 1;
    let chunk_stream = &data[6..];

    let mut stream = Vec::with_capacity(14 + chunk_stream.len() + END_MARKER.len());
    stream.extend_from_slice(MAGIC);
    stream.extend_from_slice(&width.to_be_bytes());
    stream.extend_from_slice(&height.to_be_bytes());
    stream.push(channels);
    stream.push(colorspace);
    stream.extend_from_slice(chunk_stream);
    stream.extend_from_slice(END_MARKER);

    // --- Differential 1: parse_qoi vs parse_qoi_into (reused buffer) ---
    let alloc = parse_qoi(&stream);
    let into_result = DEC_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        // Pre-dirty the retained buffer so a missed clear / partial
        // overwrite would surface as a mismatch. The `_into` contract
        // clears on entry, so this must not change a successful decode.
        for b in buf.iter_mut() {
            *b = 0xAA;
        }
        let hdr = parse_qoi_into(&stream, &mut buf);
        // Clone the decoded bytes out so we can drop the borrow before
        // the encode differential re-borrows a different thread-local.
        hdr.map(|h| (h, buf.clone()))
    });

    match (&alloc, &into_result) {
        (Ok(img), Ok((hdr, pixels))) => {
            assert_eq!(
                img.pixels, *pixels,
                "parse_qoi / parse_qoi_into decoded different pixels"
            );
            assert_eq!(img.width, hdr.width, "width disagreement");
            assert_eq!(img.height, hdr.height, "height disagreement");
            assert_eq!(img.channels, hdr.channels, "channels disagreement");
            assert_eq!(
                img.colorspace, hdr.colorspace,
                "colorspace disagreement"
            );
        }
        (Err(_), Err(_)) => { /* both reject — agreement */ }
        _ => panic!(
            "parse_qoi / parse_qoi_into disagree on accept/reject \
             (alloc ok={}, into ok={})",
            alloc.is_ok(),
            into_result.is_ok()
        ),
    }

    // --- Differential 2: encode_qoi_full vs encode_qoi_full_into ---
    // Only when the stream decoded cleanly (we then have a known-good
    // pixel buffer + header to re-encode through both paths).
    if let Ok(img) = &alloc {
        let ch = img.channels as u8;
        let cs = img.colorspace as u8;
        let fresh = encode_qoi_full(img.width, img.height, ch, cs, &img.pixels);
        let reused = ENC_BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            for b in buf.iter_mut() {
                *b = 0x55;
            }
            encode_qoi_full_into(&mut buf, img.width, img.height, ch, cs, &img.pixels);
            buf.clone()
        });
        assert_eq!(
            fresh, reused,
            "encode_qoi_full / encode_qoi_full_into produced different bytes \
             ({}x{} ch={ch} cs={cs})",
            img.width, img.height
        );
    }
});
