#![no_main]

//! Fuzz the boundary walker over arbitrary bytes: the same shapes the
//! `boundary_contract` proptest suite pins (never panic, in any mode), with
//! libFuzzer's coverage guidance instead of proptest's generation.
//!
//! Manual only: `cd src/fluent-wvr/fuzz && cargo +nightly fuzz run repair`.
//! Never runs in any gate.

use fluent_wvr::prelude::*;
use libfuzzer_sys::fuzz_target;

const FIELDS: &[&str] = &["action", "port", "mode", "note"];

fuzz_target!(|data: &[u8]| {
    let raw = String::from_utf8_lossy(data);
    let lenient = BoundaryOptions::lenient();
    let _ = repair_boundary(&raw, &lenient);
    let schema = BoundaryOptions::for_schema(FIELDS);
    let _ = repair_boundary(&raw, &schema);
    let _ = extract_members(&raw, &schema);
});
