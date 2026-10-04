//! `oxideav-core` integration layer for `oxideav-qoi`.
//!
//! Gated behind the default-on `registry` feature so image-library
//! consumers can depend on `oxideav-qoi` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.
//!
//! The module exposes:
//! * [`register`] — the unified `RuntimeContext` entry point the
//!   umbrella `oxideav` crate calls during framework initialisation.
//!   Internally calls [`register_codecs`] and [`register_containers`].
//! * [`register_codecs`] — registers the QOI codec (decoder + encoder)
//!   into a [`CodecRegistry`].
//! * [`register_containers`] — registers the `.qoi` file extension
//!   against the container name `"qoi"` so cli-convert / pipeline
//!   probing can resolve a `.qoi` output path through the central
//!   [`ContainerRegistry`] instead of a hard-coded list. QOI has no
//!   nested container layer (the file *is* the codec packet), so we
//!   register no demuxer / muxer / probe — just the extension hint.
//! * `From<QoiImage> for VideoFrame` and [`QoiImage::from_video_frame`]
//!   — the frame bridge (one packed plane + the colour-signal
//!   side-channel), plus the 1:1 [`QoiPixelFormat`] ↔ `PixelFormat`
//!   and [`ColorInfo`] ↔ `ColorSignal` mappings.
//! * The `From<QoiError> for oxideav_core::Error` conversion that lets
//!   the trait-side `Decoder` / `Encoder` impls (living in
//!   `decoder.rs` / `encoder.rs`) bubble bitstream errors up through
//!   the framework error type, and the `CodecOptionsStruct` impl for
//!   [`EncodeOptions`] (the `colorspace` option schema).

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecOptionsStruct, CodecParameters, CodecRegistry,
    ColorPrimaries, ColorSignal, ContainerRegistry, MatrixCoefficients, OptionField, OptionKind,
    OptionValue, PixelFormat, RuntimeContext, TransferCharacteristics, VideoFrame, VideoPlane,
};

use crate::error::QoiError;
use crate::image::{ColorInfo, ColorRange, QoiColorspace, QoiImage, QoiPixelFormat};
use crate::options::EncodeOptions;

/// Convert a [`QoiError`] into the framework-shared `oxideav_core::Error`
/// so trait impls in this crate can use `?` on errors returned by the
/// framework-free decode/encode functions.
impl From<QoiError> for oxideav_core::Error {
    fn from(e: QoiError) -> Self {
        match e {
            QoiError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
            QoiError::Unsupported(s) => oxideav_core::Error::Unsupported(s),
            QoiError::LimitExceeded(s) => oxideav_core::Error::ResourceExhausted(s),
            QoiError::Io(e) => oxideav_core::Error::Io(e),
        }
    }
}

// ---- pixel formats --------------------------------------------------------

/// The 1:1 name mapping from [`QoiPixelFormat`] to the framework enum.
pub fn to_core_pixel_format(pf: QoiPixelFormat) -> PixelFormat {
    match pf {
        QoiPixelFormat::Rgb24 => PixelFormat::Rgb24,
        QoiPixelFormat::Rgba => PixelFormat::Rgba,
    }
}

/// Map a framework pixel format to [`QoiPixelFormat`]; `Err` for the
/// layouts QOI cannot carry.
pub fn from_core_pixel_format(pf: PixelFormat) -> oxideav_core::Result<QoiPixelFormat> {
    match pf {
        PixelFormat::Rgb24 => Ok(QoiPixelFormat::Rgb24),
        PixelFormat::Rgba => Ok(QoiPixelFormat::Rgba),
        other => Err(oxideav_core::Error::unsupported(format!(
            "QOI: pixel format {other:?} not supported (Rgb24 / Rgba only)"
        ))),
    }
}

impl From<QoiPixelFormat> for PixelFormat {
    fn from(pf: QoiPixelFormat) -> Self {
        to_core_pixel_format(pf)
    }
}

impl TryFrom<PixelFormat> for QoiPixelFormat {
    type Error = oxideav_core::Error;
    fn try_from(pf: PixelFormat) -> oxideav_core::Result<Self> {
        from_core_pixel_format(pf)
    }
}

// ---- colour signalling ----------------------------------------------------

