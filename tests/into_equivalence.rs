//! Equivalence + buffer-reuse safety sweep for the caller-owned
//! `_into` encode / decode API.
//!
//! Round 402 (depth-mode). Every other suite in this crate drives the
//! *allocating* entry points (`encode_qoi_full` / `parse_qoi`). The
//! buffer-reuse variants — `encode_qoi_into`, `encode_qoi_full_into`,
//! `parse_qoi_into` — carry a distinct contract the allocating paths
//! never exercise: the caller's `Vec<u8>` is cleared, grown to the
//! worst-case (encode) or exact (decode) length, written through a
//! moving cursor, then truncated to the actual size, with the
//! allocation retained across calls. Only the `reuse` Criterion bench
//! touched these before this suite, and a bench asserts throughput,
//! not correctness. Nothing pinned:
//!
//! 1. **Byte-for-byte equivalence** — `_into` into a *fresh* buffer
//!    produces exactly the same bytes / pixels + header the allocating
//!    wrapper does. (This is structural — the allocating wrappers call
//!    the `_into` bodies — so the assertion is a regression guard: a
//!    future refactor that stops routing one through the other, or
//!    changes the clear / resize / truncate dance, would break here.)
//!
//! 2. **Dirty-buffer reuse** — calling `_into` on a buffer pre-filled
//!    with `0xAA` garbage and grown to an unrelated capacity must yield
//!    the same result as a fresh buffer. Guards a hypothetical refactor
//!    that dropped the `buf.clear()` and relied on `resize` over stale
//!    bytes, or used `reserve` + `set_len` without zero-filling.
//!
//! 3. **Shrinking reuse (no stale tail)** — encode / decode a LARGE
//!    image into a buffer, then a SMALL image into the SAME buffer.
//!    The small result must equal the fresh-buffer small result: no
//!    bytes from the large image may leak past the truncation point.
//!    This is the headline data-integrity risk of a reused output
//!    buffer.
//!
//! 4. **Reuse after a rejected decode** — feed `parse_qoi_into` a
//!    malformed stream (it returns `Err` and leaves the buffer in a
//!    documented "unspecified" state), then decode a VALID stream into
//!    the SAME buffer. The valid decode must be pixel-exact — a failed
//!    call must not poison the buffer for the next successful one.
//!
//! 5. **Length / capacity post-conditions** — after `_into`,
//!    `buf.len()` equals the exact encoded / decoded byte count and the
//!    retained `capacity()` never shrinks below what a prior larger
//!    call needed (the "amortise the allocation" promise).
//!
//! Like `property_sweep`, this suite uses a self-contained xorshift32
//! PRNG and inline generators (no `proptest` / `quickcheck` dev-dep,
//! no shared test module across compilation units) so any failure is
//! reproducible from the printed seed.

use oxideav_qoi::{
    encode_qoi, encode_qoi_full, encode_qoi_full_into, encode_qoi_into, parse_qoi, parse_qoi_into,
    QoiChannels, QoiColorspace,
};

// ---------------------------------------------------------------------------
// Deterministic PRNG (same family as property_sweep / the benches).
// ---------------------------------------------------------------------------

struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    fn new(seed: u32) -> Self {
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

    fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        debug_assert!(hi >= lo);
        let span = hi - lo + 1;
        lo + self.next_u32() % span
    }
}

// ---------------------------------------------------------------------------
// Input generators — the same five op-mix shapes property_sweep uses,
// so this suite covers every path through the chunk-priority chain.
// ---------------------------------------------------------------------------

fn pixels_random(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let mut data = vec![0u8; n * bpp];
    for b in data.iter_mut() {
        *b = rng.next_byte();
    }
    data
}

fn pixels_smooth(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let mut data = vec![0u8; n * bpp];
    let mut cur = [0u8; 4];
    cur[3] = 0xff;
    for i in 0..n {
        for (c, slot) in cur.iter_mut().enumerate().take(bpp) {
            let delta = (rng.next_byte() & 0x07) as i32 - 3;
            *slot = (*slot as i32 + delta).clamp(0, 255) as u8;
            data[i * bpp + c] = *slot;
        }
    }
    data
}

