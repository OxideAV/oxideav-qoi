//! Property sweep over the image-crate API contract surface: for
//! pseudo-random images (both layouts, both colorspace bytes, five
//! content generators, random 1..=64 geometry, tight or padded planes)
//! the lossless identity `decode(encode(img)) == img` holds exactly —
//! planes, colour and (empty) metadata — and every other contract entry
//! point agrees with it:
//!
//! * `info` reports the geometry / layout / colour `decode` returns;
//! * `decode_rgb8` / `decode_rgba8` equal `to_rgb8` / `to_rgba8` of the
//!   native image, which are exact (QOI is 8-bit RGB(A) already);
//! * `encode_rgb8` / `encode_rgba8` produce the bytes `encode` produces
//!   for the equivalent `from_rgb8` / `from_rgba8` image;
//! * `encode_to` / `decode_from` match the slice paths;
//! * `probe` accepts every encoded stream;
//! * the deprecated pre-contract wrappers produce identical bytes /
//!   images, so a consumer migrating sees no change.
//!
//! A self-contained xorshift32 PRNG is seeded per generator so any
//! failure is reproducible without a `proptest` / `quickcheck` dev-dep.

use oxideav_qoi::{
    decode, decode_from, decode_rgb8, decode_rgba8, encode, encode_rgb8, encode_rgba8, encode_to,
    info, probe, ColorInfo, EncodeOptions, PixelFormat, QoiColorspace, QoiImage,
};

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

#[derive(Clone, Copy, Debug)]
enum Gen {
    /// Every byte random: RGB / RGBA chunks dominate.
    Noise,
    /// Smooth two-axis gradient: DIFF / LUMA dominate.
    Gradient,
    /// Long runs of a handful of colours: RUN dominates.
    Runs,
    /// Eight colours cycling: INDEX dominates.
    Palette,
    /// Random colour with alpha toggling per pixel: RGBA chunks.
    AlphaToggle,
}

fn pixel(gen: Gen, rng: &mut Rng, x: u32, y: u32, w: u32, h: u32, state: &mut [u8; 4]) -> [u8; 4] {
    match gen {
        Gen::Noise => [rng.byte(), rng.byte(), rng.byte(), rng.byte()],
        Gen::Gradient => [
            ((x * 255) / w.max(1)) as u8,
            ((y * 255) / h.max(1)) as u8,
            (((x + y) * 127) / (w + h).max(1)) as u8,
            255,
        ],
        Gen::Runs => {
            if rng.below(40) == 0 {
                *state = [rng.byte(), rng.byte(), rng.byte(), 255];
            }
            *state
        }
        Gen::Palette => {
            const PAL: [[u8; 4]; 8] = [
                [0, 0, 0, 255],
                [255, 255, 255, 255],
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 0, 128],
                [0, 255, 255, 64],
                [255, 0, 255, 0],
            ];
            PAL[rng.below(8) as usize]
        }
        Gen::AlphaToggle => [
            rng.byte(),
            rng.byte(),
            rng.byte(),
            if rng.below(2) == 0 { 255 } else { rng.byte() },
        ],
    }
}

/// Build an image: `bpp` 3 or 4, `pad` extra bytes per row (0 = tight).
fn build(
    gen: Gen,
    rng: &mut Rng,
    w: u32,
    h: u32,
    bpp: usize,
    pad: usize,
    cs: QoiColorspace,
) -> QoiImage {
    let stride = w as usize * bpp + pad;
    let mut data = Vec::with_capacity(stride * h as usize);
    let mut state = [0u8; 4];
    for y in 0..h {
        for x in 0..w {
            let p = pixel(gen, rng, x, y, w, h, &mut state);
            data.extend_from_slice(&p[..bpp]);
        }
        for _ in 0..pad {
            data.push(rng.byte()); // padding is noise and must never leak
        }
    }
    let fmt = if bpp == 3 {
        PixelFormat::Rgb24
    } else {
        PixelFormat::Rgba
    };
    QoiImage::packed(w, h, fmt, stride, data)
        .expect("valid geometry")
        .with_color(cs.color_info())
}

