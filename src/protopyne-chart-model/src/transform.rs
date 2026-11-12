//! Pure bar transforms: time-based smoothing and price-based bricks.
//!
//! Both functions map `&[Bar] -> Vec<Bar>` over validated closed bars and
//! emit owned bars (never substitutes for invalid input — callers validate
//! first via [`protopyne_types::Bar::validate`]). Money stays `Decimal`
//! throughout; no display float appears here.

use protopyne_types::Bar;
use rust_decimal::Decimal;

use crate::convert::ChartRow;
use crate::error::ChartModelError;

/// Smoothed heikin-ashi candles over closed bars.
///
/// `ha_close = (o + h + l + c) / 4`; `ha_open` seeds at `(o + c) / 2` of the
/// first bar then recurs as `(prev_open + prev_close) / 2`; wicks stretch to
/// cover the source high/low. Timestamps, volumes, and adjustments carry
/// over unchanged.
#[must_use]
/// Smoothed heikin-ashi candles over closed bars.
/// 
/// `ha_close = (o + h + l + c) / 4`; `ha_open` seeds at `(o + c) / 2` of the
/// first bar then recurs as `(prev_open + prev_close) / 2`; wicks stretch to
/// cover the source high/low. Timestamps, volumes, and adjustments carry
/// over unchanged.
pub fn heikin_ashi(bars: &[Bar]) -> Vec<Bar> {
    let mut out = Vec::with_capacity(bars.len());
    let mut prev_open: Option<Decimal> = None;
    let mut prev_close: Option<Decimal> = None;
    for bar in bars {
        let ha_close = (bar.o + bar.h + bar.l + bar.c) / Decimal::from(4);
        let ha_open = match (prev_open, prev_close) {
            (Some(open), Some(close)) => (open + close) / Decimal::from(2),
            _ => (bar.o + bar.c) / Decimal::from(2),
        };
        let ha_high = bar.h.max(ha_open).max(ha_close);
        let ha_low = bar.l.min(ha_open).min(ha_close);
        prev_open = Some(ha_open);
        prev_close = Some(ha_close);
        out.push(
            Bar::new()
                .ts(bar.ts)
                .o(ha_open)
                .h(ha_high)
                .l(ha_low)
                .c(ha_close)
                .volume(bar.volume)
                .maybe_adj(bar.adj.clone())
                .build(),
        );
    }
    out
}

/// Heikin-Ashi over display rows: the same recurrence as [`heikin_ashi`],
/// seeded once from row zero. Display smoothing only, never a decision
/// input — the sole home of the row-space recurrence, so paint and any
/// future caller share one seeding instead of re-seeding per window
/// (windowing first would miscompute every open past the seam).
#[must_use]
/// Heikin-Ashi over display rows: the same recurrence as [`heikin_ashi`],
/// seeded once from row zero. Display smoothing only, never a decision
/// input — the sole home of the row-space recurrence, so paint and any
/// future caller share one seeding instead of re-seeding per window
/// (windowing first would miscompute every open past the seam).
pub fn heikin_rows_of(rows: &[ChartRow]) -> Vec<ChartRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut prev_open: Option<f64> = None;
    let mut prev_close: Option<f64> = None;
    for row in rows {
        let ha_close = (row.open + row.high + row.low + row.close) / 4.0;
        let ha_open = match (prev_open, prev_close) {
            (Some(open), Some(close)) => f64::midpoint(open, close),
            _ => f64::midpoint(row.open, row.close),
        };
        let ha_high = row.high.max(ha_open).max(ha_close);
        let ha_low = row.low.min(ha_open).min(ha_close);
        out.push(ChartRow {
            time: row.time,
            open: ha_open,
            high: ha_high,
            low: ha_low,
            close: ha_close,
            volume: row.volume,
        });
        prev_open = Some(ha_open);
        prev_close = Some(ha_close);
    }
    out
}

/// Fixed-brick renko series over bar closes.
///
/// Walks closes from the first bar: every `brick_size` move from the last
/// brick edge emits one brick (open at the prior edge, close at the new
/// edge, wicks collapsed to the brick body). Timestamps come from the bar
/// that triggered the brick; volume is zero (bricks aggregate price, not
/// flow). A non-positive brick size is refused fail-closed.
pub fn renko_bricks(bars: &[Bar], brick_size: Decimal) -> Result<Vec<Bar>, ChartModelError> {
    if brick_size <= Decimal::ZERO {
        return Err(ChartModelError::InvalidParam {
            what: "brick_size".to_owned(),
            reason: "must be positive".to_owned(),
        });
    }
    if bars.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut edge = bars[0].c;
    for bar in &bars[1..] {
        while (bar.c - edge).abs() >= brick_size {
            let direction = if bar.c > edge {
                Decimal::ONE
            } else {
                Decimal::NEGATIVE_ONE
            };
            let next = edge + direction * brick_size;
            let (open, close) = if direction > Decimal::ZERO {
                (edge, next)
            } else {
                (next, edge)
            };
            // Renko bricks are direction bodies: open/close are the edges,
            // wicks collapse onto the body so no second high/low math lives here.
            let (high, low) = (close.max(open), close.min(open));
            out.push(
                Bar::new()
                    .ts(bar.ts)
                    .o(open)
                    .h(high)
                    .l(low)
                    .c(close)
                    .volume(0_u64)
                    .maybe_adj(None)
                    .build(),
            );
            edge = next;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "../tests/transform.rs"]
mod tests;