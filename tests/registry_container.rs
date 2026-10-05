//! The QOI container through the framework registry (round 472):
//! probe → `open_demuxer` → `first_decoder`, and `first_encoder` →
//! `open_muxer`, pinned byte-for-byte against Layer 1 `decode`.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    CodecId, CodecParameters, ColorSignal, Error as CoreError, Frame as CoreFrame, Packet,
    PixelFormat, RuntimeContext, StreamInfo, TimeBase, VideoFrame,
};
use oxideav_qoi::container;
use oxideav_qoi::registry::to_color_signal;
use oxideav_qoi::{decode, encode, info, ColorInfo, EncodeOptions, QoiImage, CODEC_ID_STR};

const EDGECASE: &[u8] = include_bytes!("fixtures/edgecase.qoi");
const LOGO: &[u8] = include_bytes!("fixtures/qoi_logo.qoi");
const TESTCARD: &[u8] = include_bytes!("fixtures/testcard.qoi");
const TESTCARD_RGBA: &[u8] = include_bytes!("fixtures/testcard_rgba.qoi");

fn ctx() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_qoi::register(&mut ctx);
    ctx
}

fn open(ctx: &RuntimeContext, bytes: &[u8]) -> Box<dyn oxideav_core::Demuxer> {
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(bytes.to_vec()));
    container::open_demuxer(reader, &ctx.codecs).expect("open_demuxer")
}

