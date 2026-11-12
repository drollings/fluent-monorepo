//! The nested composition view and its compatibility bridge.
//!
//! The flat [`IntegrationLiteStrategyConfig`] remains the wire shape for the
//! preview and replay surfaces during the migration window. [`IntegrationLiteSpec`]
//! is the nested view the composition surface renders and the slot builders
//! read; it keeps the flat value beside the projection so a round trip is
//! exactly the bytes that entered, never a re-derived approximation.

use fluent_wvr::{Describable, FieldAccess};

use crate::error::StrategyError;
use crate::integration_lite::spec::IntegrationLiteStrategyConfig;
use crate::signals_lite::LiteIndicatorParams;

/// One entry trigger's parameters.
///
/// The fields are kind-conditional, like the filter row below: `level`
/// carries the momentum level, the exhaustion band, or the slow leg of the
/// dual exhaustion; `fast_level` only the dual's fast leg; `lookback` only
/// the dual window.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
/// One entry trigger's parameters.
/// 
/// The fields are kind-conditional, like the filter row below: `level`
/// carries the momentum level, the exhaustion band, or the slow leg of the
/// dual exhaustion; `fast_level` only the dual's fast leg; `lookback` only
/// the dual window.
pub struct TriggerSlot {
    /// Catalog id (`momentum`, `ribbon_flip`, `osc_cross`, `exhaustion`,
    /// `dual_exhaustion`).
    #[field(variants = "crate::slots::trigger::trigger_ids")]
    pub kind: String,
    /// Level or band half-width.
    pub level: f64,
    /// Dual-exhaustion fast-leg distance. Only read when kind is
    /// `dual_exhaustion`.
    pub fast_level: f64,
    /// Adaptive threshold for the momentum arm.
    pub adaptive: bool,
    /// Counter-trend lookback.
    pub lookback: i64,
}

/// One admission filter's parameters.
///
/// The numeric fields are kind-conditional: `minimum` carries the health,
/// slow-side, or volume floor; `maximum` the extension ceiling; `length`
/// the delta window. The two regime arms ride their own switches because
/// they are independent policies, not one threshold.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
/// One admission filter's parameters.
/// 
/// The numeric fields are kind-conditional: `minimum` carries the health,
/// slow-side, or volume floor; `maximum` the extension ceiling; `length`
/// the delta window. The two regime arms ride their own switches because
/// they are independent policies, not one threshold.
pub struct FilterSlot {
    /// Catalog id (`ribbon`, `ma_health`, `ribbon_health`, `slow_side`,
    /// `delta_side`, `volume_min`, `atr_distance_max`, `regime_skip`).
    #[field(variants = "crate::slots::filter::filter_ids")]
    pub kind: String,
    /// Whether the guard is armed.
    pub enable: bool,
    /// Lower threshold, when the guard has one.
    pub minimum: f64,
    /// Upper threshold, when the guard has one.
    pub maximum: f64,
    /// Trailing window, when the guard has one.
    pub length: i64,
    /// Skip the quiet regime. Only read when kind is `regime_skip`.
    pub low_volume: bool,
    /// Skip the volatile regime. Only read when kind is `regime_skip`.
    pub volatile: bool,
}

/// The entry section: triggers, filters, and the setup window.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
/// The entry section: triggers, filters, and the setup window.
pub struct EntrySpec {
    /// Up to eight trigger rows.
    #[field(items = 8)]
    pub triggers: Vec<TriggerSlot>,
    /// Up to eight filter rows.
    #[field(items = 8)]
    pub filters: Vec<FilterSlot>,
    /// Bars a trigger stays armed.
    pub setup_window: i64,
    /// Adaptive momentum threshold.
    pub adaptive_level: bool,
    /// Counter-trend filter bypass.
    pub dual_bypass: bool,
}

/// The exit section: stop, targets, trailing, breakeven, exhaustion.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[allow(clippy::struct_excessive_bools)]
/// The exit section: stop, targets, trailing, breakeven, exhaustion.
pub struct ExitSpec {
    /// Stop construction (`ATR`, `Percent`, `Swing`).
    pub stop_type: String,
    /// ATR stop multiple.
    pub stop_atr_mult: f64,
    /// Percent stop width.
    pub stop_pct: f64,
    /// Swing lookback.
    pub swing_len: i64,
    /// First target multiple.
    pub tp1_mult: f64,
    /// Second target multiple.
    pub tp2_mult: f64,
    /// Third target multiple.
    pub tp3_mult: f64,
    /// First target fraction.
    pub tp1_pct: f64,
    /// Second target fraction.
    pub tp2_pct: f64,
    /// Third target fraction.
    pub tp3_pct: f64,
    /// Breakeven arm.
    pub breakeven: bool,
    /// Trailing construction (`Chandelier`, `Percent`, `Ribbon Edge`).
    pub trail_type: String,
    /// Trailing multiple.
    pub trail_mult: f64,
    /// Tight trailing mode.
    pub trail_tight: bool,
    /// Opposite-trigger exit.
    pub opposite: bool,
    /// Exhaustion response.
    pub exhaustion: bool,
    /// Time stop in bars.
    pub time_bars: i64,
}

