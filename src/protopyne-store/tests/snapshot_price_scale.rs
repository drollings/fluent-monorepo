//! Snapshot price-scale representability: refuse, never round.
//!
//! The sealed artifact pins the market's `price_scale` into the Arrow decimal
//! schema. A value needing more digits than that scale **refuses** the seal —
//! it is never rounded and never truncated — and this suite reuses the same
//! `(scale, value)` corpus the SQLite append path is calibrated against, so
//! the working tape and its sealed artifact cannot disagree about what fits.

use super::*;
use crate::catalog::{AssetClass, Market, PriceScale};
use crate::key::SymbolId;
use crate::sqlite_tape::test_scale::{FIT, REFUSE};
use protopyne_types::Timeframe;

/// A market row at `scale`, for a direct writer call that bypasses `append`
/// (the append path refuses unrepresentable rows before they are stored, so
/// the writer must be exercised on its own).
fn market_at(scale: u32, ordinal: u32) -> Market {
    Market {
        symbol_id: SymbolId(ordinal + 1),
        symbol: format!("SCALE{scale}_{ordinal}"),
        venue: "test-venue".to_owned(),
        asset_class: AssetClass::Stock,
        price_scale: PriceScale::of(scale).expect("valid scale"),
        timeframe: Timeframe::M1,
        time_zone: "UTC".to_owned(),
    }
}

fn flat_bar(value: &str) -> protopyne_types::Bar {
    crate::commit::test_tapes::ohlc(0, value, value, value, value)
}

#[test]
fn fitting_values_seal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = SealOptions::new().build();
    for (i, case) in FIT.iter().enumerate() {
        let market = market_at(case.scale, i as u32);
        let key = TapeKey {
            symbol_id: market.symbol_id,
            timeframe: Timeframe::M1,
        };
        let writer = SnapshotWriter::new(dir.path(), options);
        writer
            .write(key, &market, &[flat_bar(case.value)])
            .unwrap_or_else(|e| panic!("'{}' must seal: {e:?}", case.name));
    }
}

#[test]
fn unrepresentable_values_refuse_the_seal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = SealOptions::new().build();
    for (i, case) in REFUSE.iter().enumerate() {
        let market = market_at(case.scale, 10_000 + i as u32);
        let key = TapeKey {
            symbol_id: market.symbol_id,
            timeframe: Timeframe::M1,
        };
        let writer = SnapshotWriter::new(dir.path(), options);
        match writer.write(key, &market, &[flat_bar(case.value)]) {
            Err(StoreError::PriceScale {
                symbol,
                value,
                scale,
            }) => {
                assert_eq!(symbol, market.symbol, "'{}' names the market", case.name);
                assert_eq!(
                    value,
                    case.value.parse::<rust_decimal::Decimal>().expect("parses"),
                    "'{}' names the price",
                    case.name
                );
                assert_eq!(scale, case.scale, "'{}' names the scale", case.name);
            }
            other => panic!("'{}' must refuse, got {other:?}", case.name),
        }
    }
}
