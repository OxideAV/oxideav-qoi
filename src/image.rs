//! The standalone image types: the shapes every `oxideav-<format>`
//! image crate shares (`IMAGE_CRATE_API`), specialised for QOI.
//!
//! * [`QoiImage`] — the native-layout image [`crate::decode`] returns
//!   and [`crate::encode`] consumes: dimensions, a [`PixelFormat`]
//!   tag, one packed [`Plane`], [`ColorInfo`] and (always empty for
//!   QOI) [`Metadata`].
//! * [`RgbImage`] / [`RgbaImage`] — the tightly packed 8-bit raw
//!   paths ([`crate::decode_rgb8`] / [`crate::decode_rgba8`],
//!   [`QoiImage::to_rgb8`] / [`QoiImage::to_rgba8`]).
//! * [`ImageInfo`] — what [`crate::info`] reads from the 14-byte
//!   header.
//! * [`QoiChannels`] / [`QoiColorspace`] / [`QoiHeader`] — the QOI
//!   header fields themselves (format-specific depth).
//!
//! Defined here (rather than reusing `oxideav_core::VideoFrame`) so the
//! crate can be built with the default `registry` feature off — i.e.
//! without depending on `oxideav-core` at all. When the `registry`
//! feature is on the `crate::registry` module provides the
//! conversions used by the trait-side `Decoder` / `Encoder` impls.

use crate::error::{QoiError, Result};

// ---------------------------------------------------------------------------
// QOI header fields
// ---------------------------------------------------------------------------

/// Channel count carried by the QOI header byte at offset 12.
///
/// Either 3 (RGB, alpha implicit `0xFF`) or 4 (RGBA). Both are decoded
/// losslessly; the encoder writes the same value back. The native
/// [`PixelFormat`] follows 1:1 ([`QoiChannels::pixel_format`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum QoiChannels {
    /// 3 bytes per pixel, R/G/B (alpha implicit `0xFF`).
    Rgb = 3,
    /// 4 bytes per pixel, R/G/B/A.
    Rgba = 4,
}

impl QoiChannels {
    /// The header byte value (3 or 4).
    pub const fn byte(self) -> u8 {
        self as u8
    }

    /// Bytes per pixel of the packed layout (3 or 4).
    pub const fn bytes_per_pixel(self) -> usize {
        self as usize
    }

    /// Parse the header byte (3 or 4); anything else is `None`.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            3 => Some(Self::Rgb),
            4 => Some(Self::Rgba),
            _ => None,
        }
    }

    /// The native pixel layout this channel count decodes to.
    pub const fn pixel_format(self) -> QoiPixelFormat {
        match self {
            Self::Rgb => QoiPixelFormat::Rgb24,
            Self::Rgba => QoiPixelFormat::Rgba,
        }
    }
}

impl From<QoiPixelFormat> for QoiChannels {
    fn from(pf: QoiPixelFormat) -> Self {
        pf.channels()
    }
}

impl From<QoiChannels> for QoiPixelFormat {
    fn from(c: QoiChannels) -> Self {
        c.pixel_format()
    }
}

/// Colorspace tag from the QOI header byte at offset 13.
///
/// Per spec this is purely informational — both values yield the same
/// pixel bytes and the codec performs no colour conversion. It is the
/// one piece of colour signalling QOI carries, and it is what
/// [`QoiImage::color`] is derived from ([`QoiColorspace::color_info`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum QoiColorspace {
    /// 0 — sRGB with linear alpha (the spec's default hint).
    #[default]
    SrgbWithLinearAlpha = 0,
    /// 1 — all channels linear.
    AllLinear = 1,
}

impl QoiColorspace {
    /// The header byte value (0 or 1).
    pub const fn byte(self) -> u8 {
        self as u8
    }

