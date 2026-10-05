//! QOI container: one single-image file becomes one [`Packet`] on
//! stream `0`, and one packet becomes one file. The same shape as the
//! other single-image containers (`oxideav-farbfeld`, `oxideav-pbm`,
//! `oxideav-bmp`) — QOI has no animation, pages or metadata to carry.
//!
//! The demuxer reads the 14-byte header only ([`crate::info`]) and
//! declares exactly what Layer 1 reports: `width` / `height`, the
//! native `pixel_format` (`Rgb24` for `channels = 3`, `Rgba` for
//! `channels = 4`) and the colour signal the `colorspace` byte defines
//! (`0` → sRGB transfer, `1` → linear; both with BT.709 primaries, the
//! crate's documented convention) — stamped on the stream because the
//! format always signals it. The pixels are decoded by the registered
//! `qoi` decoder from the one packet, which is the whole file.
//!
//! The muxer accepts exactly one packet from the `qoi` encoder (already
//! a complete file) and writes it through; a second packet is
//! [`Error::Unsupported`] — QOI holds one image.
//!
//! Lives behind the `registry` feature: the container types are all
//! defined by `oxideav-core`.

use std::io::{Read, SeekFrom, Write};

use oxideav_core::{
    CodecId, CodecParameters, CodecResolver, ContainerRegistry, Demuxer, Error, MediaType, Muxer,
    Packet, ProbeData, ProbeScore, ReadSeek, Result, StreamInfo, TimeBase, WriteSeek,
    MAX_PROBE_SCORE, PROBE_SCORE_EXTENSION,
};

use crate::registry::{to_color_signal, to_core_pixel_format};

/// Container name registered for QOI (demuxer, muxer, probe, extension).
pub const CONTAINER_NAME: &str = "qoi";

/// Register the QOI container: demuxer + muxer + `.qoi` extension + probe.
pub fn register(reg: &mut ContainerRegistry) {
    reg.register_demuxer(CONTAINER_NAME, open_demuxer);
    reg.register_muxer(CONTAINER_NAME, open_muxer);
    reg.register_extension("qoi", CONTAINER_NAME);
    reg.register_probe(CONTAINER_NAME, probe);
}

/// Content probe: the `qoif` magic is unambiguous; the `.qoi` extension
/// alone scores [`PROBE_SCORE_EXTENSION`].
pub fn probe(data: &ProbeData) -> ProbeScore {
    if crate::probe(data.buf) {
        return MAX_PROBE_SCORE;
    }
    if data.ext == Some("qoi") {
        PROBE_SCORE_EXTENSION
    } else {
        0
    }
}

/// Open a QOI file as a one-stream, one-packet container.
pub fn open_demuxer(
    mut input: Box<dyn ReadSeek>,
    _codecs: &dyn CodecResolver,
) -> Result<Box<dyn Demuxer>> {
    input.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    input.read_to_end(&mut buf)?;
    drop(input);

    if !crate::probe(&buf) {
        return Err(Error::invalid("QOI: bad magic (expected `qoif`)"));
    }
    // Header only — the same accept / reject verdict as `info`, no pixel
    // buffer is allocated here.
    let header = crate::info(&buf)?;
    let mut params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
    params.width = Some(header.width);
    params.height = Some(header.height);
    params.pixel_format = Some(to_core_pixel_format(header.format));
    // The colorspace byte is format-defined colour information: always
    // stamped (IMAGE_CRATE_API colour-signal ruling).
    params.color_signal = to_color_signal(&header.color);

    let mut pkt = Packet::new(0, TimeBase::new(1, 1), buf);
    pkt.pts = Some(0);
    pkt.dts = Some(0);
    pkt.flags.keyframe = true;

    let stream = StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, 1),
        duration: None,
        start_time: Some(0),
        params,
    };
    Ok(Box::new(QoiDemuxer {
        stream,
        packet: Some(pkt),
    }))
}

struct QoiDemuxer {
    stream: StreamInfo,
    packet: Option<Packet>,
}

impl Demuxer for QoiDemuxer {
    fn format_name(&self) -> &str {
        CONTAINER_NAME
    }

    fn streams(&self) -> &[StreamInfo] {
        std::slice::from_ref(&self.stream)
    }

    fn next_packet(&mut self) -> Result<Packet> {
        self.packet.take().ok_or(Error::Eof)
    }
}

/// Open a QOI muxer for exactly one `qoi` video stream.
pub fn open_muxer(output: Box<dyn WriteSeek>, streams: &[StreamInfo]) -> Result<Box<dyn Muxer>> {
    if streams.len() != 1 {
        return Err(Error::invalid(
            "QOI muxer: exactly one video stream expected",
        ));
    }
    let s = &streams[0];
    if s.params.media_type != MediaType::Video {
        return Err(Error::invalid("QOI muxer: stream must be video"));
    }
    if s.params.codec_id.as_str() != crate::CODEC_ID_STR {
        return Err(Error::invalid(format!(
            "QOI muxer: codec_id must be qoi (got {})",
            s.params.codec_id
        )));
    }
    Ok(Box::new(QoiMuxer {
        output,
        header_written: false,
        packets: 0,
    }))
}

struct QoiMuxer {
    output: Box<dyn WriteSeek>,
    header_written: bool,
    packets: usize,
}

impl Muxer for QoiMuxer {
    fn format_name(&self) -> &str {
        CONTAINER_NAME
    }

    fn write_header(&mut self) -> Result<()> {
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::other("QOI muxer: write_header not called"));
        }
        if self.packets > 0 {
            return Err(Error::unsupported(
                "QOI muxer: a QOI file holds exactly one image (second packet refused)",
            ));
        }
        if !crate::probe(&packet.data) {
            return Err(Error::invalid(
                "QOI muxer: packet is not a QOI file (bad magic)",
            ));
        }
        // The encoder produces a complete file in a single packet.
        self.output.write_all(&packet.data)?;
        self.packets = 1;
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if self.packets == 0 {
            return Err(Error::invalid("QOI muxer: no packet written"));
        }
        self.output.flush()?;
        Ok(())
    }
}
