#![no_main]

//! Drive the whole contract decode surface — `probe`, `info`, `decode`,
//! `decode_with` (tight limits), `decode_rgb8`, `decode_rgba8`,
//! `decode_from` — over arbitrary fuzz-supplied bytes. Every call must
//! return a `Result` (or `bool`) and never panic / abort / OOM,
//! regardless of how malformed the input is: no integer overflow (in a
//! debug build), no index out of bounds, no attacker-sized pixel buffer
//! allocated for the claimed `width * height * channels`.
//!
//! Cross-checks asserted whenever the paths succeed:
//! * `info` succeeds on every input `decode` accepts, and agrees with
//!   the decoded image on dimensions / layout / colour;
//! * `probe` is `true` for every input `info` accepts;
//! * `decode_rgb8` / `decode_rgba8` agree with `to_rgb8` / `to_rgba8`
//!   of the native image and have the exact packed size;
//! * `decode_from` over a reader yields the same image as `decode`;
//! * a tight `max_pixels` limit never lets a larger image through;
//! * the decoded image re-encodes and decodes back identically
//!   (lossless round trip through `encode` / `decode`).

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::{
    decode, decode_from, decode_rgb8, decode_rgba8, decode_with, encode, info, probe,
    DecodeOptions, EncodeOptions, QoiError,
};

fuzz_target!(|data: &[u8]| {
    let probed = probe(data);
    let header = info(data);
    if header.is_ok() {
        assert!(probed, "info accepted an input probe rejected");
    }

    // Native decode.
    let decoded = decode(data);
    let Ok(img) = decoded else {
        // A failed decode is fine; the raw paths must fail identically
        // (same entry point underneath) and never panic.
        assert!(decode_rgb8(data).is_err());
        assert!(decode_rgba8(data).is_err());
        return;
    };
    let hdr = header.expect("decode succeeded, so the header parses");
    assert_eq!(hdr.width, img.width());
    assert_eq!(hdr.height, img.height());
    assert_eq!(hdr.format, img.format());
    assert_eq!(hdr.color, img.color);
    assert_eq!(hdr.frames, 1);
    assert!(img.is_tightly_packed(), "decoder output is tightly packed");

    // Raw paths agree with the native image.
    let w = img.width() as usize;
    let h = img.height() as usize;
    let rgb = decode_rgb8(data).expect("decode_rgb8 agrees with decode on acceptance");
    assert_eq!(rgb.data.len(), w * h * 3);
    assert_eq!(rgb.data, img.to_rgb8());
    let rgba = decode_rgba8(data).expect("decode_rgba8 agrees with decode on acceptance");
    assert_eq!(rgba.data.len(), w * h * 4);
    assert_eq!(rgba.data, img.to_rgba8());

    // Reader path.
    let via_reader = decode_from(std::io::Cursor::new(data)).expect("decode_from agrees");
    assert_eq!(via_reader, img);

    // Limits: a cap one pixel below the image must refuse it.
    let pixels = (w as u64) * (h as u64);
    let tight = DecodeOptions::default().with_max_pixels(pixels - 1);
    match decode_with(data, &tight) {
        Err(QoiError::LimitExceeded(_)) => {}
        other => panic!("max_pixels = pixels - 1 must refuse the image, got {other:?}"),
    }
    let exact = DecodeOptions::default().with_max_pixels(pixels);
    assert!(decode_with(data, &exact).is_ok());

    // Lossless round trip through the contract encoder.
    let re = encode(&img, &EncodeOptions::default()).expect("every decoded image re-encodes");
    let back = decode(&re).expect("re-encoded stream decodes");
    assert_eq!(back, img, "decode(encode(img)) == img");
});