/// The risk section: sizing and the gates.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
/// The risk section: sizing and the gates.
pub struct RiskSpec {
    /// Sizing mode (`Risk Percent`, `Fixed`, …).
    pub size_mode: String,
    /// Risk fraction.
    pub risk_pct: f64,
    /// Size fraction.
    pub size_pct: f64,
    /// Fixed quantity.
    pub fixed_qty: f64,
    /// Notional cap fraction.
    pub max_notional_pct: f64,
    /// Risk ATR span.
    pub risk_atr_len: i64,
    /// Daily loss cap.
    pub max_daily_loss_pct: f64,
    /// Daily trade cap.
    pub max_trades_day: i64,
    /// Consecutive-loss cap.
    pub max_consec_losses: i64,
    /// Cooldown bars.
    pub cooldown_bars: i64,
}

/// The session section.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
/// The session section.
pub struct SessionSpec {
    /// Session window (`HHMM-HHMM`).
    pub window: String,
    /// Session filter armed.
    pub filter: bool,
    /// Flatten at the close.
    pub flatten_eod: bool,
    /// Minutes before the close to stop entering.
    pub noentry_min: i64,
    /// Minutes before the close to flatten.
    pub flatten_before: i64,
}

/// The presentation section.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
/// The presentation section.
pub struct DisplaySpec {
    /// Master switch.
    pub enabled: bool,
    /// Direction policy.
    pub direction: String,
    /// Allow a reversal.
    pub allow_reverse: bool,
    /// Warmup bars.
    pub warmup_bars: i64,
}

/// The nested composition view.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// The nested composition view.
pub struct IntegrationLiteSpec {
    /// Market-leg dials.
    pub indicator: LiteIndicatorParams,
    /// Entry triggers, filters, and setup window.
    pub entry: EntrySpec,
    /// Exit construction.
    pub exit: ExitSpec,
    /// Risk and sizing.
    pub risk: RiskSpec,
    /// Session policy.
    pub session: SessionSpec,
    /// Presentation and master switches.
    pub display: DisplaySpec,
    /// The flat wire view this projection came from. Kept beside the nested
    /// fields so a round trip returns exactly the recorded bytes; it is the
    /// compatibility window's stated source of truth.
    pub legacy: IntegrationLiteStrategyConfig,
}

impl From<&IntegrationLiteStrategyConfig> for TriggerSlot {
    fn from(config: &IntegrationLiteStrategyConfig) -> Self {
        Self {
            kind: "momentum".to_owned(),
            level: config.sig_entry_level,
            fast_level: 0.0,
            adaptive: config.sig_adaptive_level,
            lookback: config.sig_dual_lookback,
        }
    }
}

