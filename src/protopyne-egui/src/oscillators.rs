//! Trading oscillator vocabulary plus the frozen normalization catalog.
//!
//! The squash mechanism (offset/shift, steepness, band half-width) is plain
//! display math, but the kind enum and its constants mirror the reference
//! `normalizeOsc` (`_libMAs.pine`), so they live here with the rest of the
//! trading-domain display truth — not in the generic paint scaffolding.
//! Every function is `f64`-in/`f64`-out with warmup holes preserved
//! element-wise; colors never appear (series carry theme style keys resolved
//! against the generic theme config at paint time).
//!
//! The catalog is the single per-oscillator truth: each [`OscillatorKind`]
//! row carries its Pine `Oscillators` spelling, the kernel (or an explicit
//! refusal for `Cdv`/unlisted), the frozen `(offset, k, multiplier)`, the
//! supported `which` legs, and trailing-smoothing eligibility. A Pine edit
//! that changes an arm fails the shipped-source drift test here first.

use fluent_egui::{affine_squash, band_squash, map_series_optional, EguiError};

/// Oscillator identity for the normalization catalog.
///
/// Mirrors the reference oscillator enum; the `Raw` variant covers
/// pass-through sources (Pine `Smoothings.NA` / unlisted types), which share
/// the default squash constants. `Cdv` is a listed Pine oscillator with no
/// kernel: it parses but refuses evaluation with a reason, never substituting
/// the default constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OscillatorKind {
    /// Aroon up/down spread.
    Aroon,
    /// Commodity channel index.
    Cci,
    /// Cumulative delta volume: no kernel, refuses with a reason.
    Cdv,
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

/// Which leg of a multi-leg oscillator `normalizeOsc` selects.
///
/// The Pine `which` axis picks the window length for single-leg families
/// (`1 => fastLen`, otherwise `slowLen`) and the leg content for the two
/// multi-leg families (Aroon and Voss).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WhichLeg {
    /// `which = 1`: the fast leg (fastLen window).
    Fast,
    /// `which = -1`: the slow leg (slowLen window; a distinct leg for
    /// Aroon/Voss).
    Slow,
    /// `which = 0`: the spread leg (Aroon up−down, Voss voss−filter).
    Spread,
}

impl WhichLeg {
    /// Map a Pine `which` value onto a leg; `None` for unhandled values.
    #[must_use]
    pub fn from_which(which: i64) -> Option<Self> {
        match which {
            1 => Some(Self::Fast),
            -1 => Some(Self::Slow),
            0 => Some(Self::Spread),
            _ => None,
        }
    }

    /// The Pine `which` value this leg spells.
    #[must_use]
    pub fn which(self) -> i64 {
        match self {
            Self::Fast => 1,
            Self::Slow => -1,
            Self::Spread => 0,
        }
    }
}

impl OscillatorKind {
    /// Every catalog row, in Pine-dispatch order (Pine `NA` maps to `Raw`).
    #[must_use]
    pub fn all() -> [Self; 11] {
        [
            Self::Aroon,
            Self::Cci,
            Self::Cdv,
            Self::Cmo,
            Self::Mfi,
            Self::Rsi,
            Self::Srsi,
            Self::Stoch,
            Self::TrendSi,
            Self::Voss,
            Self::Raw,
        ]
    }

