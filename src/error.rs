//! Crate-local error type used by `oxideav-qoi`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`QoiError`] gains a
//! `From<QoiError> for oxideav_core::Error` impl (defined in
//! [`crate::registry`]) so the trait-side surface (`Decoder` /
//! `Encoder`) can keep returning `oxideav_core::Result<T>` while the
//! underlying parse/encode functions stay framework-free.

use core::fmt;

/// `Result` alias scoped to `oxideav-qoi`. Standalone (no `oxideav-core`)
/// callers see this; framework callers convert via the gated
/// `From<QoiError> for oxideav_core::Error` impl.
pub type Result<T> = core::result::Result<T, QoiError>;

/// Error variants returned by `oxideav-qoi`'s standalone API.
///
/// QOI is a tightly-specified format with no optional chunks, no
/// extension points, and no compression dictionary, so the only ways
/// the decoder can fail are: malformed bytes (bad magic, truncated
/// stream, missing end marker, run length 0, illegal `channels` /
/// `colorspace` enum value) and one "this looks possible per the spec
/// but our integer width can't represent it" guard for
/// `width * height * channels` overflowing `usize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QoiError {
    /// The byte stream is malformed (bad magic, truncated, …).
    InvalidData(String),
    /// The byte stream is theoretically valid but exceeds an internal
    /// limit (e.g. `width * height * 4` overflows `usize`).
    Unsupported(String),
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
}

impl fmt::Display for QoiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for QoiError {}