/// The tightly packed copy of `img`'s pixels (what the decoder returns).
fn tight(img: &QoiImage) -> QoiImage {
    let row = img.width() as usize * img.bytes_per_pixel();
    let src = img.as_bytes().unwrap();
    let mut data = Vec::with_capacity(row * img.height() as usize);
    for y in 0..img.height() as usize {
        data.extend_from_slice(&src[y * img.stride()..y * img.stride() + row]);
    }
    QoiImage::packed(img.width(), img.height(), img.format(), row, data)
        .unwrap()
        .with_color(img.color)
}

#[test]
fn decode_encode_identity_over_random_images() {
    let gens = [
        Gen::Noise,
        Gen::Gradient,
        Gen::Runs,
        Gen::Palette,
        Gen::AlphaToggle,
    ];
    let mut cases = 0usize;
    for (gi, gen) in gens.iter().enumerate() {
        let mut rng = Rng(0xC0FF_EE00 ^ (gi as u32 + 1).wrapping_mul(0x9E37_79B9));
        for case in 0..120 {
            let w = 1 + rng.below(64);
            let h = 1 + rng.below(64);
            let bpp = if rng.below(2) == 0 { 3 } else { 4 };
            let cs = if rng.below(2) == 0 {
                QoiColorspace::SrgbWithLinearAlpha
            } else {
                QoiColorspace::AllLinear
            };
            let pad = if case % 4 == 3 {
                1 + rng.below(7) as usize
            } else {
                0
            };
            let img = build(*gen, &mut rng, w, h, bpp, pad, cs);
            let ctx = format!("{gen:?} case {case}: {w}x{h} bpp={bpp} cs={cs:?} pad={pad}");

            let bytes = encode(&img, &EncodeOptions::default()).expect("encode");
            assert!(probe(&bytes), "{ctx}: probe");
            assert_eq!(bytes[12] as usize, bpp, "{ctx}: channels byte");
            assert_eq!(bytes[13], cs.byte(), "{ctx}: colorspace byte");

            let back = decode(&bytes).expect("decode");
            let expect = tight(&img);
            assert_eq!(back, expect, "{ctx}: decode(encode(img)) == img");
            assert_eq!(back.color, cs.color_info(), "{ctx}: colour from the byte");
            assert!(back.metadata.is_empty(), "{ctx}: no metadata");

            let i = info(&bytes).expect("info");
            assert_eq!(
                (i.width, i.height, i.format, i.color, i.has_alpha, i.frames),
                (w, h, back.format(), back.color, bpp == 4, 1),
                "{ctx}: info agrees"
            );

            // Raw paths vs the native conversions (padding never leaks).
            let rgba = decode_rgba8(&bytes).expect("rgba8");
            assert_eq!(rgba.data, img.to_rgba8(), "{ctx}: decode_rgba8 == to_rgba8");
            assert_eq!(rgba.data.len(), (w * h * 4) as usize);
            let rgb = decode_rgb8(&bytes).expect("rgb8");
            assert_eq!(rgb.data, img.to_rgb8(), "{ctx}: decode_rgb8 == to_rgb8");
            if bpp == 4 {
                for (p4, p3) in rgba.data.chunks_exact(4).zip(rgb.data.chunks_exact(3)) {
                    assert_eq!(&p4[..3], p3, "{ctx}: rgb8 is rgba8 minus alpha");
                }
            } else {
                assert!(
                    rgba.data.chunks_exact(4).all(|p| p[3] == 255),
                    "{ctx}: Rgb24 gets opaque alpha"
                );
            }

            // The raw encode paths are `encode` of the equivalent image
            // (colorspace forced, since raw bytes carry no colour).
            let raw = expect.as_bytes().unwrap();
            let forced = EncodeOptions::default().with_colorspace(cs);
            let raw_bytes = if bpp == 3 {
                encode_rgb8(w, h, raw, &forced)
            } else {
                encode_rgba8(w, h, raw, &forced)
            }
            .expect("raw encode");
            assert_eq!(raw_bytes, bytes, "{ctx}: encode_rgb(a)8 == encode");

            // Streaming variants.
            let mut out = Vec::new();
            encode_to(&img, &EncodeOptions::default(), &mut out).expect("encode_to");
            assert_eq!(out, bytes, "{ctx}: encode_to == encode");
            assert_eq!(
                decode_from(std::io::Cursor::new(&bytes)).expect("decode_from"),
                back,
                "{ctx}: decode_from == decode"
            );

            // Second generation is a fixed point.
            let again = encode(&back, &EncodeOptions::default()).expect("re-encode");
            assert_eq!(again, bytes, "{ctx}: encode is a fixed point after decode");
            cases += 1;
        }
    }
    assert_eq!(cases, 600);
}