/// Pump exactly as the gateway does: send, drain to NeedMore, flush,
/// drain to Eof.
fn pump(ctx: &RuntimeContext, bytes: &[u8]) -> (StreamInfo, Vec<Packet>, Vec<VideoFrame>) {
    let mut demux = open(ctx, bytes);
    assert_eq!(demux.streams().len(), 1);
    let stream = demux.streams()[0].clone();
    let mut dec = ctx
        .codecs
        .first_decoder(&stream.params)
        .expect("first_decoder");
    let mut packets = Vec::new();
    let mut frames = Vec::new();
    loop {
        match demux.next_packet() {
            Ok(pkt) => {
                dec.send_packet(&pkt).expect("send_packet");
                packets.push(pkt);
                loop {
                    match dec.receive_frame() {
                        Ok(CoreFrame::Video(v)) => frames.push(v),
                        Ok(_) => panic!("non-video frame"),
                        Err(CoreError::NeedMore) => break,
                        Err(e) => panic!("receive_frame: {e}"),
                    }
                }
            }
            Err(CoreError::Eof) => break,
            Err(e) => panic!("next_packet: {e}"),
        }
    }
    dec.flush().unwrap();
    loop {
        match dec.receive_frame() {
            Ok(CoreFrame::Video(v)) => frames.push(v),
            Ok(_) => panic!("non-video frame"),
            Err(CoreError::Eof) => break,
            Err(e) => panic!("receive_frame after flush: {e}"),
        }
    }
    (stream, packets, frames)
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

impl SharedBuf {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().get_ref().clone()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn mux(stream: &StreamInfo, packets: &[Packet]) -> Vec<u8> {
    let out = SharedBuf::default();
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(out.clone());
    let mut mux = container::open_muxer(sink, std::slice::from_ref(stream)).expect("open_muxer");
    mux.write_header().unwrap();
    for p in packets {
        mux.write_packet(p).unwrap();
    }
    mux.write_trailer().unwrap();
    out.bytes()
}

fn linear_rgba_fixture() -> Vec<u8> {
    let img = QoiImage::from_rgba8(3, 2, (0..24u8).collect()).unwrap();
    encode(
        &img.with_color(ColorInfo::linear()),
        &EncodeOptions::default(),
    )
    .unwrap()
}

/// The reference fixtures are all `channels = 4`; an `Rgb24` file from
/// the crate's own encoder completes the layout matrix.
fn rgb_fixture() -> Vec<u8> {
    let img = QoiImage::from_rgb8(5, 3, (0..45u8).map(|i| i.wrapping_mul(11)).collect()).unwrap();
    encode(&img, &EncodeOptions::default()).unwrap()
}

// ---- acceptance 1: probe ------------------------------------------------

#[test]
fn probe_names_qoi_from_magic_alone_and_with_hint_and_rejects_foreign_files() {
    let ctx = ctx();
    for bytes in [TESTCARD, TESTCARD_RGBA] {
        for hint in [None, Some("qoi")] {
            let mut cur = Cursor::new(bytes.to_vec());
            let name = ctx
                .containers
                .probe_input(&mut cur as &mut dyn oxideav_core::ReadSeek, hint)
                .unwrap();
            assert_eq!(name, "qoi");
        }
    }
    for foreign in [
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
        b"farbfeld\0\0\0\x01\0\0\0\x01".to_vec(),
        b"qoiX\0\0\0\x01\0\0\0\x01\x03\x00".to_vec(),
    ] {
        let mut cur = Cursor::new(foreign.clone());
        assert!(ctx
            .containers
            .probe_input(&mut cur as &mut dyn oxideav_core::ReadSeek, None)
            .is_err());
        let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(foreign));
        assert!(container::open_demuxer(reader, &ctx.codecs).is_err());
    }
}

// ---- acceptance 2 + 3: layout and pixels byte-exact vs Layer 1 -----------

#[test]
fn stream_declares_info_and_frames_match_layer1_decode_byte_for_byte() {
    let ctx = ctx();
    let linear = linear_rgba_fixture();
    let rgb = rgb_fixture();
    let fixtures: [(&str, &[u8]); 6] = [
        ("testcard", TESTCARD),
        ("testcard rgba", TESTCARD_RGBA),
        ("edgecase", EDGECASE),
        ("logo", LOGO),
        ("rgb24", &rgb),
        ("linear colourspace", &linear),
    ];
    let mut seen_formats = Vec::new();
    let mut seen_transfers = Vec::new();
    for (name, bytes) in fixtures {
        let header = info(bytes).unwrap();
        let expect = decode(bytes).unwrap();
        let (stream, packets, frames) = pump(&ctx, bytes);

        let p = &stream.params;
        assert_eq!(p.codec_id, CodecId::new(CODEC_ID_STR), "{name}");
        assert_eq!(p.width, Some(header.width), "{name}");
        assert_eq!(p.height, Some(header.height), "{name}");
        let want_fmt: PixelFormat = header.format.into();
        assert_eq!(p.pixel_format, Some(want_fmt), "{name}");
        assert_eq!(p.pixel_format, Some(expect.format.into()), "{name}");
        // The colorspace byte is format-defined colour: always stamped,
        // equal to what Layer 1 reports.
        assert_eq!(p.color_signal, to_color_signal(&header.color), "{name}");
        assert!(!p.color_signal.is_unspecified(), "{name}");
        assert_eq!(p.color_signal.transfer.0, header.color.transfer, "{name}");
        assert_eq!(stream.time_base, TimeBase::new(1, 1));
        assert!(p.extradata.is_empty());

        assert_eq!(packets.len(), 1, "{name}");
        assert_eq!(packets[0].data, bytes, "{name}: the whole file");
        assert_eq!(packets[0].pts, Some(0));
        assert!(packets[0].flags.keyframe);

        assert_eq!(frames.len(), 1, "{name}");
        let v = &frames[0];
        assert_eq!(v.image_planes().len(), 1);
        assert_eq!(v.planes[0].stride, expect.stride(), "{name}");
        assert_eq!(v.planes[0].data, expect.as_bytes().unwrap(), "{name}");
        assert_eq!(
            v.color_signal(),
            Some(p.color_signal),
            "{name}: frame colour"
        );
        assert_eq!(v.pts, Some(0));
        let back = QoiImage::from_video_frame(v, p).unwrap();
        assert_eq!(back, expect, "{name}: bridge rebuilds the Layer 1 image");
        seen_formats.push(want_fmt);
        seen_transfers.push(header.color.transfer);
    }
    assert!(seen_formats.contains(&PixelFormat::Rgb24));
    assert!(seen_formats.contains(&PixelFormat::Rgba));
    // Both colourspace bytes were pinned: sRGB (13) and linear (8).
    assert!(seen_transfers.contains(&ColorInfo::srgb().transfer));
    assert!(seen_transfers.contains(&ColorInfo::linear().transfer));
    assert_ne!(ColorInfo::srgb().transfer, ColorInfo::linear().transfer);
}

// ---- acceptance 5: muxer ---------------------------------------------------

fn encoder_packet(
    ctx: &RuntimeContext,
    img: &QoiImage,
    options: &[(&str, &str)],
) -> (StreamInfo, Packet) {
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(img.width);
    params.height = Some(img.height);
    params.pixel_format = Some(img.format.into());
    for (k, v) in options {
        params.options.insert(*k, *v);
    }
    let mut enc = ctx.codecs.first_encoder(&params).expect("first_encoder");
    assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMore)));
    let mut vf: VideoFrame = img.into();
    vf.pts = Some(0);
    enc.send_frame(&CoreFrame::Video(vf)).unwrap();
    let pkt = enc.receive_packet().unwrap();
    assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMore)));
    enc.flush().unwrap();
    assert!(matches!(enc.receive_packet(), Err(CoreError::Eof)));
    let stream = StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, 1),
        duration: None,
        start_time: Some(0),
        params: enc.output_params().clone(),
    };
    (stream, pkt)
}