/// [`ColorInfo`] as the framework's [`ColorSignal`] (code points map
/// 1:1).
pub fn to_color_signal(c: &ColorInfo) -> ColorSignal {
    let range = match c.range {
        ColorRange::Unspecified => oxideav_core::ColorRange::Unspecified,
        ColorRange::Limited => oxideav_core::ColorRange::Limited,
        ColorRange::Full => oxideav_core::ColorRange::Full,
    };
    ColorSignal::new(
        range,
        ColorPrimaries(c.primaries),
        TransferCharacteristics(c.transfer),
        MatrixCoefficients(c.matrix),
    )
}

/// The inverse of [`to_color_signal`].
pub fn from_color_signal(s: &ColorSignal) -> ColorInfo {
    let range = match s.range {
        oxideav_core::ColorRange::Limited => ColorRange::Limited,
        oxideav_core::ColorRange::Full => ColorRange::Full,
        _ => ColorRange::Unspecified,
    };
    ColorInfo::new(range, s.primaries.0, s.transfer.0, s.matrix.0)
}

// ---- frame bridge ---------------------------------------------------------

/// [`QoiImage`] → `VideoFrame`, moving the plane out of the image: one
/// packed image plane plus the colour-signal side-channel (QOI always
/// signals its colorspace, so the signal is always attached).
pub(crate) fn image_into_video_frame(mut image: QoiImage, pts: Option<i64>) -> VideoFrame {
    let stride = image.stride();
    let data = if image.planes.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut image.planes[0].data)
    };
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane { stride, data }],
    };
    frame.set_color_signal(to_color_signal(&image.color));
    frame
}

impl From<QoiImage> for VideoFrame {
    /// The pixel plane (`pts` `None`) plus the colour-signal
    /// side-channel.
    fn from(image: QoiImage) -> Self {
        image_into_video_frame(image, None)
    }
}

impl From<&QoiImage> for VideoFrame {
    fn from(image: &QoiImage) -> Self {
        image_into_video_frame(image.clone(), None)
    }
}

impl QoiImage {
    /// Rebuild an image from a framework frame and the stream
    /// parameters that describe it: `width`, `height` and
    /// `pixel_format` (`Rgb24` / `Rgba`) are required; the frame's
    /// first image plane becomes the pixel plane (geometry validated by
    /// [`QoiImage::new`]); the frame's colour-signal side-channel,
    /// refined over `params.color_signal`, becomes `color` when it
    /// specifies anything (else QOI's sRGB default stands).
    pub fn from_video_frame(frame: &VideoFrame, params: &CodecParameters) -> crate::Result<Self> {
        let width = params
            .width
            .ok_or_else(|| QoiError::invalid("QOI: width missing in CodecParameters"))?;
        let height = params
            .height
            .ok_or_else(|| QoiError::invalid("QOI: height missing in CodecParameters"))?;
        let core_pix = params
            .pixel_format
            .ok_or_else(|| QoiError::invalid("QOI: pixel_format missing in CodecParameters"))?;
        let pix = from_core_pixel_format(core_pix).map_err(|_| {
            QoiError::unsupported(format!(
                "QOI: pixel format {core_pix:?} not supported (Rgb24 / Rgba only)"
            ))
        })?;
        let plane = frame
            .image_planes()
            .first()
            .ok_or_else(|| QoiError::invalid("QOI: frame has no planes"))?;
        let mut img = QoiImage::packed(width, height, pix, plane.stride, plane.data.clone())?;
        // Per-frame record refines the stream-level description; an
        // entirely unspecified result keeps QOI's sRGB default.
        let sig = frame
            .color_signal()
            .unwrap_or_default()
            .or(params.color_signal);
        if !sig.is_unspecified() {
            img.color = from_color_signal(&sig);
        }
        Ok(img)
    }
}

impl TryFrom<(&VideoFrame, &CodecParameters)> for QoiImage {
    type Error = QoiError;
    fn try_from((frame, params): (&VideoFrame, &CodecParameters)) -> crate::Result<Self> {
        QoiImage::from_video_frame(frame, params)
    }
}

// ---- CodecOptionsStruct (registry-only schema for EncodeOptions) ----------

