//! Trading oscillator vocabulary plus the frozen normalization table.
//!
//! The squash mechanism (offset/shift, steepness, band half-width) is plain
//! display math, but the kind enum and its constants mirror the reference
//! `normalizeOsc` (`_libMAs.pine`), so they live here with the rest of the
//! trading-domain display truth — not in the generic paint scaffolding.
//! Every function is `f64`-in/`f64`-out with warmup holes preserved
//! element-wise; colors never appear (series carry theme style keys resolved
//! against the generic theme config at paint time).

use protopyne_kernels::bars::fast_tanh;

/// Oscillator identity for the normalization table.
///
/// Mirrors the reference oscillator enum; the `Raw` variant covers
/// pass-through sources (cumulative-delta and unlisted types), which share
/// the default squash constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OscillatorKind {
    /// Aroon up/down spread.
    Aroon,
    /// Commodity channel index.
    Cci,
    /// Chande momentum oscillator.
    Cmo,
    /// Money flow index.
    Mfi,
    /// Relative strength index.
    Rsi,
    /// Stochastic RSI.
    Srsi,
    /// Stochastic.
    Stoch,
    /// Correlation-based trend oscillator.
    TrendSi,
    /// Voss predictive filter spread.
    Voss,
    /// Pass-through source with default constants.
    Raw,
}

/// Canonical lowercase name for one oscillator kind (settings/schema spelling).
#[must_use]
pub fn oscillator_kind_name(kind: OscillatorKind) -> &'static str {
    match kind {
        OscillatorKind::Aroon => "aroon",
        OscillatorKind::Cci => "cci",
        OscillatorKind::Cmo => "cmo",
        OscillatorKind::Mfi => "mfi",
        OscillatorKind::Rsi => "rsi",
        OscillatorKind::Srsi => "srsi",
        OscillatorKind::Stoch => "stoch",
        OscillatorKind::TrendSi => "trendsi",
        OscillatorKind::Voss => "voss",
        OscillatorKind::Raw => "raw",
    }
}

/// Parse an oscillator kind name; ASCII case-insensitive, `None` for unknown
/// spellings (callers refuse with a reason instead of substituting).
#[must_use]
pub fn parse_oscillator_kind(name: &str) -> Option<OscillatorKind> {
    match name.to_ascii_lowercase().as_str() {
        "aroon" => Some(OscillatorKind::Aroon),
        "cci" => Some(OscillatorKind::Cci),
        "cmo" => Some(OscillatorKind::Cmo),
        "mfi" => Some(OscillatorKind::Mfi),
        "rsi" => Some(OscillatorKind::Rsi),
        "srsi" => Some(OscillatorKind::Srsi),
        "stoch" => Some(OscillatorKind::Stoch),
        "trendsi" => Some(OscillatorKind::TrendSi),
        "voss" => Some(OscillatorKind::Voss),
        "raw" => Some(OscillatorKind::Raw),
        _ => None,
    }
}

/// Frozen per-oscillator squash constants `(offset, k, multiplier)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OscParams {
    /// Shift applied before scaling.
    pub offset: f64,
    /// Steepness of the squash.
    pub k: f64,
    /// Output band half-width.
    pub multiplier: f64,
}

/// Look up the frozen squash constants for one oscillator.
///
/// Table mirrors the reference `normalizeOsc` arms: Aroon
/// `(-50, 0.005, 800)`, CCI `(-50, 0.0035, 500)`, CMO `(0, 0.002, 1000)`,
/// SRSI `(-50, 0.0006, 7000)`, STOCH `(-50, 0.0005, 7000)`, TRENDSI
/// `(0, 0.016, 9000)`, VOSS `(0, 1.5, 300)`; MFI, RSI, and pass-through
/// sources share the default `(-50, 0.0084, 740)`.
#[must_use]
pub fn osc_params(kind: OscillatorKind) -> OscParams {
    match kind {
        OscillatorKind::Aroon => OscParams { offset: -50.0, k: 0.005, multiplier: 800.0 },
        OscillatorKind::Cci => OscParams { offset: -50.0, k: 0.0035, multiplier: 500.0 },
        OscillatorKind::Cmo => OscParams { offset: 0.0, k: 0.002, multiplier: 1000.0 },
        OscillatorKind::Mfi | OscillatorKind::Rsi | OscillatorKind::Raw => {
            OscParams { offset: -50.0, k: 0.0084, multiplier: 740.0 }
        }
        OscillatorKind::Srsi => OscParams { offset: -50.0, k: 0.0006, multiplier: 7000.0 },
        OscillatorKind::Stoch => OscParams { offset: -50.0, k: 0.0005, multiplier: 7000.0 },
        OscillatorKind::TrendSi => OscParams { offset: 0.0, k: 0.016, multiplier: 9000.0 },
        OscillatorKind::Voss => OscParams { offset: 0.0, k: 1.5, multiplier: 300.0 },
    }
}

