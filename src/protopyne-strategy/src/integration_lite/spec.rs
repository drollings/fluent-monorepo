//! IntegrationLite strategy configuration: the strategy-group dials behind
//! the shipped `_IntegrationLiteStrategy.pine`.
//!
//! One flat schema struct carrying every `GROUP_S_*` input with its Pine
//! default, bounds, and group metadata, so the settings surface and the
//! backtester read one truth. This surface owns shape only — defaults,
//! validation, and schema groups. Semantics (trigger evaluation, sizing
//! math, stop management) land with the native signal and the equity
//! backtester, which read these same fields; until then no decision path
//! consults this struct. TP-zone weights above 100% are *not* refused here:
//! the script normalizes them down, and the backtester will do the same.

use fluent_wvr::{Describable, FieldAccess};

use crate::error::StrategyError;

/// Direction options for the strategy direction dial (script spelling).
#[must_use]
pub fn direction_options() -> Vec<String> {
    ["Both", "Long Only", "Short Only"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Sizing-mode options (script spelling).
#[must_use]
pub fn size_mode_options() -> Vec<String> {
    ["Risk Percent", "Percent of Equity", "Fixed Quantity"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Initial-stop options (script spelling).
#[must_use]
pub fn stop_type_options() -> Vec<String> {
    ["ATR", "Swing", "Percent"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Take-profit distance-unit options (script spelling).
#[must_use]
pub fn tp_basis_options() -> Vec<String> {
    ["R Multiple", "ATR"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Trailing-stop options (script spelling).
#[must_use]
pub fn trail_type_options() -> Vec<String> {
    ["Chandelier ATR", "Ribbon Edge", "Percent"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Exhaustion-response options (script spelling).
#[must_use]
pub fn exit_action_options() -> Vec<String> {
    [
        "Off",
        "Tighten Trail",
        "Partial Close",
        "Tighten + Partial",
        "Close All",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// Table-location options (Pine `Positions` spelling, lowercased member
/// names). Duplicates the paint-edge position list by value: the two crates
/// must not import each other, and the Pine enum stays the authority both
/// transcribe; a single shared home is a filed follow-up.
#[must_use]
pub fn table_position_options() -> Vec<String> {
    [
        "top_left",
        "top_center",
        "top_right",
        "middle_left",
        "middle_center",
        "middle_right",
        "bottom_left",
        "bottom_center",
        "bottom_right",
        "top_left_pane",
        "top_right_pane",
        "bottom_left_pane",
        "bottom_right_pane",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

/// One IntegrationLite strategy configuration: every strategy-group dial
/// with its script default. A new dial appears in settings without a UI
/// edit; semantics arrive with the native signal and the equity backtester.
#[derive(
    Debug,
    Clone,
    PartialEq,
    bon::Builder,
    serde::Serialize,
    serde::Deserialize,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
#[allow(clippy::struct_excessive_bools)]
pub struct IntegrationLiteStrategyConfig {
    // ── Strategy: General ──────────────────────────────────────────────
    /// Master switch for entries.
    #[field(
        desc = "Enable trading",
        group = "Strategy: General",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Allowed directions.
    #[field(
        desc = "Direction",
        group = "Strategy: General",
        tooltip = "Which side the strategy may enter",
        variants = "crate::integration_lite::direction_options",
        axis = "task-value"
    )]
    #[builder(default = "Both".to_owned())]
    #[serde(default = "default_direction")]
    pub direction: String,
    /// Flip the position on a filtered opposite signal.
    #[field(
        desc = "Reverse on opposite signal",
        group = "Strategy: General",
        tooltip = "When flat-or-opposite and a filtered opposite entry fires, flip the position in one order",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub allow_reverse: bool,
    /// Bars before the first order, so every oscillator is seeded.
    #[field(
        desc = "Warmup bars",
        min = 0,
        step = 10,
        group = "Strategy: General",
        tooltip = "No orders until this many bars have been calculated",
        axis = "task-value"
    )]
    #[builder(default = 100)]
    #[serde(default = "default_warmup_bars")]
    pub warmup_bars: i64,
    // ── Strategy: Entry Signals ────────────────────────────────────────
    /// Momentum breakout trigger.
    #[field(
        desc = "Momentum breakout",
        group = "Strategy: Entry Signals",
        tooltip = "Long when the fast oscillator crosses up through +Level; short when it crosses down through -Level",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub sig_momentum: bool,
    /// Momentum trigger level.
    #[field(
        desc = "Level",
        min = 10.0,
        step = 10.0,
        group = "Strategy: Entry Signals",
        axis = "task-value"
    )]
    #[builder(default = 100.0)]
    #[serde(default = "default_sig_entry_level")]
    pub sig_entry_level: f64,
    /// Use the adaptive threshold instead of the fixed level.
    #[field(
        desc = "Adaptive level",
        group = "Strategy: Entry Signals",
        tooltip = "Use the adaptive threshold instead of the fixed level",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub sig_adaptive_level: bool,
    /// Exhaustion-reversal trigger.
    #[field(
        desc = "Exhaustion reversal",
        group = "Strategy: Entry Signals",
        tooltip = "Counter-trend entry on bearish/bullish exhaustion ending",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub sig_exhaust_entry: bool,
    /// Ribbon-flip trigger.
    #[field(
        desc = "Ribbon flip",
        group = "Strategy: Entry Signals",
        tooltip = "Long when EIT crosses above the ribbon signal; short when below",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub sig_ribbon_flip: bool,
    /// Fast/slow oscillator-cross trigger.
    #[field(
        desc = "Fast/slow oscillator cross",
        group = "Strategy: Entry Signals",
        tooltip = "Long when the fast oscillator crosses above the slow one; short when below",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub sig_osc_cross: bool,
    /// Exhaustion level for exhaustion exits and reversal entries.
    #[field(
        desc = "Exhaustion level",
        min = 50.0,
        step = 10.0,
        group = "Strategy: Entry Signals",
        tooltip = "Fast oscillator crossing back through this level marks exhaustion",
        axis = "task-value"
    )]
    #[builder(default = 200.0)]
    #[serde(default = "default_sig_exhaust_level")]
    pub sig_exhaust_level: f64,
    /// Setup window: a trigger stays armed N more bars for filters to pass.
    #[field(
        desc = "Setup window (bars)",
        min = 0,
        max = 20,
        group = "Strategy: Entry Signals",
        tooltip = "0 = filters must pass on the trigger bar",
        axis = "task-value"
    )]
    #[builder(default = 0)]
    #[serde(default = "default_zero_i64")]
    pub sig_window: i64,
    // ── Dual-oscillator exhaustion (Strategy: Entry Signals) ──────────
    /// Dual-oscillator exhaustion trigger.
    #[field(
        desc = "Dual-oscillator exhaustion",
        group = "Strategy: Entry Signals",
        inline = "dual1",
        tooltip = "Counter-trend entry on exhaustion confirmed by both oscillators",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub sig_dual: bool,
    /// Slow-oscillator level for the dual trigger.
    #[field(
        desc = "Slow level",
        min = 10.0,
        step = 10.0,
        group = "Strategy: Entry Signals",
        inline = "dual1",
        axis = "task-value"
    )]
    #[builder(default = 150.0)]
    #[serde(default = "default_sig_dual_slow")]
    pub sig_dual_slow: f64,
    /// Fast-oscillator level for the dual trigger.
    #[field(
        desc = "Fast level",
        min = 10.0,
        step = 10.0,
        group = "Strategy: Entry Signals",
        inline = "dual1",
        axis = "task-value"
    )]
    #[builder(default = 200.0)]
    #[serde(default = "default_sig_dual_fast")]
    pub sig_dual_fast: f64,
    /// Fast-oscillator lookback for the dual trigger.
    #[field(
        desc = "Fast osc lookback (bars)",
        min = 0,
        max = 10,
        group = "Strategy: Entry Signals",
        inline = "dual2",
        tooltip = "0 = the fast oscillator must be beyond its level on the same bar",
        axis = "task-value"
    )]
    #[builder(default = 0)]
    #[serde(default = "default_zero_i64")]
    pub sig_dual_lookback: i64,
    /// Bypass trend filters for the dual trigger.
    #[field(
        desc = "Bypass trend filters",
        group = "Strategy: Entry Signals",
        inline = "dual2",
        tooltip = "Skip ribbon, MA-health, ribbon-health, slow-side and extension filters for this signal only",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub sig_dual_bypass: bool,
    /// Let the dual trigger also fire exhaustion exits.
    #[field(
        desc = "Also count as exhaustion for exits",
        group = "Strategy: Entry Signals",
        tooltip = "An open trade's exhaustion response also fires on the opposite dual signal",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub sig_dual_exit: bool,
    // ── Strategy: Entry Filters ────────────────────────────────────────
    /// Ribbon-alignment filter.
    #[field(
        desc = "Ribbon alignment",
        group = "Strategy: Entry Filters",
        tooltip = "Long needs EIT above the ribbon signal; short needs below",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub filt_ribbon: bool,
    /// MA-health alignment filter.
    #[field(
        desc = "MA health alignment",
        group = "Strategy: Entry Filters",
        inline = "f2",
        tooltip = "Long needs MA health above +Min; short needs below -Min",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub filt_ma_health: bool,
    /// MA-health minimum.
    #[field(
        desc = "Min",
        min = 0.0,
        max = 100.0,
        step = 5.0,
        group = "Strategy: Entry Filters",
        inline = "f2",
        axis = "task-value"
    )]
    #[builder(default = 10.0)]
    #[serde(default = "default_filt_ma_health_min")]
    pub filt_ma_health_min: f64,
    /// Ribbon-health alignment filter.
    #[field(
        desc = "Ribbon health alignment",
        group = "Strategy: Entry Filters",
        tooltip = "Long needs ribbon health above zero; short needs below",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_ribbon_health: bool,
    /// Slow-oscillator side filter.
    #[field(
        desc = "Slow oscillator side",
        group = "Strategy: Entry Filters",
        inline = "f4",
        tooltip = "Long needs the slow oscillator above +Min; short needs below -Min",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_slow: bool,
    /// Slow-oscillator minimum (may be negative to allow slight disagreement).
    #[field(
        desc = "Min",
        step = 10.0,
        group = "Strategy: Entry Filters",
        inline = "f4",
        axis = "task-value"
    )]
    #[builder(default = 0.0)]
    #[serde(default = "default_zero_f64")]
    pub filt_slow_min: f64,
    /// Delta-volume alignment filter.
    #[field(
        desc = "Delta volume alignment",
        group = "Strategy: Entry Filters",
        inline = "f5",
        tooltip = "Long needs average buy-minus-sell volume positive over N bars",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_delta: bool,
    /// Delta-averaging window in bars.
    #[field(
        desc = "Bars",
        min = 1,
        max = 20,
        group = "Strategy: Entry Filters",
        inline = "f5",
        axis = "task-value"
    )]
    #[builder(default = 3)]
    #[serde(default = "default_filt_delta_len")]
    pub filt_delta_len: i64,
    /// Minimum relative-volume filter.
    #[field(
        desc = "Min relative volume",
        group = "Strategy: Entry Filters",
        inline = "f6",
        tooltip = "Requires current volume over volume SMA to be at least this multiple",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_volume: bool,
    /// Relative-volume multiple.
    #[field(
        desc = "x",
        min = 0.1,
        step = 0.1,
        group = "Strategy: Entry Filters",
        inline = "f6",
        axis = "task-value"
    )]
    #[builder(default = 0.8)]
    #[serde(default = "default_filt_volume_min")]
    pub filt_volume_min: f64,
    /// Maximum-distance-from-mean filter.
    #[field(
        desc = "Max distance from mean",
        group = "Strategy: Entry Filters",
        inline = "f7",
        tooltip = "Skips entries when price is stretched more than this many ATRs from the mean",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_extension: bool,
    /// Extension limit in ATRs.
    #[field(
        desc = "ATR",
        min = 0.5,
        step = 0.25,
        group = "Strategy: Entry Filters",
        inline = "f7",
        axis = "task-value"
    )]
    #[builder(default = 2.5)]
    #[serde(default = "default_filt_extension_max")]
    pub filt_extension_max: f64,
    /// Skip low-volume-regime bars.
    #[field(
        desc = "Skip low-volume regime",
        group = "Strategy: Entry Filters",
        inline = "f8",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_regime_lowvol: bool,
    /// Skip volatile-regime bars.
    #[field(
        desc = "Skip volatile regime",
        group = "Strategy: Entry Filters",
        inline = "f8",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub filt_regime_volatile: bool,
    // ── Strategy: Position Size & Initial Stop ─────────────────────────
    /// Sizing mode.
    #[field(
        desc = "Sizing mode",
        group = "Strategy: Position Size & Initial Stop",
        tooltip = "Risk Percent sizes so hitting the stop loses this % of equity",
        variants = "crate::integration_lite::size_mode_options",
        axis = "task-value"
    )]
    #[builder(default = default_size_mode())]
    #[serde(default = "default_size_mode")]
    pub size_mode: String,
    /// Risk percent of equity per trade.
    #[field(
        desc = "Risk % of equity",
        min = 0.05,
        max = 10.0,
        step = 0.05,
        group = "Strategy: Position Size & Initial Stop",
        axis = "task-value"
    )]
    #[builder(default = 0.5)]
    #[serde(default = "default_risk_pct")]
    pub risk_pct: f64,
    /// Notional percent of equity per trade.
    #[field(
        desc = "Notional % of equity",
        min = 1.0,
        max = 500.0,
        step = 5.0,
        group = "Strategy: Position Size & Initial Stop",
        axis = "task-value"
    )]
    #[builder(default = 25.0)]
    #[serde(default = "default_size_pct")]
    pub size_pct: f64,
    /// Fixed quantity per trade.
    #[field(
        desc = "Fixed quantity",
        min = 0.0,
        step = 1.0,
        group = "Strategy: Position Size & Initial Stop",
        axis = "task-value"
    )]
    #[builder(default = 1.0)]
    #[serde(default = "default_fixed_qty")]
    pub fixed_qty: f64,
    /// Hard notional cap per trade (100 = no leverage).
    #[field(
        desc = "Max notional % of equity",
        min = 1.0,
        max = 1000.0,
        step = 10.0,
        group = "Strategy: Position Size & Initial Stop",
        tooltip = "Hard cap on position value for every sizing mode",
        axis = "task-value"
    )]
    #[builder(default = 100.0)]
    #[serde(default = "default_max_notional_pct")]
    pub max_notional_pct: f64,
    /// ATR length behind the risk stop.
    #[field(
        desc = "Risk ATR length",
        min = 2,
        max = 100,
        group = "Strategy: Position Size & Initial Stop",
        inline = "atr",
        axis = "task-value"
    )]
    #[builder(default = 14)]
    #[serde(default = "default_risk_atr_len")]
    pub risk_atr_len: i64,
    /// Initial-stop construction.
    #[field(
        desc = "Initial stop",
        group = "Strategy: Position Size & Initial Stop",
        inline = "stop1",
        tooltip = "ATR, swing beyond recent extremes, or a fixed percent of price",
        variants = "crate::integration_lite::stop_type_options",
        axis = "task-value"
    )]
    #[builder(default = default_stop_type())]
    #[serde(default = "default_stop_type")]
    pub stop_type: String,
    /// ATR multiple for ATR stops.
    #[field(
        desc = "ATR mult",
        min = 0.2,
        step = 0.1,
        group = "Strategy: Position Size & Initial Stop",
        inline = "stop2",
        axis = "task-value"
    )]
    #[builder(default = 1.5)]
    #[serde(default = "default_stop_atr_mult")]
    pub stop_atr_mult: f64,
    /// Percent distance for percent stops.
    #[field(
        desc = "Percent",
        min = 0.05,
        step = 0.05,
        group = "Strategy: Position Size & Initial Stop",
        inline = "stop2",
        axis = "task-value"
    )]
    #[builder(default = 1.0)]
    #[serde(default = "default_stop_pct")]
    pub stop_pct: f64,
    /// Swing lookback in bars.
    #[field(
        desc = "Swing bars",
        min = 2,
        max = 60,
        group = "Strategy: Position Size & Initial Stop",
        inline = "stop3",
        axis = "task-value"
    )]
    #[builder(default = 8)]
    #[serde(default = "default_swing_len")]
    pub swing_len: i64,
    /// Swing buffer in ATRs.
    #[field(
        desc = "Swing buffer (ATR)",
        min = 0.0,
        step = 0.05,
        group = "Strategy: Position Size & Initial Stop",
        inline = "stop3",
        axis = "task-value"
    )]
    #[builder(default = 0.25)]
    #[serde(default = "default_swing_buf_atr")]
    pub swing_buf_atr: f64,
    /// Stop-distance floor in ATRs.
    #[field(
        desc = "Min stop (ATR)",
        min = 0.1,
        step = 0.1,
        group = "Strategy: Position Size & Initial Stop",
        inline = "clamp",
        tooltip = "Stop distance is clamped into this range for every stop type",
        axis = "task-value"
    )]
    #[builder(default = 0.5)]
    #[serde(default = "default_min_stop_atr")]
    pub min_stop_atr: f64,
    /// Stop-distance ceiling in ATRs.
    #[field(
        desc = "Max stop (ATR)",
        min = 0.5,
        step = 0.25,
        group = "Strategy: Position Size & Initial Stop",
        inline = "clamp",
        axis = "task-value"
    )]
    #[builder(default = 4.0)]
    #[serde(default = "default_max_stop_atr")]
    pub max_stop_atr: f64,
    // ── Strategy: Take Profit Zones ────────────────────────────────────
    /// Zone-distance unit.
    #[field(
        desc = "Zone distance unit",
        group = "Strategy: Take Profit Zones",
        tooltip = "R Multiple measures from the stop distance, ATR from the entry ATR",
        variants = "crate::integration_lite::tp_basis_options",
        axis = "task-value"
    )]
    #[builder(default = default_tp_basis())]
    #[serde(default = "default_tp_basis")]
    pub tp_basis: String,
    /// First take-profit distance.
    #[field(
        desc = "TP1",
        min = 0.1,
        step = 0.1,
        group = "Strategy: Take Profit Zones",
        inline = "tp1",
        axis = "task-value"
    )]
    #[builder(default = 1.0)]
    #[serde(default = "default_tp1_mult")]
    pub tp1_mult: f64,
    /// First take-profit share of the position.
    #[field(
        desc = "% of position",
        min = 0.0,
        max = 100.0,
        step = 5.0,
        group = "Strategy: Take Profit Zones",
        inline = "tp1",
        axis = "task-value"
    )]
    #[builder(default = 30.0)]
    #[serde(default = "default_tp1_pct")]
    pub tp1_pct: f64,
    /// Second take-profit distance.
    #[field(
        desc = "TP2",
        min = 0.1,
        step = 0.1,
        group = "Strategy: Take Profit Zones",
        inline = "tp2",
        axis = "task-value"
    )]
    #[builder(default = 2.0)]
    #[serde(default = "default_tp2_mult")]
    pub tp2_mult: f64,
    /// Second take-profit share of the position.
    #[field(
        desc = "% of position",
        min = 0.0,
        max = 100.0,
        step = 5.0,
        group = "Strategy: Take Profit Zones",
        inline = "tp2",
        axis = "task-value"
    )]
    #[builder(default = 30.0)]
    #[serde(default = "default_tp2_pct")]
    pub tp2_pct: f64,
    /// Third take-profit distance.
    #[field(
        desc = "TP3",
        min = 0.1,
        step = 0.1,
        group = "Strategy: Take Profit Zones",
        inline = "tp3",
        tooltip = "A zone with 0% is disabled; whatever is left runs on the stop",
        axis = "task-value"
    )]
    #[builder(default = 3.5)]
    #[serde(default = "default_tp3_mult")]
    pub tp3_mult: f64,
    /// Third take-profit share of the position.
    #[field(
        desc = "% of position",
        min = 0.0,
        max = 100.0,
        step = 5.0,
        group = "Strategy: Take Profit Zones",
        inline = "tp3",
        axis = "task-value"
    )]
    #[builder(default = 0.0)]
    #[serde(default = "default_zero_f64")]
    pub tp3_pct: f64,
    // ── Strategy: Breakeven ────────────────────────────────────────────
    /// Move the stop to breakeven.
    #[field(
        desc = "Move stop to breakeven",
        group = "Strategy: Breakeven",
        inline = "be1",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub be_enable: bool,
    /// Breakeven once any take-profit fills.
    #[field(
        desc = "After any TP fills",
        group = "Strategy: Breakeven",
        inline = "be1",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub be_on_tp: bool,
    /// Breakeven at this profit in ATRs (0 disables the profit trigger).
    #[field(
        desc = "Or at profit (ATR)",
        min = 0.0,
        step = 0.25,
        group = "Strategy: Breakeven",
        inline = "be2",
        tooltip = "0 disables the profit trigger",
        axis = "task-value"
    )]
    #[builder(default = 1.0)]
    #[serde(default = "default_be_trigger_atr")]
    pub be_trigger_atr: f64,
    /// Breakeven offset past entry in ATRs.
    #[field(
        desc = "Offset (ATR)",
        min = 0.0,
        step = 0.05,
        group = "Strategy: Breakeven",
        inline = "be2",
        tooltip = "Places the stop slightly beyond entry to cover fees and slippage",
        axis = "task-value"
    )]
    #[builder(default = 0.1)]
    #[serde(default = "default_be_offset_atr")]
    pub be_offset_atr: f64,
    // ── Strategy: Trailing Stop ────────────────────────────────────────
    /// Trailing-stop construction.
    #[field(
        desc = "Trail type",
        group = "Strategy: Trailing Stop",
        tooltip = "Chandelier trails the best price, ribbon edge trails the band, percent trails by percent off the extreme",
        variants = "crate::integration_lite::trail_type_options",
        axis = "task-value"
    )]
    #[builder(default = default_trail_type())]
    #[serde(default = "default_trail_type")]
    pub trail_type: String,
    /// Normal trailing distance in ATRs.
    #[field(
        desc = "Normal ATR mult",
        min = 0.2,
        step = 0.1,
        group = "Strategy: Trailing Stop",
        inline = "t1",
        axis = "task-value"
    )]
    #[builder(default = 2.5)]
    #[serde(default = "default_trail_mult")]
    pub trail_mult: f64,
    /// Tight trailing distance after exhaustion.
    #[field(
        desc = "Tight (after exhaustion)",
        min = 0.1,
        step = 0.1,
        group = "Strategy: Trailing Stop",
        inline = "t1",
        axis = "task-value"
    )]
    #[builder(default = 1.0)]
    #[serde(default = "default_trail_tight_mult")]
    pub trail_tight_mult: f64,
    /// Ribbon-edge buffer in ATRs.
    #[field(
        desc = "Ribbon buffer (ATR)",
        min = 0.0,
        step = 0.1,
        group = "Strategy: Trailing Stop",
        inline = "t2",
        axis = "task-value"
    )]
    #[builder(default = 0.5)]
    #[serde(default = "default_trail_ribbon_buf")]
    pub trail_ribbon_buf: f64,
    /// Percent distance for percent trails (tight mode halves it).
    #[field(
        desc = "Percent",
        min = 0.05,
        step = 0.05,
        group = "Strategy: Trailing Stop",
        inline = "t2",
        tooltip = "Tight mode halves this percentage",
        axis = "task-value"
    )]
    #[builder(default = 1.0)]
    #[serde(default = "default_trail_pct")]
    pub trail_pct: f64,
    /// Trail from entry immediately.
    #[field(
        desc = "Activate: immediately",
        group = "Strategy: Trailing Stop",
        inline = "a1",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub trail_immediate: bool,
    /// Trail once any take-profit fills.
    #[field(
        desc = "After any TP",
        group = "Strategy: Trailing Stop",
        inline = "a1",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub trail_on_tp: bool,
    /// Trail when the exhaustion response fires.
    #[field(
        desc = "On exhaustion",
        group = "Strategy: Trailing Stop",
        inline = "a2",
        tooltip = "Activates when the exhaustion response below fires",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub trail_on_exhaustion: bool,
    /// Trail once profit reaches the trigger.
    #[field(
        desc = "At profit (ATR)",
        group = "Strategy: Trailing Stop",
        inline = "a2",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub trail_on_profit: bool,
    /// Profit trigger in ATRs.
    #[field(
        desc = "At profit (ATR)",
        min = 0.25,
        step = 0.25,
        group = "Strategy: Trailing Stop",
        inline = "a2",
        axis = "task-value"
    )]
    #[builder(default = 1.5)]
    #[serde(default = "default_trail_profit_atr")]
    pub trail_profit_atr: f64,
    // ── Strategy: Exhaustion & Exit Signals ────────────────────────────
    /// Exhaustion response.
    #[field(
        desc = "Exhaustion response",
        group = "Strategy: Exhaustion & Exit Signals",
        tooltip = "What to do when the fast oscillator crosses back through the exhaustion level against the trade",
        variants = "crate::integration_lite::exit_action_options",
        axis = "task-value"
    )]
    #[builder(default = default_exit_action())]
    #[serde(default = "default_exit_action")]
    pub exit_action: String,
    /// Share of the open position closed on exhaustion.
    #[field(
        desc = "Partial close %",
        min = 5.0,
        max = 100.0,
        step = 5.0,
        group = "Strategy: Exhaustion & Exit Signals",
        inline = "x1",
        tooltip = "Percent of the still-open position closed on exhaustion, once per trade",
        axis = "task-value"
    )]
    #[builder(default = 50.0)]
    #[serde(default = "default_exit_partial_pct")]
    pub exit_partial_pct: f64,
    /// Exhaustion response only when in profit.
    #[field(
        desc = "Only when in profit",
        group = "Strategy: Exhaustion & Exit Signals",
        inline = "x1",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub exit_only_profit: bool,
    /// Close on an opposite trigger (before filters).
    #[field(
        desc = "Exit on opposite trigger",
        group = "Strategy: Exhaustion & Exit Signals",
        tooltip = "Close the trade when an opposite entry trigger fires",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub exit_opposite: bool,
    /// Exit on a ribbon flip against the trade.
    #[field(
        desc = "Exit on ribbon flip against",
        group = "Strategy: Exhaustion & Exit Signals",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub exit_ribbon: bool,
    /// Exit on a fast-oscillator zero-cross against the trade.
    #[field(
        desc = "Exit on fast osc zero-cross against",
        group = "Strategy: Exhaustion & Exit Signals",
        axis = "task-value"
    )]
    #[builder(default = default_false())]
    #[serde(default = "default_false")]
    pub exit_zero_cross: bool,
    /// Time stop in bars (0 disables).
    #[field(
        desc = "Time stop (bars, 0 = off)",
        min = 0,
        step = 10,
        group = "Strategy: Exhaustion & Exit Signals",
        axis = "task-value"
    )]
    #[builder(default = 0)]
    #[serde(default = "default_zero_i64")]
    pub exit_time_bars: i64,
    // ── Strategy: Cooldowns & Daily Limits ─────────────────────────────
    /// Cooldown after any exit, in bars.
    #[field(
        desc = "Cooldown after exit (bars)",
        min = 0,
        max = 500,
        group = "Strategy: Cooldowns & Daily Limits",
        axis = "task-value"
    )]
    #[builder(default = 3)]
    #[serde(default = "default_cd_bars")]
    pub cd_bars: i64,
    /// Extra cooldown after a losing exit, in bars.
    #[field(
        desc = "Extra cooldown after loss (bars)",
        min = 0,
        max = 500,
        group = "Strategy: Cooldowns & Daily Limits",
        axis = "task-value"
    )]
    #[builder(default = 10)]
    #[serde(default = "default_cd_loss_bars")]
    pub cd_loss_bars: i64,
    /// Maximum trades per day (0 disables).
    #[field(
        desc = "Max trades per day (0 = off)",
        min = 0,
        max = 100,
        group = "Strategy: Cooldowns & Daily Limits",
        axis = "task-value"
    )]
    #[builder(default = 8)]
    #[serde(default = "default_max_trades_day")]
    pub max_trades_day: i64,
    /// Halt entries for the day after this many losses in a row (0 disables).
    #[field(
        desc = "Halt after N losses in a row (0 = off)",
        min = 0,
        max = 20,
        group = "Strategy: Cooldowns & Daily Limits",
        tooltip = "Stops new entries for the rest of the day",
        axis = "task-value"
    )]
    #[builder(default = 3)]
    #[serde(default = "default_max_consec_losses")]
    pub max_consec_losses: i64,
    /// Flatten and halt when equity falls this far below the day start (0 disables).
    #[field(
        desc = "Max daily loss % (0 = off)",
        min = 0.0,
        max = 20.0,
        step = 0.25,
        group = "Strategy: Cooldowns & Daily Limits",
        tooltip = "Flattens and halts for the rest of the day",
        axis = "correctness"
    )]
    #[builder(default = 2.0)]
    #[serde(default = "default_max_daily_loss_pct")]
    pub max_daily_loss_pct: f64,
    /// Flatten at the end of the session.
    #[field(
        desc = "Flatten at end of session",
        group = "Strategy: Cooldowns & Daily Limits",
        inline = "eod",
        tooltip = "Intraday charts only, and only when the session filter is enabled",
        axis = "task-value"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub flatten_eod: bool,
    /// Flatten this many minutes before the session end.
    #[field(
        desc = "Min before end",
        min = 0,
        max = 120,
        group = "Strategy: Cooldowns & Daily Limits",
        inline = "eod",
        axis = "task-value"
    )]
    #[builder(default = 0)]
    #[serde(default = "default_zero_i64")]
    pub flatten_min_before: i64,
    /// No new entries in the final minutes of the session.
    #[field(
        desc = "No new entries final (min)",
        min = 0,
        max = 240,
        group = "Strategy: Cooldowns & Daily Limits",
        inline = "eod",
        axis = "task-value"
    )]
    #[builder(default = 5)]
    #[serde(default = "default_noentry_min")]
    pub noentry_min: i64,
    // ── Strategy: Display & Alerts ─────────────────────────────────────
    /// Paint trade markers and levels.
    #[field(
        desc = "Trade markers & levels",
        tab = "visibility",
        group = "Strategy: Display & Alerts",
        inline = "vis1"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub show_markers: bool,
    /// Long marker color.
    #[field(
        desc = "Long marker",
        format = "color",
        group = "Strategy: Display & Alerts",
        inline = "vis1"
    )]
    #[builder(default = default_marker_long_color())]
    #[serde(default = "default_marker_long_color")]
    pub marker_long_color: String,
    /// Short marker color.
    #[field(
        desc = "Short marker",
        format = "color",
        group = "Strategy: Display & Alerts",
        inline = "vis1"
    )]
    #[builder(default = default_marker_short_color())]
    #[serde(default = "default_marker_short_color")]
    pub marker_short_color: String,
    /// Paint the status table.
    #[field(
        desc = "Status table",
        tab = "visibility",
        group = "Strategy: Display & Alerts",
        inline = "vis2"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub show_table: bool,
    /// Status table location.
    #[field(
        desc = "Table location",
        group = "Strategy: Display & Alerts",
        inline = "vis2",
        variants = "crate::integration_lite::table_position_options"
    )]
    #[builder(default = default_table_position())]
    #[serde(default = "default_table_position")]
    pub table_position: String,
    /// Send alerts on entries and exits.
    #[field(
        desc = "Send alert() on entries and exits",
        group = "Strategy: Display & Alerts"
    )]
    #[builder(default = default_true())]
    #[serde(default = "default_true")]
    pub send_alerts: bool,
}

