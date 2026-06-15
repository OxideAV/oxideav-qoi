//! Canonical-encoding (chunk-minimality) property sweep.
//!
//! Round 316 (depth-mode): the crate is feature-complete against the
//! one-page qoiformat.org spec and already has a deterministic
//! `property_sweep.rs` asserting lossless roundtrip, the worst-case /
//! solid-fill size bounds, header/end-marker echo, encoder determinism,
//! and idempotent re-encode. Those invariants all hold for *any*
//! decodable stream — they confirm the encoder produces *a* correct
//! file, not that it produces *the* canonical one.
//!
//! This sweep adds the missing class of invariant: the encoder's output
//! is the spec-minimal stream. The spec's encoder section mandates a
//! fixed chunk-selection priority order
//!
//! ```text
//! RUN > INDEX > DIFF > LUMA > RGB / RGBA
//! ```
//!
//! and explicitly calls out one canonical-form constraint — a valid
//! encoder must not emit two consecutive `QOI_OP_INDEX` chunks that
//! resolve to the same slot (it must use `QOI_OP_RUN` instead, because
//! the second pixel equals the first). A roundtrip test cannot catch a
//! regression where the encoder picks a *legal but oversized* chunk
//! (e.g. an `RGB` where a `DIFF` fit, or an `INDEX` where a `RUN`
//! applied): such a stream still decodes pixel-exact, so invariants
//! 1–6 of `property_sweep.rs` stay green while the output silently
//! bloats.
//!
//! ## How the check works
//!
//! The crate's `iter_ops` walker yields one typed `QoiOp` per chunk but
//! is intentionally *stateless* with respect to the running pixel array
//! and `prev` pixel (its delta fields are raw, un-applied). To decide
//! whether each emitted chunk was the highest-priority legal choice,
//! this test re-derives the decoder's running state in lockstep with
//! the walk: it maintains the same 64-slot index array and `prev`
//! pixel the spec decoder maintains, applies each chunk to advance that
//! state, and — *before* applying — asserts that no strictly-higher
//! priority chunk was available for the pixel(s) this chunk produces.
//!
//! Concretely, for the chunk the encoder chose to encode pixel `cur`
//! (the pixel obtained by applying the chunk to `prev`):
//!
//! * **RUN** is only legal when `cur == prev`. Conversely, whenever
//!   `cur == prev` the encoder MUST have chosen RUN — so any
//!   non-`Run` chunk whose resulting pixel equals `prev` is a
//!   priority-order violation. Runs must also be `1..=62` (the spec
//!   range; 63/64 are shadowed by the RGB/RGBA tags) and the walk must
//!   never show two adjacent runs of the same pixel that *could* have
//!   been merged into one ≤62 chunk.
//! * **INDEX** must be chosen whenever `cur != prev` and the running
//!   array already holds `cur` at `hash(cur)`. So any DIFF / LUMA /
//!   RGB / RGBA whose `cur` was already in the index at its hash slot
//!   is a violation. Conversely an `INDEX` chunk must point at a slot
//!   that genuinely holds `cur` — and the spec's no-consecutive-
//!   redundant-INDEX rule falls out of the RUN-beats-INDEX check
//!   above (a repeat pixel is a RUN, never an INDEX).
//! * **DIFF** must be chosen over LUMA/RGB/RGBA whenever `cur != prev`,
//!   the alpha is unchanged, and all three deltas fit `−2..=+1`.
//! * **LUMA** must be chosen over RGB/RGBA whenever DIFF didn't fit but
//!   alpha is unchanged and `dg ∈ −32..=31`, `dr-dg`/`db-dg ∈ −8..=7`.
//! * **RGB** is legal only when alpha is unchanged but neither DIFF nor
//!   LUMA fit; **RGBA** only when alpha changed.
//!
//! Each emitted chunk is checked against exactly this ladder, so the
//! test fails the instant the encoder emits any chunk a strictly
//! higher-priority rule could have replaced.
//!
//! The sweep reuses the same five input-shape generators + xorshift32
//! PRNG family as `property_sweep.rs` (kept inline, no `proptest` /
//! `quickcheck` dev-dep) so it covers the whole chunk-priority chain
//! across thousands of pseudo-random inputs, plus a handful of
//! hand-built streams that pin the spec's named edge cases.

use oxideav_qoi::{encode_qoi_full, iter_ops, qoi_hash, QoiOp};

// ---------------------------------------------------------------------------
// Deterministic PRNG (same family as property_sweep.rs / the benches).
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
// Input generators — same shapes property_sweep.rs uses, so the
// canonical-form ladder is exercised across the full op-mix surface.
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

