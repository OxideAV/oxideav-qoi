//! The root vocabulary of the image-crate API contract
//! (`IMAGE_CRATE_API`): `probe` / `info` / `decode*` / `encode*`.
//!
//! Every function here is framework-free (builds with
//! `default-features = false`) and is the single implementation the
//! registry `Decoder` / `Encoder` adapters call.

use std::io::{Read, Write};

use crate::decoder;
use crate::encoder;
use crate::error::Result;
use crate::image::{ImageInfo, QoiImage, RgbImage, RgbaImage};
use crate::options::{DecodeOptions, EncodeOptions};
use crate::MAGIC;

/// `true` when `bytes` starts with the QOI magic `qoif`. Total,
/// allocation-free, `false` on short input. Says nothing about the
/// header fields or the body — see [`info`].
pub fn probe(bytes: &[u8]) -> bool {
    bytes.len() >= MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC
}

/// Header only: dimensions, native [`crate::PixelFormat`], `frames`
/// (always 1), alpha, colour (from the colorspace byte) and the
/// (always absent) metadata flags, plus the raw `channels` /
/// `colorspace` fields. Reads the 14-byte header and nothing else;
/// accepts an input as short as the header.
///
/// Errors: [`crate::QoiError::InvalidData`] for a short input, wrong
/// magic, `channels` ∉ {3, 4}, `colorspace` ∉ {0, 1}, or a zero
/// dimension.
pub fn info(bytes: &[u8]) -> Result<ImageInfo> {
    decoder::parse_header(bytes).map(|h| ImageInfo::from_header(&h))
}

/// Decode a complete QOI file into its native layout (`Rgb24` or
/// `Rgba`, one tightly packed plane) with [`DecodeOptions::default`]
/// (decoded plane capped at 1 GiB).
pub fn decode(bytes: &[u8]) -> Result<QoiImage> {
    decode_with(bytes, &DecodeOptions::default())
}

/// [`decode`] with explicit limits. Every limit is checked against
/// the header before the pixel buffer is allocated
/// ([`crate::QoiError::LimitExceeded`]).
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<QoiImage> {
    decoder::decode_with(bytes, opts)
}

/// Decode straight to tightly packed 8-bit RGB (alpha dropped for
/// `Rgba` sources), default limits.
pub fn decode_rgb8(bytes: &[u8]) -> Result<RgbImage> {
    let img = decode(bytes)?;
    Ok(RgbImage::new(img.width, img.height, img.to_rgb8()))
}

/// Decode straight to tightly packed 8-bit RGBA (alpha `255` for
/// `Rgb24` sources), default limits.
pub fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
    let img = decode(bytes)?;
    Ok(RgbaImage::new(img.width, img.height, img.to_rgba8()))
}

/// Read `r` to end and [`decode`] it. QOI has no streaming decode
/// (the chunk walk needs the trailing end marker), so the whole input
/// is buffered. Read failures surface as [`crate::QoiError::Io`].
pub fn decode_from<R: Read>(mut r: R) -> Result<QoiImage> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    decode(&buf)
}

/// Encode `image` as a complete QOI file. Both native layouts encode
/// as given (a plane with row padding is repacked); the colorspace
/// byte follows `opts` or the image's colour; metadata cannot be
/// carried and is ignored. There is no 8-bit RGB(A) input QOI cannot
/// represent, so [`crate::QoiError::Unsupported`] is reserved for
/// geometry that overflows `usize`.
pub fn encode(image: &QoiImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    encoder::encode_image(image, opts)
}

/// Encode tightly packed 8-bit RGB (`3 × width × height` bytes) as an
/// `Rgb24` (`channels = 3`) QOI file. The colorspace byte is
/// [`EncodeOptions::colorspace`] or sRGB. A short buffer is
/// [`crate::QoiError::InvalidData`].
pub fn encode_rgb8(width: u32, height: u32, rgb: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    encode(&QoiImage::from_rgb8(width, height, rgb.to_vec())?, opts)
}

/// Encode tightly packed 8-bit RGBA (`4 × width × height` bytes) as an
/// `Rgba` (`channels = 4`) QOI file. The colorspace byte is
/// [`EncodeOptions::colorspace`] or sRGB. A short buffer is
/// [`crate::QoiError::InvalidData`].
pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    encode(&QoiImage::from_rgba8(width, height, rgba.to_vec())?, opts)
}