    /// Parse the header byte (0 or 1); anything else is `None`.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::SrgbWithLinearAlpha),
            1 => Some(Self::AllLinear),
            _ => None,
        }
    }

    /// The H.273 description of this colorspace byte:
    ///
    /// | Byte | [`ColorInfo`] |
    /// |------|---------------|
    /// | 0 (sRGB with linear alpha) | full range, primaries 1 (BT.709 / sRGB), transfer 13 (IEC 61966-2-1 sRGB), matrix 0 (identity / RGB) |
    /// | 1 (all channels linear)    | full range, primaries 1, transfer 8 (linear), matrix 0 |
    ///
    /// QOI's spec says nothing about primaries; sRGB / BT.709 is the
    /// convention this crate documents for both bytes (the byte only
    /// distinguishes the transfer).
    pub const fn color_info(self) -> ColorInfo {
        match self {
            Self::SrgbWithLinearAlpha => ColorInfo::srgb(),
            Self::AllLinear => ColorInfo::linear(),
        }
    }

    /// The inverse of [`Self::color_info`]: transfer 8 (linear) maps
    /// to [`QoiColorspace::AllLinear`], every other transfer (sRGB,
    /// unspecified, …) to [`QoiColorspace::SrgbWithLinearAlpha`] —
    /// QOI has no third value, so the byte can only record whether the
    /// samples are linear.
    pub const fn from_color_info(c: &ColorInfo) -> Self {
        if c.transfer == ColorInfo::TRANSFER_LINEAR {
            Self::AllLinear
        } else {
            Self::SrgbWithLinearAlpha
        }
    }
}

/// Cheap header-only view of a QOI file: the four fields of the
/// 14-byte header, as the chunk walkers ([`crate::iter_ops`],
/// [`crate::parse_qoi_into`]) report them.
///
/// The contract-shaped equivalent for callers of the root API is
/// [`ImageInfo`] (via [`crate::info`]); `QoiHeader` is the raw QOI
/// view and stays alongside it.
///
/// Header validation is the same set of checks the full decoder runs
/// before touching the chunk stream: magic = `qoif`, `channels` ∈
/// `{3, 4}`, `colorspace` ∈ `{0, 1}`, width and height ≠ 0. The
/// post-header chunk stream + end marker are NOT inspected — a file
/// whose header parses successfully can still fail [`crate::decode`]
/// later if the body is truncated or the end marker is wrong.
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

impl QoiHeader {
    /// Bytes of the decoded packed plane: `width × height × channels`
    /// (checked, `None` on `u64` overflow).
    pub fn decoded_len(&self) -> Option<u64> {
        u64::from(self.width)
            .checked_mul(u64::from(self.height))?
            .checked_mul(self.channels.bytes_per_pixel() as u64)
    }
}

// ---------------------------------------------------------------------------
// Contract types
// ---------------------------------------------------------------------------

/// Pixel layouts the standalone `oxideav-qoi` API can produce /
/// consume.
///
/// Variant names mirror `oxideav_core::PixelFormat` exactly, so the
/// `crate::registry` conversion layer is a 1:1 match. QOI has
/// exactly two layouts, both packed (one plane), both 8-bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QoiPixelFormat {
    /// 8-bit RGB, 3 bytes per pixel (`channels = 3`).
    Rgb24,
    /// 8-bit RGBA, 4 bytes per pixel (`channels = 4`).
    Rgba,
}

/// The contract name for [`QoiPixelFormat`].
pub type PixelFormat = QoiPixelFormat;

impl QoiPixelFormat {
    /// Bytes per pixel (3 or 4).
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb24 => 3,
            Self::Rgba => 4,
        }
    }

    /// `true` for [`QoiPixelFormat::Rgba`].
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba)
    }

    /// The QOI header `channels` value for this layout.
    pub const fn channels(self) -> QoiChannels {
        match self {
            Self::Rgb24 => QoiChannels::Rgb,
            Self::Rgba => QoiChannels::Rgba,
        }
    }
}

/// One pixel plane: `stride` bytes per row, `data` holding at least
/// `stride × (height − 1) + width × bytes_per_pixel` bytes (rows may
/// carry padding past the visible width). QOI layouts are packed, so a
/// [`QoiImage`] has exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plane {
    /// Bytes per row.
    pub stride: usize,
    /// Row-major bytes.
    pub data: Vec<u8>,
}

