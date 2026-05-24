#![no_main]

//! Decode arbitrary fuzz-supplied bytes through `parse_qoi`. The
//! decoder must always return a `Result` and never panic / abort /
//! OOM, regardless of how malformed the input is.
//!
//! The contract under test is purely that the call *returns*: a
//! malformed stream yields `Err(QoiError::…)`, a well-formed one
//! yields `Ok(QoiImage)`, and neither path may panic, integer-overflow
//! (in a debug build), index out of bounds, or try to allocate an
//! attacker-controlled pixel buffer the size of the claimed
//! `width * height * channels`. The return value is intentionally
//! discarded.

use libfuzzer_sys::fuzz_target;
use oxideav_qoi::parse_qoi;

fuzz_target!(|data: &[u8]| {
    let _ = parse_qoi(data);
});
