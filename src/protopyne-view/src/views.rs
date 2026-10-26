//! Neutral read DTOs: the server's wire shapes in display vocabulary.
//!
//! Every display-enum field (`direction`, `structure`, `state`,
//! `trading_state`) is a stable string tag matching the wire exactly — the
//! crate never links `strategy`/`venue` enums, so the tags are the contract.
//! Money stays exact (`protopyne_types::Money`); floats are display-native.

use protopyne_types::Money;

/// One broker's declared capability flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrokerCapabilityView {
    /// Broker id.
    pub venue: String,
    /// Market-data streams.
    pub streams: bool,
    /// Account summaries.
    pub accounts: bool,
    /// Order reads.
    pub orders: bool,
    /// Execution/deal reads.
    pub deals: bool,
    /// Credential refresh.
    pub auth_refresh: bool,
    /// Native broker bars.
    pub native_bars: bool,
}

/// One broker account summary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrokerAccountView {
    /// Broker id.
    pub venue: String,
    /// Account identifier.
    pub account: String,
    /// Account currency.
    pub currency: String,
    /// Net liquidation.
    pub net_liquid: Money,
    /// Buying power.
    pub buying_power: Money,
    /// Margin used.
    pub margin_used: Money,
    /// When the broker read was observed.
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

/// One order leg.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrokerLegView {
    /// Contract identifier.
    pub contract: String,
    /// Side tag (`Buy`/`Sell`).
    pub side: String,
    /// Quantity.
    pub quantity: u32,
    /// Limit price per contract.
    pub price: Money,
}

/// One broker order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrokerOrderView {
    /// Broker id.
    pub venue: String,
    /// Account identifier.
    pub account: String,
    /// Deterministic client id.
    pub client_id: String,
    /// Proposal attribution.
    pub proposal_id: String,
    /// Strategy attribution.
    pub strategy: String,
    /// Underlying.
    pub underlying: String,
    /// Side tag.
    pub side: String,
    /// Lifecycle status tag.
    pub status: String,
    /// Requested quantity.
    pub quantity: u32,
    /// Filled quantity.
    pub filled_quantity: u32,
    /// Limit price.
    pub limit_price: Money,
    /// Average fill price, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_price: Option<Money>,
    /// Fees paid.
    pub fees_paid: Money,
    /// Last update.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Legs, when the order is a package.
    #[serde(default)]
    pub legs: Vec<BrokerLegView>,
}

/// One broker execution.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrokerDealView {
    /// Broker id.
    pub venue: String,
    /// Account identifier.
    pub account: String,
    /// Deal identifier.
    pub deal_id: String,
    /// Client id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Broker order id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Contract, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    /// Underlying.
    pub underlying: String,
    /// Side tag.
    pub side: String,
    /// Executed quantity.
    pub quantity: u32,
    /// Execution price.
    pub price: Money,
    /// Commission.
    pub fee: Money,
    /// Execution timestamp.
    pub executed_at: chrono::DateTime<chrono::Utc>,
    /// Observation timestamp.
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

/// Response envelope for a broker read refusal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrokerRefusal {
    /// Refusal reason.
    pub error: String,
}

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

/// Portfolio source tag for the venue's booked fills: what actually filled.
pub const PORTFOLIO_SOURCE_BOOKED: &str = "booked";

/// Portfolio source tag for the rehearsal projection over a preview run.
pub const PORTFOLIO_SOURCE_REHEARSAL: &str = "rehearsal";

/// Aggregate portfolio over one source: the venue book or a preview run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PortfolioView {
    /// Net positions by underlying.
    pub positions: Vec<PositionView>,
    /// Orders filled in the folded source.
    pub filled_orders: u64,
    /// Commission paid in the folded source.
    pub total_fees: Money,
    /// Net signed contracts across positions.
    pub open_contracts: i64,
    /// Trading state when the view was published, as a stable string tag.
    pub trading_state: String,
    /// Which source produced the view, as a stable string tag
    /// ([`PORTFOLIO_SOURCE_BOOKED`] or [`PORTFOLIO_SOURCE_REHEARSAL`]).
    #[serde(default = "default_portfolio_source")]
    pub source: String,
}