fn default_exit_action() -> String {
    "Tighten + Partial".to_owned()
}

fn default_exit_partial_pct() -> f64 {
    50.0
}

fn default_cd_bars() -> i64 {
    3
}

fn default_cd_loss_bars() -> i64 {
    10
}

fn default_max_trades_day() -> i64 {
    8
}

fn default_max_consec_losses() -> i64 {
    3
}

fn default_max_daily_loss_pct() -> f64 {
    2.0
}

fn default_noentry_min() -> i64 {
    5
}

fn default_marker_long_color() -> String {
    "#20B46C".to_owned()
}

fn default_marker_short_color() -> String {
    "#B42020".to_owned()
}

fn default_table_position() -> String {
    "top_right".to_owned()
}

impl Default for IntegrationLiteStrategyConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Fail-closed shape check for one option dial.
fn check_option(slot: &str, value: &str, options: &[String]) -> Result<(), StrategyError> {
    if options.iter().any(|option| option == value) {
        return Ok(());
    }
    Err(StrategyError::InvalidElement(format!(
        "{slot} {value:?} is not a listed option"
    )))
}

/// Fail-closed range check for one integer dial.
fn check_int(slot: &str, value: i64, min: i64, max: Option<i64>) -> Result<(), StrategyError> {
    if value < min || max.is_some_and(|ceiling| value > ceiling) {
        return Err(StrategyError::InvalidElement(format!(
            "{slot} {value} sits outside its [{min}, {}] window",
            max.map_or("unbounded".to_owned(), |ceiling| ceiling.to_string())
        )));
    }
    Ok(())
}

