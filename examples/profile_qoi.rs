//! Standalone profiling driver for the QOI encoder and decoder.
//!
//! Round 175 (depth-mode profiling): the three Criterion harnesses
//! (`benches/{decode,encode,roundtrip}.rs`, round 156) measure
//! steady-state throughput in a sampling framework, but they're a poor
//! target for `samply` / `perf record` / `cargo flamegraph` because
//! Criterion's warm-up + sampling layers + estimator math show up in
//! the profile and bury the real codec hot paths. This example is a
//! flat measure-this-thing driver: it builds a deterministic
//! synthesised pixel buffer once, then runs a fixed iteration count of
//! whichever path was requested with a single `Instant::now()` /
//! `elapsed()` pair around the whole loop. Throughput is printed at
//! the end so it doubles as a quick A/B harness for the inner
//! tweak-remeasure loop when Criterion's per-run overhead is too
//! coarse.
//!
//! Usage:
//!
//!     cargo run --example profile_qoi --release -- <mode> [<iters>]
//!
//! Modes:
//!
//!     decode      — encode each scenario once outside the loop, decode
//!                   N times against the cached bytes
//!     encode      — synth pixels, encode N times (decoder cost excluded)
//!     roundtrip   — synth pixels, encode + decode every iteration
//!     all         — run every mode (default)
//!
//! With `samply`:
//!
//!     samply record -- ./target/release/examples/profile_qoi encode 5000
//!     samply record -- ./target/release/examples/profile_qoi decode 5000
//!
//! With `cargo flamegraph` (needs `cargo install flamegraph`):
//!
//!     cargo flamegraph --example profile_qoi -- encode 5000
//!
//! No external files are read — every input is synthesised in-driver
//! from a deterministic xorshift32 seed, matching the Criterion bench
//! harnesses so profile output and bench numbers correspond. Inputs
//! cover five cost-axis scenarios spanning the chunk-selection regimes
//! the spec's priority chain (RUN > INDEX > DIFF > LUMA > RGB / RGBA)
//! routes through.

use std::env;
use std::io::Write;
use std::time::Instant;

use oxideav_qoi::{encode_qoi, parse_qoi};

/// xorshift32 — same constant the bench harnesses use so the profile
/// and bench inputs are byte-identical.
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

/// Natural-image RGBA gradient with a touch of xorshift noise — the
/// mixed-op baseline that exercises DIFF / LUMA / RGB / INDEX in
/// roughly equal proportions. Matches `benches/decode.rs`
/// `build_gradient_rgba` byte-for-byte.
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

/// Larger RGB24 VGA gradient — the alpha-unchanged path with no RGBA
/// chunks. Matches `benches/decode.rs` `build_gradient_rgb24`.
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

/// Single-colour 512×512 RGBA fill — stresses `QOI_OP_RUN`'s
/// flush-every-62-pixels path; nearly all chunks should be RUNs.
/// Matches `benches/decode.rs` `build_solid_rgba`.
fn build_solid_rgba(width: u32, height: u32) -> Vec<u8> {
    [200u8, 50, 25, 255].repeat((width as usize) * (height as usize))
}

/// Per-pixel-changing-alpha worst case — forces almost-every-pixel
/// `QOI_OP_RGBA`, the slow path with no compression. Matches
/// `benches/decode.rs` `build_alpha_changing_rgba`.
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

/// 8-colour cycle — exercises the INDEX-chunk hot path where the
/// 64-entry running array sees repeated hits. Matches
/// `benches/decode.rs` `build_index_friendly_rgba`.
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

/// All five cost-axis scenarios, mirroring the Criterion benches.
struct Scenario {
    name: &'static str,
    width: u32,
    height: u32,
    /// Per-pixel byte count (3 for RGB, 4 for RGBA) — drives the
    /// throughput print and the encoder's `channels` argument.
    bytes_per_pixel: usize,
    /// Builder selector. Each scenario emits a fresh pixel buffer
    /// matching its `bytes_per_pixel` and dimensions.
    kind: ScenarioKind,
    /// Default iteration count for the encode path. The decode path
    /// is roughly 1-2x cheaper here (QOI is symmetric); the driver
    /// uses the same count for all three modes.
    encode_iters_default: u32,
}

#[derive(Clone, Copy)]
enum ScenarioKind {
    GradientRgba,
    GradientRgb24,
    SolidRgba,
    AlphaChangingRgba,
    IndexFriendlyRgba,
}

fn scenarios() -> &'static [Scenario] {
    &[
        Scenario {
            name: "rgba/320x240/gradient",
            width: 320,
            height: 240,
            bytes_per_pixel: 4,
            kind: ScenarioKind::GradientRgba,
            encode_iters_default: 200,
        },
        Scenario {
            name: "rgb24/640x480/gradient",
            width: 640,
            height: 480,
            bytes_per_pixel: 3,
            kind: ScenarioKind::GradientRgb24,
            encode_iters_default: 80,
        },
        Scenario {
            name: "rgba/512x512/solid-run",
            width: 512,
            height: 512,
            bytes_per_pixel: 4,
            kind: ScenarioKind::SolidRgba,
            encode_iters_default: 200,
        },
        Scenario {
            name: "rgba/320x240/alpha-changing",
            width: 320,
            height: 240,
            bytes_per_pixel: 4,
            kind: ScenarioKind::AlphaChangingRgba,
            encode_iters_default: 200,
        },
        Scenario {
            name: "rgba/320x240/index-cycle",
            width: 320,
            height: 240,
            bytes_per_pixel: 4,
            kind: ScenarioKind::IndexFriendlyRgba,
            encode_iters_default: 300,
        },
    ]
}