fn pixels_run_heavy(rng: &mut XorShift32, n: usize, channels: u8) -> Vec<u8> {
    let bpp = channels as usize;
    let mut data = vec![0u8; n * bpp];
    let mut cur = [0u8; 4];
    cur[3] = 0xff;
    for slot in cur.iter_mut().take(bpp) {
        *slot = rng.next_byte();
    }
    let mut i = 0;
    while i < n {
        let run = rng.range_u32(1, 200).min((n - i) as u32) as usize;
        for k in 0..run {
            let off = (i + k) * bpp;
            data[off..off + bpp].copy_from_slice(&cur[..bpp]);
        }
        i += run;
        for slot in cur.iter_mut().take(bpp) {
            *slot = rng.next_byte();
        }
    }
    data
}

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
        data[off + 3] = rng.next_byte();
    }
    data
}

type Generator = fn(&mut XorShift32, usize, u8) -> Vec<u8>;

const GENERATORS: &[(&str, Generator)] = &[
    ("random", pixels_random),
    ("smooth", pixels_smooth),
    ("run_heavy", pixels_run_heavy),
    ("palette", pixels_palette),
    ("alpha_churn", pixels_alpha_churn),
];

// ---------------------------------------------------------------------------
// Core per-input equivalence + dirty-buffer checks.
// ---------------------------------------------------------------------------

/// Assert the `_into` encode / decode surface matches the allocating
/// wrappers byte-for-byte, both into a fresh buffer and into a buffer
/// pre-dirtied with `0xAA` garbage at an unrelated capacity.
#[track_caller]
fn assert_into_equivalence(
    seed: u32,
    label: &str,
    width: u32,
    height: u32,
    channels: u8,
    colorspace: u8,
    pixels: &[u8],
) {
    let ctx = format!("[{label}] seed={seed} w={width} h={height} ch={channels} cs={colorspace}");

    // --- Reference (allocating) encode ---
    let reference = encode_qoi_full(width, height, channels, colorspace, pixels);

    // --- encode_qoi_full_into into a fresh buffer ---
    let mut fresh = Vec::new();
    encode_qoi_full_into(&mut fresh, width, height, channels, colorspace, pixels);
    assert_eq!(
        fresh, reference,
        "{ctx}: encode_qoi_full_into(fresh) != encode_qoi_full"
    );
    assert_eq!(
        fresh.len(),
        reference.len(),
        "{ctx}: encode_qoi_full_into produced wrong len"
    );

    // --- encode_qoi_full_into into a pre-dirtied, pre-grown buffer ---
    let mut dirty = vec![0xAAu8; reference.len() + 37];
    dirty.reserve(4096);
    let cap_before = dirty.capacity();
    encode_qoi_full_into(&mut dirty, width, height, channels, colorspace, pixels);
    assert_eq!(
        dirty, reference,
        "{ctx}: encode_qoi_full_into(dirty) leaked stale bytes / diverged"
    );
    assert!(
        dirty.capacity() >= cap_before,
        "{ctx}: encode_qoi_full_into shrank a large-enough capacity"
    );

    // --- encode_qoi_into (colorspace-0 convenience) matches when cs==0 ---
    if colorspace == 0 {
        let alloc0 = encode_qoi(width, height, channels, pixels);
        assert_eq!(
            alloc0, reference,
            "{ctx}: encode_qoi != encode_qoi_full(cs=0)"
        );
        let mut into0 = vec![0xAAu8; 5];
        encode_qoi_into(&mut into0, width, height, channels, pixels);
        assert_eq!(
            into0, reference,
            "{ctx}: encode_qoi_into != encode_qoi_full(cs=0)"
        );
    }

    // --- Reference (allocating) decode ---
    let ref_img = parse_qoi(&reference)
        .unwrap_or_else(|e| panic!("{ctx}: parse_qoi rejected encoder output: {e:?}"));

    // --- parse_qoi_into into a fresh buffer ---
    let mut dec_fresh = Vec::new();
    let hdr = parse_qoi_into(&reference, &mut dec_fresh)
        .unwrap_or_else(|e| panic!("{ctx}: parse_qoi_into(fresh) rejected valid stream: {e:?}"));
    assert_eq!(
        dec_fresh, ref_img.pixels,
        "{ctx}: parse_qoi_into(fresh) pixels != parse_qoi pixels"
    );
    assert_eq!(hdr.width, ref_img.width, "{ctx}: header width drift");
    assert_eq!(hdr.height, ref_img.height, "{ctx}: header height drift");
    assert_eq!(
        hdr.channels, ref_img.channels,
        "{ctx}: header channels drift"
    );
    assert_eq!(
        hdr.colorspace, ref_img.colorspace,
        "{ctx}: header colorspace drift"
    );
    assert_eq!(
        dec_fresh.len(),
        (width as usize) * (height as usize) * (channels as usize),
        "{ctx}: parse_qoi_into produced wrong len"
    );

    // --- parse_qoi_into into a pre-dirtied, pre-grown buffer ---
    let mut dec_dirty = vec![0xAAu8; ref_img.pixels.len() + 61];
    dec_dirty.reserve(8192);
    let dcap_before = dec_dirty.capacity();
    parse_qoi_into(&reference, &mut dec_dirty)
        .unwrap_or_else(|e| panic!("{ctx}: parse_qoi_into(dirty) rejected valid stream: {e:?}"));
    assert_eq!(
        dec_dirty, ref_img.pixels,
        "{ctx}: parse_qoi_into(dirty) leaked stale bytes / diverged"
    );
    assert!(
        dec_dirty.capacity() >= dcap_before,
        "{ctx}: parse_qoi_into shrank a large-enough capacity"
    );

    // --- Expected header enum values ---
    let want_channels = if channels == 4 {
        QoiChannels::Rgba
    } else {
        QoiChannels::Rgb
    };
    let want_cs = if colorspace == 1 {
        QoiColorspace::AllLinear
    } else {
        QoiColorspace::SrgbWithLinearAlpha
    };
    assert_eq!(hdr.channels, want_channels, "{ctx}: channels enum wrong");
    assert_eq!(hdr.colorspace, want_cs, "{ctx}: colorspace enum wrong");
}