#[test]
#[allow(deprecated)]
fn deprecated_wrappers_are_byte_identical_to_the_contract_path() {
    let mut rng = Rng(0x1234_5678);
    for case in 0..60 {
        let w = 1 + rng.below(32);
        let h = 1 + rng.below(32);
        let bpp = if case % 2 == 0 { 3 } else { 4 };
        let cs_byte = (case % 3 == 2) as u8;
        let cs = QoiColorspace::from_byte(cs_byte).unwrap();
        let img = build(Gen::Noise, &mut rng, w, h, bpp, 0, cs);
        let px = img.as_bytes().unwrap();

        let new = encode(&img, &EncodeOptions::default()).unwrap();
        let old = oxideav_qoi::encode_qoi_full(w, h, bpp as u8, cs_byte, px);
        assert_eq!(old, new, "encode_qoi_full == encode");
        if cs_byte == 0 {
            assert_eq!(
                oxideav_qoi::encode_qoi(w, h, bpp as u8, px),
                new,
                "encode_qoi == encode"
            );
        }
        let a = oxideav_qoi::parse_qoi(&new).unwrap();
        let b = decode(&new).unwrap();
        assert_eq!(a, b, "parse_qoi == decode");
        let hdr = oxideav_qoi::parse_qoi_header(&new).unwrap();
        assert_eq!(
            hdr,
            info(&new).unwrap().header(),
            "parse_qoi_header == info"
        );
        let mut reuse = Vec::new();
        let hdr2 = oxideav_qoi::parse_qoi_into(&new, &mut reuse).unwrap();
        assert_eq!(hdr2, hdr);
        assert_eq!(
            reuse,
            b.into_raw(),
            "parse_qoi_into pixels == decode pixels"
        );
    }
}

#[test]
fn colour_defaults_and_overrides() {
    let img = QoiImage::from_rgb8(2, 1, vec![1, 2, 3, 4, 5, 6]).unwrap();
    // A fresh image is sRGB (QOI's default byte 0).
    assert_eq!(img.color, ColorInfo::srgb());
    assert_eq!(img.colorspace(), QoiColorspace::SrgbWithLinearAlpha);
    // Unspecified colour also maps to byte 0 (QOI has no third value).
    let unspec = img.clone().with_color(ColorInfo::unspecified());
    let bytes = encode(&unspec, &EncodeOptions::default()).unwrap();
    assert_eq!(bytes[13], 0);
    // Only a linear transfer selects byte 1 …
    let lin = img.clone().with_color(ColorInfo::srgb().with_transfer(8));
    assert_eq!(encode(&lin, &EncodeOptions::default()).unwrap()[13], 1);
    // … and the option overrides the image either way.
    let force0 = EncodeOptions::default().with_colorspace(QoiColorspace::SrgbWithLinearAlpha);
    assert_eq!(encode(&lin, &force0).unwrap()[13], 0);
    let force1 = EncodeOptions::default().with_colorspace(QoiColorspace::AllLinear);
    assert_eq!(encode(&img, &force1).unwrap()[13], 1);
    // Decoding the forced-linear stream reports the linear colour.
    assert_eq!(
        decode(&encode(&img, &force1).unwrap()).unwrap().color,
        ColorInfo::linear()
    );
}