// ---------------------------------------------------------------------------
// Reference decoder state — re-derived in lockstep with the chunk walk
// so the canonical-form ladder can be checked against `prev` + index.
// This is the spec's decoder running state, NOT a copy of the crate's
// decoder: the test reconstructs it independently from the typed ops.
// ---------------------------------------------------------------------------

struct RunningState {
    prev: [u8; 4],
    index: [[u8; 4]; 64],
}

impl RunningState {
    fn new() -> Self {
        Self {
            prev: [0, 0, 0, 255],
            index: [[0, 0, 0, 0]; 64],
        }
    }

    /// Resolve the pixel a non-RUN op produces, given current `prev`.
    /// Returns `None` for `Run` / `Truncated` (handled separately).
    fn resolve(&self, op: &QoiOp) -> Option<[u8; 4]> {
        match *op {
            QoiOp::Rgb { r, g, b } => Some([r, g, b, self.prev[3]]),
            QoiOp::Rgba { r, g, b, a } => Some([r, g, b, a]),
            QoiOp::Index { index } => Some(self.index[index as usize]),
            QoiOp::Diff { dr, dg, db } => Some([
                self.prev[0].wrapping_add(dr as u8),
                self.prev[1].wrapping_add(dg as u8),
                self.prev[2].wrapping_add(db as u8),
                self.prev[3],
            ]),
            QoiOp::Luma { dg, dr_dg, db_dg } => {
                let dgi = dg as i32;
                let dr = dgi + dr_dg as i32;
                let db = dgi + db_dg as i32;
                Some([
                    self.prev[0].wrapping_add(dr as u8),
                    self.prev[1].wrapping_add(dg as u8),
                    self.prev[2].wrapping_add(db as u8),
                    self.prev[3],
                ])
            }
            QoiOp::Run { .. } | QoiOp::Truncated { .. } => None,
        }
    }

    /// Advance state by writing one resolved pixel (RUN advances by
    /// `length` copies of `prev`; everything else writes `cur`).
    fn advance_one(&mut self, cur: [u8; 4]) {
        self.index[qoi_hash(cur) as usize] = cur;
        self.prev = cur;
    }
}

/// Is `cur` encodable as DIFF relative to `prev` (alpha unchanged,
/// all three deltas in −2..=+1)?
fn diff_fits(prev: [u8; 4], cur: [u8; 4]) -> bool {
    if cur[3] != prev[3] {
        return false;
    }
    let dr = cur[0].wrapping_sub(prev[0]) as i8 as i32;
    let dg = cur[1].wrapping_sub(prev[1]) as i8 as i32;
    let db = cur[2].wrapping_sub(prev[2]) as i8 as i32;
    (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db)
}

/// Is `cur` encodable as LUMA relative to `prev` (alpha unchanged,
/// dg in −32..=31, dr-dg and db-dg in −8..=7)?
fn luma_fits(prev: [u8; 4], cur: [u8; 4]) -> bool {
    if cur[3] != prev[3] {
        return false;
    }
    let dr = cur[0].wrapping_sub(prev[0]) as i8 as i32;
    let dg = cur[1].wrapping_sub(prev[1]) as i8 as i32;
    let db = cur[2].wrapping_sub(prev[2]) as i8 as i32;
    let dr_dg = dr - dg;
    let db_dg = db - dg;
    (-32..=31).contains(&dg) && (-8..=7).contains(&dr_dg) && (-8..=7).contains(&db_dg)
}