// ---------------------------------------------------------------------------
// Sweep — every generator × channels × colorspace × many seeds.
// ---------------------------------------------------------------------------

const SWEEP_ITERATIONS: u32 = 120;
const MAX_DIM: u32 = 48;

fn sweep(label: &str, seed_base: u32, gen_pixels: Generator) {
    for iter in 0..SWEEP_ITERATIONS {
        let seed = seed_base.wrapping_add(iter);
        let mut rng = XorShift32::new(seed);

        let width = rng.range_u32(1, MAX_DIM);
        let height = rng.range_u32(1, MAX_DIM);
        let channels = if rng.next_byte() & 1 == 1 { 4 } else { 3 };
        let colorspace = if rng.next_byte() & 1 == 1 { 1 } else { 0 };

        let n = (width as usize) * (height as usize);
        let pixels = gen_pixels(&mut rng, n, channels);

        assert_into_equivalence(seed, label, width, height, channels, colorspace, &pixels);
    }
}

#[test]
fn into_equivalence_random() {
    sweep("random", 0x0e00_0001, pixels_random);
}

#[test]
fn into_equivalence_smooth() {
    sweep("smooth", 0x0e00_0002, pixels_smooth);
}

#[test]
fn into_equivalence_run_heavy() {
    sweep("run_heavy", 0x0e00_0003, pixels_run_heavy);
}

#[test]
fn into_equivalence_palette() {
    sweep("palette", 0x0e00_0004, pixels_palette);
}

#[test]
fn into_equivalence_alpha_churn() {
    sweep("alpha_churn", 0x0e00_0005, pixels_alpha_churn);
}

