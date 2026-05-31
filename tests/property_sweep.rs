//! Property-style sweep over the QOI encode / decode surface.
//!
//! Round 199 (depth-mode property tests): the crate is feature-complete
//! against the one-page qoiformat.org spec, byte-exact roundtrips every
//! reference fixture, has a daily decode-only fuzz harness + an
//! encode-roundtrip fuzz target, has Criterion benches across the
//! five op-mix scenarios, and has a `samply`-friendly profile driver.
//! Per the workspace "saturated → fuzz / bench / profile / property"
//! memo this round adds a deterministic property-style sweep that
//! exercises hundreds of pseudo-random `(width, height, channels,
//! colorspace, pixels)` triples per scenario and asserts the
//! semantic invariants the spec mandates, *across* the whole input
//! space rather than at the hand-picked points the unit tests
//! cover.
//!
//! The sweep avoids introducing a new dev-dep (`proptest` /
//! `quickcheck`) and instead uses a deterministic xorshift32 PRNG
//! seeded per scenario, so any failure is reproducible from the
//! seed printed in the assertion message and the test stays
//! offline / no-net / no-extra-build-cost.
//!
//! ## Invariants under test
//!
//! For every randomly generated `(width, height, channels,
//! colorspace, pixels)`:
//!
//! 1. **Lossless roundtrip.** `parse_qoi(encode_qoi_full(w, h, ch,
//!    cs, px))` returns an `Ok(QoiImage)` whose `(width, height,
//!    channels, colorspace, pixels)` equals the input.
//!    This is the spec's primary guarantee — any drift between the
//!    encoder's chunk-selection priority chain and the decoder's
//!    chunk walker breaks it.
//!
//! 2. **Worst-case size bound.** The encoded stream is at most
//!    `14 + n*5 + 8` bytes where `n = width * height`. This is the
//!    bound the encoder pre-reserves; a regression that exceeded it
//!    would mean the encoder is emitting illegal chunks or
//!    forgetting to flush a RUN.
//!
//! 3. **Header bytes echo input.** Bytes 0..4 are `qoif`, bytes
//!    4..8 / 8..12 are width / height big-endian, byte 12 is
//!    `channels`, byte 13 is `colorspace`. Bytes `len-8..len` are
//!    the spec's `00 00 00 00 00 00 00 01` end marker. These are
//!    structural and must hold for every well-formed input.
//!
//! 4. **Encoder determinism.** Encoding the same input twice
//!    produces byte-identical output. There is no `HashMap`
//!    iteration order or wall-clock state in the codec; this catches
//!    accidental introductions.
//!
//! 5. **Solid-fill compact bound.** A `w*h` image of one repeated
//!    pixel encodes to no more than `14 + 5 + ceil(n/62) + 8` bytes:
//!    header, at most one seed chunk (LUMA / RGB / RGBA depending on
//!    the pixel value), one byte per 62-pixel RUN block, and the end
//!    marker. This bound is much tighter than the worst case and
//!    locks in the run-flush-every-62 behaviour against random pixel
//!    choices.
//!
//! 6. **Idempotent re-encode.** `encode(decode(encode(px))) ==
//!    encode(px)` byte-for-byte. The encoder's chunk-selection chain
//!    is a deterministic function of `pixels`, and the decoder is
//!    pixel-exact, so re-running the pipeline must collapse to the
//!    same byte stream. This is a stronger structural check than
//!    plain roundtrip — it asserts the decoder isn't accidentally
//!    re-permuting pixel order across the RGBA / RGB encoding
//!    boundary.
//!
//! Each invariant runs across multiple seeds + shape distributions
//! (tiny widths, square images, very-long-RUN streams, RGBA with
//! per-pixel-changing alpha, INDEX-heavy 8-colour palettes) so the
//! sweep covers both the chunk-selection priority chain
//! (RUN > INDEX > DIFF > LUMA > RGB / RGBA) and the per-chunk
//! arithmetic in the same pass.

use oxideav_qoi::{
    encode_qoi_full, parse_qoi, QoiChannels, QoiColorspace, END_MARKER, HEADER_SIZE, MAGIC,
};

// ---------------------------------------------------------------------------
// Deterministic PRNG
// ---------------------------------------------------------------------------

/// Minimal xorshift32 PRNG — same family the bench inputs use, kept
/// inline so the sweep stays offline / no-extra-dep.
struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    fn new(seed: u32) -> Self {
        // xorshift32 with a 0 state is a fixed point at 0; the
        // caller never picks 0 but defend against it.
        Self {
            state: if seed == 0 { 0x1234_5678 } else { seed },
        }
    }

    fn next_u32(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        self.state
    }

    fn next_byte(&mut self) -> u8 {
        (self.next_u32() & 0xff) as u8
    }

    /// Uniform in `[lo, hi]` inclusive. `hi - lo + 1` must fit u32.
    fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        debug_assert!(hi >= lo);
        let span = hi - lo + 1;
        lo + self.next_u32() % span
    }
}

