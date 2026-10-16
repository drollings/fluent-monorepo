//! Price-scale calibration: the round-trip is the control group.
//!
//! For a table of `(scale, value)` pairs spanning sub-cent instruments,
//! crypto-style scales, whole-dollar instruments, and the `Decimal`
//! maximum, every fitting value must write and read back byte-identically
//! (`normalize().to_string()` equality) — that is the control group, and
//! it comes first. Pairs needing more digits than the market's scale must
//! refuse — counted in `append`, named by `repair_one` — and must not
//! appear in the store. The ulp pair pins the boundary exactly: one digit
//! short of the scale round-trips, one digit past it refuses.
//!
//! The tally mirrors the kernels parity idiom: counts plus every mismatch
//! named in one message, so a failure reports what diverged instead of
//! stopping at the first case.

use super::*;
use protopyne_types::Timeframe;

use crate::catalog::PriceScale;
use crate::commit::test_tapes::{bar, ohlc};
use crate::fold::FoldPolicy;
use crate::range::TimeRange;
use crate::catalog::test_scale::{FIT, REFUSE, canonical};
use crate::tape::test_setup::{VENUE, m1_key, memory_tape, with_registry_caps};

/// Tally of the round-trip sweep: counts plus every mismatch named.
struct ScaleReport {
    compared: usize,
    matched: usize,
    mismatches: Vec<String>,
}

fn assert_scale_parity(report: &ScaleReport, context: &str) {
    assert!(
        report.matched == report.compared,
        "{context}: {} mismatch(es) over {} compared{}",
        report.mismatches.len(),
        report.compared,
        report.mismatches.iter().fold(String::new(), |mut acc, m| {
            acc.push_str("; ");
            acc.push_str(m);
            acc
        }),
    );
}

#[tokio::test]
async fn fitting_values_round_trip_byte_identically() {
    with_registry_caps(async {
        let tape = memory_tape();
        let mut report = ScaleReport {
            compared: 0,
            matched: 0,
            mismatches: Vec::new(),
        };
        for (i, case) in FIT.iter().enumerate() {
            let symbol = format!("FIT{}_{}", case.scale, i);
            let symbol_id = tape
                .register(
                    &symbol,
                    VENUE,
                    crate::catalog::AssetClass::Stock,
                    PriceScale::of(case.scale).expect("calibration scale valid"),
                    "UTC",
                    protopyne_types::Timeframe::M1,
                )
                .expect("registration succeeds");
            let key = m1_key(symbol_id);
            let probe = bar(0, case.value);
            let outcome = tape
                .append(key, &[probe], WritePolicy::InsertIfAbsent)
                .expect("fitting batch appends");
            report.compared += 1;
            if outcome
                != (AppendOutcome {
                    inserted: 1,
                    conflicts: 0,
                    rejected: 0,
                })
            {
                report.mismatches.push(format!(
                    "{}: expected full insert, got {outcome:?}",
                    case.name
                ));
                continue;
            }
            let series = tape
                .read(&ReadRequest {
                    symbol: symbol.clone(),
                    venue: VENUE.to_owned(),
                    timeframe: Timeframe::M1,
                    range: TimeRange::unbounded(),
                    limit: Some(1),
                    policy: FoldPolicy::ExtendToFullBuckets,
                })
                .expect("read back");
            let want = canonical(case.value);
            let got: Vec<String> = series
                .bars
                .iter()
                .flat_map(|b| [b.o, b.h, b.l, b.c])
                .map(|p| p.normalize().to_string())
                .collect();
            if series.bars.len() == 1 && got.iter().all(|p| *p == want) {
                report.matched += 1;
            } else {
                report.mismatches.push(format!(
                    "{}: expected four legs of {want}, got {got:?}",
                    case.name
                ));
            }
        }
        assert_scale_parity(&report, "price-scale round-trip");
    })
    .await;
}

