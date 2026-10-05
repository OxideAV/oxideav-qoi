#![no_main]

//! Framework-path fuzz harness: the bytes are a file handed to the QOI
//! container demuxer, its packet goes through the registered `qoi`
//! decoder, and the packet is muxed back.
//!
//! Contract: every call returns to its caller. A `panic!`, slice OOB,
//! integer overflow in debug, or OOM abort is a finding; `Err` on any
//! input is fine. The decoder is held to a small pixel budget through
//! `DecoderLimits` so a legal-but-huge header is refused, not allocated.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{DecoderLimits, Error, RuntimeContext};
use oxideav_qoi::container;

const MAX_PIXELS: u64 = 1 << 20; // 1 Mpx budget per frame

fuzz_target!(|data: &[u8]| {
    let mut ctx = RuntimeContext::new();
    oxideav_qoi::register(&mut ctx);

    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(data.to_vec()));
    let Ok(mut demux) = container::open_demuxer(reader, &ctx.codecs) else {
        return;
    };
    let stream = demux.streams()[0].clone();
    let _ = demux.metadata();

    let mut params = stream.params.clone();
    let mut limits = DecoderLimits::default();
    limits.max_pixels_per_frame = MAX_PIXELS;
    limits.max_alloc_bytes_per_frame = MAX_PIXELS * 4;
    params.limits = limits;
    // The header already passed; refuse the pixel buffer when the
    // declared geometry is over the budget (the decoder decodes with the
    // crate's default 1 GiB cap).
    let px = u64::from(params.width.unwrap_or(0)) * u64::from(params.height.unwrap_or(0));
    if px > MAX_PIXELS {
        return;
    }
    let Ok(mut dec) = ctx.codecs.first_decoder(&params) else {
        return;
    };

    let mut packets = Vec::new();
    while let Ok(pkt) = demux.next_packet() {
        let _ = dec.send_packet(&pkt);
        loop {
            match dec.receive_frame() {
                Ok(_) => {}
                Err(Error::NeedMore) | Err(Error::Eof) => break,
                Err(_) => break,
            }
        }
        packets.push(pkt);
    }
    let _ = dec.flush();
    while dec.receive_frame().is_ok() {}

    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    if let Ok(mut mux) = container::open_muxer(sink, std::slice::from_ref(&stream)) {
        let _ = mux.write_header();
        for p in &packets {
            let _ = mux.write_packet(p);
        }
        let _ = mux.write_trailer();
    }
});