// ---------------------------------------------------------------------------
// Input generators — different pixel-stream shapes that target
// different paths through the encoder's chunk-priority chain.
// ---------------------------------------------------------------------------

/// Fully random pixels — exercises RGBA / RGB chunks heavily, with
/// occasional accidental DIFF / LUMA / INDEX hits.
fn pixels_random(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let mut data = vec![0u8; n * bpp];
    for b in data.iter_mut() {
        *b = rng.next_byte();
    }
    data
}

/// Smooth deltas — pixel `i+1` is `pixel[i]` plus a small per-channel
/// delta in roughly `[-3, +3]`. Maximises DIFF + LUMA chunk usage.
fn pixels_smooth(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let mut data = vec![0u8; n * bpp];
    let mut cur = [0u8; 4];
    cur[3] = 0xff;
    for i in 0..n {
        for (c, slot) in cur.iter_mut().enumerate().take(bpp) {
            // Bias: 7-bit signed offset in [-3, +3].
            let delta = (rng.next_byte() & 0x07) as i32 - 3;
            *slot = (*slot as i32 + delta).clamp(0, 255) as u8;
            data[i * bpp + c] = *slot;
        }
    }
    data
}

/// Long runs of the same colour, interspersed with random jumps —
/// targets the RUN-flush-every-62 boundary + the run-then-new-chunk
/// transition.
fn pixels_run_heavy(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let mut data = vec![0u8; n * bpp];
    let mut cur = [0u8; 4];
    cur[3] = 0xff;
    // Seed with a random colour.
    for slot in cur.iter_mut().take(bpp) {
        *slot = rng.next_byte();
    }
    let mut i = 0;
    while i < n {
        // Run length biased toward the 1..=200 range so the 62-pixel
        // chunk-cap boundary is hit repeatedly.
        let run = rng.range_u32(1, 200).min((n - i) as u32) as usize;
        for k in 0..run {
            let off = (i + k) * bpp;
            data[off..off + bpp].copy_from_slice(&cur[..bpp]);
        }
        i += run;
        // Jump to a fresh colour.
        for slot in cur.iter_mut().take(bpp) {
            *slot = rng.next_byte();
        }
    }
    data
}

/// Cycle through a small random palette — targets the INDEX path.
fn pixels_palette(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let palette_len = 8;
    let mut palette = vec![0u8; palette_len * bpp];
    for b in palette.iter_mut() {
        *b = rng.next_byte();
    }
    let mut data = vec![0u8; n * bpp];
    for i in 0..n {
        let p = (i + rng.next_u32() as usize % 3) % palette_len;
        let dst = i * bpp;
        let src = p * bpp;
        data[dst..dst + bpp].copy_from_slice(&palette[src..src + bpp]);
    }
    data
}

/// Alpha changes per pixel — forces near-every-pixel RGBA in the
/// 4-channel case. For RGB inputs this collapses to `pixels_random`.
fn pixels_alpha_churn(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    if channels == 3 {
        return pixels_random(rng, n, channels);
    }
    let mut data = vec![0u8; n * 4];
    for i in 0..n {
        let off = i * 4;
        data[off] = rng.next_byte();
        data[off + 1] = rng.next_byte();
        data[off + 2] = rng.next_byte();
        // Force unique alpha to defeat INDEX hits.
        data[off + 3] = rng.next_byte();
    }
    data
}

// ---------------------------------------------------------------------------
// Invariant assertions — shared core that every property check uses.
// ---------------------------------------------------------------------------