/// Rows written before the source tag existed read as the rehearsal
/// projection they were.
fn default_portfolio_source() -> String {
    PORTFOLIO_SOURCE_REHEARSAL.to_owned()
}

impl Default for PortfolioView {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            filled_orders: 0,
            total_fees: Money::zero(),
            open_contracts: 0,
            // Views boot allowing everything, matching a fresh session.
            trading_state: "Enabled".to_owned(),
            // An empty portfolio carries no booked fills yet.
            source: PORTFOLIO_SOURCE_BOOKED.to_owned(),
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
/// byte order so the server-side swap is invisible on the wire. The store
/// fields stay in the same order, and each is `None`-skipped so a view with
/// no store reading on record serialises byte-identically to the form
/// without them. The tape fields follow the same rule: each is
/// `None`-skipped and reports only once a tape read has been observed, so a
/// server that never served through its tape keeps the pre-tape bytes. The
/// last-miss tag rides the same stable-tag convention as the other
/// display-enum fields (`"NoTape"`, `"UnsupportedTimeframe"`,
/// `"UnknownSymbol"`, `"Denied"`, `"Store"`).
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
    /// Unresolved store holes the last classification reported, if observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_open_gaps: Option<u64>,
    /// Whether the price store answered its last coverage probe, if observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_reachable: Option<bool>,
    /// Age of the newest committed bar in seconds, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_watermark_age_secs: Option<u64>,
    /// Tape reads the store permit denied, if any tape read was observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_denied: Option<u64>,
    /// The most recent tape-miss kind, if any miss was observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_last_miss: Option<String>,
    /// Tape reads with no tape configured, if any tape read was observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_no_tape: Option<u64>,
    /// Tape reads the durable tape served, if any tape read was observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_served: Option<u64>,
    /// Tape reads that failed for another store reason, if observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_store_error: Option<u64>,
    /// Tape reads whose symbol the tape does not hold, if observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_unknown_symbol: Option<u64>,
    /// Tape reads whose label named an unknown bar spacing, if observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_unsupported_timeframe: Option<u64>,
    /// Trading state, as a stable string tag.
    pub trading_state: String,
    /// Serving crate version.
    pub version: String,
    /// Open ingest gaps, when an ingest service is attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_open_gaps: Option<u64>,
    /// Ingest watermark age in seconds, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_watermark_age_secs: Option<u64>,
    /// Last ingest repair status, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_last_repair: Option<String>,
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
    /// Where the numbers came from (`ChainDerived` when priced from a chain,
    /// else `BudgetEcho`, the budget fields as an upper bound). An absent
    /// value reads as empty — unknown provenance, never as a computed
    /// exposure.
    #[serde(default)]
    pub basis: String,
}

/// Side of a manual ticket, in the vocabulary the ticket form picks from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TicketSide {
    /// Buy at a limit (or better).
    Buy,
    /// Sell at a limit (or better).
    Sell,
}

/// One manual ticket draft: the four fields the terminal's form fills.
///
/// This is the neutral **shape** of a manual order, not the venue's intent.
/// The terminal fills it and the core turns it into an `OrderIntent` (minting
/// the ticket identity and validating every value), so the intent schema —
/// and the authority to submit one — never crosses into the paint edge.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TicketDraft {
    /// Buy or sell.
    pub side: TicketSide,
    /// Whole contracts (or shares).
    pub quantity: u32,
    /// Limit price per contract/share, as decimal text: the core parses it
    /// with the venue's own scale-guarded rule, so a malformed price refuses
    /// there instead of being re-validated here.
    pub limit_price: String,
    /// Underlying symbol.
    pub symbol: String,
}