impl Plane {
    /// Wrap a plane buffer with its row stride.
    pub fn new(stride: usize, data: Vec<u8>) -> Self {
        Self { stride, data }
    }
}

/// Nominal sample range (H.273 `VideoFullRangeFlag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorRange {
    /// No range was signalled.
    #[default]
    Unspecified,
    /// Limited (video / studio) range: `VideoFullRangeFlag == 0`.
    Limited,
    /// Full (PC) range: `VideoFullRangeFlag == 1`.
    Full,
}

/// Colour signalling of an image: the sample range plus the H.273
/// `ColourPrimaries` / `TransferCharacteristics` /
/// `MatrixCoefficients` code points (`2` = unspecified).
///
/// For QOI, [`crate::decode`] derives it from the header's colorspace
/// byte ([`QoiColorspace::color_info`]): QOI samples are always full
/// range RGB (`matrix` 0), with sRGB (13) or linear (8) transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ColorInfo {
    /// Sample range.
    pub range: ColorRange,
    /// H.273 `ColourPrimaries` code point (`1` = BT.709 / sRGB, `2` =
    /// unspecified).
    pub primaries: u8,
    /// H.273 `TransferCharacteristics` code point (`13` = sRGB, `8` =
    /// linear, `2` = unspecified).
    pub transfer: u8,
    /// H.273 `MatrixCoefficients` code point (`0` = identity / RGB).
    pub matrix: u8,
}

impl ColorInfo {
    /// H.273 "unspecified" code point.
    pub const UNSPECIFIED: u8 = 2;
    /// H.273 `MatrixCoefficients` identity (RGB / GBR) code point.
    pub const MATRIX_IDENTITY: u8 = 0;
    /// H.273 `ColourPrimaries` BT.709 / sRGB code point.
    pub const PRIMARIES_BT709: u8 = 1;
    /// H.273 `TransferCharacteristics` IEC 61966-2-1 sRGB code point.
    pub const TRANSFER_SRGB: u8 = 13;
    /// H.273 `TransferCharacteristics` linear code point.
    pub const TRANSFER_LINEAR: u8 = 8;

    /// Build a description from its four parts.
    pub const fn new(range: ColorRange, primaries: u8, transfer: u8, matrix: u8) -> Self {
        Self {
            range,
            primaries,
            transfer,
            matrix,
        }
    }

    /// Every field unspecified.
    pub const fn unspecified() -> Self {
        Self::new(
            ColorRange::Unspecified,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
        )
    }

    /// sRGB (IEC 61966-2-1): BT.709 primaries, sRGB transfer, identity
    /// matrix, full range — QOI colorspace byte `0`.
    pub const fn srgb() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_SRGB,
            Self::MATRIX_IDENTITY,
        )
    }

    /// Linear RGB: BT.709 primaries, linear transfer (8), identity
    /// matrix, full range — QOI colorspace byte `1`.
    pub const fn linear() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_LINEAR,
            Self::MATRIX_IDENTITY,
        )
    }

    /// QOI's default when nothing else is known: [`ColorInfo::srgb`]
    /// (the spec's default colorspace byte is `0`).
    pub const fn qoi_default() -> Self {
        Self::srgb()
    }

    /// Set the range.
    pub fn with_range(mut self, range: ColorRange) -> Self {
        self.range = range;
        self
    }

    /// Set the primaries code point.
    pub fn with_primaries(mut self, primaries: u8) -> Self {
        self.primaries = primaries;
        self
    }

    /// Set the transfer code point.
    pub fn with_transfer(mut self, transfer: u8) -> Self {
        self.transfer = transfer;
        self
    }

    /// Set the matrix code point.
    pub fn with_matrix(mut self, matrix: u8) -> Self {
        self.matrix = matrix;
        self
    }

    /// `true` when both primaries and transfer are specified (`!= 2`).
    pub fn is_specified(&self) -> bool {
        self.primaries != Self::UNSPECIFIED && self.transfer != Self::UNSPECIFIED
    }
}

