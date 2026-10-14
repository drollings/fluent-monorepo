#![forbid(unsafe_code)]

//! Neutral read-model contract between the trading core and the terminal.
//!
//! The terminal renders but never links the trading path, and the core
//! never links the terminal — so both sides meet here, in display vocabulary
//! only. Read DTOs mirror the server's wire shapes with display-enum fields
//! carried as stable string tags (never linked `strategy`/`venue` enums),
//! and [`TerminalGateway`] is the object-safe read abstraction (plus one
//! `arm` submit that forwards to the core) with a [`MockGateway`] for
//! headless terminal tests. Depends only on `protopyne-types` plus
//! `serde`/`serde_json`: no `egui`, no `strategy`/`venue`/`server`.

pub mod gateway;
pub mod views;

pub use gateway::{GatewayError, MockGateway, TerminalGateway};
pub use views::{
    ArmResponse, ChartBar, ChartBars, CommandStatus, DenyCount, HealthView, JournalRowView,
    OverlaySeries, PortfolioView, PositionView, PreviewResponse, ProposalView, ReplayStepView,
    ReplayView, WorstCase,
};