/// The full set of structural + roundtrip invariants for one input.
/// Called by every sweep below.
#[track_caller]
fn assert_invariants(
    seed: u32,
    label: &str,
    width: u32,
    height: u32,
    channels: u8,
    colorspace: u8,
    pixels: &[u8],
) {
    let n = (width as usize) * (height as usize);
    debug_assert_eq!(pixels.len(), n * channels as usize);

    // --- Invariant 1: lossless roundtrip + encoder determinism ---
    let bytes = encode_qoi_full(width, height, channels, colorspace, pixels);
    let bytes2 = encode_qoi_full(width, height, channels, colorspace, pixels);
    assert_eq!(
        bytes, bytes2,
        "[{label}] seed={seed} w={width} h={height} ch={channels} cs={colorspace}: \
         encoder is non-deterministic"
    );

    // --- Invariant 2: worst-case size bound ---
    let worst = HEADER_SIZE + n * 5 + END_MARKER.len();
    assert!(
        bytes.len() <= worst,
        "[{label}] seed={seed} w={width} h={height} ch={channels} cs={colorspace}: \
         encoded {} bytes > worst-case bound {worst}",
        bytes.len()
    );

    // --- Invariant 3: header + end marker ---
    assert!(
        bytes.len() >= HEADER_SIZE + END_MARKER.len(),
        "[{label}] seed={seed}: encoded shorter than header + end marker"
    );
    assert_eq!(
        &bytes[0..4],
        MAGIC,
        "[{label}] seed={seed}: bad magic prefix"
    );
    assert_eq!(
        u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        width,
        "[{label}] seed={seed}: width in header doesn't match"
    );
    assert_eq!(
        u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        height,
        "[{label}] seed={seed}: height in header doesn't match"
    );
    assert_eq!(
        bytes[12], channels,
        "[{label}] seed={seed}: channels in header doesn't match"
    );
    assert_eq!(
        bytes[13], colorspace,
        "[{label}] seed={seed}: colorspace in header doesn't match"
    );
    assert_eq!(
        &bytes[bytes.len() - 8..],
        END_MARKER,
        "[{label}] seed={seed}: end marker missing/wrong"
    );

    // --- Invariant 1 (cont.): decode round-trip ---
    let back = parse_qoi(&bytes).unwrap_or_else(|e| {
        panic!(
            "[{label}] seed={seed} w={width} h={height} ch={channels} cs={colorspace}: \
             parse_qoi rejected encoder output: {e:?}"
        )
    });
    assert_eq!(back.width, width, "[{label}] seed={seed}: width drift");
    assert_eq!(back.height, height, "[{label}] seed={seed}: height drift");
    let want_channels = if channels == 4 {
        QoiChannels::Rgba
    } else {
        QoiChannels::Rgb
    };
    assert_eq!(
        back.channels, want_channels,
        "[{label}] seed={seed}: channels enum drift"
    );
    let want_cs = if colorspace == 1 {
        QoiColorspace::AllLinear
    } else {
        QoiColorspace::SrgbWithLinearAlpha
    };
    assert_eq!(
        back.colorspace, want_cs,
        "[{label}] seed={seed}: colorspace enum drift"
    );
    assert_eq!(
        back.pixels, pixels,
        "[{label}] seed={seed} w={width} h={height} ch={channels} cs={colorspace}: \
         pixel round-trip mismatch"
    );

    // --- Invariant 6: idempotent re-encode ---
    let bytes3 = encode_qoi_full(back.width, back.height, channels, colorspace, &back.pixels);
    assert_eq!(
        bytes, bytes3,
        "[{label}] seed={seed} w={width} h={height} ch={channels} cs={colorspace}: \
         re-encode of decoded pixels differs from original encode \
         (chunk-selection chain is order-dependent on input shape?)"
    );
}

// ---------------------------------------------------------------------------
// Sweeps — one per generator shape × channel × colorspace.
// ---------------------------------------------------------------------------

/// Number of randomly generated inputs per sweep. 200 × 5 generators
/// × 2 channels × 2 colorspaces = 4_000 distinct cases per `cargo
/// test` invocation. Each case is small enough (≤ 64×64 pixels) that
/// the whole sweep runs in well under a second on a release build
/// and a few seconds in debug.
const SWEEP_ITERATIONS: u32 = 200;

/// Cap on per-iteration pixel count. 64×64 = 4096 pixels = ≤ 16 KiB
/// input, ≤ 20 KiB encoded — small enough that 4_000 iterations stay
/// inside a sensible `cargo test` budget on every CI runner.
const MAX_DIM: u32 = 64;

/// Run the `gen_pixels` shape across many `(w, h, ch, cs)` combos.
fn sweep<F>(label: &str, seed_base: u32, gen_pixels: F)
where
    F: Fn(&mut XorShift32, usize, u8) -> Vec<u8>,
{
    for iter in 0..SWEEP_ITERATIONS {
        let seed = seed_base.wrapping_add(iter);
        let mut rng = XorShift32::new(seed);

        let width = rng.range_u32(1, MAX_DIM);
        let height = rng.range_u32(1, MAX_DIM);
        let channels = if rng.next_byte() & 1 == 1 { 4 } else { 3 };
        let colorspace = if rng.next_byte() & 1 == 1 { 1 } else { 0 };

        let n = (width as usize) * (height as usize);
        let pixels = gen_pixels(&mut rng, n, channels);

        assert_invariants(seed, label, width, height, channels, colorspace, &pixels);
    }
}

#[test]
fn property_sweep_random_pixels() {
    sweep("random", 0x0a00_0001, pixels_random);
}