/// [`encode`] into a writer. Write failures surface as
/// [`crate::QoiError::Io`].
pub fn encode_to<W: Write>(image: &QoiImage, opts: &EncodeOptions, mut w: W) -> Result<()> {
    let bytes = encode(image, opts)?;
    w.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{ColorInfo, PixelFormat, QoiChannels, QoiColorspace};
    use crate::QoiError;

    fn checker(w: u32, h: u32, bpp: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w as usize * h as usize * bpp);
        for y in 0..h {
            for x in 0..w {
                let q = ((x & 1) + 2 * (y & 1)) as u8;
                let px = [
                    [255, 0, 0, 255],
                    [0, 255, 0, 255],
                    [0, 0, 255, 200],
                    [255, 255, 255, 128],
                ][q as usize];
                v.extend_from_slice(&px[..bpp]);
            }
        }
        v
    }

    #[test]
    fn probe_is_total_and_magic_only() {
        assert!(!probe(b""));
        assert!(!probe(b"qoi"));
        assert!(probe(b"qoif"));
        assert!(probe(
            &encode_rgba8(1, 1, &[1, 2, 3, 4], &EncodeOptions::default()).unwrap()
        ));
        assert!(!probe(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn info_reads_header_only() {
        let bytes = encode_rgba8(5, 3, &checker(5, 3, 4), &EncodeOptions::default()).unwrap();
        let i = info(&bytes[..14]).unwrap();
        assert_eq!((i.width, i.height), (5, 3));
        assert_eq!(i.format, PixelFormat::Rgba);
        assert_eq!(i.frames, 1);
        assert!(i.has_alpha);
        assert_eq!(i.color, ColorInfo::srgb());
        assert!(!i.has_icc && !i.has_exif && !i.has_xmp);
        assert_eq!(i.channels, QoiChannels::Rgba);
        assert_eq!(i.colorspace, QoiColorspace::SrgbWithLinearAlpha);
        assert_eq!(i.header().width, 5);

        let rgb = encode_rgb8(
            2,
            2,
            &checker(2, 2, 3),
            &EncodeOptions::default().with_colorspace(QoiColorspace::AllLinear),
        )
        .unwrap();
        let i = info(&rgb).unwrap();
        assert_eq!(i.format, PixelFormat::Rgb24);
        assert!(!i.has_alpha);
        assert_eq!(i.color, ColorInfo::linear());
        assert!(matches!(info(b"qoif"), Err(QoiError::InvalidData(_))));
    }

    #[test]
    fn decode_fills_native_layout_and_colour() {
        let px = checker(4, 4, 3);
        let bytes = encode_rgb8(4, 4, &px, &EncodeOptions::default()).unwrap();
        let img = decode(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (4, 4));
        assert_eq!(img.format(), PixelFormat::Rgb24);
        assert_eq!(img.planes.len(), 1);
        assert_eq!(img.planes[0].stride, 12);
        assert_eq!(img.as_bytes().unwrap(), &px[..]);
        assert_eq!(img.color, ColorInfo::srgb());
        assert!(img.metadata.is_empty());
        assert!(img.is_tightly_packed());
    }

    #[test]
    fn rgb8_and_rgba8_raw_paths() {
        let px = checker(3, 2, 4);
        let bytes = encode_rgba8(3, 2, &px, &EncodeOptions::default()).unwrap();
        let rgba = decode_rgba8(&bytes).unwrap();
        assert_eq!((rgba.width, rgba.height), (3, 2));
        assert_eq!(rgba.as_bytes(), &px[..]);
        let rgb = decode_rgb8(&bytes).unwrap();
        let expect: Vec<u8> = px.chunks_exact(4).flat_map(|p| p[..3].to_vec()).collect();
        assert_eq!(rgb.into_raw(), expect);

        let px3 = checker(3, 2, 3);
        let bytes = encode_rgb8(3, 2, &px3, &EncodeOptions::default()).unwrap();
        let rgba = decode_rgba8(&bytes).unwrap();
        let expect: Vec<u8> = px3
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        assert_eq!(rgba.data, expect);
    }

    #[test]
    fn lossless_round_trip_both_layouts_both_colorspaces() {
        for (bpp, fmt) in [(3usize, PixelFormat::Rgb24), (4, PixelFormat::Rgba)] {
            for cs in [QoiColorspace::SrgbWithLinearAlpha, QoiColorspace::AllLinear] {
                let px = checker(7, 5, bpp);
                let img = QoiImage::packed(7, 5, fmt, 7 * bpp, px)
                    .unwrap()
                    .with_color(cs.color_info());
                let bytes = encode(&img, &EncodeOptions::default()).unwrap();
                assert_eq!(bytes[12] as usize, bpp);
                assert_eq!(bytes[13], cs.byte());
                let back = decode(&bytes).unwrap();
                assert_eq!(back, img, "decode(encode(img)) == img for {fmt:?} / {cs:?}");
            }
        }
    }

    #[test]
    fn encode_repacks_padded_planes_and_forces_colorspace() {
        // 2×2 RGB with a 4-byte stride padding per row.
        let data = vec![
            1, 2, 3, 4, 5, 6, 0, 0, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0, 0, 0,
        ];
        let img = QoiImage::packed(2, 2, PixelFormat::Rgb24, 10, data).unwrap();
        let bytes = encode(
            &img,
            &EncodeOptions::default().with_colorspace(QoiColorspace::AllLinear),
        )
        .unwrap();
        assert_eq!(bytes[13], 1);
        let back = decode(&bytes).unwrap();
        assert_eq!(
            back.as_bytes().unwrap(),
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12][..]
        );
        assert_eq!(back.color, ColorInfo::linear());
    }

    #[test]
    fn raw_encode_rejects_short_buffers_without_panicking() {
        assert!(matches!(
            encode_rgb8(2, 2, &[0; 11], &EncodeOptions::default()),
            Err(QoiError::InvalidData(_))
        ));
        assert!(matches!(
            encode_rgba8(0, 2, &[], &EncodeOptions::default()),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn decode_with_limits_fire_before_allocation() {
        let bytes = encode_rgba8(8, 8, &checker(8, 8, 4), &EncodeOptions::default()).unwrap();
        let o = DecodeOptions::default().with_max_width(7u32);
        assert!(matches!(
            decode_with(&bytes, &o),
            Err(QoiError::LimitExceeded(_))
        ));
        let o = DecodeOptions::default().with_max_bytes(255u64);
        assert!(matches!(
            decode_with(&bytes, &o),
            Err(QoiError::LimitExceeded(_))
        ));
        let o = DecodeOptions::default()
            .with_max_pixels(64u64)
            .with_max_bytes(256u64);
        assert!(decode_with(&bytes, &o).is_ok());
        // A hostile header claiming 60000×60000 (14.4 GB) trips the
        // default 1 GiB cap without touching the allocator.
        let mut hostile = bytes.clone();
        hostile[4..8].copy_from_slice(&60000u32.to_be_bytes());
        hostile[8..12].copy_from_slice(&60000u32.to_be_bytes());
        assert!(matches!(decode(&hostile), Err(QoiError::LimitExceeded(_))));
        // Lifting the cap falls through to the physical "chunk stream
        // can't produce that many pixels" guard.
        assert!(matches!(
            decode_with(&hostile, &DecodeOptions::default().unlimited()),
            Err(QoiError::InvalidData(_))
        ));
    }

    #[test]
    fn decode_from_and_encode_to_round_trip() {
        let px = checker(3, 3, 4);
        let img = QoiImage::from_rgba8(3, 3, px).unwrap();
        let mut out = Vec::new();
        encode_to(&img, &EncodeOptions::default(), &mut out).unwrap();
        let back = decode_from(std::io::Cursor::new(&out)).unwrap();
        assert_eq!(back, img);

        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
        }
        assert!(matches!(decode_from(Failing), Err(QoiError::Io(_))));
        struct Sink;
        impl Write for Sink {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(matches!(
            encode_to(&img, &EncodeOptions::default(), Sink),
            Err(QoiError::Io(_))
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_wrappers_agree_with_the_contract_path() {
        let px = checker(6, 4, 4);
        let old = crate::encode_qoi(6, 4, 4, &px);
        let new = encode_rgba8(6, 4, &px, &EncodeOptions::default()).unwrap();
        assert_eq!(old, new);
        let old_lin = crate::encode_qoi_full(6, 4, 4, 1, &px);
        let new_lin = encode_rgba8(
            6,
            4,
            &px,
            &EncodeOptions::default().with_colorspace(QoiColorspace::AllLinear),
        )
        .unwrap();
        assert_eq!(old_lin, new_lin);
        let a = crate::parse_qoi(&new).unwrap();
        let b = decode(&new).unwrap();
        assert_eq!(a, b);
        let h = crate::parse_qoi_header(&new).unwrap();
        assert_eq!(h, info(&new).unwrap().header());
    }
}