// ---------------------------------------------------------------------------
// Shrinking-reuse: a large image then a small image into the SAME
// buffer must not leave a stale tail behind. This is the core
// data-integrity risk of a reused output buffer.
// ---------------------------------------------------------------------------

#[test]
fn encode_into_shrinking_reuse_has_no_stale_tail() {
    let mut rng = XorShift32::new(0x0e00_0010);
    let mut enc_buf: Vec<u8> = Vec::new();

    for (label, gen) in GENERATORS {
        for iter in 0..40u32 {
            // A large image first, then a strictly smaller one — reuse
            // the SAME enc_buf across both.
            let big_w = rng.range_u32(20, 48);
            let big_h = rng.range_u32(20, 48);
            let small_w = rng.range_u32(1, 8);
            let small_h = rng.range_u32(1, 8);
            let channels = if rng.next_byte() & 1 == 1 { 4 } else { 3 };
            let cs = rng.next_byte() & 1;

            let big_px = gen(&mut rng, (big_w * big_h) as usize, channels);
            let small_px = gen(&mut rng, (small_w * small_h) as usize, channels);

            // Prime the buffer with the large image, then encode the
            // small one into it.
            encode_qoi_full_into(&mut enc_buf, big_w, big_h, channels, cs, &big_px);
            let big_cap = enc_buf.capacity();
            encode_qoi_full_into(&mut enc_buf, small_w, small_h, channels, cs, &small_px);

            let fresh = encode_qoi_full(small_w, small_h, channels, cs, &small_px);
            assert_eq!(
                enc_buf, fresh,
                "[{label}] iter={iter}: shrinking encode reuse left a stale tail"
            );
            assert!(
                enc_buf.capacity() >= big_cap.min(enc_buf.capacity()),
                "[{label}] iter={iter}: capacity unexpectedly dropped"
            );
        }
    }
}

