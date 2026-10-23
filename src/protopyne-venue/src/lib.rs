#![forbid(unsafe_code)]

//! Journaled paper-trading venue: one `Venue` trait for paper now, sandbox
//! later, and backtest over the same fill code.
//!
//! The safety case is ordering, not cleverness: journal the intent before
//! any venue call, gate risk before journaling, retry through query-by-ID
//! (never blind resubmit), reconcile against venue truth, and halt entries
//! on any disagreement. Supervision composes [`Scope`](fluent_concurrency::scope::Scope),
//! [`SupervisedBatch`](fluent_concurrency::batch::SupervisedBatch),
//! [`ResultPool`](fluent_concurrency::pool::ResultPool), credit flow, and
//! [`first_accept_in_order`](fluent_concurrency::ladder::first_accept_in_order).

pub mod account;
pub mod alert;
pub mod auth;
pub mod backfill;
pub mod broker;
pub mod cert_soak;
pub mod command;
pub mod depth;
pub mod error;
pub mod facts;
pub mod feed;
pub mod health;
pub mod journal;
pub mod live;
pub mod market;
pub mod order;
pub mod paper;
pub mod pipeline;
pub mod position;
pub mod quote;
pub mod reading;
pub mod reconcile;
pub mod registry;
pub mod session;
pub mod stream;
pub mod submit;
pub mod tastytrade;
pub mod tastytrade_account;
pub mod tastytrade_feed;
pub mod ticket;
pub mod venue;

pub use account::{
    AccountError, AccountFuture, AccountRange, AccountReader, AccountSummary, BrokerDeal,
};
pub use auth::{
    AuthConfig, AuthDriver, AuthError, AuthFuture, AuthHandle, AuthHealth, AuthMethod, Credential,
    CredentialLease, CredentialSource, CredentialState, CredentialStore, FileCredentialStore,
    MemoryCredentialStore, TastytradeSessionDriver, TokenManager,
};
pub use broker::{BrokerBundle, BrokerCapabilities, BrokerError, BrokerId, BrokerRegistry};
pub use feed::{
    bar_update, AdapterOutcome, ChannelFeed, FeedCapabilities, FeedError, FeedFuture, FeedHealth,
    FeedSession, FeedStats, FeedStatsSnapshot, FeedSubscription, MarketDataFeed, TradeBarAdapter,
};
pub use market::{Aggressor, BarUpdate, TradePrint};

