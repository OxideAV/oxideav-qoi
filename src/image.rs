//! Standalone image container returned by `oxideav-qoi`'s framework-free
//! decode API.
//!
//! Defined here (rather than reusing `oxideav_core::VideoFrame`) so the
//! crate can be built with the default `registry` feature off — i.e.
//! without depending on `oxideav-core` at all. When the `registry`
//! feature is on the [`crate::registry`] module provides the
//! conversions used by the trait-side `Decoder` / `Encoder` impls.

/// Channel count carried by a [`QoiImage`].
///
/// The QOI header byte at offset 12 is either 3 (RGB, alpha implicit)
/// or 4 (RGBA). Both are decoded losslessly; the encoder writes the
/// same value back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QoiChannels {
    /// 3 bytes per pixel, R/G/B (alpha implicit `0xFF`).
    Rgb = 3,
    /// 4 bytes per pixel, R/G/B/A.
    Rgba = 4,
}

/// Colorspace tag from the QOI header byte at offset 13.
///
/// Per spec this is purely informational — both values yield the same
/// pixel bytes and the codec doesn't perform any colour conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QoiColorspace {
    /// 0 — sRGB with linear alpha (the default qoi-spec hint).
    SrgbWithLinearAlpha = 0,
    /// 1 — all channels linear.
    AllLinear = 1,
}

/// Cheap header-only view of a QOI file, returned by
/// [`crate::parse_qoi_header`].
///
/// Lets callers probe an in-memory `.qoi` byte slice for its on-disk
/// metadata — width, height, channels, colorspace — without spending
/// the time and memory needed to walk the chunk stream and materialise
/// every pixel. Useful for thumbnail-grid probing, output-size
/// estimation before allocating a decode buffer, and rejecting files
/// whose dimensions exceed a per-application limit before the full
/// decoder gets involved.
///
/// The four fields are the same as the corresponding fields on
/// [`QoiImage`], with the same `(u32, u32, QoiChannels, QoiColorspace)`
/// types — so a future `parse_qoi(bytes)?` call on the same bytes will
/// agree byte-for-byte with what the header view reports here.
///
/// Header validation is the same set of checks the full decoder runs
/// before touching the chunk stream: magic = `qoif`, `channels` ∈
/// `{3, 4}`, `colorspace` ∈ `{0, 1}`, width and height ≠ 0. The
/// post-header chunk stream + end marker are NOT inspected — a file
/// whose header parses successfully here can still fail
/// [`crate::parse_qoi`] later if the body is truncated or the end
/// marker is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QoiHeader {
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// 3 (RGB) or 4 (RGBA).
    pub channels: QoiChannels,
    /// Colorspace hint (informational; does not affect pixel bytes).
    pub colorspace: QoiColorspace,
}

/// One decoded QOI image, framework-free shape.
///
/// `pixels` is a flat row-major byte buffer with no row padding:
///
/// * `channels == Rgb`  → `pixels.len() == width * height * 3`, byte
///   order R/G/B per pixel.
/// * `channels == Rgba` → `pixels.len() == width * height * 4`, byte
///   order R/G/B/A per pixel.
///
/// `pts` is `None` from the standalone [`crate::parse_qoi`] entry
/// point. The registry-backed `Decoder` impl passes `pts` through
/// from the surrounding `Packet`.
#[derive(Debug, Clone)]
pub struct QoiImage {
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// 3 (RGB) or 4 (RGBA).
    pub channels: QoiChannels,
    /// Colorspace hint (informational; does not affect pixel bytes).
    pub colorspace: QoiColorspace,
    /// Flat tightly-packed pixel buffer.
    pub pixels: Vec<u8>,
    /// Optional presentation timestamp. Always `None` from the
    /// standalone decode path.
    pub pts: Option<i64>,
}
