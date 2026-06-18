//! Criterion benchmarks for the chunk re-serialization path
//! (`QoiOp::write_to`).
//!
//! Round 332 (depth-mode benchmarks). The `op_walk` bench measures the
//! *forward* direction — walking a chunk stream into typed `QoiOp`
//! values with `iter_ops` / `iter_ops_strict`. `write_to` is the
//! *inverse*: given a `QoiOp`, it appends that op's complete on-wire
//! chunk (leading tag byte plus every body byte the format defines) to
//! a caller buffer, so `iter_ops(input)` → `write_to` round-trips an
//! in-spec chunk stream byte-for-byte. It has fuzz coverage
//! (`op_write`) and contract tests, but no benchmark, so the cost of
//! the re-serialization loop — per-op tag/body emit plus `Vec` growth —
//! was not measurable. This file closes that gap, mirroring the
//! `op_walk` bench's five image shapes byte-for-byte so an `op_write`
//! row lines up against the matching `op_walk` row.
//!
//! Each scenario pre-collects the op list once (outside the timed
//! region) with `iter_ops_strict`, then the timed closure walks that
//! `&[QoiOp]` and calls `write_to` on each into a reused output buffer.
//! That isolates the serialization cost from the chunk-walk parse cost
//! the `op_walk` bench already covers.
//!
//! Two harness flavours per shape:
//!   - **reuse** — a single output `Vec` cleared (not dropped) between
//!     iterations, so the steady-state cost is pure per-op emit with no
//!     allocation. This is the hot path a round-tripping tool runs.
//!   - **fresh** — a new `Vec` per iteration (capacity pre-reserved to
//!     the known encoded length), folding in first-write growth cost.
//!
//! Each scenario synthesises a fresh QOI on the fly with the public
//! encoder and walks the encoded bytes into ops. No fixture files are
//! committed. The five shapes match `benches/op_walk.rs` exactly:
//!
//!   - **op_write_gradient_rgba_320x240**: mixed DIFF / LUMA / RGB /
//!     INDEX op stream — the natural-image baseline.
//!   - **op_write_gradient_rgb24_640x480**: larger VGA RGB stream
//!     (alpha-unchanged path, no RGBA chunks).
//!   - **op_write_solid_rgba_512x512**: a RUN-dominated stream — almost
//!     every op is a tag-only `Run`, the cheapest emit arm.
//!   - **op_write_alpha_changing_rgba_320x240**: an RGBA-dominated
//!     stream — the 5-byte chunk arm with the most body bytes emitted.
//!   - **op_write_index_friendly_rgba_320x240**: an INDEX-heavy stream
//!     from an 8-colour palette.
//!
//! Run with:
//!     cargo bench -p oxideav-qoi --bench op_write

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_qoi::{encode_qoi, iter_ops_strict, QoiOp};

/// Cheap deterministic xorshift32 — synthesises "natural-ish" per-pixel
/// values so the bench inputs aren't trivially compressible / branch-
/// predictable. Mirrors `benches/op_walk.rs`.
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
    // into RUN chunks. Mirrors `benches/op_walk.rs`.
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

/// Pre-collect the op list (and the encoded byte length, used as the
/// `fresh` flavour's pre-reserve) for one shape's encoded QOI bytes.
fn ops_for(bytes: &[u8]) -> (Vec<QoiOp>, usize) {
    let (_hdr, ops) = iter_ops_strict(bytes).expect("iter_ops_strict");
    // Sum the encoded length so the round-trip output buffer reserve in
    // the `fresh` flavour matches what the ops will emit.
    let total: usize = ops.iter().map(|op| op.encoded_len()).sum();
    (ops, total)
}

/// Drive a `(reuse, fresh)` pair on one shape's pre-collected ops.
fn bench_shape(c: &mut Criterion, name: &str, throughput: u64, bytes: Vec<u8>, sample_size: usize) {
    let (ops, encoded_len) = ops_for(&bytes);
    let mut g = c.benchmark_group(name);
    g.throughput(Throughput::Bytes(throughput));
    if sample_size != 0 {
        g.sample_size(sample_size);
    }
    // Reuse: one output buffer cleared between iterations — steady-state
    // per-op emit with no allocation.
    g.bench_function(BenchmarkId::new("write_to", "reuse"), |b| {
        let mut out: Vec<u8> = Vec::with_capacity(encoded_len);
        b.iter(|| {
            out.clear();
            for op in criterion::black_box(&ops) {
                op.write_to(&mut out);
            }
            criterion::black_box(out.len())
        });
    });
    // Fresh: a new buffer per iteration (capacity pre-reserved) — folds
    // in the first-write growth cost.
    g.bench_function(BenchmarkId::new("write_to", "fresh"), |b| {
        b.iter(|| {
            let mut out: Vec<u8> = Vec::with_capacity(encoded_len);
            for op in criterion::black_box(&ops) {
                op.write_to(&mut out);
            }
            out.len()
        });
    });
    g.finish();
}

fn bench_op_write_gradient_rgba_320x240(c: &mut Criterion) {
    let pixels = build_gradient_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    bench_shape(c, "op_write_gradient_rgba_320x240", 320 * 240 * 4, bytes, 0);
}

fn bench_op_write_gradient_rgb24_640x480(c: &mut Criterion) {
    let pixels = build_gradient_rgb24(640, 480);
    let bytes = encode_qoi(640, 480, 3, &pixels);
    bench_shape(
        c,
        "op_write_gradient_rgb24_640x480",
        640 * 480 * 3,
        bytes,
        20,
    );
}

fn bench_op_write_solid_rgba_512x512(c: &mut Criterion) {
    let pixels = build_solid_rgba(512, 512);
    let bytes = encode_qoi(512, 512, 4, &pixels);
    bench_shape(c, "op_write_solid_rgba_512x512", 512 * 512 * 4, bytes, 0);
}

fn bench_op_write_alpha_changing_rgba_320x240(c: &mut Criterion) {
    let pixels = build_alpha_changing_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    bench_shape(
        c,
        "op_write_alpha_changing_rgba_320x240",
        320 * 240 * 4,
        bytes,
        0,
    );
}

fn bench_op_write_index_friendly_rgba_320x240(c: &mut Criterion) {
    let pixels = build_index_friendly_rgba(320, 240);
    let bytes = encode_qoi(320, 240, 4, &pixels);
    bench_shape(
        c,
        "op_write_index_friendly_rgba_320x240",
        320 * 240 * 4,
        bytes,
        0,
    );
}

criterion_group!(
    benches,
    bench_op_write_gradient_rgba_320x240,
    bench_op_write_gradient_rgb24_640x480,
    bench_op_write_solid_rgba_512x512,
    bench_op_write_alpha_changing_rgba_320x240,
    bench_op_write_index_friendly_rgba_320x240,
);
criterion_main!(benches);