/// Structure a rehearsal is opened for, in the picker's vocabulary.
///
/// A **hint**, not a decision: the core maps it to its own structure kind and
/// refuses a shape it cannot price. The neutral crate never links the
/// strategy enum, so this mirror is what crosses the read boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StructureKindHint {
    /// Single stock or single option leg.
    Single,
    /// Vertical spread (two legs, same expiry).
    Vertical,
    /// Calendar spread (same strikes, different expiries).
    Calendar,
    /// Iron condor (two paired verticals).
    Condor,
    /// Uncovered short option with unbounded loss.
    NakedShort,
}

impl StructureKindHint {
    /// The picker tag (`"Single"`, `"Vertical"`, …) as a hint, `None` for a
    /// tag this contract does not know.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "Single" => Some(Self::Single),
            "Vertical" => Some(Self::Vertical),
            "Calendar" => Some(Self::Calendar),
            "Condor" => Some(Self::Condor),
            "NakedShort" => Some(Self::NakedShort),
            _ => None,
        }
    }
}

/// The economics the core books on every rehearsal proposal for one
/// underlying and structure.
///
/// The core owns these numbers: a replay start takes them from here instead
/// of authoring its own. Money rides decimal text (the neutral crate keeps
/// exactness without narrowing the scale at the boundary).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RehearsalEconomics {
    /// Strategy name carried on every proposal for attribution.
    pub strategy: String,
    /// Worst-case loss booked per proposal, as decimal text.
    pub max_loss: String,
    /// Committed notional booked per proposal, as decimal text.
    pub notional: String,
    /// Days to expiry requested per proposal (the nearest expiry the
    /// authority admits).
    pub dte_days: u32,
    /// Id of the grant these values come from.
    pub grant_id: String,
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
/// round-trips byte-stable, so route payloads stay identical. This is the
/// neutral wire projection of the server's typed preview response, which
/// serves the same shape with the run tree attached.
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
    /// Bars skipped for non-finite or unrepresentable floats. Absent on the
    /// wire means none were dropped.
    #[serde(default)]
    pub dropped: u64,
}

/// One selectable instrument in a symbol catalog.
///
/// `asset_class` is a stable display tag (`Stocks`, `Funds`, `Futures`,
/// `Crypto`): the terminal groups and filters the Symbol Selection Modal by
/// it, and the view crate never links a market-data taxonomy to do so.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolInfoView {
    /// Ticker symbol (e.g. `GLD`).
    pub symbol: String,
    /// Human-readable instrument name.
    pub name: String,
    /// Asset class tag (`Stocks`, `Funds`, `Futures`, `Crypto`).
    pub asset_class: String,
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
    /// Bars without a value, mirroring `ChartBars.dropped`. Absent on the
    /// wire means none were dropped.
    #[serde(default)]
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

/// Parameters for opening an interactive replay session.
///
/// Neutral data only: the core builds the strategy, risk engine, and grant
/// from these knobs (a supplied grant document, else its fixed rehearsal
/// grant), so the terminal never links the trading path. `start_bar` is the
/// first bar the session decides; earlier bars feed indicator context only
/// (signal inputs match a batch run from that bar, with fresh budgets).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayStartParams {
    /// Registry slot the session occupies; replaces any session already there.
    pub session_id: String,
    /// Underlying the replay trades (grant scope permitting).
    pub underlying: String,
    /// Strategy name carried on every proposal for attribution.
    pub strategy_name: String,
    /// Worst-case loss booked per proposal.
    pub max_loss: Money,
    /// Committed notional booked per proposal.
    pub notional: Money,
    /// Days to expiry booked per proposal.
    pub dte_days: u32,
    /// Bars skipped before the first proposal (warmup honesty).
    pub warmup_bars: usize,
    /// Deterministic proposal identifiers (`run-{seed}-{index}`).
    pub seed: u64,
    /// First bar the session decides (0-based, at most the bar count).
    pub start_bar: u64,
    /// Requested structure tag (`"Single"`, `"Vertical"`, …): the core
    /// builds the replay strategy and grant scope from it. Defaults to
    /// `"Single"`, so payloads written before the picker decode unchanged.
    #[serde(default = "default_replay_structure")]
    pub structure: String,
    /// Grant document; `None` replays under the core's fixed rehearsal grant.
    pub grant: Option<serde_json::Value>,
}

