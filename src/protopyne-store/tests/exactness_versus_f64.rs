//! Exactness versus `f64`: the store matches `Decimal` wherever `f64`
//! differs.
//!
//! Over a generated grid of `(scale, value)` pairs — integers across the
//! 2^53 boundary where binary floating point stops representing ones,
//! fractions at exactly each scale, the `Decimal` maximum — every fitting
//! value must read back as its `Decimal` text, and the sweep must contain
//! at least one case where the `f64` print differs. That divergence is
//! counted and listed, never assumed: if a platform ever printed these
//! values exactly in binary floating point, the suite would fail on the
//! missing divergence rather than pass vacuously. The grid is generated,
//! so widening a digit range extends the proof without writing a new test.
//!
//! The `f64` leg uses the raw decimal-to-binary conversion directly. The
//! store itself never converts: display `f64` appears only at the
//! chart-model boundary, and no store column is that place.

use super::*;
use protopyne_types::Timeframe;
use rust_decimal::prelude::ToPrimitive;

use crate::catalog::PriceScale;
use crate::commit::test_tapes::bar;
use crate::fold::FoldPolicy;
use crate::range::TimeRange;
use crate::tape::test_setup::{VENUE, m1_key, memory_tape, with_registry_caps};

/// Scales under test: whole-dollar, cents, crypto, and the Decimal maximum.
const SCALES: &[u32] = &[0, 2, 8, 28];

/// Integers every scale must carry exactly — including past 2^53, where
/// binary floating point stops representing ones.
const BEYOND_F64_ONES: &[&str] = &["9007199254740993", "79228162514264337593543950335"];

/// Generated values fitting `scale`: small integers, powers of ten across
/// the float boundary, the smallest fraction the scale names, and the
/// integers binary floating point cannot carry.
fn sweep_values(scale: u32) -> Vec<String> {
    let mut values = vec!["0".to_owned(), "7".to_owned(), "-9".to_owned()];
    for exp in 0..20 {
        values.push(format!("1{}", "0".repeat(exp)));
    }
    if scale > 0 {
        values.push(format!("0.{}1", "0".repeat(scale as usize - 1)));
    }
    values.extend(BEYOND_F64_ONES.iter().map(|s| (*s).to_owned()));
    values
}

/// Tally of the sweep: every stored price compared as text, every `f64`
/// divergence listed. Mirrors the kernels parity shape — counts plus each
/// mismatch named in one message.
struct ExactnessReport {
    compared: usize,
    stored_exact: usize,
    store_mismatches: Vec<String>,
    f64_diverged: Vec<(String, String)>,
}

fn canonical(text: &str) -> String {
    text.parse::<rust_decimal::Decimal>()
        .expect("sweep value parses")
        .normalize()
        .to_string()
}

#[tokio::test]
async fn store_matches_decimal_wherever_f64_differs() {
    with_registry_caps(async {
        let tape = memory_tape();
        let mut report = ExactnessReport {
            compared: 0,
            stored_exact: 0,
            store_mismatches: Vec::new(),
            f64_diverged: Vec::new(),
        };
        for scale in SCALES {
            let values = sweep_values(*scale);
            let symbol = format!("EXACT{scale}");
            let symbol_id = tape
                .register(
                    &symbol,
                    VENUE,
                    crate::catalog::AssetClass::Stock,
                    PriceScale::of(*scale).expect("sweep scale valid"),
                    "UTC",
                    protopyne_types::Timeframe::M1,
                )
                .expect("registration succeeds");
            let key = m1_key(symbol_id);
            let batch: Vec<protopyne_types::Bar> = values
                .iter()
                .enumerate()
                .map(|(step, value)| bar(step as i64, value))
                .collect();
            let outcome = tape
                .append(key, &batch, WritePolicy::InsertIfAbsent)
                .expect("fitting sweep appends");
            assert_eq!(
                outcome.rejected, 0,
                "scale {scale}: the whole generated grid fits, nothing refused"
            );
            let series = tape
                .read(&ReadRequest {
                    symbol: symbol.clone(),
                    venue: VENUE.to_owned(),
                    timeframe: Timeframe::M1,
                    range: TimeRange::unbounded(),
                    limit: Some(values.len()),
                    policy: FoldPolicy::ExtendToFullBuckets,
                })
                .expect("read back");
            assert_eq!(
                series.bars.len(),
                values.len(),
                "scale {scale}: nothing lost"
            );
            for (stored, text) in series.bars.iter().zip(&values) {
                let want = canonical(text);
                report.compared += 4;
                for (leg, price) in [stored.o, stored.h, stored.l, stored.c].iter().enumerate() {
                    if price.normalize().to_string() == want {
                        report.stored_exact += 1;
                    } else {
                        report.store_mismatches.push(format!(
                            "scale {scale} leg {leg} of {text}: stored {}",
                            price.normalize()
                        ));
                    }
                }
                // The documented divergence: what binary floating point
                // would have kept instead.
                if let Some(flat) = text
                    .parse::<rust_decimal::Decimal>()
                    .expect("sweep value parses")
                    .to_f64()
                {
                    let printed = format!("{flat}");
                    if printed != want {
                        report.f64_diverged.push((want, printed));
                    }
                }
            }
        }
        assert!(
            report.stored_exact == report.compared,
            "exactness: {} store mismatch(es) over {} compared{}",
            report.store_mismatches.len(),
            report.compared,
            report
                .store_mismatches
                .iter()
                .fold(String::new(), |mut acc, m| {
                    acc.push_str("; ");
                    acc.push_str(m);
                    acc
                }),
        );
        assert!(
            !report.f64_diverged.is_empty(),
            "exactness vacuous: no sweep value diverged under f64"
        );
    })
    .await;
}