#[test]
fn encoder_to_muxer_to_demuxer_to_decoder_round_trips_both_layouts() {
    let ctx = ctx();
    let rgb = QoiImage::from_rgb8(4, 3, (0..36u8).map(|i| i.wrapping_mul(7)).collect()).unwrap();
    let rgba = QoiImage::from_rgba8(2, 5, (0..40u8).map(|i| 255 - i).collect()).unwrap();
    for (name, img, opts) in [
        ("rgb", &rgb, &[][..]),
        ("rgba", &rgba, &[][..]),
        ("rgba linear", &rgba, &[("colorspace", "linear")][..]),
    ] {
        let (stream, pkt) = encoder_packet(&ctx, img, opts);
        let file = mux(&stream, std::slice::from_ref(&pkt));
        assert_eq!(file, pkt.data, "{name}: the packet is written verbatim");
        let back = decode(&file).unwrap();
        assert_eq!(back.planes, img.planes, "{name}");
        assert_eq!(back.format, img.format, "{name}");
        let header = info(&file).unwrap();
        if opts.is_empty() {
            assert_eq!(header.color, ColorInfo::srgb(), "{name}");
        } else {
            assert_eq!(header.color, ColorInfo::linear(), "{name}");
        }
        // Through the registry again: identical planes and colour.
        let (s, _, frames) = pump(&ctx, &file);
        assert_eq!(s.params.pixel_format, Some(img.format.into()), "{name}");
        assert_eq!(s.params.color_signal, to_color_signal(&header.color));
        assert_eq!(frames[0].planes[0].data, img.as_bytes().unwrap(), "{name}");
    }
}

#[test]
fn muxer_rejects_wrong_streams_second_packets_and_non_qoi_payloads() {
    let ctx = ctx();
    let img = QoiImage::from_rgb8(1, 1, vec![1, 2, 3]).unwrap();
    let (stream, pkt) = encoder_packet(&ctx, &img, &[]);

    let mut wrong = stream.clone();
    wrong.params.codec_id = CodecId::new("png");
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    assert!(container::open_muxer(sink, std::slice::from_ref(&wrong)).is_err());
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    assert!(container::open_muxer(sink, &[stream.clone(), stream.clone()]).is_err());

    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    let mut m = container::open_muxer(sink, std::slice::from_ref(&stream)).unwrap();
    assert!(m.write_packet(&pkt).is_err(), "write_header not called");
    m.write_header().unwrap();
    assert!(m.write_trailer().is_err(), "no packet written");
    assert!(m
        .write_packet(&Packet::new(0, TimeBase::new(1, 1), b"not a qoi".to_vec()))
        .is_err());
    m.write_packet(&pkt).unwrap();
    // A second image: QOI holds one, said the same way Layer 1 would.
    assert!(matches!(
        m.write_packet(&pkt),
        Err(CoreError::Unsupported(_))
    ));
    m.write_trailer().unwrap();
}

// ---- acceptance 6: register installs codec AND container ------------------

#[test]
fn register_installs_codec_and_container() {
    let mut ctx = RuntimeContext::new();
    oxideav_qoi::__oxideav_entry(&mut ctx);
    let id = CodecId::new(CODEC_ID_STR);
    assert!(ctx.codecs.has_decoder(&id));
    assert!(ctx.codecs.has_encoder(&id));
    assert!(ctx.containers.demuxer_names().any(|n| n == "qoi"));
    assert!(ctx.containers.muxer_names().any(|n| n == "qoi"));
    assert_eq!(ctx.containers.container_for_extension("qoi"), Some("qoi"));
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(TESTCARD.to_vec()));
    let d = ctx
        .containers
        .open_demuxer("qoi", reader, &ctx.codecs)
        .unwrap();
    assert_eq!(d.format_name(), "qoi");
    assert_eq!(d.metadata(), &[]);
}

// ---- acceptance 7: hostile input never panics ------------------------------

#[test]
fn hostile_inputs_fail_cleanly() {
    let ctx = ctx();
    let cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"qoif".to_vec(),
        b"qoif\0\0\0\x01\0\0\0".to_vec(), // truncated header
        b"qoif\0\0\0\0\0\0\0\x01\x03\x00".to_vec(), // zero width
        b"qoif\0\0\0\x01\0\0\0\x01\x05\x00".to_vec(), // channels 5
        b"qoif\0\0\0\x01\0\0\0\x01\x03\x02".to_vec(), // colorspace 2
        b"qoif\xff\xff\xff\xff\xff\xff\xff\xff\x04\x00".to_vec(), // absurd dims, no body
        TESTCARD[..TESTCARD.len() / 2].to_vec(), // truncated body
    ];
    for bytes in &cases {
        let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(bytes.clone()));
        if let Ok(mut d) = container::open_demuxer(reader, &ctx.codecs) {
            // The header parsed; the demuxer must not have allocated the
            // picture, and the decoder must fail cleanly.
            let params = d.streams()[0].params.clone();
            let mut dec = ctx.codecs.first_decoder(&params).unwrap();
            while let Ok(p) = d.next_packet() {
                let _ = dec.send_packet(&p);
                while dec.receive_frame().is_ok() {}
            }
        }
    }
    // The absurd header opens (header-only) and the stream says so.
    let absurd = b"qoif\xff\xff\xff\xff\xff\xff\xff\xff\x04\x00".to_vec();
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(absurd));
    let d = container::open_demuxer(reader, &ctx.codecs).unwrap();
    assert_eq!(d.streams()[0].params.width, Some(u32::MAX));
    // Zero-length packet into the decoder.
    let mut dec = ctx
        .codecs
        .first_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR)))
        .unwrap();
    assert!(dec
        .send_packet(&Packet::new(0, TimeBase::new(1, 1), Vec::new()))
        .is_err());
    assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
    let _ = ColorSignal::unspecified();
}