/// The framework's options schema for the QOI encoder — what makes the
/// `colorspace` knob discoverable to `oxideav list`, validatable by the
/// pipeline's JSON-options checker, and parsed with uniform error
/// messages.
///
/// `colorspace` accepts the numeric forms `"0"` / `"1"` and the
/// symbolic names `"srgb"` (= 0, sRGB with linear alpha) and `"linear"`
/// (= 1, all channels linear). Unknown keys and out-of-set values are
/// rejected by `parse_options` before `apply` runs; absent → derive
/// from the frame's colour signal ([`EncodeOptions::colorspace`] =
/// `None`).
impl CodecOptionsStruct for EncodeOptions {
    const SCHEMA: &'static [OptionField] = &[OptionField {
        name: "colorspace",
        // Accept both the numeric and the symbolic spellings; the Enum
        // kind validates the value against this exact set in
        // `parse_options` before `apply` is called.
        kind: OptionKind::Enum(&["0", "srgb", "1", "linear"]),
        default: OptionValue::String(String::new()),
        help: "QOI colorspace header byte: 0/\"srgb\" (sRGB with linear \
               alpha) or 1/\"linear\" (all channels linear). Absent: \
               follows the frame's colour signal (linear transfer → 1, \
               else 0). Informational only — does not change pixel bytes.",
    }];

    fn apply(&mut self, key: &str, value: &OptionValue) -> oxideav_core::Result<()> {
        match key {
            "colorspace" => {
                self.colorspace = Some(match value.as_str()? {
                    "0" | "srgb" => QoiColorspace::SrgbWithLinearAlpha,
                    "1" | "linear" => QoiColorspace::AllLinear,
                    // Unreachable in practice: the Enum schema already
                    // restricts the value set. Kept as a defensive arm.
                    other => {
                        return Err(oxideav_core::Error::invalid(format!(
                            "QOI encoder: invalid colorspace {other:?}"
                        )))
                    }
                });
                Ok(())
            }
            // Unreachable: parse_options rejects unknown keys against
            // SCHEMA before apply runs.
            other => Err(oxideav_core::Error::invalid(format!(
                "QOI encoder: unknown option {other:?}"
            ))),
        }
    }
}

/// Register the QOI codec into the supplied [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("qoi_sw")
        .with_intra_only(true)
        .with_lossless(true)
        // QOI's header stores width / height in u32 BE — there's no
        // structural limit short of u32::MAX. Cap at a generous but
        // memory-safe size so a malicious header can't spike a 16 GB
        // allocation guess in the registry layer.
        .with_max_size(65535, 65535)
        .with_pixel_formats(vec![PixelFormat::Rgba, PixelFormat::Rgb24]);
    reg.register(
        CodecInfo::new(CodecId::new(crate::CODEC_ID_STR))
            .capabilities(caps)
            .decoder(crate::decoder::make_decoder)
            .encoder(crate::encoder::make_encoder)
            // Declare the encoder's recognised option keys (just
            // `colorspace`) so `oxideav list` / pipeline JSON validation
            // can discover and check them.
            .encoder_options::<EncodeOptions>(),
    );
}

/// Register the `.qoi` file extension against the container name
/// `"qoi"` so consumers (cli-convert, pipeline output probing, …) can
/// resolve a `.qoi` output path through the central
/// [`ContainerRegistry`] instead of a hard-coded extension list.
///
/// QOI is a single-image format with no nested container layer — the
/// file *is* the codec packet — so we register only the extension
/// hint here, no demuxer / muxer / probe. Callers that just want the
/// codec side should keep using [`register_codecs`].
pub fn register_containers(reg: &mut ContainerRegistry) {
    reg.register_extension("qoi", "qoi");
}

/// Unified entry point: install every codec and container provided by
/// `oxideav-qoi` into a [`RuntimeContext`].
///
/// Also wired into `oxideav_meta::register_all` via the
/// [`oxideav_core::register!`] macro below.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("qoi", register);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qoi_extension_resolves_to_qoi_container() {
        let mut reg = ContainerRegistry::new();
        register_containers(&mut reg);
        assert_eq!(reg.container_for_extension("qoi"), Some("qoi"));
        assert_eq!(reg.container_for_extension("QOI"), Some("qoi")); // case insensitive
    }

    #[test]
    fn register_via_runtime_context_installs_factories() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        assert!(
            ctx.codecs.decoder_ids().next().is_some(),
            "register(ctx) should install codec decoder factories"
        );
        assert_eq!(
            ctx.containers.container_for_extension("qoi"),
            Some("qoi"),
            "register(ctx) should install .qoi extension hint"
        );
    }

    #[test]
    fn encoder_options_schema_is_discoverable_through_the_registry() {
        // The colorspace knob must be reachable via the registry's
        // schema lookup so `oxideav list` / pipeline JSON validation can
        // discover and check it.
        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        let id = CodecId::new(crate::CODEC_ID_STR);
        let schema = reg
            .encoder_options_schema(&id)
            .expect("QOI encoder should expose an options schema");
        assert!(
            schema.iter().any(|f| f.name == "colorspace"),
            "schema must list the colorspace option"
        );
    }
}
