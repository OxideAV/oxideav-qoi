//! Black-box validation against reference `.qoi` fixtures from
//! qoiformat.org.
//!
//! For each fixture we:
//! 1. Decode it with [`oxideav_qoi::parse_qoi`].
//! 2. Re-encode the decoded pixels with [`oxideav_qoi::encode_qoi_full`].
//! 3. Assert the round-tripped bytes are **byte-for-byte identical** to
//!    the original file.
//!
//! Step 3 is a much stronger check than just "the round-trip preserves
//! pixels". It says our encoder picks exactly the same chunk for every
//! pixel as the reference encoder did when it produced the fixture —
//! same RUN lengths, same INDEX hits, same DIFF/LUMA/RGB/RGBA
//! choices. The only way that's true is if our hash, our chunk
//! priority, and our delta arithmetic all match the spec.
//!
//! These fixtures were downloaded from
//! <https://qoiformat.org/qoi_test_images.zip> (the dataset linked
//! from the QOI homepage). They are not modified.

use oxideav_qoi::{encode_qoi_full, parse_qoi, QoiChannels, QoiColorspace};

fn check_byte_exact_roundtrip(path: &str, fixture: &[u8]) {
    let img = parse_qoi(fixture).unwrap_or_else(|e| panic!("{path}: parse failed: {e:?}"));

    let channels = match img.channels {
        QoiChannels::Rgb => 3,
        QoiChannels::Rgba => 4,
    };
    let colorspace = match img.colorspace {
        QoiColorspace::SrgbWithLinearAlpha => 0,
        QoiColorspace::AllLinear => 1,
    };

    let re = encode_qoi_full(img.width, img.height, channels, colorspace, &img.pixels);

    if re != fixture {
        // Surface the first mismatch + size delta to make it easy to
        // diagnose without dumping kilobytes into the test log.
        let first = re
            .iter()
            .zip(fixture.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(usize::min(re.len(), fixture.len()));
        panic!(
            "{path}: re-encoded bytes differ from reference. \
             original_len={} re_len={} first_diff_at={} \
             ref[first..first+8]={:02x?} re[first..first+8]={:02x?}",
            fixture.len(),
            re.len(),
            first,
            &fixture[first..(first + 8).min(fixture.len())],
            &re[first..(first + 8).min(re.len())],
        );
    }
}

#[test]
fn edgecase_qoi_byte_exact() {
    let bytes = include_bytes!("fixtures/edgecase.qoi");
    check_byte_exact_roundtrip("edgecase.qoi", bytes);
}

#[test]
fn qoi_logo_byte_exact() {
    let bytes = include_bytes!("fixtures/qoi_logo.qoi");
    check_byte_exact_roundtrip("qoi_logo.qoi", bytes);
}

#[test]
fn testcard_byte_exact() {
    let bytes = include_bytes!("fixtures/testcard.qoi");
    check_byte_exact_roundtrip("testcard.qoi", bytes);
}

#[test]
fn testcard_rgba_byte_exact() {
    let bytes = include_bytes!("fixtures/testcard_rgba.qoi");
    check_byte_exact_roundtrip("testcard_rgba.qoi", bytes);
}

/// Round-trip *only* (no byte-exact check) — confirms `parse_qoi` then
/// `encode_qoi` then `parse_qoi` again produces the same pixel buffer
/// even on fixtures we skip the byte-exact check on.
#[test]
fn pixel_roundtrip_all_fixtures() {
    for (name, bytes) in [
        ("edgecase.qoi", &include_bytes!("fixtures/edgecase.qoi")[..]),
        ("qoi_logo.qoi", &include_bytes!("fixtures/qoi_logo.qoi")[..]),
        ("testcard.qoi", &include_bytes!("fixtures/testcard.qoi")[..]),
        (
            "testcard_rgba.qoi",
            &include_bytes!("fixtures/testcard_rgba.qoi")[..],
        ),
    ] {
        let first = parse_qoi(bytes).unwrap_or_else(|e| panic!("{name}: first parse: {e:?}"));
        let channels = first.channels as u8;
        let re = encode_qoi_full(
            first.width,
            first.height,
            channels,
            first.colorspace as u8,
            &first.pixels,
        );
        let again = parse_qoi(&re).unwrap_or_else(|e| panic!("{name}: re-parse: {e:?}"));
        assert_eq!(again.width, first.width, "{name}: width drift");
        assert_eq!(again.height, first.height, "{name}: height drift");
        assert_eq!(again.channels, first.channels, "{name}: channels drift");
        assert_eq!(
            again.pixels.len(),
            first.pixels.len(),
            "{name}: pixel buffer length drift"
        );
        assert_eq!(again.pixels, first.pixels, "{name}: pixel bytes drift");
    }
}
