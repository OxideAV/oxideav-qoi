#![no_main]

//! Structure-aware fuzz of the framework-side `Decoder` trait path.
//!
//! The other decode targets drive the standalone [`parse_qoi`]
//! function. This one drives the `oxideav_core::Decoder` trait impl —
//! `send_packet` followed by `receive_frame` (heap `VideoFrame`) and
//! `receive_arena_frame` (zero-copy arena `Frame` carrying the
//! QOI-overridden `FrameHeader`). That arena build is the only decode-
//! path allocation logic the standalone targets never reach.
//!
//! Header synthesis matches `chunk_walk`: the first 6 fuzz bytes pick a
//! spec-valid header shape (width / height clamped to 1..=64, channels
//! ∈ {3,4}, colorspace ∈ {0,1}) and the rest become the chunk stream,
//! wrapped with a correct 14-byte header and trailing 8-byte end
//! marker. The layout follows the qoiformat.org specification (mirrored
//! under `docs/image/qoi/`).
//!
//! Contracts under test:
//! * `send_packet` + `receive_frame` returns (Ok or Err), never panics
//!   / aborts / OOMs.
//! * `send_packet` + `receive_arena_frame` likewise.
//! * When both succeed on the same stream they agree: same decoded pixel
//!   bytes, same plane stride, and the arena `FrameHeader` reports the
//!   true `(width, height)` and a packed pixel format (NOT the
//!   `Gray8` / `width = stride` mislabel the trait default would give).

use libfuzzer_sys::fuzz_target;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase};
use oxideav_qoi::decoder::make_decoder;
use oxideav_qoi::{END_MARKER, MAGIC};

const MAX_DIM: u32 = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }
    let raw_w = u16::from_be_bytes([data[0], data[1]]) as u32;
    let raw_h = u16::from_be_bytes([data[2], data[3]]) as u32;
    let width = (raw_w % MAX_DIM) + 1;
    let height = (raw_h % MAX_DIM) + 1;
    let channels: u8 = if data[4] & 1 == 1 { 4 } else { 3 };
    let colorspace: u8 = data[5] & 1;
    let pixel_format = if channels == 4 {
        PixelFormat::Rgba
    } else {
        PixelFormat::Rgb24
    };
    let chunk_stream = &data[6..];

    // Assemble a spec-valid header + chunk stream + end marker.
    let mut buf = Vec::with_capacity(14 + chunk_stream.len() + END_MARKER.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf.push(channels);
    buf.push(colorspace);
    buf.extend_from_slice(chunk_stream);
    buf.extend_from_slice(END_MARKER);

    let mut pkt = Packet::new(0, TimeBase::new(1, 1), buf);
    pkt.pts = Some(7);
    let params = CodecParameters::video(CodecId::new("qoi"));

    // Path A: send_packet + receive_frame (heap VideoFrame). Each
    // receive path consumes the single pending frame, so use a fresh
    // decoder per path from the same packet.
    let mut frame_pixels: Option<(Vec<u8>, usize)> = None;
    if let Ok(mut dec) = make_decoder(&params) {
        if dec.send_packet(&pkt).is_ok() {
            if let Ok(Frame::Video(vf)) = dec.receive_frame() {
                if let Some(p0) = vf.planes.first() {
                    frame_pixels = Some((p0.data.clone(), p0.stride));
                }
            }
        }
    }

    // Path B: send_packet + receive_arena_frame (zero-copy arena).
    let mut arena_pixels: Option<(Vec<u8>, u32, u32, PixelFormat)> = None;
    if let Ok(mut dec) = make_decoder(&params) {
        if dec.send_packet(&pkt).is_ok() {
            if let Ok(frame) = dec.receive_arena_frame() {
                let hdr = frame.header();
                if let Some(plane) = frame.plane(0) {
                    arena_pixels = Some((plane.to_vec(), hdr.width, hdr.height, hdr.pixel_format));
                }
            }
        }
    }

    // Cross-check: when both paths produced a frame they must agree.
    if let (Some((fp, stride)), Some((ap, aw, ah, afmt))) = (&frame_pixels, &arena_pixels) {
        assert_eq!(
            fp, ap,
            "receive_frame and receive_arena_frame pixel bytes differ"
        );
        assert_eq!(*aw, width, "arena FrameHeader width must be the true width");
        assert_eq!(*ah, height, "arena FrameHeader height must be the true height");
        assert_eq!(*afmt, pixel_format, "arena FrameHeader pixel format");
        let bpp = if pixel_format == PixelFormat::Rgba { 4 } else { 3 };
        assert_eq!(*stride, width as usize * bpp, "plane stride");
    }
});