// ---------------------------------------------------------------------------
// The canonical-form assertion: walk the encoder output, check each
// chunk is the highest-priority legal choice per the spec ladder.
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_canonical(seed: u32, label: &str, width: u32, height: u32, channels: u8, pixels: &[u8]) {
    let bytes = encode_qoi_full(width, height, channels, /* colorspace */ 0, pixels);
    let (_hdr, ops) = iter_ops(&bytes).unwrap_or_else(|e| {
        panic!("[{label}] seed={seed}: iter_ops rejected encoder output: {e:?}")
    });

    let mut st = RunningState::new();
    // Tracks the previous chunk so we can assert no two adjacent RUNs
    // (which a canonical encoder merges into one ≤62 chunk) and no
    // mergeable run-then-run-of-same-pixel slip through.
    let mut prev_was_run_of: Option<[u8; 4]> = None;
    let mut prev_run_len: u8 = 0;

    for op in ops {
        match op {
            QoiOp::Truncated { tag, .. } => {
                panic!(
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     encoder produced a truncated chunk (tag={tag:#04x}) — \
                     own output must never be mid-chunk"
                );
            }
            QoiOp::Run { length } => {
                // Spec: RUN length is 1..=62 (63/64 shadowed by RGB/RGBA).
                assert!(
                    (1..=62).contains(&length),
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     RUN length {length} out of spec range 1..=62"
                );
                let run_pixel = st.prev; // RUN replays the previous pixel.

                // Canonical form: a RUN of the same pixel must not be
                // split across two adjacent ≤62 chunks unless the first
                // was already maxed at 62. Two adjacent runs of the same
                // pixel where the first is < 62 means the encoder failed
                // to merge them.
                if let Some(p) = prev_was_run_of {
                    if p == run_pixel {
                        assert_eq!(
                            prev_run_len, 62,
                            "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                             two adjacent RUNs of the same pixel where the first \
                             ({prev_run_len}) was not maxed at 62 — should have merged"
                        );
                    }
                }

                // Advance state by `length` copies of `prev`. All land in
                // the same hash slot, so a single index store suffices.
                st.advance_one(run_pixel);
                prev_was_run_of = Some(run_pixel);
                prev_run_len = length;
                continue;
            }
            _ => {}
        }

        // Non-RUN chunk: resolve the pixel it produces.
        let cur = st
            .resolve(&op)
            .expect("non-RUN/Truncated op always resolves a pixel");

        // --- Ladder rung 1: RUN beats everything. ---
        // If this pixel equals `prev`, the encoder MUST have emitted a
        // RUN, not this chunk. (This also subsumes the spec's
        // no-redundant-consecutive-INDEX rule: a repeat pixel is a RUN.)
        assert_ne!(
            cur, st.prev,
            "[{label}] seed={seed} w={width} h={height} ch={channels}: \
             chunk {op:?} encodes a pixel equal to `prev` — a RUN was required"
        );

        // --- Ladder rung 2: INDEX beats DIFF/LUMA/RGB/RGBA. ---
        // If the running array already holds `cur` at hash(cur), the
        // encoder MUST have emitted an INDEX. So any non-INDEX chunk
        // here whose `cur` is in the index is a violation; and an INDEX
        // chunk must point at a slot that genuinely holds `cur`.
        let h = qoi_hash(cur) as usize;
        let in_index = st.index[h] == cur;
        match op {
            QoiOp::Index { index } => {
                assert_eq!(
                    index as usize, h,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     INDEX points at slot {index} but hash(cur)={h}"
                );
                assert!(
                    in_index,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     INDEX at slot {h} does not hold the produced pixel {cur:?}"
                );
            }
            _ => {
                assert!(
                    !in_index,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     chunk {op:?} produced pixel {cur:?} already present in the \
                     index at slot {h} — an INDEX was required"
                );
            }
        }

        // --- Ladder rungs 3-6: DIFF > LUMA > RGB/RGBA. ---
        let dfits = diff_fits(st.prev, cur);
        let lfits = luma_fits(st.prev, cur);
        match op {
            QoiOp::Index { .. } => { /* already validated above */ }
            QoiOp::Diff { .. } => {
                assert!(
                    dfits,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     DIFF chunk {op:?} but deltas don't fit DIFF range"
                );
            }
            QoiOp::Luma { .. } => {
                assert!(
                    !dfits,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     LUMA chunk {op:?} where a DIFF would have fit"
                );
                assert!(
                    lfits,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     LUMA chunk {op:?} but deltas don't fit LUMA range"
                );
            }
            QoiOp::Rgb { .. } => {
                // RGB is legal only when alpha unchanged but neither
                // DIFF nor LUMA fit.
                assert_eq!(
                    cur[3], st.prev[3],
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     RGB chunk but alpha changed — RGBA was required"
                );
                assert!(
                    !dfits && !lfits,
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     RGB chunk {op:?} where a DIFF/LUMA would have fit"
                );
            }
            QoiOp::Rgba { .. } => {
                // RGBA is only required when alpha changed. (An RGBA
                // with unchanged alpha would be wasteful, but the
                // encoder only reaches RGBA on the alpha-changed path,
                // so assert that.)
                assert_ne!(
                    cur[3], st.prev[3],
                    "[{label}] seed={seed} w={width} h={height} ch={channels}: \
                     RGBA chunk where alpha was unchanged — RGB (or smaller) \
                     was required"
                );
            }
            QoiOp::Run { .. } | QoiOp::Truncated { .. } => unreachable!(),
        }

        st.advance_one(cur);
        prev_was_run_of = None;
        prev_run_len = 0;
    }
}

// ---------------------------------------------------------------------------
// Sweeps — one per generator shape, mirroring property_sweep.rs.
// ---------------------------------------------------------------------------

const SWEEP_ITERATIONS: u32 = 200;
const MAX_DIM: u32 = 64;

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

        let n = (width as usize) * (height as usize);
        let pixels = gen_pixels(&mut rng, n, channels);

        assert_canonical(seed, label, width, height, channels, &pixels);
    }
}

