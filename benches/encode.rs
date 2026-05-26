//! Criterion benchmarks for the QOI encoder hot path.
//!
//! Round 156 (depth-mode benchmarks): companion to `decode.rs`. The
//! encoder picks among QOI_OP_RUN / INDEX / DIFF / LUMA / RGB / RGBA
//! per pixel using the spec-mandated priority; each scenario below
//! exercises a different op-mix so future optimisation rounds can
//! A/B-test changes against the round-156 baseline.
//!
//! Scenarios:
//!
//!   - **encode_gradient_rgba_320x240**: 320×240 RGBA gradient with
//!     light xorshift noise — the natural-image baseline. Exercises
//!     DIFF / LUMA / RGB picker fall-through.
//!   - **encode_gradient_rgb24_640x480**: 640×480 RGB gradient — the
//!     larger VGA case in the alpha-unchanged path.
//!   - **encode_solid_rgba_512x512**: 512×512 single-colour RGBA fill
//!     — the QOI_OP_RUN hot path; encoder spends nearly all its time
//!     in the run-extension branch.
//!   - **encode_alpha_changing_rgba_320x240**: 320×240 RGBA where every
//!     pixel changes alpha — forces almost-every-pixel RGBA, the
//!     slowest encode case (no compression).
//!   - **encode_index_friendly_rgba_320x240**: 320×240 RGBA cycling
//!     through 8 distinct colours — encoder picks INDEX for the bulk
//!     of pixels after the first lap.
//!
//! Run with:
//!     cargo bench -p oxideav-qoi --bench encode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_qoi::encode_qoi;

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
            data[idx + 3] = xorshift_byte(&mut state);
        }
    }
    data
}

fn build_index_friendly_rgba(width: u32, height: u32) -> Vec<u8> {
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

fn bench_encode_gradient_rgba_320x240(c: &mut Criterion) {
    let pixels = build_gradient_rgba(320, 240);
    let mut g = c.benchmark_group("encode_gradient_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| encode_qoi(320, 240, 4, criterion::black_box(&pixels)));
    });
    g.finish();
}

fn bench_encode_gradient_rgb24_640x480(c: &mut Criterion) {
    let pixels = build_gradient_rgb24(640, 480);
    let mut g = c.benchmark_group("encode_gradient_rgb24_640x480");
    g.throughput(Throughput::Bytes((640 * 480 * 3) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb24/640x480"), |b| {
        b.iter(|| encode_qoi(640, 480, 3, criterion::black_box(&pixels)));
    });
    g.finish();
}

fn bench_encode_solid_rgba_512x512(c: &mut Criterion) {
    let pixels = build_solid_rgba(512, 512);
    let mut g = c.benchmark_group("encode_solid_rgba_512x512");
    g.throughput(Throughput::Bytes((512 * 512 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/512x512"), |b| {
        b.iter(|| encode_qoi(512, 512, 4, criterion::black_box(&pixels)));
    });
    g.finish();
}

fn bench_encode_alpha_changing_rgba_320x240(c: &mut Criterion) {
    let pixels = build_alpha_changing_rgba(320, 240);
    let mut g = c.benchmark_group("encode_alpha_changing_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| encode_qoi(320, 240, 4, criterion::black_box(&pixels)));
    });
    g.finish();
}

fn bench_encode_index_friendly_rgba_320x240(c: &mut Criterion) {
    let pixels = build_index_friendly_rgba(320, 240);
    let mut g = c.benchmark_group("encode_index_friendly_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| encode_qoi(320, 240, 4, criterion::black_box(&pixels)));
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_encode_gradient_rgba_320x240,
    bench_encode_gradient_rgb24_640x480,
    bench_encode_solid_rgba_512x512,
    bench_encode_alpha_changing_rgba_320x240,
    bench_encode_index_friendly_rgba_320x240,
);
criterion_main!(benches);