/// The default structure tag for payloads that predate the picker.
fn default_replay_structure() -> String {
    "Single".to_owned()
}

/// One replayed bar's verdict, in display vocabulary.
///
/// Direction and structure ride the same stable tags as [`ProposalView`]
/// (`"Long"`, `"Vertical"`, …); both are `None` when no proposal was
/// emitted. `state` is the last journaled lifecycle target, or the skip
/// reason verbatim when nothing was proposed. `legs` carries one row per
/// executed spread leg, read off the venue's own executions (empty for
/// singles, refusals, and verdicts recorded before leg rows existed).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayDecisionView {
    /// Absolute bar index into the replayed history.
    pub bar_index: u64,
    /// Deterministic proposal id (`run-{seed}-{index}`).
    pub proposal_id: String,
    /// Bar timestamp (RFC 3339), exactly as the core recorded it.
    pub ts: String,
    /// Signal direction tag, when a proposal was emitted.
    pub direction: Option<String>,
    /// Option structure tag, when a proposal was emitted.
    pub structure: Option<String>,
    /// Last lifecycle target, or the skip reason when nothing was proposed.
    pub state: String,
    /// Signal conviction (`0.0` when no proposal was emitted).
    pub conviction: f64,
    /// Whether an order reached the venue.
    pub submitted: bool,
    /// Contracts the venue filled.
    pub filled_quantity: u32,
    /// Average fill price, when filled.
    pub fill_price: Option<Money>,
    /// Skip reason, when nothing reached the venue for a non-gate reason.
    pub skipped: Option<String>,
    /// The session refused a multi-leg structure for the missing
    /// option-chain feed. Set beside `skipped`, never parsed from it, so a
    /// terminal can count spread refusals without matching reason text.
    pub chain_gated: bool,
    /// Per-leg execution rows for a filled spread, in fill order. Display
    /// only: the terminal shows what the venue filled, never authority.
    /// Empty for singles, refusals, and older payloads (which decode
    /// unchanged through the default).
    #[serde(default)]
    pub legs: Vec<ReplayLegView>,
}

/// One executed spread leg, in display vocabulary: the OCC contract, the
/// fill side (`"buy"`/`"sell"`), contracts filled, slipped price, and
/// commission.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayLegView {
    /// Contract filled, as its OCC symbol.
    pub contract: String,
    /// Fill side tag.
    pub side: String,
    /// Contracts filled on this leg.
    pub quantity: u32,
    /// Slipped execution price per contract.
    pub price: Money,
    /// Commission on this leg.
    pub fee: Money,
}

/// Net position in one underlying, folded from the session's fills.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayPositionView {
    /// Underlying symbol.
    pub underlying: String,
    /// Signed contracts (or shares); negative is short.
    pub quantity: i64,
    /// Volume-weighted average entry price.
    pub avg_price: Money,
}

/// Interactive replay state: cursor, verdicts so far, fills, and journal.
///
/// Decisions and journal rows cover decided bars only — the session never
/// reports a bar it has not stepped. `active` is false only on the terminal
/// stop shape (a live session always reports true); the gateway refuses a
/// stopped or unknown session instead of serving an empty one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayStateView {
    /// Registry slot this state belongs to.
    pub session_id: String,
    /// Whether the session is live.
    pub active: bool,
    /// Index of the next bar a step will decide.
    pub cursor: u64,
    /// Bars in the replay window.
    pub total_bars: u64,
    /// Verdicts in replay order, decided bars only.
    pub decisions: Vec<ReplayDecisionView>,
    /// Net positions folded from the session's fills.
    pub positions: Vec<ReplayPositionView>,
    /// Submitted orders so far.
    pub filled_orders: u64,
    /// Commission paid so far.
    pub total_fees: Money,
    /// Journaled lifecycle transitions in emission order.
    pub journal: Vec<JournalRowView>,
}