#[tokio::test]
async fn unrepresentable_values_refuse_and_leave_no_row() {
    with_registry_caps(async {
        let tape = memory_tape();
        for (i, case) in REFUSE.iter().enumerate() {
            let symbol = format!("REFUSE{}_{}", case.scale, i);
            let symbol_id = tape
                .register(
                    &symbol,
                    VENUE,
                    crate::catalog::AssetClass::Stock,
                    PriceScale::of(case.scale).expect("calibration scale valid"),
                    "UTC",
                    protopyne_types::Timeframe::M1,
                )
                .expect("registration succeeds");
            let key = m1_key(symbol_id);
            let probe = bar(0, case.value);
            let outcome = tape
                .append(
                    key,
                    std::slice::from_ref(&probe),
                    WritePolicy::InsertIfAbsent,
                )
                .expect("refusal counts, it does not error the batch");
            assert_eq!(
                outcome,
                AppendOutcome {
                    inserted: 0,
                    conflicts: 0,
                    rejected: 1
                },
                "{}: the row is counted, never stored",
                case.name
            );
            let err = tape
                .read(&ReadRequest {
                    symbol: symbol.clone(),
                    venue: VENUE.to_owned(),
                    timeframe: Timeframe::M1,
                    range: TimeRange::unbounded(),
                    limit: Some(1),
                    policy: FoldPolicy::ExtendToFullBuckets,
                })
                .expect_err("an empty tape refuses");
            assert!(
                matches!(err, StoreError::Plan(PlanError::UncoveredRange { .. })),
                "{}: refused rows leave no row to plan against: {err:?}",
                case.name
            );
            assert_eq!(
                tape.coverage(key).expect("coverage answers"),
                None,
                "{}: an empty tape covers nothing",
                case.name
            );
            // The named refusal: the explicit correction path returns the
            // error with its symbol, value, and scale attached.
            match tape.repair_one(key, probe.ts.timestamp_millis(), &probe) {
                Err(StoreError::PriceScale {
                    symbol: got_symbol,
                    value,
                    scale,
                }) => {
                    assert_eq!(got_symbol, symbol, "{}: names the market", case.name);
                    assert_eq!(
                        value,
                        case.value.parse::<rust_decimal::Decimal>().expect("parses"),
                        "{}: names the price",
                        case.name
                    );
                    assert_eq!(scale, case.scale, "{}: names the scale", case.name);
                }
                other => panic!(
                    "{}: expected a PriceScale refusal, got {other:?}",
                    case.name
                ),
            }
            let err = tape
                .read(&ReadRequest {
                    symbol,
                    venue: VENUE.to_owned(),
                    timeframe: Timeframe::M1,
                    range: TimeRange::unbounded(),
                    limit: Some(1),
                    policy: FoldPolicy::ExtendToFullBuckets,
                })
                .expect_err("an empty tape refuses");
            assert!(
                matches!(err, StoreError::Plan(PlanError::UncoveredRange { .. })),
                "{}: the refused repair stores nothing to plan against: {err:?}",
                case.name
            );
        }
    })
    .await;
}

#[tokio::test]
async fn refusal_names_the_first_offending_leg() {
    with_registry_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                "MIXED",
                VENUE,
                crate::catalog::AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        // Open, high, and low fit and the bar is valid; only the close
        // runs past the scale.
        let probe = ohlc(0, "100.55", "100.56", "100.54", "100.555");
        let outcome = tape
            .append(
                key,
                std::slice::from_ref(&probe),
                WritePolicy::InsertIfAbsent,
            )
            .expect("refusal counts");
        assert_eq!(outcome.rejected, 1, "one row refused");
        assert_eq!(outcome.inserted, 0, "nothing stored");
        match tape.repair_one(key, probe.ts.timestamp_millis(), &probe) {
            Err(StoreError::PriceScale { value, .. }) => assert_eq!(
                value,
                "100.555".parse::<rust_decimal::Decimal>().expect("parses"),
                "the refusal names the close, not a fitting leg"
            ),
            other => panic!("expected a PriceScale refusal, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn scale_construction_is_validated_at_both_entries() {
    assert!(PriceScale::of(28).is_ok(), "28 is the Decimal maximum");
    assert!(PriceScale::of(29).is_err(), "29 digits do not exist");
    assert_eq!(PriceScale::new().build().digits, 2, "equity-cents default");
    // The builder form is infallible, so validation is a separate,
    // enforced step: an unvalidated builder value cannot register.
    let unvalidated = PriceScale::new().digits(29).build();
    assert!(protopyne_types::Validate::validate(&unvalidated).is_err());
    with_registry_caps(async {
        let tape = memory_tape();
        match tape.register(
            "WIDE",
            VENUE,
            crate::catalog::AssetClass::Stock,
            unvalidated,
            "UTC",
            protopyne_types::Timeframe::M1,
        ) {
            Err(StoreError::Config(_)) => {}
            other => panic!("unrepresentable scale refuses at registration, got {other:?}"),
        }
    })
    .await;
}