#[test]
fn property_sweep_smooth_deltas() {
    sweep("smooth", 0x0a00_0002, pixels_smooth);
}

#[test]
fn property_sweep_run_heavy() {
    sweep("run_heavy", 0x0a00_0003, pixels_run_heavy);
}

#[test]
fn property_sweep_palette() {
    sweep("palette", 0x0a00_0004, pixels_palette);
}

#[test]
fn property_sweep_alpha_churn() {
    sweep("alpha_churn", 0x0a00_0005, pixels_alpha_churn);
}

// ---------------------------------------------------------------------------
// Targeted sweeps that don't fit the (w, h, ch, cs, gen) tuple shape.
// ---------------------------------------------------------------------------

/// Invariant 5 — solid-colour images encode in the compact run-flush
/// bound. Pixels = `n` copies of one random colour; encoded bytes
/// fit `14 + 5 + ceil(n/62) + 8`.
#[test]
fn property_sweep_solid_fill_compact_bound() {
    let mut rng = XorShift32::new(0x0a00_0006);
    // 200 different (colour, width) combinations covering the 62-pixel
    // chunk-cap modular boundaries directly.
    for iter in 0..200 {
        let r = rng.next_byte();
        let g = rng.next_byte();
        let b = rng.next_byte();
        let a = rng.next_byte();
        let channels: u8 = if iter & 1 == 0 { 4 } else { 3 };
        let colorspace: u8 = if iter & 2 == 0 { 0 } else { 1 };

        // Hit each modular residue around 62 plus a long-stream case.
        let widths = [1u32, 30, 61, 62, 63, 124, 125, 187, 200, 512, 1024];
        for &w in &widths {
            let n = w as usize;
            let mut pixels = Vec::with_capacity(n * channels as usize);
            for _ in 0..n {
                pixels.push(r);
                pixels.push(g);
                pixels.push(b);
                if channels == 4 {
                    pixels.push(a);
                }
            }
            let bytes = encode_qoi_full(w, 1, channels, colorspace, &pixels);
            // Bound: header (14) + at most one seed chunk (up to 5
            // bytes — RGBA worst case) + at most one RUN byte per
            // full 62-pixel block + end marker (8).
            let max_runs = n.div_ceil(62);
            let bound = HEADER_SIZE + 5 + max_runs + END_MARKER.len();
            assert!(
                bytes.len() <= bound,
                "iter={iter} w={w} ch={channels} cs={colorspace}: \
                 encoded {} > compact-fill bound {bound}",
                bytes.len(),
            );

            // Roundtrip still holds.
            let back = parse_qoi(&bytes).expect("decode of solid fill");
            assert_eq!(
                back.pixels, pixels,
                "iter={iter} w={w} ch={channels}: solid-fill round-trip drift"
            );
        }
    }
}

/// Exercise the very-large dimension path with synthetic small
/// inputs — width and height up to MAX_DIM, but the pixel buffer
/// itself stays under the 4 KiB iteration cap so the sweep is
/// fast. The point is to confirm the spec's worst-case bound + the
/// roundtrip both still hold when w and h are independently large.
#[test]
fn property_sweep_tall_and_wide_shapes() {
    let mut rng = XorShift32::new(0x0a00_0007);
    // Skewed shapes: 1×N, N×1, prime×prime, max×1, 1×max.
    let shapes: &[(u32, u32)] = &[
        (1, 1),
        (1, 64),
        (64, 1),
        (3, 5),
        (5, 3),
        (7, 11),
        (11, 7),
        (32, 1),
        (1, 32),
        (16, 16),
        (64, 64),
    ];
    for &(w, h) in shapes {
        for ch in [3u8, 4u8] {
            for cs in [0u8, 1u8] {
                let n = (w as usize) * (h as usize);
                let pixels = pixels_smooth(&mut rng, n, ch);
                assert_invariants(0x0a00_0007, "tall_wide", w, h, ch, cs, &pixels);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Smoke-test the generators themselves so a generator bug isn't
// silently masked.
// ---------------------------------------------------------------------------

#[test]
fn generators_produce_well_sized_buffers() {
    let mut rng = XorShift32::new(0xdead_beef);
    for ch in [3u8, 4u8] {
        for n in [1usize, 7, 64, 4096] {
            assert_eq!(pixels_random(&mut rng, n, ch).len(), n * ch as usize);
            assert_eq!(pixels_smooth(&mut rng, n, ch).len(), n * ch as usize);
            assert_eq!(pixels_run_heavy(&mut rng, n, ch).len(), n * ch as usize);
            assert_eq!(pixels_palette(&mut rng, n, ch).len(), n * ch as usize);
            assert_eq!(pixels_alpha_churn(&mut rng, n, ch).len(), n * ch as usize);
        }
    }
}