/// Project the flat wire config onto the nested entry section.
///
/// This is the one flat→entry mapping: the slot builders read the entry,
/// never the flat config, so the flat struct reaches the slots only through
/// this projection. Every entry dial has a lossless slot field; the section
/// dials without a slot (setup window, adaptive default, dual bypass) ride
/// the entry section directly.
#[must_use]
/// Project the flat wire config onto the nested entry section.
/// 
/// This is the one flat→entry mapping: the slot builders read the entry,
/// never the flat config, so the flat struct reaches the slots only through
/// this projection. Every entry dial has a lossless slot field; the section
/// dials without a slot (setup window, adaptive default, dual bypass) ride
/// the entry section directly.
pub fn entry_from_config(config: &IntegrationLiteStrategyConfig) -> EntrySpec {
    let mut triggers = Vec::new();
    if config.sig_momentum {
        triggers.push(TriggerSlot::from(config));
    }
    if config.sig_ribbon_flip {
        triggers.push(TriggerSlot {
            kind: "ribbon_flip".to_owned(),
            ..TriggerSlot::default()
        });
    }
    if config.sig_osc_cross {
        triggers.push(TriggerSlot {
            kind: "osc_cross".to_owned(),
            ..TriggerSlot::default()
        });
    }
    if config.sig_exhaust_entry {
        triggers.push(TriggerSlot {
            kind: "exhaustion".to_owned(),
            level: config.sig_exhaust_level,
            ..TriggerSlot::default()
        });
    }
    if config.sig_dual {
        triggers.push(TriggerSlot {
            kind: "dual_exhaustion".to_owned(),
            level: config.sig_dual_slow,
            fast_level: config.sig_dual_fast,
            lookback: config.sig_dual_lookback,
            ..TriggerSlot::default()
        });
    }
    let mut filters = Vec::new();
    for (kind, enable, minimum, maximum, length, low_volume, volatile) in [
        ("ribbon", config.filt_ribbon, 0.0, 0.0, 0, false, false),
        (
            "ma_health",
            config.filt_ma_health,
            config.filt_ma_health_min,
            0.0,
            0,
            false,
            false,
        ),
        (
            "ribbon_health",
            config.filt_ribbon_health,
            0.0,
            0.0,
            0,
            false,
            false,
        ),
        (
            "slow_side",
            config.filt_slow,
            config.filt_slow_min,
            0.0,
            0,
            false,
            false,
        ),
        (
            "delta_side",
            config.filt_delta,
            0.0,
            0.0,
            config.filt_delta_len,
            false,
            false,
        ),
        (
            "volume_min",
            config.filt_volume,
            config.filt_volume_min,
            0.0,
            0,
            false,
            false,
        ),
        (
            "atr_distance_max",
            config.filt_extension,
            0.0,
            config.filt_extension_max,
            0,
            false,
            false,
        ),
        (
            "regime_skip",
            config.filt_regime_lowvol || config.filt_regime_volatile,
            0.0,
            0.0,
            0,
            config.filt_regime_lowvol,
            config.filt_regime_volatile,
        ),
    ] {
        filters.push(FilterSlot {
            kind: kind.to_owned(),
            enable,
            minimum,
            maximum,
            length,
            low_volume,
            volatile,
        });
    }
    EntrySpec {
        triggers,
        filters,
        setup_window: config.sig_window,
        adaptive_level: config.sig_adaptive_level,
        dual_bypass: config.sig_dual_bypass,
    }
}

impl From<&IntegrationLiteStrategyConfig> for IntegrationLiteSpec {
    fn from(config: &IntegrationLiteStrategyConfig) -> Self {
        let entry = entry_from_config(config);
        let indicator = LiteIndicatorParams::default();
        Self {
            indicator: indicator.clone(),
            entry,
            exit: ExitSpec {
                stop_type: config.stop_type.clone(),
                stop_atr_mult: config.stop_atr_mult,
                stop_pct: config.stop_pct,
                swing_len: config.swing_len,
                tp1_mult: config.tp1_mult,
                tp2_mult: config.tp2_mult,
                tp3_mult: config.tp3_mult,
                tp1_pct: config.tp1_pct,
                tp2_pct: config.tp2_pct,
                tp3_pct: config.tp3_pct,
                breakeven: config.be_enable,
                trail_type: config.trail_type.clone(),
                trail_mult: config.trail_mult,
                trail_tight: (config.trail_tight_mult - 1.0).abs() > 1e-12,
                opposite: config.exit_opposite,
                exhaustion: config.trail_on_exhaustion,
                time_bars: config.exit_time_bars,
            },
            risk: RiskSpec {
                size_mode: config.size_mode.clone(),
                risk_pct: config.risk_pct,
                size_pct: config.size_pct,
                fixed_qty: config.fixed_qty,
                max_notional_pct: config.max_notional_pct,
                risk_atr_len: config.risk_atr_len,
                max_daily_loss_pct: config.max_daily_loss_pct,
                max_trades_day: config.max_trades_day,
                max_consec_losses: config.max_consec_losses,
                cooldown_bars: config.cd_bars,
            },
            session: SessionSpec {
                window: indicator.session.clone(),
                filter: indicator.session_filter,
                flatten_eod: config.flatten_eod,
                noentry_min: config.noentry_min,
                flatten_before: config.flatten_min_before,
            },
            display: DisplaySpec {
                enabled: config.enabled,
                direction: config.direction.clone(),
                allow_reverse: config.allow_reverse,
                warmup_bars: config.warmup_bars,
            },
            legacy: config.clone(),
        }
    }
}

impl TryFrom<&IntegrationLiteSpec> for IntegrationLiteStrategyConfig {
    type Error = StrategyError;

    fn try_from(spec: &IntegrationLiteSpec) -> Result<Self, Self::Error> {
        Ok(spec.legacy.clone())
    }
}

#[cfg(test)]
#[path = "../../tests/migrate.rs"]
mod tests;