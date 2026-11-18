//! Heckbert "nice number" ticks for axes.
//!
//! Generic over the axis: given a value window and a tick budget, pick a
//! round step (1/2/2.5/5 × 10ⁿ) whose grid covers the window within budget.
//! Time-axis weighting stays caller-side; this module only spaces values.

use serde::{Deserialize, Serialize};

/// One axis tick: value, label, and hierarchy weight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tick {
    /// Tick value in axis units.
    pub value: f64,
    /// Preformatted label for the paint edge.
    pub label: String,
    /// Hierarchy weight (0 minor … 2 major) for staged rendering.
    pub weight: u8,
}

/// Pick round tick values covering `[min, max]` within `max_ticks`.
///
/// Task value (legibility): the step/density choice serves readable axes,
/// never a decision. Returns an empty vector for degenerate windows
/// (non-finite bounds, `min >= max`, `max_ticks == 0`) — the caller renders
/// no ticks rather than a misleading grid. Never panics.
#[must_use]
pub fn nice_ticks(min: f64, max: f64, max_ticks: usize) -> Vec<Tick> {
    if !min.is_finite() || !max.is_finite() || min >= max || max_ticks == 0 {
        return Vec::new();
    }
    let max_ticks = max_ticks.min(64);
    let raw_step = (max - min) / max_ticks as f64;
    let step = nice_step(raw_step);
    if !step.is_finite() || step <= 0.0 {
        return Vec::new();
    }
    let start = (min / step).floor() * step;
    let precision = precision_for_step(step);
    let mut ticks = Vec::new();
    // Closed form `start + i*step`: accumulation (`value += step`) drifted a
    // few ULPs per tick, so a republished window could shift a label.
    // The `max_ticks * 4` bound is the runaway guard when `step` underflows
    // the window scale; the `max_ticks * 2` cap is the hard budget.
    for i in 0..=(max_ticks * 4) {
        let value = start + i as f64 * step;
        if value > max {
            break;
        }
        if value >= min {
            ticks.push(Tick {
                value,
                label: format!("{value:.precision$}"),
                weight: weight_for(value, step),
            });
        }
        if ticks.len() > max_ticks * 2 {
            break;
        }
    }
    ticks.truncate(max_ticks * 2);
    ticks
}

/// Round `raw` up to the next 1/2/2.5/5/10 multiple of its decade.
fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return f64::NAN;
    }
    let decade = 10_f64.powf(raw.log10().floor());
    let fraction = raw / decade;
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 2.5 {
        2.5
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_fraction * decade
}

/// Decimals needed to print one step without trailing noise.
fn precision_for_step(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 2;
    }
    let places = -step.log10().floor() as i32;
    places.clamp(0, 8) as usize
}

/// Coarse hierarchy: multiples of 5 steps read major.
fn weight_for(value: f64, step: f64) -> u8 {
    if step == 0.0 || !step.is_finite() {
        return 0;
    }
    let multiple = (value / step).round();
    if (multiple % 5.0).abs() < 1e-9 {
        2
    } else {
        u8::from((multiple % 2.0).abs() < 1e-9)
    }
}

#[cfg(test)]
#[path = "../tests/ticks.rs"]
mod tests;