impl Default for ColorInfo {
    /// [`ColorInfo::qoi_default`].
    fn default() -> Self {
        Self::qoi_default()
    }
}

/// The metadata blobs every image crate surfaces: an ICC profile, an
/// Exif payload, an XMP packet and a file gamma.
///
/// QOI has no metadata mechanism of any kind, so every field is
/// `None` on a decoded image and the encoder ignores (cannot carry)
/// whatever a caller sets. The type exists so [`QoiImage`] has the
/// same shape as every other image crate's image.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    /// ICC profile bytes. Always `None` from the decoder.
    pub icc: Option<Vec<u8>>,
    /// Exif payload. Always `None` from the decoder.
    pub exif: Option<Vec<u8>>,
    /// XMP packet. Always `None` from the decoder.
    pub xmp: Option<Vec<u8>>,
    /// File gamma. Always `None` from the decoder.
    pub gamma: Option<f32>,
}

impl Metadata {
    /// Empty metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear) the ICC profile.
    pub fn with_icc(mut self, icc: impl Into<Option<Vec<u8>>>) -> Self {
        self.icc = icc.into();
        self
    }

    /// Set (or clear) the Exif payload.
    pub fn with_exif(mut self, exif: impl Into<Option<Vec<u8>>>) -> Self {
        self.exif = exif.into();
        self
    }

    /// Set (or clear) the XMP packet.
    pub fn with_xmp(mut self, xmp: impl Into<Option<Vec<u8>>>) -> Self {
        self.xmp = xmp.into();
        self
    }

    /// Set (or clear) the file gamma.
    pub fn with_gamma(mut self, gamma: impl Into<Option<f32>>) -> Self {
        self.gamma = gamma.into();
        self
    }

    /// `true` when no field is set.
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none() && self.xmp.is_none() && self.gamma.is_none()
    }
}

/// Decoded QOI image in its native layout, as returned by
/// [`crate::decode`] and consumed by [`crate::encode`].
///
/// `planes` holds exactly one packed plane (`Rgb24` or `Rgba`, stride
/// `width × bytes_per_pixel` from the decoder); `color` is derived from
/// the header's colorspace byte; `metadata` is always empty (QOI has
/// none). QOI has no palette, so there is no `palette` field.
///
/// Construct with [`QoiImage::new`] / [`QoiImage::from_rgb8`] /
/// [`QoiImage::from_rgba8`], which validate the plane geometry so an
/// inconsistent image cannot exist and [`QoiImage::to_rgb8`] /
/// [`QoiImage::to_rgba8`] are infallible.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct QoiImage {
    /// Image width in pixels (≥ 1).
    pub width: u32,
    /// Image height in pixels (≥ 1).
    pub height: u32,
    /// Native pixel layout.
    pub format: PixelFormat,
    /// Pixel planes — exactly one for QOI.
    pub planes: Vec<Plane>,
    /// Colour signalling (range + H.273 code points), from the
    /// colorspace byte.
    pub color: ColorInfo,
    /// ICC / Exif / XMP / gamma — always empty for QOI.
    pub metadata: Metadata,
}