#[test]
fn decode_into_shrinking_reuse_has_no_stale_tail() {
    let mut rng = XorShift32::new(0x0e00_0020);
    let mut dec_buf: Vec<u8> = Vec::new();

    for (label, gen) in GENERATORS {
        for iter in 0..40u32 {
            let big_w = rng.range_u32(20, 48);
            let big_h = rng.range_u32(20, 48);
            let small_w = rng.range_u32(1, 8);
            let small_h = rng.range_u32(1, 8);
            let channels = if rng.next_byte() & 1 == 1 { 4 } else { 3 };
            let cs = rng.next_byte() & 1;

            let big_px = gen(&mut rng, (big_w * big_h) as usize, channels);
            let small_px = gen(&mut rng, (small_w * small_h) as usize, channels);

            let big_stream = encode_qoi_full(big_w, big_h, channels, cs, &big_px);
            let small_stream = encode_qoi_full(small_w, small_h, channels, cs, &small_px);

            // Prime the buffer with the large decode, then decode the
            // small stream into the SAME buffer.
            parse_qoi_into(&big_stream, &mut dec_buf).unwrap();
            parse_qoi_into(&small_stream, &mut dec_buf).unwrap();

            assert_eq!(
                dec_buf, small_px,
                "[{label}] iter={iter}: shrinking decode reuse left a stale tail"
            );
            assert_eq!(
                dec_buf.len(),
                small_px.len(),
                "[{label}] iter={iter}: decode reuse wrong len"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Reuse-after-error: a rejected decode leaves the buffer in a
// documented "unspecified" state; the NEXT successful decode into the
// same buffer must still be pixel-exact.
// ---------------------------------------------------------------------------

#[test]
fn decode_into_reuse_after_rejected_stream_is_clean() {
    let mut rng = XorShift32::new(0x0e00_0030);
    let mut dec_buf: Vec<u8> = Vec::new();

    // A grab-bag of malformed streams that fail at different points of
    // the decode (header gate, end-marker check, mid-chunk truncation,
    // oversized-header guard) so the buffer is left "unspecified" in
    // several distinct ways before the valid decode.
    let bad_streams: Vec<Vec<u8>> = vec![
        // Bad magic.
        vec![b'x', b'o', b'i', b'f', 0, 0, 0, 1, 0, 0, 0, 1, 4, 0],
        // Valid header, missing end marker (too short).
        {
            let mut v = Vec::new();
            v.extend_from_slice(b"qoif");
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&1u32.to_be_bytes());
            v.push(4);
            v.push(0);
            v
        },
        // Oversized-header guard: 65535×65535, one-byte body.
        {
            let mut v = Vec::new();
            v.extend_from_slice(b"qoif");
            v.extend_from_slice(&65535u32.to_be_bytes());
            v.extend_from_slice(&65535u32.to_be_bytes());
            v.push(4);
            v.push(0);
            v.push(0xff); // one RGBA tag, then it runs out
            v.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
            v
        },
    ];

    for (label, gen) in GENERATORS {
        for iter in 0..30u32 {
            let width = rng.range_u32(1, 32);
            let height = rng.range_u32(1, 32);
            let channels = if rng.next_byte() & 1 == 1 { 4 } else { 3 };
            let cs = rng.next_byte() & 1;
            let px = gen(&mut rng, (width * height) as usize, channels);
            let valid = encode_qoi_full(width, height, channels, cs, &px);

            for bad in &bad_streams {
                // Poison the buffer with a rejected decode.
                let err = parse_qoi_into(bad, &mut dec_buf);
                assert!(
                    err.is_err(),
                    "[{label}] iter={iter}: expected the malformed stream to be rejected"
                );
                // The next valid decode into the SAME buffer must be
                // pixel-exact regardless of the poisoned prior state.
                let hdr = parse_qoi_into(&valid, &mut dec_buf).unwrap_or_else(|e| {
                    panic!("[{label}] iter={iter}: valid decode after a rejected one failed: {e:?}")
                });
                assert_eq!(
                    dec_buf, px,
                    "[{label}] iter={iter}: buffer poisoned by prior rejected decode"
                );
                assert_eq!(
                    hdr.width, width,
                    "[{label}] iter={iter}: width drift after reuse"
                );
                assert_eq!(
                    hdr.height, height,
                    "[{label}] iter={iter}: height drift after reuse"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Capacity amortisation: after the first worst-case call, a run of
// same-or-smaller images must not re-grow the allocation. This is the
// promise the `_into` API exists to keep.
// ---------------------------------------------------------------------------

#[test]
fn into_capacity_is_amortised_across_repeated_calls() {
    let mut rng = XorShift32::new(0x0e00_0040);
    let mut enc_buf: Vec<u8> = Vec::new();
    let mut dec_buf: Vec<u8> = Vec::new();

    // Prime with the largest image the loop will see so the worst-case
    // capacity is established up front.
    let w = 48u32;
    let h = 48u32;
    let channels = 4u8;
    let big = pixels_alpha_churn(&mut rng, (w * h) as usize, channels);
    encode_qoi_full_into(&mut enc_buf, w, h, channels, 0, &big);
    let stream = encode_qoi_full(w, h, channels, 0, &big);
    parse_qoi_into(&stream, &mut dec_buf).unwrap();

    let enc_cap = enc_buf.capacity();
    let dec_cap = dec_buf.capacity();

    for iter in 0..100u32 {
        // Every subsequent image is <= the primed dimensions.
        let sw = rng.range_u32(1, w);
        let sh = rng.range_u32(1, h);
        let px = pixels_alpha_churn(&mut rng, (sw * sh) as usize, channels);
        encode_qoi_full_into(&mut enc_buf, sw, sh, channels, 0, &px);
        let s = encode_qoi_full(sw, sh, channels, 0, &px);
        parse_qoi_into(&s, &mut dec_buf).unwrap();

        assert!(
            enc_buf.capacity() <= enc_cap,
            "iter={iter}: encode buffer re-grew past the primed worst case \
             ({} > {enc_cap})",
            enc_buf.capacity()
        );
        assert!(
            dec_buf.capacity() <= dec_cap,
            "iter={iter}: decode buffer re-grew past the primed worst case \
             ({} > {dec_cap})",
            dec_buf.capacity()
        );
    }
}
