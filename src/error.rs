//! Crate-local error type used by `oxideav-qoi`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`QoiError`] gains a
//! `From<QoiError> for oxideav_core::Error` impl (defined in
//! `crate::registry`) so the trait-side surface (`Decoder` /
//! `Encoder`) can keep returning `oxideav_core::Result<T>` while the
//! underlying decode/encode functions stay framework-free.

use core::fmt;

/// `Result` alias scoped to `oxideav-qoi`. Standalone (no `oxideav-core`)
/// callers see this; framework callers convert via the gated
/// `From<QoiError> for oxideav_core::Error` impl.
pub type Result<T> = core::result::Result<T, QoiError>;

/// The contract name for [`QoiError`].
pub type Error = QoiError;

/// Error variants returned by `oxideav-qoi`'s standalone API.
///
/// QOI is a tightly-specified format with no optional chunks, no
/// extension points, and no compression dictionary, so the decoder
/// fails only on malformed bytes (bad magic, truncated stream, missing
/// end marker, illegal `channels` / `colorspace` value, zero
/// dimension), on a [`crate::DecodeOptions`] limit, or on an image
/// whose byte count the platform cannot address. The encoder fails on
/// caller-supplied images whose geometry does not match their pixel
/// buffer, or whose layout QOI cannot carry.
#[derive(Debug)]
#[non_exhaustive]
pub enum QoiError {
    /// The byte stream is malformed (bad magic, truncated, …), or a
    /// caller-assembled image has inconsistent geometry.
    InvalidData(String),
    /// The input is theoretically valid but this crate cannot handle
    /// it: a layout QOI cannot carry, or `width * height * channels`
    /// overflowing `usize`.
    Unsupported(String),
    /// A [`crate::DecodeOptions`] limit (dimensions / pixels / bytes)
    /// would be exceeded; nothing was allocated.
    LimitExceeded(String),
    /// A read / write on a caller-supplied stream failed
    /// ([`crate::decode_from`] / [`crate::encode_to`]).
    Io(std::io::Error),
}

impl QoiError {
    /// Construct a [`QoiError::InvalidData`] from a stringy message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct a [`QoiError::Unsupported`] from a stringy message.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Construct a [`QoiError::LimitExceeded`] from a stringy message.
    pub fn limit(msg: impl Into<String>) -> Self {
        Self::LimitExceeded(msg.into())
    }
}

impl From<std::io::Error> for QoiError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for QoiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::LimitExceeded(s) => write!(f, "limit exceeded: {s}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for QoiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