impl QoiImage {
    /// Assemble an image from its geometry, layout and planes (exactly
    /// one for QOI). Colour is [`ColorInfo::qoi_default`] (sRGB) and
    /// metadata empty; the `with_*` builders fill those in.
    ///
    /// Validates the geometry and returns [`QoiError::InvalidData`]
    /// when `width` or `height` is `0` (QOI cannot carry an empty
    /// image), when there is not exactly one plane, when the plane's
    /// `stride` is below `width × bytes_per_pixel`, or when its `data`
    /// is shorter than `stride × (height − 1) + width × bytes_per_pixel`.
    pub fn new(width: u32, height: u32, format: PixelFormat, planes: Vec<Plane>) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(QoiError::invalid("QOI: zero dimension"));
        }
        if planes.len() != 1 {
            return Err(QoiError::invalid(format!(
                "QOI: expected exactly one packed plane, got {}",
                planes.len()
            )));
        }
        let row_bytes = (width as usize)
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| QoiError::unsupported("QOI: row size overflows usize"))?;
        let plane = &planes[0];
        if plane.stride < row_bytes {
            return Err(QoiError::invalid(format!(
                "QOI: stride {} below row size {row_bytes}",
                plane.stride
            )));
        }
        let needed = plane
            .stride
            .checked_mul(height as usize - 1)
            .and_then(|n| n.checked_add(row_bytes))
            .ok_or_else(|| QoiError::unsupported("QOI: plane size overflows usize"))?;
        if plane.data.len() < needed {
            return Err(QoiError::invalid(format!(
                "QOI: plane holds {} bytes, geometry needs {needed}",
                plane.data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            format,
            planes,
            color: ColorInfo::qoi_default(),
            metadata: Metadata::default(),
        })
    }

    /// One packed plane with an explicit row stride (`stride ≥ width ×
    /// bytes_per_pixel`). Same validation as [`Self::new`].
    pub fn packed(
        width: u32,
        height: u32,
        format: PixelFormat,
        stride: usize,
        data: Vec<u8>,
    ) -> Result<Self> {
        Self::new(width, height, format, vec![Plane::new(stride, data)])
    }

    /// Tightly packed `Rgb24` from `3 × width × height` bytes (more is
    /// tolerated; fewer is [`QoiError::InvalidData`]).
    pub fn from_rgb8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let stride = (width as usize)
            .checked_mul(3)
            .ok_or_else(|| QoiError::unsupported("QOI: row size overflows usize"))?;
        Self::packed(width, height, PixelFormat::Rgb24, stride, data)
    }

    /// Tightly packed `Rgba` from `4 × width × height` bytes (more is
    /// tolerated; fewer is [`QoiError::InvalidData`]).
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let stride = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| QoiError::unsupported("QOI: row size overflows usize"))?;
        Self::packed(width, height, PixelFormat::Rgba, stride, data)
    }

    /// Set the colour signalling. The encoder maps it back onto the
    /// colorspace byte ([`QoiColorspace::from_color_info`]) unless
    /// [`crate::EncodeOptions::colorspace`] overrides it.
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// Set the metadata. QOI cannot carry any of it; the encoder
    /// ignores it.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Native pixel layout.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// The QOI header `channels` value of this image (3 or 4).
    pub fn channels(&self) -> QoiChannels {
        self.format.channels()
    }

    /// The QOI header `colorspace` byte this image's [`Self::color`]
    /// maps to ([`QoiColorspace::from_color_info`]).
    pub fn colorspace(&self) -> QoiColorspace {
        QoiColorspace::from_color_info(&self.color)
    }

    /// Number of bytes per pixel for [`Self::format`].
    pub fn bytes_per_pixel(&self) -> usize {
        self.format.bytes_per_pixel()
    }

    /// Row stride in bytes of the pixel plane.
    pub fn stride(&self) -> usize {
        self.planes.first().map(|p| p.stride).unwrap_or(0)
    }

    /// `true` when the layout carries alpha (`Rgba`).
    pub fn has_alpha(&self) -> bool {
        self.format.has_alpha()
    }

    /// `true` when the plane is tightly packed (`stride == width ×
    /// bytes_per_pixel` and no trailing bytes) — always the case for a
    /// decoder-produced image.
    pub fn is_tightly_packed(&self) -> bool {
        let row = self.width as usize * self.bytes_per_pixel();
        self.planes
            .first()
            .is_some_and(|p| p.stride == row && p.data.len() == row * self.height as usize)
    }

    /// The pixel bytes — `Some` for every QOI image (both layouts are
    /// packed, one plane). Includes row padding when the plane's
    /// stride exceeds `width × bytes_per_pixel`.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.planes.first().map(|p| p.data.as_slice())
    }

    /// Consume the image and return its plane bytes.
    pub fn into_raw(self) -> Vec<u8> {
        let mut planes = self.planes.into_iter();
        let mut out = planes.next().map(|p| p.data).unwrap_or_default();
        for p in planes {
            out.extend_from_slice(&p.data);
        }
        out
    }

    /// Pixel bytes of the single plane (empty if none).
    pub(crate) fn data(&self) -> &[u8] {
        self.as_bytes().unwrap_or(&[])
    }

    /// The pixels as one tightly packed `width × height ×
    /// bytes_per_pixel` buffer in the native layout: a borrow when the
    /// plane already is tightly packed, a repacked copy when it carries
    /// row padding.
    pub(crate) fn packed_pixels(&self) -> std::borrow::Cow<'_, [u8]> {
        let w = self.width as usize;
        let h = self.height as usize;
        let row = w * self.bytes_per_pixel();
        let stride = self.stride();
        let src = self.data();
        if stride == row && src.len() == row * h {
            return std::borrow::Cow::Borrowed(src);
        }
        let mut out = vec![0u8; row * h];
        for (y, dst) in out.chunks_exact_mut(row).enumerate() {
            if let Some(s) = src.get(y * stride..y * stride + row) {
                dst.copy_from_slice(s);
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// Tightly packed 8-bit RGBA, `4 × width` bytes per row, alpha
    /// `255` where the source is `Rgb24`. Exact (QOI is 8-bit RGB(A)
    /// already; no colour management is applied).
    pub fn to_rgba8(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * 4];
        let bpp = self.bytes_per_pixel();
        let stride = self.stride();
        let src = self.data();
        let row_bytes = w * bpp;
        for y in 0..h {
            let Some(row) = src.get(y * stride..y * stride + row_bytes) else {
                break;
            };
            let dst = &mut out[y * w * 4..(y + 1) * w * 4];
            match self.format {
                PixelFormat::Rgba => dst.copy_from_slice(row),
                PixelFormat::Rgb24 => {
                    for (s, px) in row.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
                        px[0] = s[0];
                        px[1] = s[1];
                        px[2] = s[2];
                        px[3] = 255;
                    }
                }
            }
        }
        out
    }

    /// Tightly packed 8-bit RGB, `3 × width` bytes per row. Alpha is
    /// dropped (no compositing: a transparent pixel keeps its colour
    /// samples).
    pub fn to_rgb8(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * 3];
        let bpp = self.bytes_per_pixel();
        let stride = self.stride();
        let src = self.data();
        let row_bytes = w * bpp;
        for y in 0..h {
            let Some(row) = src.get(y * stride..y * stride + row_bytes) else {
                break;
            };
            let dst = &mut out[y * w * 3..(y + 1) * w * 3];
            match self.format {
                PixelFormat::Rgb24 => dst.copy_from_slice(row),
                PixelFormat::Rgba => {
                    for (s, px) in row.chunks_exact(4).zip(dst.chunks_exact_mut(3)) {
                        px.copy_from_slice(&s[..3]);
                    }
                }
            }
        }
        out
    }
}

