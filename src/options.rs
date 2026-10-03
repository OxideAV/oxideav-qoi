//! Decode-side limits ([`DecodeOptions`]) and encode-side knobs
//! ([`EncodeOptions`]) of the standalone API.

use crate::error::{QoiError, Result};
use crate::image::QoiColorspace;

/// Limits and strictness for [`crate::decode_with`].
///
/// Every limit is checked against the 14-byte header **before** any
/// pixel buffer is allocated, so a hostile header fails with
/// [`QoiError::LimitExceeded`] instead of committing memory. The
/// defaults are: no dimension / pixel-count limit, decoded plane
/// capped at [`DecodeOptions::DEFAULT_MAX_BYTES`] (1 GiB), `strict =
/// false`.
///
/// `strict` is accepted for contract uniformity but has no effect on
/// QOI: the specification has no advisory ("should") rules — the
/// magic, header field ranges, exact pixel count, chunk bounds and the
/// 8-byte end marker are all mandatory and are enforced in both modes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// Reject images wider than this (pixels).
    pub max_width: Option<u32>,
    /// Reject images taller than this (pixels).
    pub max_height: Option<u32>,
    /// Reject images with more than this many pixels (`width ×
    /// height`).
    pub max_pixels: Option<u64>,
    /// Reject images whose decoded plane would exceed this many bytes
    /// (`width × height × channels`).
    pub max_bytes: Option<u64>,
    /// No effect for QOI (see the type docs); kept for the contract.
    pub strict: bool,
}

impl DecodeOptions {
    /// Default [`Self::max_bytes`]: 1 GiB of decoded plane.
    pub const DEFAULT_MAX_BYTES: u64 = 1 << 30;

    /// The defaults (see the type docs).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or lift with `None`) the width limit.
    pub fn with_max_width(mut self, max_width: impl Into<Option<u32>>) -> Self {
        self.max_width = max_width.into();
        self
    }

    /// Set (or lift with `None`) the height limit.
    pub fn with_max_height(mut self, max_height: impl Into<Option<u32>>) -> Self {
        self.max_height = max_height.into();
        self
    }

    /// Set (or lift with `None`) the pixel-count limit.
    pub fn with_max_pixels(mut self, max_pixels: impl Into<Option<u64>>) -> Self {
        self.max_pixels = max_pixels.into();
        self
    }

    /// Set (or lift with `None`) the decoded-bytes limit.
    pub fn with_max_bytes(mut self, max_bytes: impl Into<Option<u64>>) -> Self {
        self.max_bytes = max_bytes.into();
        self
    }

    /// Set strict mode (no effect for QOI; see the type docs).
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Lift every limit (`max_*` all `None`).
    pub fn unlimited(mut self) -> Self {
        self.max_width = None;
        self.max_height = None;
        self.max_pixels = None;
        self.max_bytes = None;
        self
    }

    /// Check a header's geometry against the limits. `bytes` is the
    /// decoded plane size the native layout implies.
    pub(crate) fn check(&self, width: u32, height: u32, bytes: u64) -> Result<()> {
        if let Some(m) = self.max_width {
            if width > m {
                return Err(QoiError::limit(format!(
                    "QOI: width {width} exceeds max_width {m}"
                )));
            }
        }
        if let Some(m) = self.max_height {
            if height > m {
                return Err(QoiError::limit(format!(
                    "QOI: height {height} exceeds max_height {m}"
                )));
            }
        }
        let pixels = u64::from(width) * u64::from(height);
        if let Some(m) = self.max_pixels {
            if pixels > m {
                return Err(QoiError::limit(format!(
                    "QOI: {pixels} pixels exceed max_pixels {m}"
                )));
            }
        }
        if let Some(m) = self.max_bytes {
            if bytes > m {
                return Err(QoiError::limit(format!(
                    "QOI: decoded plane of {bytes} bytes exceeds max_bytes {m}"
                )));
            }
        }
        Ok(())
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_width: None,
            max_height: None,
            max_pixels: None,
            max_bytes: Some(Self::DEFAULT_MAX_BYTES),
            strict: false,
        }
    }
}

/// Encoder knobs for [`crate::encode`] / [`crate::encode_rgb8`] /
/// [`crate::encode_rgba8`].
///
/// QOI has exactly one: the informational `colorspace` header byte.
/// `None` (the default) derives it from the image's colour signalling
/// ([`QoiColorspace::from_color_info`]: transfer 8 → `AllLinear`,
/// anything else → `SrgbWithLinearAlpha`), which is what makes
/// `decode(encode(img)) == img` hold for both bytes; the raw
/// `encode_rgb8` / `encode_rgba8` paths carry no colour and write
/// `SrgbWithLinearAlpha`. `Some(cs)` forces the byte regardless of the
/// image. The byte never changes the pixel bytes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct EncodeOptions {
    /// Override for the header colorspace byte (`None` = from the
    /// image's `color`, sRGB for the raw paths).
    pub colorspace: Option<QoiColorspace>,
}

impl EncodeOptions {
    /// The defaults (colorspace from the image).
    pub fn new() -> Self {
        Self::default()
    }

    /// Force (or with `None`, derive) the header colorspace byte.
    pub fn with_colorspace(mut self, colorspace: impl Into<Option<QoiColorspace>>) -> Self {
        self.colorspace = colorspace.into();
        self
    }

    /// The colorspace byte to write for an image whose colour maps to
    /// `derived`.
    pub(crate) fn resolve(&self, derived: QoiColorspace) -> QoiColorspace {
        self.colorspace.unwrap_or(derived)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_fire_in_order() {
        let o = DecodeOptions::default()
            .with_max_width(10u32)
            .with_max_height(10u32)
            .with_max_pixels(50u64)
            .with_max_bytes(100u64);
        assert!(o.check(5, 5, 75).is_ok());
        assert!(matches!(o.check(11, 1, 1), Err(QoiError::LimitExceeded(_))));
        assert!(matches!(o.check(1, 11, 1), Err(QoiError::LimitExceeded(_))));
        assert!(matches!(o.check(8, 8, 1), Err(QoiError::LimitExceeded(_))));
        assert!(matches!(
            o.check(5, 5, 101),
            Err(QoiError::LimitExceeded(_))
        ));
        assert!(o.unlimited().check(u32::MAX, u32::MAX, u64::MAX).is_ok());
    }

    #[test]
    fn defaults_cap_bytes_only() {
        let d = DecodeOptions::default();
        assert_eq!(d.max_width, None);
        assert_eq!(d.max_height, None);
        assert_eq!(d.max_pixels, None);
        assert_eq!(d.max_bytes, Some(1 << 30));
        assert!(!d.strict);
    }

    #[test]
    fn encode_options_resolve() {
        let d = EncodeOptions::default();
        assert_eq!(
            d.resolve(QoiColorspace::AllLinear),
            QoiColorspace::AllLinear
        );
        let f = EncodeOptions::default().with_colorspace(QoiColorspace::SrgbWithLinearAlpha);
        assert_eq!(
            f.resolve(QoiColorspace::AllLinear),
            QoiColorspace::SrgbWithLinearAlpha
        );
    }
}
