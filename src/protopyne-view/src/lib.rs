#![forbid(unsafe_code)]

//! Neutral read-model contract between the trading core and the terminal.
//!
//! The terminal renders but never links the trading path, and the core
//! never links the terminal — so both sides meet here, in display vocabulary
//! only. Read DTOs mirror the server's wire shapes with display-enum fields
//! carried as stable string tags (never linked `strategy`/`venue` enums),
//! and [`TerminalGateway`] is the object-safe read abstraction (plus the
//! core-forwarded submits) with a [`MockGateway`] for headless terminal
//! tests. Depends only on `protopyne-types` plus
//! `serde`/`serde_json`: no `egui`, no `strategy`/`venue`/`server`.

pub mod gateway;
pub mod symbols;
pub mod views;

pub use gateway::{validate_symbol_timeframe, GatewayError, MockGateway, TerminalGateway};
pub use symbols::{
    discovered_symbol_rows, pick_default_symbol, symbol_catalog, AssetClass, InstrumentDecl,
    SYMBOL_CATALOG,
};
pub use views::{
    ArmResponse, BrokerAccountView, BrokerCapabilityView, BrokerDealView, BrokerLegView,
    BrokerOrderView, BrokerRefusal, ChartBar, ChartBars, CommandStatus, DenyCount, HealthView,
    JournalRowView, OverlaySeries, PortfolioView, PositionView, PreviewResponse, ProposalView,
    ReplayDecisionView, ReplayLegView, ReplayPositionView, ReplayStartParams, ReplayStateView,
    ReplayStepView, ReplayView, SymbolInfoView, TicketDriftView, TicketView, WorstCase,
    PORTFOLIO_SOURCE_BOOKED, PORTFOLIO_SOURCE_REHEARSAL,
};