#[test]
fn canonical_random_pixels() {
    sweep("random", 0x0c00_0001, pixels_random);
}

#[test]
fn canonical_smooth_deltas() {
    sweep("smooth", 0x0c00_0002, pixels_smooth);
}

#[test]
fn canonical_run_heavy() {
    sweep("run_heavy", 0x0c00_0003, pixels_run_heavy);
}

#[test]
fn canonical_palette() {
    sweep("palette", 0x0c00_0004, pixels_palette);
}

#[test]
fn canonical_alpha_churn() {
    sweep("alpha_churn", 0x0c00_0005, pixels_alpha_churn);
}

// ---------------------------------------------------------------------------
// Hand-built streams pinning the spec's named canonical-form edges.
// ---------------------------------------------------------------------------

/// A solid fill longer than 62 pixels must encode as one seed chunk
/// then maxed RUN chunks — each preceding RUN exactly 62 — never a
/// short-then-short pair. Walk the chunks and confirm the only ops are
/// the seed + RUNs, every RUN but possibly the last is 62.
#[test]
fn canonical_solid_fill_runs_are_maxed() {
    for &(w, ch) in &[
        (125u32, 4u8),
        (125, 3),
        (200, 4),
        (63, 3),
        (62, 4),
        (124, 3),
    ] {
        let n = w as usize;
        let mut pixels = Vec::with_capacity(n * ch as usize);
        let (r, g, b, a) = (10u8, 20, 30, 40);
        for _ in 0..n {
            pixels.push(r);
            pixels.push(g);
            pixels.push(b);
            if ch == 4 {
                pixels.push(a);
            }
        }
        // Run the full ladder check first.
        assert_canonical(0xfeed, "solid", w, 1, ch, &pixels);

        // Then specifically: collect RUN lengths; all but the last must
        // be 62.
        let bytes = encode_qoi_full(w, 1, ch, 0, &pixels);
        let (_h, ops) = iter_ops(&bytes).unwrap();
        let runs: Vec<u8> = ops
            .filter_map(|op| match op {
                QoiOp::Run { length } => Some(length),
                _ => None,
            })
            .collect();
        if runs.len() > 1 {
            for (i, &len) in runs.iter().enumerate() {
                if i + 1 < runs.len() {
                    assert_eq!(
                        len, 62,
                        "solid w={w} ch={ch}: non-final RUN #{i} length {len} != 62 \
                         (canonical encoder maxes intermediate runs)"
                    );
                }
            }
        }
    }
}

/// The spec's explicit constraint: no two consecutive INDEX chunks to
/// the same slot. Build a stream that revisits the same palette colour
/// after an intervening different colour, so each revisit is a genuine
/// INDEX (not a RUN), and confirm no adjacent INDEX pair shares a slot.
#[test]
fn canonical_no_consecutive_same_index() {
    // ABAB… two colours: every other pixel is an INDEX hit on the
    // *other* colour, never the same one twice in a row, and never a
    // RUN (adjacent pixels always differ).
    let n = 200usize;
    let mut pixels = Vec::with_capacity(n * 4);
    let a = [200u8, 50, 90, 255];
    let b = [40u8, 160, 210, 255];
    for i in 0..n {
        let p = if i % 2 == 0 { a } else { b };
        pixels.extend_from_slice(&p);
    }
    assert_canonical(0xabab, "abab", n as u32, 1, 4, &pixels);

    let bytes = encode_qoi_full(n as u32, 1, 4, 0, &pixels);
    let (_h, ops) = iter_ops(&bytes).unwrap();
    let mut last_index: Option<u8> = None;
    for op in ops {
        match op {
            QoiOp::Index { index } => {
                if let Some(prev) = last_index {
                    assert_ne!(
                        prev, index,
                        "two consecutive INDEX chunks to the same slot {index} — \
                         spec forbids this (a RUN must be used instead)"
                    );
                }
                last_index = Some(index);
            }
            _ => last_index = None,
        }
    }
}
