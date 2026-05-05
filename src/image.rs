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