pub use alert::{
    poll_alerts, Alert, AlertClass, AlertCursor, AlertSink, AlertSource, FanoutSink, FileSink,
    GateWatch, MemorySink,
};
pub use backfill::{
    fetch_through, missing_range, splice, BackfillGate, BackfillHandle, BackfillReport,
    BackfillSource, BackfillUnit, BACKFILL_ASSET,
};
pub use cert_soak::{
    fault_schedule, run_paper_soak, SoakOutcome, ALERTS_FILE, BREAKER_FILE, JOURNAL_FILE,
    RECONCILE_FILE,
};
pub use command::{
    cancel_all_detail, cancel_all_ids, close_all_detail, close_all_intents, decide_proposal,
    loop_started, open_groups_of, proposal_bodies, replay_attestation, replay_grant,
    replay_proposal_states, replay_proposals, replay_trading_state, CommandKind, CommandUnit,
    GrantHandle, JournaledProposal, LegGroup, PortfolioRegistry, PortfolioView, ReplicaOrder,
    StateHandle, StateReplica, TradingState, TradingStateUnit, COMMAND_ARM_GRANT_KIND,
    COMMAND_ATTEST_KIND, COMMAND_CANCEL_ALL_KIND, COMMAND_CLOSE_ALL_KIND, COMMAND_DISARM_KIND,
    COMMAND_RESUME_KIND, COMMAND_REVOKE_GRANT_KIND, COMMAND_START_LOOP_KIND, OPERATOR_CLIENT_ID,
    PROPOSAL_BODY_KIND, PROPOSAL_TRANSITION_KIND, TRADING_ENABLED_ASSET,
};
pub use depth::{BookManager, DarkFinding, DepthBook, DepthDelta, PriceLevel, DEFAULT_MAX_LEVELS};
pub use error::VenueError;
pub use facts::VenueBookFacts;
pub use health::{
    check_route, check_trade, may_trade, serving_allows_entries, serving_verdict, GateClass,
    GateFailure, GateReport, HealthGate, HealthLimits, HealthState, HealthVerdict, RouteIntent,
};
pub use journal::{
    Journal, JournalEvent, ReplayedState, VenueStorage, CORPORATE_KIND, INTENT_KIND, LEG_FILL_KIND,
};
pub use live::{
    boot_live, positions_of, LiveAttachments, LiveBackfillSpec, LiveBoot, LiveSession,
    StreamConnect, StreamDriver, TastytradeVenue,
};
pub use market::{MarketEvent, MarketEventKind};
pub use order::{
    client_id_for, qty_of, ActiveOrderState, CorporateEvent, CorporateEventArgs, CorporateOutcome,
    Fill, OrderIntent, OrderRecord, OrderSide, OrderStatus, Position, TimeInForce,
};
pub use paper::{Fault, PaperConfig, PaperVenue};
pub use pipeline::{DecisionPipeline, GateAxis, PipelineRequest, PipelineVerdict};
pub use position::{
    calculate_pnl_realised, calculate_pnl_unrealised, calculate_price_entry_average,
    update_from_fill, PositionExited, PositionFill, PositionSide, SpreadPosition,
};
pub use quote::{
    FeeSchedule, LiquidityFilter, Quote, Slippage, DEFAULT_SLIPPAGE_BPS, PACKAGE_TOLERANCE_BPS,
};
pub use reading::{GateReading, GateVerdict};
pub use reconcile::{
    journal_leg_net, journal_net, reconcile, reconcile_legs, JournalBook, ReconcileGate,
    ReconcileHandle, ReconcileMismatch, ReconcileReport, ReconcileSource, ReconcileUnit,
    RECONCILE_ASSET,
};
pub use registry::VenueRegistry;
pub use session::{check_unit, SessionConfig, TradingSession};
pub use stream::{
    terminal_reason, ReconnectPolicy, StreamBreaker, StreamConfig, StreamHealthUnit, StreamOutcome,
    SupervisedSource, SupervisedStream, STREAM_HEALTH_ASSET,
};
pub use submit::{
    safe_submit, submit_guarded, Recorder, SubmissionRecorder, SUBMIT_RETRIES, SUBMIT_TIMEOUT,
};
pub use tastytrade::{
    transform_account_event, transform_chain_quote, transform_quote, validate_subscription,
    TastytradeConfig, TastytradeTransport, TransportError, RETRY_BACKOFF_BASE_MS, RETRY_JITTER_PCT,
    RETRY_MAX_ATTEMPTS,
};
pub use ticket::{
    ConfirmedFill, DriftKind, ImportedPosition, ManualTicket, TicketDrift, TicketRead,
    TicketTracker, TICKET_CONFIRMED_KIND, TICKET_ISSUED_KIND,
};
pub use venue::{
    AccountEvent, AccountSnapshot, BalanceEntry, ChainSnapshotFuture, ChainSnapshotSource,
    DryRunOutcome, LegExecution, LegQuoteSource, QuoteSource, Venue,
};

#[cfg(test)]
#[path = "../tests/soak.rs"]
mod soak_tests;

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;

#[cfg(test)]
#[path = "../tests/spread_calibration.rs"]
mod spread_calibration_tests;

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;

#[cfg(test)]
#[path = "../tests/symbol_index.rs"]
mod symbol_index_tests;

#[cfg(test)]
#[path = "../tests/library_only.rs"]
mod library_only_tests;

#[cfg(test)]
#[path = "../tests/ib_conformance.rs"]
mod ib_conformance_tests;
