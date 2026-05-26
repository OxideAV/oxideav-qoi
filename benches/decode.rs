//! Criterion benchmarks for the QOI decoder hot path.
//!
//! Round 156 (depth-mode benchmarks): oxideav-qoi covers the full
//! one-page qoiformat.org spec, byte-exact roundtrips every reference
//! fixture, and has a daily decode-only fuzz harness. Per the
//! workspace "saturated → fuzz / bench / profile" memo this round
//! wires up criterion benches mirroring the bmp / png / cinepak / tta
//! / flac shape so future optimisation rounds can A/B-test changes to
//! the decoder hot path.
//!
//! This file covers the **decoder**; sibling files cover `encode` and
//! `roundtrip`.
//!
//! Each scenario is self-contained: the bench encodes a fresh QOI on
//! the fly with the public encoder API and then iterates `parse_qoi`
//! on the encoded bytes. No fixture files are committed.
//!
//!   - **decode_gradient_rgba_320x240**: 320×240 RGBA gradient with a
//!     touch of xorshift noise — the natural-image baseline that
//!     exercises DIFF / LUMA / RGB / INDEX in mixed proportions.
//!   - **decode_gradient_rgb24_640x480**: 640×480 RGB gradient — a
//!     larger VGA case in the alpha-unchanged path (no RGBA chunks).
//!   - **decode_solid_rgba_512x512**: 512×512 single-colour RGBA fill —
//!     stresses the QOI_OP_RUN flush-every-62-pixels path; nearly all
//!     chunks should be RUNs.
//!   - **decode_alpha_changing_rgba_320x240**: 320×240 RGBA where
//!     alpha changes per pixel — forces almost-every-pixel QOI_OP_RGBA,
//!     the slow path with no compression.
//!   - **decode_index_friendly_rgba_320x240**: 320×240 RGBA cycling
//!     through a small palette — exercises the INDEX-chunk hot path
//!     where the 64-entry running array sees repeated hits.
//!
//! Run with:
//!     cargo bench -p oxideav-qoi --bench decode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_qoi::{encode_qoi, parse_qoi};

/// Cheap deterministic xorshift32 — synthesises "natural-ish" per-pixel
/// values so the bench inputs aren't trivially compressible / branch-
/// predictable.
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

fn build_gradient_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut state: u32 = 0x1234_5678;
    for r in 0..height as usize {
        for c in 0..width as usize {
            let base_y = ((r * 255) / (height as usize).max(1)) as u32;
            let base_x = ((c * 255) / (width as usize).max(1)) as u32;
            let idx = (r * width as usize + c) * 4;
            data[idx] = (((base_x + base_y) / 2).min(255)) as u8;
            data[idx + 1] = base_y.min(255) as u8;
            data[idx + 2] = base_x.min(255) as u8;
            data[idx + 3] = 0xff;
            // Stir in some xorshift so DIFF / LUMA don't trivially
            // collapse the whole row.
            data[idx] = data[idx].wrapping_add(xorshift_byte(&mut state) & 0x07);
        }
    }
    data
}

fn build_gradient_rgb24(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0u8; (width as usize) * (height as usize) * 3];
    let mut state: u32 = 0x2345_6789;
    for r in 0..height as usize {
        for c in 0..width as usize {
            let base_y = ((r * 255) / (height as usize).max(1)) as u32;
            let base_x = ((c * 255) / (width as usize).max(1)) as u32;
            let idx = (r * width as usize + c) * 3;
            data[idx] = (((base_x + base_y) / 2).min(255)) as u8;
            data[idx + 1] = base_y.min(255) as u8;
            data[idx + 2] = base_x.min(255) as u8;
            data[idx] = data[idx].wrapping_add(xorshift_byte(&mut state) & 0x07);
        }
    }
    data
}

fn build_solid_rgba(width: u32, height: u32) -> Vec<u8> {
    // (200, 50, 25, 255) — non-zero pixel so the encoder's first chunk
    // is a LUMA from (0,0,0,255), then the remaining (w*h - 1) pixels
    // collapse into ceil((w*h-1) / 62) RUN chunks. Mirrors the
    // `solid_color_uses_runs` unit test pattern at much larger scale.
    [200u8, 50, 25, 255].repeat((width as usize) * (height as usize))
}

fn build_alpha_changing_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut state: u32 = 0x3456_789a;
    for r in 0..height as usize {
        for c in 0..width as usize {
            let idx = (r * width as usize + c) * 4;
            data[idx] = xorshift_byte(&mut state);
            data[idx + 1] = xorshift_byte(&mut state);
            data[idx + 2] = xorshift_byte(&mut state);
            // Force a unique alpha per pixel so the encoder cannot reuse
            // INDEX hits and is forced down the RGBA path.
            data[idx + 3] = xorshift_byte(&mut state);
        }
    }
    data
}

fn build_index_friendly_rgba(width: u32, height: u32) -> Vec<u8> {
    // Cycle through a deterministic 8-colour palette. The running pixel
    // array has 64 slots so 8 distinct colours land in 8 different
    // slots, and the encoder picks QOI_OP_INDEX for repeat hits.
    let palette: [[u8; 4]; 8] = [
        [200, 50, 25, 255],
        [25, 200, 50, 255],
        [50, 25, 200, 255],
        [200, 200, 25, 255],
        [25, 200, 200, 255],
        [200, 25, 200, 255],
        [128, 128, 128, 255],
        [10, 10, 10, 255],
    ];
    let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
    for r in 0..height as usize {
        for c in 0..width as usize {
            let p = palette[(r * 3 + c) % 8];
            let idx = (r * width as usize + c) * 4;
            data[idx..idx + 4].copy_from_slice(&p);
        }
    }
    data
}

fn bench_decode_gradient_rgba_320x240(c: &mut Criterion) {
    let pixels = build_gradient_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    let mut g = c.benchmark_group("decode_gradient_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| parse_qoi(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_gradient_rgb24_640x480(c: &mut Criterion) {
    let pixels = build_gradient_rgb24(640, 480);
    let bytes = encode_qoi(640, 480, 3, &pixels);
    let mut g = c.benchmark_group("decode_gradient_rgb24_640x480");
    g.throughput(Throughput::Bytes((640 * 480 * 3) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb24/640x480"), |b| {
        b.iter(|| parse_qoi(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_solid_rgba_512x512(c: &mut Criterion) {
    let pixels = build_solid_rgba(512, 512);
    let bytes = encode_qoi(512, 512, 4, &pixels);
    let mut g = c.benchmark_group("decode_solid_rgba_512x512");
    g.throughput(Throughput::Bytes((512 * 512 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/512x512"), |b| {
        b.iter(|| parse_qoi(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_alpha_changing_rgba_320x240(c: &mut Criterion) {
    let pixels = build_alpha_changing_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    let mut g = c.benchmark_group("decode_alpha_changing_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| parse_qoi(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_index_friendly_rgba_320x240(c: &mut Criterion) {
    let pixels = build_index_friendly_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    let mut g = c.benchmark_group("decode_index_friendly_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| parse_qoi(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_decode_gradient_rgba_320x240,
    bench_decode_gradient_rgb24_640x480,
    bench_decode_solid_rgba_512x512,
    bench_decode_alpha_changing_rgba_320x240,
    bench_decode_index_friendly_rgba_320x240,
);
criterion_main!(benches);
