//! Fill calibration: must-fill vs. must-NOT-fill sets plus the
//! slippage-sweep report that freezes the default tolerance.
//!
//! Protocol: the must-fill set proves liquid marketable orders trade (a fill
//! model that never fills rehearses nothing); the must-NOT-fill control set
//! proves illiquid, wide, stale, and non-marketable flow is refused with
//! reasons. The sweep then prices tolerance: fill-rate against the adverse
//! cost proxy, before the default ships.

use crate::*;
use chrono::{TimeZone, Utc};
use protopyne_types::Money;
use rust_decimal::Decimal;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn quote_at(bid: &str, ask: &str, size: u64) -> Quote {
    Quote::new()
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(size)
        .ask_size(size)
        .ts(ts())
        .build()
}

fn liquid() -> Quote {
    quote_at("600.00", "600.10", 50)
}

fn intent_for(limit: &str, qty: u32) -> OrderIntent {
    OrderIntent::new()
        .client_id(format!("cal-{limit}-{qty}"))
        .proposal_id("cal".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(qty)
        .limit_price(money(limit))
        .build()
}

fn venue_with(slippage_bps: u32) -> PaperVenue {
    PaperVenue::new(
        PaperConfig::new()
            .slippage(Slippage::new().bps(slippage_bps).build())
            .build(),
    )
}

/// Adverse-selection proxy for one fill: basis points paid beyond mid.
/// Positive means the taker paid over a fair split; lower is better.
fn adverse_bps(fill_price: Money, mid: Money) -> f64 {
    use rust_decimal::prelude::ToPrimitive as _;
    let fill = fill_price.as_decimal().to_f64().unwrap_or(f64::NAN);
    let mid = mid.as_decimal().to_f64().unwrap_or(f64::NAN);
    (fill - mid) / mid * 10_000.0
}

#[tokio::test]
async fn liquid_marketable_orders_must_fill() {
    let venue = venue_with(DEFAULT_SLIPPAGE_BPS);
    for (limit, qty) in [("600.50", 1), ("600.50", 2), ("601.00", 10)] {
        let record = venue
            .submit(intent_for(limit, qty), &liquid(), now())
            .await
            .unwrap_or_else(|e| panic!("liquid {limit}x{qty} must fill: {e}"));
        assert_eq!(record.filled_quantity, qty);
    }
}

#[tokio::test]
async fn illiquid_wide_stale_and_resting_flow_must_not_fill() {
    let venue = venue_with(DEFAULT_SLIPPAGE_BPS);
    let cases: Vec<(&str, Quote, &str, u32)> = vec![
        ("wide spread", quote_at("600.00", "660.00", 50), "665.00", 2),
        ("thin size", quote_at("600.00", "600.10", 1), "600.50", 2),
        ("oversize", quote_at("600.00", "600.10", 50), "600.50", 500),
        ("would rest", quote_at("600.00", "600.10", 50), "599.00", 2),
        ("crossed", quote_at("600.20", "600.10", 50), "600.50", 2),
    ];
    for (label, quote, limit, qty) in cases {
        let err = venue
            .submit(intent_for(limit, qty), &quote, now())
            .await
            .expect_err(&format!("{label} must not fill"));
        assert!(
            matches!(err, VenueError::Refused(_)),
            "{label} refused with reason, got {err:?}"
        );
    }
    let stale_at = Utc.with_ymd_and_hms(2026, 6, 1, 13, 0, 0).unwrap();
    let err = venue
        .submit(intent_for("600.50", 2), &liquid(), stale_at)
        .await
        .expect_err("stale must not fill");
    assert!(matches!(err, VenueError::Refused(_)));
}

#[tokio::test]
async fn slippage_sweep_freezes_the_default_tolerance() {
    let mid = liquid().mid().expect("mid");
    let mut last_cost = f64::NEG_INFINITY;
    let mut default_cost = f64::NAN;
    for bps in [0_u32, 5, 10, 25, 50, 100] {
        let venue = venue_with(bps);
        let mut fills = 0_usize;
        let mut cost_sum = 0.0;
        for (limit, qty) in [("600.20", 1), ("600.25", 2), ("600.30", 10)] {
            let record = venue
                .submit(intent_for(limit, qty), &liquid(), now())
                .await
                .expect("liquid fills at every tolerance");
            fills += 1;
            cost_sum += adverse_bps(record.avg_price.expect("priced"), mid);
        }
        assert_eq!(fills, 3, "fill-rate stays 1.0: fills are liquidity-gated");
        let mean_cost = cost_sum / fills as f64;
        assert!(
            mean_cost >= last_cost,
            "adverse cost must not fall as tolerance rises (bps={bps})"
        );
        last_cost = mean_cost;
        if bps == DEFAULT_SLIPPAGE_BPS {
            default_cost = mean_cost;
        }
    }
    assert_eq!(DEFAULT_SLIPPAGE_BPS, 10, "default stays 10 bps");
    assert!(
        default_cost <= 5.0,
        "default tolerance keeps adverse cost under 5 bps of price, got {default_cost:.2}"
    );
}