/// One manual ticket awaiting the human's fill report: the exact ticket text
/// the operator places by hand, plus the structured fields the confirm form
/// echoes back. Display vocabulary only — the core alone parses intents
/// (the crate never links `venue` enums; side rides a stable string tag).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TicketView {
    /// Ticket identifier (the intent's client id).
    pub ticket_id: String,
    /// Exact human ticket text (side, quantity, underlying, limit,
    /// good-until, legs by OCC symbol).
    pub rendered: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Order side, as a stable string tag (`"Buy"`, `"Sell"`).
    pub side: String,
    /// Contracts (or shares) the human was asked to place.
    pub quantity: u32,
    /// Limit price the human was asked to place at.
    pub limit_price: Money,
}

/// One flagged disagreement between tickets, fills, and the read-only
/// import: which ticket (or underlying) disagrees, how, and why. The kind
/// rides the core's variant name verbatim (`"QuantityMismatch"`,
/// `"PriceOutsideLimit"`, `"ImportMismatch"`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TicketDriftView {
    /// Ticket (or underlying, for import drift) that disagrees.
    pub ticket_id: String,
    /// How it disagrees, as a stable string tag.
    pub kind: String,
    /// Human-readable reason.
    pub detail: String,
}

/// Event-window label for books expiring in the coming week: the only
/// event exposure derivable from fills alone (expiry dates come from the
/// legs' OCC symbols; no corporate-action calendar feeds the fold yet).
pub const CONCENTRATION_EVENT_EXPIRY_WEEK: &str = "expiry-week";

/// Event-window label for everything outside the coming week, including
/// dateless books: no event exposure the fold can see.
pub const CONCENTRATION_EVENT_UNEXPOSED: &str = "unexposed";

/// Expiry key for books that name no expiry: dateless exposure is reported,
/// never dropped, so the partitions reconcile to the total.
pub const CONCENTRATION_NO_EXPIRY: &str = "no-expiry";

/// One concentration bucket: open notional and its fraction of the total.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConcentrationShare {
    /// Bucket label: underlying symbol, `YYYY-MM-DD` expiry (or
    /// [`CONCENTRATION_NO_EXPIRY`]), or event label.
    pub key: String,
    /// Open notional in this bucket (quantity × entry average).
    pub notional: Money,
    /// Fraction of total open notional.
    pub share: f64,
}

/// Concentration over the fold's open books: three partitions of the same
/// open notional — by underlying, by expiry, and by event window. A
/// projection, never a gate: every share is a fraction of what the books
/// hold, and an empty book yields empty partitions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConcentrationReport {
    /// Open-notional shares by underlying symbol.
    #[serde(default)]
    pub by_underlying: Vec<ConcentrationShare>,
    /// Open-notional shares by expiry (`YYYY-MM-DD`, dateless under
    /// [`CONCENTRATION_NO_EXPIRY`]).
    #[serde(default)]
    pub by_expiry: Vec<ConcentrationShare>,
    /// Open-notional shares by event window ([`CONCENTRATION_EVENT_EXPIRY_WEEK`]
    /// vs [`CONCENTRATION_EVENT_UNEXPOSED`]).
    #[serde(default)]
    pub by_event: Vec<ConcentrationShare>,
    /// Total open notional the shares partition.
    pub total_notional: Money,
}