/// Fail-closed range check for one float dial.
fn check_float(slot: &str, value: f64, min: f64, max: Option<f64>) -> Result<(), StrategyError> {
    if !value.is_finite() || value < min || max.is_some_and(|ceiling| value > ceiling) {
        return Err(StrategyError::InvalidElement(format!(
            "{slot} {value} sits outside its [{min}, {}] window",
            max.map_or("unbounded".to_owned(), |ceiling| ceiling.to_string())
        )));
    }
    Ok(())
}

/// Fail-closed hex-color shape check (`#RRGGBB`): the paint edge parses for
/// real, this only keeps malformed text out of stored documents.
fn check_color(slot: &str, value: &str) -> Result<(), StrategyError> {
    let bytes = value.as_bytes();
    let hex = |b: u8| b.is_ascii_hexdigit();
    if bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|b| hex(*b)) {
        return Ok(());
    }
    Err(StrategyError::InvalidElement(format!(
        "{slot} {value:?} is not a #RRGGBB color"
    )))
}

impl fluent_wvr::validate::Validate for IntegrationLiteStrategyConfig {
    type Err = StrategyError;
    /// Fail-closed self-consistency check: every dial inside its Pine
    /// window, every option dial naming a listed option. TP-zone weights
    /// are deliberately NOT refused above 100%: the script normalizes them
    /// and the backtester will too.
    fn validate(&self) -> Result<(), Self::Err> {
        check_int("warmup_bars", self.warmup_bars, 0, None)?;
        check_float("sig_entry_level", self.sig_entry_level, 10.0, None)?;
        check_float("sig_exhaust_level", self.sig_exhaust_level, 50.0, None)?;
        check_int("sig_window", self.sig_window, 0, Some(20))?;
        check_float("sig_dual_slow", self.sig_dual_slow, 10.0, None)?;
        check_float("sig_dual_fast", self.sig_dual_fast, 10.0, None)?;
        check_int("sig_dual_lookback", self.sig_dual_lookback, 0, Some(10))?;
        check_float(
            "filt_ma_health_min",
            self.filt_ma_health_min,
            0.0,
            Some(100.0),
        )?;
        check_float("filt_slow_min", self.filt_slow_min, f64::NEG_INFINITY, None)?;
        check_int("filt_delta_len", self.filt_delta_len, 1, Some(20))?;
        check_float("filt_volume_min", self.filt_volume_min, 0.1, None)?;
        check_float("filt_extension_max", self.filt_extension_max, 0.5, None)?;
        check_option("direction", &self.direction, &direction_options())?;
        check_option("size_mode", &self.size_mode, &size_mode_options())?;
        check_float("risk_pct", self.risk_pct, 0.05, Some(10.0))?;
        check_float("size_pct", self.size_pct, 1.0, Some(500.0))?;
        check_float("fixed_qty", self.fixed_qty, 0.0, None)?;
        check_float("max_notional_pct", self.max_notional_pct, 1.0, Some(1000.0))?;
        check_int("risk_atr_len", self.risk_atr_len, 2, Some(100))?;
        check_option("stop_type", &self.stop_type, &stop_type_options())?;
        check_float("stop_atr_mult", self.stop_atr_mult, 0.2, None)?;
        check_float("stop_pct", self.stop_pct, 0.05, None)?;
        check_int("swing_len", self.swing_len, 2, Some(60))?;
        check_float("swing_buf_atr", self.swing_buf_atr, 0.0, None)?;
        check_float("min_stop_atr", self.min_stop_atr, 0.1, None)?;
        check_float("max_stop_atr", self.max_stop_atr, 0.5, None)?;
        if self.min_stop_atr > self.max_stop_atr {
            return Err(StrategyError::InvalidElement(format!(
                "min_stop_atr {} exceeds max_stop_atr {}",
                self.min_stop_atr, self.max_stop_atr
            )));
        }
        check_option("tp_basis", &self.tp_basis, &tp_basis_options())?;
        check_float("tp1_mult", self.tp1_mult, 0.1, None)?;
        check_float("tp1_pct", self.tp1_pct, 0.0, Some(100.0))?;
        check_float("tp2_mult", self.tp2_mult, 0.1, None)?;
        check_float("tp2_pct", self.tp2_pct, 0.0, Some(100.0))?;
        check_float("tp3_mult", self.tp3_mult, 0.1, None)?;
        check_float("tp3_pct", self.tp3_pct, 0.0, Some(100.0))?;
        check_float("be_trigger_atr", self.be_trigger_atr, 0.0, None)?;
        check_float("be_offset_atr", self.be_offset_atr, 0.0, None)?;
        check_option("trail_type", &self.trail_type, &trail_type_options())?;
        check_float("trail_mult", self.trail_mult, 0.2, None)?;
        check_float("trail_tight_mult", self.trail_tight_mult, 0.1, None)?;
        check_float("trail_ribbon_buf", self.trail_ribbon_buf, 0.0, None)?;
        check_float("trail_pct", self.trail_pct, 0.05, None)?;
        check_float("trail_profit_atr", self.trail_profit_atr, 0.25, None)?;
        check_option("exit_action", &self.exit_action, &exit_action_options())?;
        check_float("exit_partial_pct", self.exit_partial_pct, 5.0, Some(100.0))?;
        check_int("exit_time_bars", self.exit_time_bars, 0, None)?;
        check_int("cd_bars", self.cd_bars, 0, Some(500))?;
        check_int("cd_loss_bars", self.cd_loss_bars, 0, Some(500))?;
        check_int("max_trades_day", self.max_trades_day, 0, Some(100))?;
        check_int("max_consec_losses", self.max_consec_losses, 0, Some(20))?;
        check_float(
            "max_daily_loss_pct",
            self.max_daily_loss_pct,
            0.0,
            Some(20.0),
        )?;
        check_int("flatten_min_before", self.flatten_min_before, 0, Some(120))?;
        check_int("noentry_min", self.noentry_min, 0, Some(240))?;
        check_color("marker_long_color", &self.marker_long_color)?;
        check_color("marker_short_color", &self.marker_short_color)?;
        check_option(
            "table_position",
            &self.table_position,
            &table_position_options(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/integration_lite.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/trigger_calibration.rs"]
mod trigger_calibration_tests;

fn default_direction() -> String {
    "Both".to_owned()
}

fn default_warmup_bars() -> i64 {
    100
}

fn default_sig_entry_level() -> f64 {
    100.0
}

fn default_sig_exhaust_level() -> f64 {
    200.0
}

fn default_zero_i64() -> i64 {
    0
}

fn default_sig_dual_slow() -> f64 {
    150.0
}

fn default_sig_dual_fast() -> f64 {
    200.0
}

fn default_zero_f64() -> f64 {
    0.0
}

fn default_filt_ma_health_min() -> f64 {
    10.0
}

fn default_filt_delta_len() -> i64 {
    3
}

fn default_filt_volume_min() -> f64 {
    0.8
}

fn default_filt_extension_max() -> f64 {
    2.5
}

fn default_size_mode() -> String {
    "Risk Percent".to_owned()
}

fn default_risk_pct() -> f64 {
    0.5
}

fn default_size_pct() -> f64 {
    25.0
}

fn default_fixed_qty() -> f64 {
    1.0
}

fn default_max_notional_pct() -> f64 {
    100.0
}

fn default_risk_atr_len() -> i64 {
    14
}

fn default_stop_type() -> String {
    "ATR".to_owned()
}

fn default_stop_atr_mult() -> f64 {
    1.5
}

fn default_stop_pct() -> f64 {
    1.0
}

fn default_swing_len() -> i64 {
    8
}

fn default_swing_buf_atr() -> f64 {
    0.25
}

fn default_min_stop_atr() -> f64 {
    0.5
}

fn default_max_stop_atr() -> f64 {
    4.0
}

fn default_tp_basis() -> String {
    "R Multiple".to_owned()
}

fn default_tp1_mult() -> f64 {
    1.0
}

fn default_tp1_pct() -> f64 {
    30.0
}

fn default_tp2_mult() -> f64 {
    2.0
}

fn default_tp2_pct() -> f64 {
    30.0
}

fn default_tp3_mult() -> f64 {
    3.5
}

fn default_be_trigger_atr() -> f64 {
    1.0
}

fn default_be_offset_atr() -> f64 {
    0.1
}

fn default_trail_type() -> String {
    "Chandelier ATR".to_owned()
}

fn default_trail_mult() -> f64 {
    2.5
}

fn default_trail_tight_mult() -> f64 {
    1.0
}

fn default_trail_ribbon_buf() -> f64 {
    0.5
}

fn default_trail_pct() -> f64 {
    1.0
}

fn default_trail_profit_atr() -> f64 {
    1.5
}