/// Tightly packed 8-bit RGB image: `width × height × 3` bytes,
/// row-major, no padding. What [`crate::decode_rgb8`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 3` bytes, R, G, B per pixel.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Wrap a packed RGB buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }
}

/// Tightly packed 8-bit RGBA image: `width × height × 4` bytes,
/// row-major, no padding. What [`crate::decode_rgba8`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbaImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes, R, G, B, A per pixel.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Wrap a packed RGBA buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }
}

/// What [`crate::info`] reads from the 14-byte header without
/// decoding pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Native layout [`crate::decode`] would return.
    pub format: PixelFormat,
    /// Number of images in the file — always `1` for QOI.
    pub frames: u32,
    /// `true` when `format` is `Rgba`.
    pub has_alpha: bool,
    /// Colour signalling derived from the colorspace byte.
    pub color: ColorInfo,
    /// Always `false` (QOI carries no ICC profile).
    pub has_icc: bool,
    /// Always `false` (QOI carries no Exif).
    pub has_exif: bool,
    /// Always `false` (QOI carries no XMP).
    pub has_xmp: bool,
    /// QOI extra: the header `channels` field.
    pub channels: QoiChannels,
    /// QOI extra: the header `colorspace` field.
    pub colorspace: QoiColorspace,
}