impl Default for ConcentrationReport {
    /// An empty book concentrates nothing: no partitions, zero total.
    fn default() -> Self {
        Self {
            by_underlying: Vec::new(),
            by_expiry: Vec::new(),
            by_event: Vec::new(),
            total_notional: Money::zero(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/views.rs"]
mod tests;

/// A backtest schema: the composition description plus the registry
/// enumerations a form picker must offer. The schema is data: the terminal
/// renders it without naming any strategy type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestSchemaView {
    /// The composition description (nested spec shape).
    pub schema: serde_json::Value,
}

/// Parameters for one backtest run. Money rides decimal text (never a JSON
/// number); the time window is RFC3339; the grant is an optional explicit
/// document, defaulting to a permissive single-structure rehearsal grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestRunParams {
    /// The nested composition document the run binds.
    pub spec: serde_json::Value,
    /// Deterministic seed.
    pub seed: u64,
    /// Opening equity, decimal text.
    pub initial_equity: String,
    /// Window start (RFC3339), or the tape start when absent.
    pub start: Option<String>,
    /// Window end (RFC3339), or the tape end when absent.
    pub end: Option<String>,
    /// Explicit grant document, or the default when absent.
    pub grant: Option<serde_json::Value>,
}

/// One labelled metric; the value rides text so an absent metric reads
/// `None` instead of an invented zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestMetricView {
    /// Metric label.
    pub label: String,
    /// Metric value, or `None` when the book cannot measure it.
    pub value: Option<String>,
}

/// One closed trade; every price rides decimal text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestTradeView {
    /// Signal side that opened the trade.
    pub long: bool,
    /// Shares closed.
    pub quantity: u32,
    /// Entry bar timestamp (RFC3339).
    pub entry_at: String,
    /// Volume-weighted entry price, decimal text.
    pub entry_price: String,
    /// Exit bar timestamp (RFC3339).
    pub exit_at: String,
    /// Volume-weighted exit price, decimal text.
    pub exit_price: String,
    /// Signed realized P&L after fees, decimal text.
    pub pnl: String,
    /// Bars held, entry to exit inclusive.
    pub bars_held: u64,
    /// Why the position closed.
    pub exit_reason: String,
}

/// One equity-curve point; equity rides decimal text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EquityPointView {
    /// Bar timestamp (RFC3339).
    pub at: String,
    /// Marked equity, decimal text.
    pub equity: String,
}

/// One chart marker; the kind is a stable tag (`entry`, `exit`, `stop`,
/// `target`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestMarkerView {
    /// Bar timestamp (RFC3339).
    pub at: String,
    /// Marker price, decimal text.
    pub price: String,
    /// Marker kind.
    pub kind: String,
}

/// The reference status table's eight rows at one bar: position, day P&L,
/// trades today, loss streak, cooldown, halt line, win rate, net P&L. Formatted
/// text mirrors the reference table exactly; the terminal paints it directly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestStatusView {
    /// Bar timestamp (RFC3339).
    pub at: String,
    /// Open position (`LONG n`, `SHORT n`, or `Flat`).
    pub position: String,
    /// Day P&L (`#.##%`).
    pub day_pnl_pct: String,
    /// Trades taken today (`n`, or `n / max` when capped).
    pub trades_today: String,
    /// Consecutive losses into today.
    pub loss_streak: u64,
    /// Cooldown left (`N bars`, or `Ready`).
    pub cooldown: String,
    /// Halt line (`HALTED: reason`, `Active`, or `Disabled`).
    pub status: String,
    /// Win rate over closed trades (`#.#%`, or `-` with no closes).
    pub win_rate: String,
    /// Realized P&L over closed trades, decimal text.
    pub net_pnl: String,
}

/// A handle to one stored backtest run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestRunRef {
    /// Registry id the run is stored under.
    pub run_id: String,
}

/// One stored backtest run, fully projected: metrics, equity curve, trades,
/// markers, the final status row, and the declared warnings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BacktestRunView {
    /// Registry id.
    pub run_id: String,
    /// The run's reproducibility pin (data, code, config).
    pub manifest_json: String,
    /// Labelled metrics; an unmeasurable metric reads `None`.
    pub metrics: Vec<BacktestMetricView>,
    /// Marked equity per bar.
    pub equity: Vec<EquityPointView>,
    /// Closed trades in close order.
    pub trades: Vec<BacktestTradeView>,
    /// Entry/exit markers in bar order.
    pub markers: Vec<BacktestMarkerView>,
    /// The final bar's status row, when the run decided at least one bar.
    pub status: Option<BacktestStatusView>,
    /// Declared limits and unavailable surfaces beside the metrics.
    pub warnings: Vec<String>,
}
