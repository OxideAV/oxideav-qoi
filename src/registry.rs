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
//! * The `From<QoiError> for oxideav_core::Error` conversion that lets
//!   the trait-side `Decoder` / `Encoder` impls (living in
//!   `decoder.rs` / `encoder.rs`) bubble bitstream errors up through
//!   the framework error type.

use oxideav_core::{CodecCapabilities, CodecId, PixelFormat, RuntimeContext};
use oxideav_core::{CodecInfo, CodecRegistry, ContainerRegistry};

use crate::error::QoiError;

/// Convert a [`QoiError`] into the framework-shared `oxideav_core::Error`
/// so trait impls in this crate can use `?` on errors returned by the
/// framework-free decode/encode functions.
impl From<QoiError> for oxideav_core::Error {
    fn from(e: QoiError) -> Self {
        match e {
            QoiError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
            QoiError::Unsupported(s) => oxideav_core::Error::Unsupported(s),
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
            .encoder(crate::encoder::make_encoder),
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
/// Also wired into [`oxideav_meta::register_all`] via the
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
}
