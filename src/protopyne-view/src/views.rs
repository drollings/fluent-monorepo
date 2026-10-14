//! Neutral read DTOs: the server's wire shapes in display vocabulary.
//!
//! Every display-enum field (`direction`, `structure`, `state`,
//! `trading_state`) is a stable string tag matching the wire exactly — the
//! crate never links `strategy`/`venue` enums, so the tags are the contract.
//! Money stays exact (`protopyne_types::Money`); floats are display-native.

use protopyne_types::Money;

/// One inbox row: what the strategy proposed and where it stands.
///
/// Mirrors the proposals route payload field-for-field; `direction`,
/// `structure`, and `state` carry the wire's variant names verbatim
/// (`"Long"`, `"Vertical"`, `"Proposed"`, …).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalView {
    /// Stable proposal identifier.
    pub id: String,
    /// Strategy attribution.
    pub strategy: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Direction of exposure, as a stable string tag.
    pub direction: String,
    /// Option structure, as a stable string tag.
    pub structure: String,
    /// Limit price submitted.
    pub limit_price: Money,
    /// Signal conviction of the bundled preview signal.
    pub conviction: f64,
    /// Lifecycle state after the preview run, as a stable string tag.
    pub state: String,
}

/// Net position in one underlying.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PositionView {
    /// Underlying symbol.
    pub underlying: String,
    /// Signed contracts (or shares); negative is short.
    pub quantity: i64,
    /// Average entry price.
    pub avg_price: Money,
}

/// Aggregate portfolio over the latest preview run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PortfolioView {
    /// Net positions by underlying.
    pub positions: Vec<PositionView>,
    /// Orders filled in the latest run.
    pub filled_orders: u64,
    /// Commission paid in the latest run.
    pub total_fees: Money,
    /// Net signed contracts across positions.
    pub open_contracts: i64,
    /// Trading state when the view was published, as a stable string tag.
    pub trading_state: String,
}

impl Default for PortfolioView {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            filled_orders: 0,
            total_fees: Money::from(0),
            open_contracts: 0,
            // Views boot allowing everything, matching a fresh session.
            trading_state: "Enabled".to_owned(),
        }
    }
}

/// Last operator command the service observed: read-only echo of the
/// journaled command plane.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandStatus {
    /// Command kind (`halt`, `resume`, `close_all`, `cancel_all`).
    pub kind: String,
    /// Human-readable outcome.
    pub detail: String,
    /// Outcome time in epoch milliseconds.
    pub at_ms: i64,
}

/// Liveness plus command-plane status, mirroring the health route payload.
///
/// Field order is alphabetical on purpose: the route historically
/// serialized a `serde_json::Map` (sorted keys), and the DTO keeps that
/// byte order so the server-side swap is invisible on the wire.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HealthView {
    /// Whether new entries currently route.
    pub allows_entries: bool,
    /// Whether exits currently route.
    pub allows_exits: bool,
    /// Last operator command observed, if any.
    pub last_command: Option<CommandStatus>,
    /// Liveness marker (`ok`).
    pub status: String,
    /// Trading state, as a stable string tag.
    pub trading_state: String,
    /// Serving crate version.
    pub version: String,
}

/// Worst-case exposure of an armed grant, shown before confirmation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorstCase {
    /// Maximum trades authorized.
    pub max_trades: u32,
    /// Maximum contracts authorized.
    pub max_contracts: u32,
    /// Maximum committed loss authorized.
    pub max_loss: Money,
    /// Maximum committed notional authorized.
    pub max_notional: Money,
}

/// Response to a successful arming.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArmResponse {
    /// Grant identifier armed.
    pub grant_id: String,
    /// Worst case if every budget is consumed at its limit.
    pub worst_case: WorstCase,
    /// Always true on success.
    pub armed: bool,
}

/// One deny reason and how many bars it kept out of the venue.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DenyCount {
    /// The reason, as recorded on the decision.
    pub reason: String,
    /// Bars carrying this reason.
    pub bars: u64,
}

/// Preview outcome: the run plus why bars did not trade.
///
/// `run` stays an opaque JSON value on this side of the boundary: the typed
/// run tree (manifest, per-bar verdicts, report) is core-owned, and the
/// terminal reads the typed deny histogram, not the run. The value
/// round-trips byte-stable, so route payloads stay identical.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreviewResponse {
    /// The backtest run, opaque here, byte-stable on the wire.
    pub run: serde_json::Value,
    /// Deny reasons across the run, most frequent first.
    pub deny_histogram: Vec<DenyCount>,
}

/// One journaled lifecycle event, read-only.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JournalRowView {
    /// Monotonic sequence number.
    pub seq: i64,
    /// Client ID the event belongs to.
    pub client_id: String,
    /// Event kind (`intent`, `submitted`, `filled`, …).
    pub kind: String,
    /// Machine-readable detail (JSON).
    pub detail: String,
    /// Event time in epoch milliseconds.
    pub at_ms: i64,
}

/// One chartable bar row, mirroring the bars route payload.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChartBar {
    /// Bar close time as epoch seconds.
    pub time: i64,
    /// Open as chart-native float.
    pub open: f64,
    /// High as chart-native float.
    pub high: f64,
    /// Low as chart-native float.
    pub low: f64,
    /// Close as chart-native float.
    pub close: f64,
    /// Traded volume.
    pub volume: u64,
}

/// Chart bars payload: rows plus how many bars never made it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChartBars {
    /// Chartable bars in history order.
    pub rows: Vec<ChartBar>,
    /// Bars skipped for non-finite or unrepresentable floats.
    pub dropped: u64,
}

/// One overlay series (indicator values aligned with chart bars).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverlaySeries {
    /// Kernel id (`sma`, `ema`, `bb`, or `trima`).
    pub kind: String,
    /// Averaging length.
    pub period: i64,
    /// Per-bar values (`None` inside warmup). Indices always match bar
    /// indices — values are never dropped, so rows and overlays agree
    /// positionally.
    pub values: Vec<Option<f64>>,
    /// Bars without a value, mirroring `ChartBars.dropped`.
    pub dropped: u64,
}

/// One replayable step: a bar the core can re-run to, and how far the
/// recorded run got.
///
/// Neutral vocabulary only: a step names a bar and a timestamp, never a
/// pane, a graph key, or a run type. Which panes a step touches is the
/// terminal's own plan, so no core replay type crosses this boundary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayStepView {
    /// Absolute bar index into the replayed history.
    pub bar_index: u64,
    /// Bar close time as RFC 3339, exactly as the core recorded it.
    pub ts: String,
    /// Whether the recorded run reached this step.
    pub done: bool,
    /// Whether the core recorded a checkpoint here. Checkpoints are the
    /// positions an operator scrubs *to*; a step without one is still
    /// scrubbable, it just has no named anchor behind it.
    pub checkpoint: bool,
}

/// Replay read model: where the scrub stands, and the ordered steps it may
/// stand at.
///
/// `cursor` is the task-value answer to "what did the strategy see at this
/// bar" — an exact index the core supplies. `total_bars` is the window the
/// steps span; `cursor == total_bars` is the head (nothing rewound). The
/// revision bookkeeping a terminal uses to decide which panes to recompute
/// never crosses this boundary: it is the terminal's own bookkeeping.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayView {
    /// Bar index on screen (0-based, at most `total_bars`).
    pub cursor: u64,
    /// Bars in the replay window.
    pub total_bars: u64,
    /// Steps in bar order, one per bar in the window.
    pub steps: Vec<ReplayStepView>,
}

#[cfg(test)]
#[path = "../tests/views.rs"]
mod tests;