    /// Canonical lowercase name (settings/schema spelling).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Aroon => "aroon",
            Self::Cci => "cci",
            Self::Cdv => "cdv",
            Self::Cmo => "cmo",
            Self::Mfi => "mfi",
            Self::Rsi => "rsi",
            Self::Srsi => "srsi",
            Self::Stoch => "stoch",
            Self::TrendSi => "trendsi",
            Self::Voss => "voss",
            Self::Raw => "raw",
        }
    }

    /// Pine `UTIL.Oscillators` spelling (`Raw` carries the Pine `NA` value).
    #[must_use]
    pub fn pine_name(self) -> &'static str {
        match self {
            Self::Aroon => "AROON",
            Self::Cci => "CCI",
            Self::Cdv => "CDV",
            Self::Cmo => "CMO",
            Self::Mfi => "MFI",
            Self::Rsi => "RSI",
            Self::Srsi => "SRSI",
            Self::Stoch => "STOCH",
            Self::TrendSi => "TRENDSI",
            Self::Voss => "VOSS",
            Self::Raw => "-",
        }
    }

    /// Kernel that produces the raw series, or `None` for pass-through
    /// (`Raw`), refused (`Cdv`), and recipe families without a single kernel
    /// (`Srsi`, `TrendSi`).
    #[must_use]
    pub fn kernel_id(self) -> Option<&'static str> {
        match self {
            Self::Aroon => Some("aroon"),
            Self::Cci => Some("cci"),
            Self::Cmo => Some("cmo"),
            Self::Mfi => Some("mfi"),
            Self::Rsi => Some("rsi"),
            Self::Stoch => Some("stoch"),
            Self::Voss => Some("voss"),
            Self::Cdv | Self::Srsi | Self::TrendSi | Self::Raw => None,
        }
    }

    /// Frozen squash constants, or `None` for the refused `Cdv` row (never a
    /// substitute default).
    #[must_use]
    pub fn params(self) -> Option<OscParams> {
        let params = match self {
            Self::Aroon => OscParams {
                offset: -50.0,
                k: 0.005,
                multiplier: 800.0,
            },
            Self::Cci => OscParams {
                offset: -50.0,
                k: 0.0035,
                multiplier: 500.0,
            },
            Self::Cdv => return None,
            Self::Cmo => OscParams {
                offset: 0.0,
                k: 0.002,
                multiplier: 1000.0,
            },
            Self::Mfi | Self::Rsi | Self::Raw => OscParams {
                offset: -50.0,
                k: 0.0084,
                multiplier: 740.0,
            },
            Self::Srsi => OscParams {
                offset: -50.0,
                k: 0.0006,
                multiplier: 7000.0,
            },
            Self::Stoch => OscParams {
                offset: -50.0,
                k: 0.0005,
                multiplier: 7000.0,
            },
            Self::TrendSi => OscParams {
                offset: 0.0,
                k: 0.016,
                multiplier: 9000.0,
            },
            Self::Voss => OscParams {
                offset: 0.0,
                k: 1.5,
                multiplier: 300.0,
            },
        };
        Some(params)
    }

    /// The `which` legs this family handles. Single-leg families list the
    /// window-length axis (fast/slow); Aroon and Voss add the spread leg.
    #[must_use]
    pub fn which_legs(self) -> &'static [WhichLeg] {
        match self {
            Self::Aroon | Self::Voss => &[WhichLeg::Fast, WhichLeg::Slow, WhichLeg::Spread],
            Self::Cci
            | Self::Cdv
            | Self::Cmo
            | Self::Mfi
            | Self::Rsi
            | Self::Srsi
            | Self::Stoch
            | Self::TrendSi
            | Self::Raw => &[WhichLeg::Fast, WhichLeg::Slow],
        }
    }

    /// Whether trailing `factorySmoothing` applies to this family (the Pine
    /// gate smooths every arm uniformly when armed).
    #[must_use]
    pub fn smoothing_eligible(self) -> bool {
        // The reference applies smoothing after the arm switch for every
        // oscillator, so all rows stay eligible; a Pine edit that exempts an
        // arm fails the drift test.
        true
    }

    /// Whether the family evaluates (has frozen constants). Only the refused
    /// `Cdv` row does not.
    #[must_use]
    pub fn is_evaluable(self) -> bool {
        self.params().is_some()
    }

    /// Why the family refuses evaluation, or `None` when it evaluates.
    #[must_use]
    pub fn refusal_reason(self) -> Option<&'static str> {
        match self {
            Self::Cdv => {
                Some("cdv has no kernel; the delta-volume profile is not a price oscillator")
            }
            _ => None,
        }
    }
}

/// Canonical lowercase name for one oscillator kind (settings/schema spelling).
#[must_use]
pub fn oscillator_kind_name(kind: OscillatorKind) -> &'static str {
    kind.id()
}

/// One oscillator row for catalog-backed pickers: the catalog id plus the
/// normalization payload, so pickers list the catalog without re-spelling it.
#[derive(Debug, Clone, PartialEq)]
pub struct OscillatorOption {
    /// Canonical catalog id (the config value).
    pub id: String,
    /// Pine `Oscillators` spelling.
    pub pine_name: String,
    /// Supported `which` legs (1 fast, -1 slow, 0 spread).
    pub legs: Vec<i64>,
    /// Whether the row evaluates (has frozen constants).
    pub evaluable: bool,
    /// Why the row refuses, when it does.
    pub refusal: Option<String>,
}

/// Every oscillator kind in Pine-dispatch order: the oscillator picker.
/// Refused rows list with their reason — advertised, never runnable.
#[must_use]
pub fn oscillator_options() -> Vec<OscillatorOption> {
    OscillatorKind::all()
        .iter()
        .map(|kind| OscillatorOption {
            id: kind.id().to_owned(),
            pine_name: kind.pine_name().to_owned(),
            legs: kind
                .which_legs()
                .iter()
                .copied()
                .map(WhichLeg::which)
                .collect(),
            evaluable: kind.is_evaluable(),
            refusal: kind.refusal_reason().map(str::to_owned),
        })
        .collect()
}

