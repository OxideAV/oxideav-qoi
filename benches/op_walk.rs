//! Criterion benchmarks for the streaming chunk-walk decode path
//! (`iter_ops` / `iter_ops_strict`).
//!
//! Round 318 (depth-mode benchmarks). The `decode` / `encode` /
//! `roundtrip` / `reuse` benches all exercise the pixel-materialising
//! `parse_qoi` path (header → chunk dispatch → write decoded pixels
//! into an output buffer + maintain the 64-slot running array + `prev`
//! pixel). `iter_ops` is a *separate* decode entry point added later:
//! it walks the chunk stream into typed `QoiOp` values **without**
//! materialising a pixel buffer, maintaining the running array, or
//! tracking `prev` — just per-byte tag dispatch and bounds checks. It
//! has fuzz coverage (`op_iter`) and contract tests but no benchmark,
//! so the cost of the bare op-dispatch loop (vs. full decode) was not
//! measurable. This file closes that gap, mirroring the `decode`
//! bench's five image shapes byte-for-byte so an `op_walk` row lines
//! up against the matching `decode` row.
//!
//! Two harness flavours per shape:
//!   - **iter_ops** — the lazy iterator. The closure folds each op's
//!     `tag()` byte into an accumulator so the optimiser cannot elide
//!     the walk; nothing is allocated per op.
//!   - **iter_ops_strict** — the eager variant that materialises a
//!     `Vec<QoiOp>` (and folds truncation into an `Err`). Isolates the
//!     per-op `Vec` push / growth cost against the allocation-free
//!     lazy walk on identical input.
//!
//! Each scenario synthesises a fresh QOI on the fly with the public
//! encoder and walks the encoded bytes. No fixture files are committed.
//!
//!   - **op_walk_gradient_rgba_320x240**: mixed DIFF / LUMA / RGB /
//!     INDEX op stream — the natural-image baseline.
//!   - **op_walk_gradient_rgb24_640x480**: larger VGA RGB stream
//!     (alpha-unchanged path, no RGBA chunks).
//!   - **op_walk_solid_rgba_512x512**: a RUN-dominated stream — almost
//!     every op is a `Run`, the cheapest dispatch arm.
//!   - **op_walk_alpha_changing_rgba_320x240**: an RGBA-dominated
//!     stream — the 5-byte chunk arm with the most body-byte reads.
//!   - **op_walk_index_friendly_rgba_320x240**: an INDEX-heavy stream
//!     from an 8-colour palette.
//!
//! Run with:
//!     cargo bench -p oxideav-qoi --bench op_walk

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_qoi::{encode_qoi, iter_ops, iter_ops_strict};

/// Cheap deterministic xorshift32 — synthesises "natural-ish" per-pixel
/// values so the bench inputs aren't trivially compressible / branch-
/// predictable. Mirrors `benches/decode.rs`.
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
    // (200, 50, 25, 255) — non-zero pixel so the encoder's first chunk
    // is a LUMA from (0,0,0,255), then the remaining pixels collapse
    // into RUN chunks. Mirrors `benches/decode.rs`.
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
            // Unique alpha per pixel forces the encoder down the RGBA
            // path so the op stream is RGBA-dominated.
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

/// Fold the lazy `iter_ops` walk into a single accumulator over each
/// op's `tag()` byte so the optimiser cannot elide the iteration. No
/// allocation happens per op.
fn walk_lazy(bytes: &[u8]) -> u64 {
    let (_hdr, it) = iter_ops(criterion::black_box(bytes)).expect("iter_ops");
    let mut acc: u64 = 0;
    for op in it {
        acc = acc.wrapping_add(op.tag() as u64);
    }
    acc
}

/// Drive a `(lazy iter_ops, eager iter_ops_strict)` pair on one shape.
fn bench_shape(c: &mut Criterion, name: &str, throughput: u64, bytes: Vec<u8>, sample_size: usize) {
    let mut g = c.benchmark_group(name);
    g.throughput(Throughput::Bytes(throughput));
    if sample_size != 0 {
        g.sample_size(sample_size);
    }
    g.bench_function(BenchmarkId::new("iter_ops", "lazy"), |b| {
        b.iter(|| walk_lazy(&bytes));
    });
    g.bench_function(BenchmarkId::new("iter_ops_strict", "eager"), |b| {
        b.iter(|| {
            let (_hdr, ops) =
                iter_ops_strict(criterion::black_box(&bytes)).expect("iter_ops_strict");
            ops.len()
        });
    });
    g.finish();
}

fn bench_op_walk_gradient_rgba_320x240(c: &mut Criterion) {
    let pixels = build_gradient_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    bench_shape(c, "op_walk_gradient_rgba_320x240", 320 * 240 * 4, bytes, 0);
}

fn bench_op_walk_gradient_rgb24_640x480(c: &mut Criterion) {
    let pixels = build_gradient_rgb24(640, 480);
    let bytes = encode_qoi(640, 480, 3, &pixels);
    bench_shape(
        c,
        "op_walk_gradient_rgb24_640x480",
        640 * 480 * 3,
        bytes,
        20,
    );
}

fn bench_op_walk_solid_rgba_512x512(c: &mut Criterion) {
    let pixels = build_solid_rgba(512, 512);
    let bytes = encode_qoi(512, 512, 4, &pixels);
    bench_shape(c, "op_walk_solid_rgba_512x512", 512 * 512 * 4, bytes, 0);
}

fn bench_op_walk_alpha_changing_rgba_320x240(c: &mut Criterion) {
    let pixels = build_alpha_changing_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    bench_shape(
        c,
        "op_walk_alpha_changing_rgba_320x240",
        320 * 240 * 4,
        bytes,
        0,
    );
}

fn bench_op_walk_index_friendly_rgba_320x240(c: &mut Criterion) {
    let pixels = build_index_friendly_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    bench_shape(
        c,
        "op_walk_index_friendly_rgba_320x240",
        320 * 240 * 4,
        bytes,
        0,
    );
}

criterion_group!(
    benches,
    bench_op_walk_gradient_rgba_320x240,
    bench_op_walk_gradient_rgb24_640x480,
    bench_op_walk_solid_rgba_512x512,
    bench_op_walk_alpha_changing_rgba_320x240,
    bench_op_walk_index_friendly_rgba_320x240,
);
criterion_main!(benches);