/// Map one raw oscillator reading into the shared display band.
///
/// Pure and total over finite inputs. Non-finite inputs propagate through the
/// float arithmetic (for example `NaN` stays `NaN`); series-level callers
/// turn non-finite results into holes. The squash is the shared
/// [`fast_tanh`] algebraic sigmoid (`x / (1 + |x|)`), so no second formula
/// is written here.
#[must_use]
pub fn normalize_oscillator(raw: f64, kind: OscillatorKind) -> f64 {
    let params = osc_params(kind);
    let x = (raw + params.offset) * params.k;
    fast_tanh(x) * params.multiplier
}

/// Map a raw oscillator series into the shared display band.
///
/// Length-preserving: output has one slot per input slot, `None` stays
/// `None`, and non-finite squash results become holes rather than
/// substituted values.
#[must_use]
pub fn normalize_series(raw: &[Option<f64>], kind: OscillatorKind) -> Vec<Option<f64>> {
    raw.iter()
        .map(|slot| {
            slot.map(|value| normalize_oscillator(value, kind)).filter(|mapped| mapped.is_finite())
        })
        .collect()
}

/// Half-width of the bounded oscillator band: the zero-centered scale every
/// sub-pane readout shares (`-100 .. +100`).
pub const BOUNDED_OSC_SCALE: f64 = 100.0;
/// Divisor of the bounded compressor: an unbounded reading of `±100` lands on
/// `±CALIBRATED_BOUNDED_PIVOT_THRESHOLD` after squashing.
pub const SQUASH_100_DIVISOR: f64 = 100.0;
/// Pivot threshold on the bounded `[-100, +100]` band (task value: alert
/// fitness). Bijective with [`crate::integration_lite::CALIBRATED_PIVOT_THRESHOLD`]
/// (`100.0`) under [`squash_to_100`], so the same swing reversals fire.
pub const CALIBRATED_BOUNDED_PIVOT_THRESHOLD: f64 = 50.0;

/// Compress an unbounded reading onto the zero-centered `(-100, +100)` band.
///
/// Strictly monotonic and bijective from `(-∞, +∞)` onto
/// `(-BOUNDED_OSC_SCALE, +BOUNDED_OSC_SCALE)` through the shared
/// [`fast_tanh`]; maps `0.0 → 0.0` and `±SQUASH_100_DIVISOR →
/// ±CALIBRATED_BOUNDED_PIVOT_THRESHOLD`. Non-finite inputs yield `NaN` (the
/// series-level callers turn that into a hole).
#[must_use]
pub fn squash_to_100(value: f64) -> f64 {
    fast_tanh(value / SQUASH_100_DIVISOR) * BOUNDED_OSC_SCALE
}

/// Compress a series onto the bounded band, preserving warmup holes.
///
/// Length-preserving: `None` stays `None`, and a non-finite squash result
/// becomes a hole rather than a substituted value.
#[must_use]
pub fn squash_series_to_100(series: &[Option<f64>]) -> Vec<Option<f64>> {
    series
        .iter()
        .map(|slot| slot.map(squash_to_100).filter(|mapped| mapped.is_finite()))
        .collect()
}

/// Map one raw oscillator reading onto the bounded `[-100, +100]` band.
///
/// Composes the frozen per-kind squash with the bounded compressor, so the
/// bounded reading is `squash_to_100(normalize_oscillator(raw, kind))`.
#[must_use]
pub fn normalize_oscillator_bounded(raw: f64, kind: OscillatorKind) -> f64 {
    squash_to_100(normalize_oscillator(raw, kind))
}

/// Map a raw oscillator series onto the bounded band, preserving warmup holes.
#[must_use]
pub fn normalize_series_bounded(raw: &[Option<f64>], kind: OscillatorKind) -> Vec<Option<f64>> {
    squash_series_to_100(&normalize_series(raw, kind))
}

#[cfg(test)]
#[path = "../tests/oscillators.rs"]
mod tests;