/// Ids an oscillator field may hold: exactly what selection validation
/// accepts, so selector schemas and validators read one list.
#[must_use]
pub fn evaluable_oscillator_ids() -> Vec<String> {
    OscillatorKind::all()
        .iter()
        .filter(|kind| kind.is_evaluable())
        .map(|kind| kind.id().to_owned())
        .collect()
}

/// Parse an oscillator kind name; ASCII case-insensitive over the catalog id
/// and Pine spelling, `None` for unknown spellings (callers refuse with a
/// reason instead of substituting). A trailing space does not parse: names
/// are exact, so a typo surfaces instead of silently matching.
#[must_use]
pub fn parse_oscillator_kind(name: &str) -> Option<OscillatorKind> {
    let lower = name.to_ascii_lowercase();
    OscillatorKind::all()
        .into_iter()
        .find(|kind| lower == kind.id() || lower == kind.pine_name().to_ascii_lowercase())
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

/// Refuse a non-evaluable kind with the catalog's own reason.
fn refuse(kind: OscillatorKind) -> EguiError {
    EguiError::InvalidParam {
        what: "oscillator".to_owned(),
        reason: kind
            .refusal_reason()
            .unwrap_or("unspecified refusal")
            .to_owned(),
    }
}

/// Map one raw oscillator reading into the shared display band, or refuse a
/// non-evaluable kind (`Cdv`) with a reason.
///
/// Pure and total over finite inputs for evaluable kinds. Non-finite inputs
/// propagate through the float arithmetic (for example `NaN` stays `NaN`);
/// series-level callers turn non-finite results into holes. Delegates the
/// shift/scale/squash/widen shape to the generic band mapper; the per-kind
/// catalog above stays the only calibration.
pub fn normalize_oscillator(raw: f64, kind: OscillatorKind) -> Result<f64, EguiError> {
    let params = kind.params().ok_or_else(|| refuse(kind))?;
    Ok(affine_squash(
        raw,
        params.offset,
        params.k,
        params.multiplier,
    ))
}

/// Map a raw oscillator series into the shared display band, or refuse a
/// non-evaluable kind with a reason.
///
/// Length-preserving: output has one slot per input slot, `None` stays
/// `None`, and non-finite squash results become holes rather than
/// substituted values.
pub fn normalize_series(
    raw: &[Option<f64>],
    kind: OscillatorKind,
) -> Result<Vec<Option<f64>>, EguiError> {
    let params = kind.params().ok_or_else(|| refuse(kind))?;
    Ok(map_series_optional(raw, |value| {
        affine_squash(value, params.offset, params.k, params.multiplier)
    }))
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

/// Display rules sit at ±this band on every normalized oscillator pane;
/// the extremes squash below fixes exactly these points, so thresholds
/// painted over the display mean what the calibration measured.
pub const DISPLAY_RULE_BAND: f64 = 100.0;

/// Half-width of the normalized display extremes: overshoot past the ±100
/// rules compresses toward these asymptotes instead of clipping, the
/// shipped `_ApexOscillators` reading (0 centerline, ±100
/// overbought/oversold, ±200 extremes).
pub const DISPLAY_EXTREME_BAND: f64 = 200.0;

/// One mapped display leg: the dialed catalog squash constrained into the
/// extremes band.
///
/// Total over finite inputs: the catalog affine squash under `params`,
/// scaled by `gain`, centered by `trim`, then squeezed toward the ±200
/// asymptotes. Length-preserving with holes preserved; an out-of-band or
/// non-finite midpoint becomes a hole, never a substituted value. The band
/// fixes 0 and ±100 exactly, so threshold crossings are identical to the
/// unbounded squash's while overshoot past ±100 compresses toward ±200.
#[must_use]
pub fn display_mapped_series(
    raw: &[Option<f64>],
    params: &OscParams,
    gain: f64,
    trim: f64,
) -> Vec<Option<f64>> {
    map_series_optional(raw, |value| {
        band_squash(
            gain * affine_squash(value, params.offset, params.k, params.multiplier) + trim,
            DISPLAY_RULE_BAND,
            DISPLAY_EXTREME_BAND,
        )
    })
}

/// Compress an unbounded reading onto the zero-centered `(-100, +100)` band.
///
/// Strictly monotonic and bijective from `(-∞, +∞)` onto
/// `(-BOUNDED_OSC_SCALE, +BOUNDED_OSC_SCALE)` through the shared generic
/// sigmoid; maps `0.0 → 0.0` and `±SQUASH_100_DIVISOR →
/// ±CALIBRATED_BOUNDED_PIVOT_THRESHOLD`. Non-finite inputs yield `NaN` (the
/// series-level callers turn that into a hole).
#[must_use]
pub fn squash_to_100(value: f64) -> f64 {
    band_squash(value, SQUASH_100_DIVISOR, BOUNDED_OSC_SCALE)
}

/// Compress a series onto the bounded band, preserving warmup holes.
///
/// Length-preserving: `None` stays `None`, and a non-finite squash result
/// becomes a hole rather than a substituted value.
#[must_use]
pub fn squash_series_to_100(series: &[Option<f64>]) -> Vec<Option<f64>> {
    map_series_optional(series, squash_to_100)
}

/// Map one raw oscillator reading onto the bounded `[-100, +100]` band, or
/// refuse a non-evaluable kind.
///
/// Composes the frozen per-kind squash with the bounded compressor, so the
/// bounded reading is `squash_to_100(normalize_oscillator(raw, kind))`.
pub fn normalize_oscillator_bounded(raw: f64, kind: OscillatorKind) -> Result<f64, EguiError> {
    Ok(squash_to_100(normalize_oscillator(raw, kind)?))
}

/// Map a raw oscillator series onto the bounded band, preserving warmup
/// holes, or refuse a non-evaluable kind.
///
/// One fused lift over the composed scalar: staging (`squash_series` over
/// `normalize_series`) would allocate an intermediate series for the same
/// slots. The fusion is exact — a non-finite midpoint squashes to `NaN`
/// through the generic sigmoid (`NaN/(1+NaN)`, `±inf/±inf`), which the lift
/// turns into the same hole the staged filter would.
pub fn normalize_series_bounded(
    raw: &[Option<f64>],
    kind: OscillatorKind,
) -> Result<Vec<Option<f64>>, EguiError> {
    let params = kind.params().ok_or_else(|| refuse(kind))?;
    Ok(map_series_optional(raw, |value| {
        squash_to_100(affine_squash(
            value,
            params.offset,
            params.k,
            params.multiplier,
        ))
    }))
}

/// One kernel output's display mapping: the catalog kind plus the leg whose
/// window the kernel's length matches, or `None` for a leg that paints raw.
///
/// This is the taxonomy the shared pane gates on: single-output catalog
/// kernels map their `value` leg, the stochastic family maps both legs
/// (the %D leg lives in the same bounded 0..100 space), aroon maps both
/// legs through the unit bridge (fractions rescale to the Pine 0..100 arm),
/// and everything else — price bands, multi-output recipes, unmapped
/// families — reads raw and refuses the merge. The leg is provenance:
/// display squashes by kind, and every mapped leg shares its kernel's
/// length window, which the slow leg names.
#[must_use]
pub fn oscillator_mapping_for_output(
    kernel: &str,
    output: &str,
) -> Option<(OscillatorKind, WhichLeg)> {
    match (kernel, output) {
        ("rsi", "value") => Some((OscillatorKind::Rsi, WhichLeg::Slow)),
        ("cci", "value") => Some((OscillatorKind::Cci, WhichLeg::Slow)),
        ("cmo", "value") => Some((OscillatorKind::Cmo, WhichLeg::Slow)),
        ("mfi", "value") => Some((OscillatorKind::Mfi, WhichLeg::Slow)),
        ("stoch", "value" | "d") => Some((OscillatorKind::Stoch, WhichLeg::Slow)),
        ("aroon", "value" | "down") => Some((OscillatorKind::Aroon, WhichLeg::Slow)),
        _ => None,
    }
}
/// Rescale native aroon fractions into the catalog's 0..100 arm.
///
/// The kernel emits window fractions (`back/length`); the catalog row is
/// calibrated for Pine-scale percentages, so the bridge maps each fraction
/// to its 0..100 arm: `100 * (P * r - 1) / (P - 1)`. The bridge is exact:
/// both engines share the window and break ties to the most recent bar, so
/// it reproduces the shipped arm bar-for-bar (pinned by the parity test).
/// Holes stay holes; the degenerate length-1 arm divides to `NaN`, which
/// the lift turns into holes — exactly Pine's `0/0 = na`. Lengths below 1
/// pass through, as the kernel batch refuses those before display ever
/// runs.
#[must_use]
pub fn rescale_aroon_output(raw: &[Option<f64>], length: i64) -> Vec<Option<f64>> {
    if length < 1 {
        return raw.to_vec();
    }
    let period = length as f64;
    map_series_optional(raw, |value| 100.0 * (period * value - 1.0) / (period - 1.0))
}

#[cfg(test)]
#[path = "../tests/oscillators.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/oscillator_lift.rs"]
mod lift_tests;

#[cfg(test)]
#[path = "../tests/oscillator_catalog.rs"]
mod catalog_tests;
