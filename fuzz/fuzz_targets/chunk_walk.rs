#![no_main]

//! Structure-aware QOI chunk-stream walk.
//!
//! The plain `decode` target feeds wholly attacker-controlled bytes to
//! [`parse_qoi`], so most iterations die at the 14-byte header gate
//! (`qoif` magic, channels ∈ {3,4}, colorspace ∈ {0,1}, non-zero
//! dimensions) before a single chunk is dispatched. This target instead
//! synthesizes a *spec-valid* header and a correct trailing 8-byte end
//! marker, then hands the fuzzer's bytes to the decoder as the chunk
//! stream in between. That keeps the decoder past the header on (nearly)
//! every run, concentrating libfuzzer's coverage feedback on the six
//! per-op decode paths — `QOI_OP_RGB` / `RGBA` / `INDEX` / `DIFF` /
//! `LUMA` / `RUN` — and the truncation / overrun checks that guard
//! them.
//!
//! The header layout, the six chunk encodings, and the end marker all
//! follow the qoiformat.org specification (mirrored under
//! `docs/image/qoi/`).
//!
//! The only contract under test is that the decode call *returns*: any
//! chunk stream — well-formed, truncated mid-pixel, claiming more
//! pixels than the stream can decode, or carrying a 2-bit RUN that
//! collides with the 8-bit `0xfe` / `0xff` tags — must yield `Ok` or
//! `Err`, never a panic, abort, integer overflow (in a debug build),
//! out-of-bounds index, or attacker-sized allocation. The result is
//! intentionally discarded.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{parse_qoi, END_MARKER, MAGIC};

// Cap the synthesized image dimensions. The decoder rejects any header
// claiming more pixels than `chunks.len() * 62` can possibly decode, so
// it never over-allocates — but a tiny cap also keeps each iteration's
// pixel buffer small and the fuzzer's throughput high. 4096 pixels at 4
// channels is a 16 KiB worst-case decode buffer.
const MAX_DIM: u32 = 64;

fuzz_target!(|data: &[u8]| {
    // First 6 bytes pick the header shape; the rest is the chunk
    // stream. Below that we can't form a meaningful header + stream.
    if data.len() < 6 {
        // Still exercise the very-short path through the real entry
        // point so those inputs aren't simply ignored.
        let _ = parse_qoi(data);
        return;
    }

    // width / height: two bytes each, clamped to 1..=MAX_DIM so the
    // claimed pixel count stays small. Mapping 0 -> 1 avoids the
    // zero-dimension early-reject so the chunk stream actually runs.
    let raw_w = u16::from_be_bytes([data[0], data[1]]) as u32;
    let raw_h = u16::from_be_bytes([data[2], data[3]]) as u32;
    let width = (raw_w % MAX_DIM) + 1;
    let height = (raw_h % MAX_DIM) + 1;
    // channels low bit -> 3 or 4; colorspace low bit -> 0 or 1. Both
    // are the only spec-valid values, so the header always passes the
    // field checks and the decoder proceeds into the chunk walk.
    let channels: u8 = if data[4] & 1 == 1 { 4 } else { 3 };
    let colorspace: u8 = data[5] & 1;
    let chunk_stream = &data[6..];

    // Assemble: 14-byte header + fuzzer chunk stream + 8-byte end
    // marker. The decoder strips the trailing marker before walking,
    // so a valid marker here lets a well-formed stream decode all the
    // way through instead of failing the marker check.
    let mut buf = Vec::with_capacity(14 + chunk_stream.len() + END_MARKER.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf.push(channels);
    buf.push(colorspace);
    buf.extend_from_slice(chunk_stream);
    buf.extend_from_slice(END_MARKER);

    // The contract: returns, never panics.
    let _ = parse_qoi(&buf);
});