fn build_pixels(scen: &Scenario) -> Vec<u8> {
    match scen.kind {
        ScenarioKind::GradientRgba => build_gradient_rgba(scen.width, scen.height),
        ScenarioKind::GradientRgb24 => build_gradient_rgb24(scen.width, scen.height),
        ScenarioKind::SolidRgba => build_solid_rgba(scen.width, scen.height),
        ScenarioKind::AlphaChangingRgba => build_alpha_changing_rgba(scen.width, scen.height),
        ScenarioKind::IndexFriendlyRgba => build_index_friendly_rgba(scen.width, scen.height),
    }
}

fn encode_once(scen: &Scenario, pixels: &[u8]) -> Vec<u8> {
    encode_qoi(scen.width, scen.height, scen.bytes_per_pixel as u8, pixels)
}

fn decode_once(bytes: &[u8]) -> usize {
    let img = parse_qoi(bytes).expect("parse_qoi");
    // Sum a buffer length to keep the optimiser from dropping the call.
    img.pixels.len()
}

fn print_throughput_line(label: &str, scen: &Scenario, iters: u32, elapsed_secs: f64, extra: &str) {
    let raw_bytes_per_iter = scen.width as usize * scen.height as usize * scen.bytes_per_pixel;
    let total_bytes = raw_bytes_per_iter * iters as usize;
    let per_iter_ms = elapsed_secs * 1000.0 / iters as f64;
    let mib_per_s = (total_bytes as f64) / elapsed_secs / (1024.0 * 1024.0);
    println!(
        "  {label:9} {name:34} iters={iters:>5} {per_iter_ms:8.3} ms/iter  {mib_per_s:8.2} MiB/s (raw){extra}",
        name = scen.name,
    );
}

fn profile_encode(iters_override: Option<u32>) {
    println!("== encode ==");
    for scen in scenarios() {
        let iters = iters_override.unwrap_or(scen.encode_iters_default);
        let pixels = build_pixels(scen);

        // One warm-up so any first-call lazy init isn't charged to
        // iteration #1's bucket.
        let _ = encode_once(scen, &pixels);

        let t = Instant::now();
        let mut total_out_bytes = 0u64;
        for _ in 0..iters {
            let out = std::hint::black_box(encode_once(
                std::hint::black_box(scen),
                std::hint::black_box(&pixels),
            ));
            total_out_bytes += out.len() as u64;
            std::hint::black_box(out);
        }
        let elapsed = t.elapsed().as_secs_f64();
        let compressed_bytes_per_iter = total_out_bytes / iters.max(1) as u64;
        let raw_bytes_per_iter = scen.width as usize * scen.height as usize * scen.bytes_per_pixel;
        let ratio = compressed_bytes_per_iter as f64 / raw_bytes_per_iter as f64;
        let extra = format!("  out={compressed_bytes_per_iter}B/iter ({ratio:.3} of input)");
        print_throughput_line("encode", scen, iters, elapsed, &extra);
        std::io::stdout().flush().ok();
    }
}

fn profile_decode(iters_override: Option<u32>) {
    println!("== decode ==");
    for scen in scenarios() {
        let iters = iters_override.unwrap_or(scen.encode_iters_default);

        // Encode once OUTSIDE the timed region.
        let pixels = build_pixels(scen);
        let bytes = encode_once(scen, &pixels);

        // Warm up: one decode pass.
        let _ = decode_once(&bytes);

        let t = Instant::now();
        let mut sink = 0usize;
        for _ in 0..iters {
            sink ^= decode_once(std::hint::black_box(&bytes));
        }
        std::hint::black_box(sink);
        let elapsed = t.elapsed().as_secs_f64();
        print_throughput_line("decode", scen, iters, elapsed, "");
        std::io::stdout().flush().ok();
    }
}

fn profile_roundtrip(iters_override: Option<u32>) {
    println!("== roundtrip ==");
    for scen in scenarios() {
        let iters = iters_override.unwrap_or(scen.encode_iters_default);
        let pixels = build_pixels(scen);

        // Warm-up.
        {
            let bytes = encode_once(scen, &pixels);
            let _ = decode_once(&bytes);
        }

        let t = Instant::now();
        for _ in 0..iters {
            let bytes = std::hint::black_box(encode_once(
                std::hint::black_box(scen),
                std::hint::black_box(&pixels),
            ));
            let n = decode_once(&bytes);
            std::hint::black_box(n);
        }
        let elapsed = t.elapsed().as_secs_f64();
        print_throughput_line("roundtrip", scen, iters, elapsed, "");
        std::io::stdout().flush().ok();
    }
}

fn main() {
    let mut args = env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "all".to_string());
    let iters_override: Option<u32> = args.next().and_then(|s| s.parse().ok());

    println!(
        "=== oxideav-qoi profile (mode={mode}, iters={}) ===",
        iters_override
            .map(|n| n.to_string())
            .unwrap_or_else(|| "default".to_string()),
    );
    println!();

    match mode.as_str() {
        "encode" => profile_encode(iters_override),
        "decode" => profile_decode(iters_override),
        "roundtrip" => profile_roundtrip(iters_override),
        "all" => {
            profile_encode(iters_override);
            println!();
            profile_decode(iters_override);
            println!();
            profile_roundtrip(iters_override);
        }
        other => {
            eprintln!("unknown mode: {other:?}");
            eprintln!("usage: profile_qoi [encode|decode|roundtrip|all] [<iters>]");
            std::process::exit(2);
        }
    }
}