impl ImageInfo {
    /// Build the info record from a parsed header.
    pub fn from_header(hdr: &QoiHeader) -> Self {
        Self {
            width: hdr.width,
            height: hdr.height,
            format: hdr.channels.pixel_format(),
            frames: 1,
            has_alpha: hdr.channels == QoiChannels::Rgba,
            color: hdr.colorspace.color_info(),
            has_icc: false,
            has_exif: false,
            has_xmp: false,
            channels: hdr.channels,
            colorspace: hdr.colorspace,
        }
    }

    /// The raw QOI header this record was read from.
    pub fn header(&self) -> QoiHeader {
        QoiHeader {
            width: self.width,
            height: self.height,
            channels: self.channels,
            colorspace: self.colorspace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorspace_round_trips_through_color_info() {
        for cs in [QoiColorspace::SrgbWithLinearAlpha, QoiColorspace::AllLinear] {
            assert_eq!(QoiColorspace::from_color_info(&cs.color_info()), cs);
        }
        assert_eq!(
            QoiColorspace::from_color_info(&ColorInfo::unspecified()),
            QoiColorspace::SrgbWithLinearAlpha
        );
        let srgb = ColorInfo::srgb();
        assert_eq!(
            (srgb.range, srgb.primaries, srgb.transfer, srgb.matrix),
            (ColorRange::Full, 1, 13, 0)
        );
        let lin = ColorInfo::linear();
        assert_eq!(
            (lin.range, lin.primaries, lin.transfer, lin.matrix),
            (ColorRange::Full, 1, 8, 0)
        );
    }

    #[test]
    fn new_validates_geometry() {
        assert!(matches!(
            QoiImage::new(0, 1, PixelFormat::Rgb24, vec![Plane::new(0, vec![])]),
            Err(QoiError::InvalidData(_))
        ));
        assert!(matches!(
            QoiImage::new(1, 1, PixelFormat::Rgb24, vec![]),
            Err(QoiError::InvalidData(_))
        ));
        assert!(matches!(
            QoiImage::new(2, 1, PixelFormat::Rgb24, vec![Plane::new(3, vec![0; 6])]),
            Err(QoiError::InvalidData(_))
        ));
        assert!(matches!(
            QoiImage::from_rgba8(2, 2, vec![0; 15]),
            Err(QoiError::InvalidData(_))
        ));
        let ok = QoiImage::from_rgba8(2, 2, vec![0; 16]).unwrap();
        assert!(ok.is_tightly_packed());
        assert_eq!(ok.channels(), QoiChannels::Rgba);
        assert_eq!(ok.colorspace(), QoiColorspace::SrgbWithLinearAlpha);
        // Padded last row is allowed: stride × (h − 1) + row.
        let padded = QoiImage::packed(2, 2, PixelFormat::Rgb24, 8, vec![0; 14]).unwrap();
        assert!(!padded.is_tightly_packed());
        assert_eq!(padded.packed_pixels().len(), 12);
    }

    #[test]
    fn to_rgb8_and_to_rgba8_are_exact_for_both_layouts() {
        let rgba = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let img = QoiImage::from_rgba8(2, 1, rgba.clone()).unwrap();
        assert_eq!(img.to_rgba8(), rgba);
        assert_eq!(img.to_rgb8(), vec![1, 2, 3, 5, 6, 7]);
        let rgb = vec![1, 2, 3, 5, 6, 7];
        let img = QoiImage::from_rgb8(2, 1, rgb.clone()).unwrap();
        assert_eq!(img.to_rgb8(), rgb);
        assert_eq!(img.to_rgba8(), vec![1, 2, 3, 255, 5, 6, 7, 255]);
        // Row padding is skipped.
        let img =
            QoiImage::packed(1, 2, PixelFormat::Rgb24, 4, vec![1, 2, 3, 9, 4, 5, 6, 9]).unwrap();
        assert_eq!(img.to_rgb8(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(img.to_rgba8(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
