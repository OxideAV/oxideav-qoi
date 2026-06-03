//! Criterion benchmarks for the round-225 buffer-reuse `_into` API
//! surface.
//!
//! These benches measure the headline benefit of `encode_qoi_into` /
//! `parse_qoi_into` over the allocating `encode_qoi` / `parse_qoi`
//! wrappers: a tight loop that encodes (or decodes) many same-sized
//! images one after another should reuse the same output buffer
//! across calls, so the allocator only runs once (on the first
//! iteration that needs the worst-case capacity). Every subsequent
//! iteration is a pure length-update + memcpy.
//!
//! Each scenario runs the same 4×4 RGBA encode/decode pair `N`
//! times per criterion iteration so the per-call allocator cost has
//! a chance to dominate over the bench-harness overhead. A tiny
//! image is chosen on purpose: the encoder's worst-case allocation
//! is `14 + n*5 + 8` = 98 bytes for 4×4×RGBA, so the relative cost
//! of the `Vec::with_capacity` call in the allocating wrapper is at
//! its highest — which is the exact use case the reuse API is
//! designed for (image servers, thumbnail batches, encode-loop
//! converters).
//!
//! Run with:
//!     cargo bench -p oxideav-qoi --bench reuse

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_qoi::{encode_qoi, encode_qoi_into, parse_qoi, parse_qoi_into};

/// Number of encode (or decode) calls inside a single bench
/// iteration. Larger N amortises the per-iter criterion overhead
/// across more codec calls so the per-call allocation cost is
/// easier to see in the reported throughput.
const CALLS_PER_ITER: usize = 256;

/// Width / height of the benchmark image. 64×64 RGBA is small
/// enough to keep the bench fast (the four scenarios collectively
/// finish in well under a minute) but large enough that the
/// encoder's worst-case allocation — `14 + 64*64*5 + 8 = 20502`
/// bytes — actually costs the allocator something to satisfy. At
/// the original 4×4 size the worst-case allocation is only 98
/// bytes, well inside the small-block allocator's free-list fast
/// path; the per-iter criterion overhead dominates and the
/// alloc-vs-reuse delta drowns in noise.
const BENCH_DIM: u32 = 64;

fn build_small_rgba(width: u32, height: u32) -> Vec<u8> {
    // A deterministic small checker pattern. Exercises the encoder's
    // chunk priority chain (LUMA / RGB / RGBA / INDEX) rather than
    // a degenerate solid-fill that would short-circuit through pure
    // RUN chunks.
    let palette: [[u8; 4]; 4] = [
        [200, 50, 25, 255],
        [25, 200, 50, 255],
        [50, 25, 200, 255],
        [200, 200, 25, 200],
    ];
    let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
    for r in 0..height as usize {
        for c in 0..width as usize {
            let q = ((c & 1) + 2 * (r & 1)) % 4;
            let idx = (r * width as usize + c) * 4;
            data[idx..idx + 4].copy_from_slice(&palette[q]);
        }
    }
    data
}

fn bench_encode_alloc_per_call(c: &mut Criterion) {
    let pixels = build_small_rgba(BENCH_DIM, BENCH_DIM);
    let bytes_per_call = (BENCH_DIM * BENCH_DIM * 4) as usize;
    let mut g = c.benchmark_group("encode_alloc_per_call_64x64_x256");
    g.throughput(Throughput::Bytes((bytes_per_call * CALLS_PER_ITER) as u64));
    g.bench_function(BenchmarkId::from_parameter("alloc_per_call"), |b| {
        b.iter(|| {
            // `encode_qoi` allocates a fresh `Vec<u8>` on every
            // call — the per-iteration `Vec::with_capacity` +
            // `Vec::truncate` work that the `_into` variant
            // amortises.
            for _ in 0..CALLS_PER_ITER {
                let bytes = encode_qoi(BENCH_DIM, BENCH_DIM, 4, criterion::black_box(&pixels));
                criterion::black_box(bytes);
            }
        });
    });
    g.finish();
}

fn bench_encode_reused_buffer(c: &mut Criterion) {
    let pixels = build_small_rgba(BENCH_DIM, BENCH_DIM);
    let bytes_per_call = (BENCH_DIM * BENCH_DIM * 4) as usize;
    let mut g = c.benchmark_group("encode_reused_buffer_64x64_x256");
    g.throughput(Throughput::Bytes((bytes_per_call * CALLS_PER_ITER) as u64));
    g.bench_function(BenchmarkId::from_parameter("reuse"), |b| {
        b.iter(|| {
            // One pre-allocated `Vec<u8>` shared across all 256
            // calls. After the first call grows the capacity to the
            // worst case, every subsequent call is a `resize(...,
            // 0)` (pure length update) + encode + `truncate`.
            let mut buf: Vec<u8> = Vec::new();
            for _ in 0..CALLS_PER_ITER {
                encode_qoi_into(
                    &mut buf,
                    BENCH_DIM,
                    BENCH_DIM,
                    4,
                    criterion::black_box(&pixels),
                );
                criterion::black_box(&buf);
            }
        });
    });
    g.finish();
}

fn bench_decode_alloc_per_call(c: &mut Criterion) {
    let pixels = build_small_rgba(BENCH_DIM, BENCH_DIM);
    let bytes = encode_qoi(BENCH_DIM, BENCH_DIM, 4, &pixels);
    let bytes_per_call = (BENCH_DIM * BENCH_DIM * 4) as usize;
    let mut g = c.benchmark_group("decode_alloc_per_call_64x64_x256");
    g.throughput(Throughput::Bytes((bytes_per_call * CALLS_PER_ITER) as u64));
    g.bench_function(BenchmarkId::from_parameter("alloc_per_call"), |b| {
        b.iter(|| {
            // `parse_qoi` allocates a fresh `Vec<u8>` per call for
            // the pixel buffer. This bench captures the cost the
            // `_into` variant erases.
            for _ in 0..CALLS_PER_ITER {
                let img = parse_qoi(criterion::black_box(&bytes)).expect("decode");
                criterion::black_box(img);
            }
        });
    });
    g.finish();
}

fn bench_decode_reused_buffer(c: &mut Criterion) {
    let pixels = build_small_rgba(BENCH_DIM, BENCH_DIM);
    let bytes = encode_qoi(BENCH_DIM, BENCH_DIM, 4, &pixels);
    let bytes_per_call = (BENCH_DIM * BENCH_DIM * 4) as usize;
    let mut g = c.benchmark_group("decode_reused_buffer_64x64_x256");
    g.throughput(Throughput::Bytes((bytes_per_call * CALLS_PER_ITER) as u64));
    g.bench_function(BenchmarkId::from_parameter("reuse"), |b| {
        b.iter(|| {
            let mut pix_buf: Vec<u8> = Vec::new();
            for _ in 0..CALLS_PER_ITER {
                let _ = parse_qoi_into(criterion::black_box(&bytes), &mut pix_buf)
                    .expect("decode into");
                criterion::black_box(&pix_buf);
            }
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_encode_alloc_per_call,
    bench_encode_reused_buffer,
    bench_decode_alloc_per_call,
    bench_decode_reused_buffer,
);
criterion_main!(benches);
