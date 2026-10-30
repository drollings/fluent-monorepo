//! Trading-status truth table: every command-plane state crossed with every
//! store-reading shape, pinned at the health view before the serving path
//! composes venue gates.
//!
//! Entries route only under an enabled book AND a passing store verdict;
//! exits route under every state and every reading, so a halted book stays
//! manageable. The matrix below is today's behavior, asserted cell by cell
//! so the venue-function swap cannot move one silently.

use super::*;
use std::sync::Mutex;

use protopyne_store::StoreHealth;

fn shared_with(trading: TradingState, reading: StoreHealth) -> Shared {
    std::sync::Arc::new(AppState {
        limiter: Limiter::new(32),
        history: Vec::new(),
        history_timeframe: Timeframe::M15,
        history_symbol: String::new(),
        root_bars: Mutex::new(std::collections::HashMap::new()),
        inbox: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
        journal: Arc::new(Journal::ephemeral()),
        journal_permit: VenueStorage::new(std::path::PathBuf::from("test-command-journal")),
        portfolio: Mutex::new(PortfolioView::default()),
        booked: Mutex::new(Vec::new()),
        feed: tokio::sync::broadcast::channel(16).0,
        trading_state: StateHandle::new(trading),
        last_command: Mutex::new(None),
        store_health: Mutex::new(reading),
        tape: None,
        tape_serve_stats: Arc::new(TapeServeStats::default()),
        fallback_latch: Mutex::new(FallbackLatch::default()),
        replay: Mutex::new(ReplayRegistry::new()),
        backtests: Mutex::new(crate::backtest_registry::BacktestRegistry::new()),
        chain_history: None,
        history_root: None,
        broker: protopyne_venue::BrokerRegistry::new(),
        ingest_health: Mutex::new(None),
        tickets: Mutex::new(protopyne_venue::TicketTracker::new()),
    })
}

fn healthy_reading() -> StoreHealth {
    StoreHealth {
        observed: true,
        ..StoreHealth::default()
    }
}

fn outage_reading() -> StoreHealth {
    StoreHealth {
        reachable: false,
        observed: true,
        ..StoreHealth::default()
    }
}

#[test]
fn entries_route_only_when_enabled_and_healthy() {
    let cases = [
        (TradingState::Enabled, healthy_reading(), true),
        (TradingState::Enabled, StoreHealth::default(), true),
        (TradingState::Enabled, outage_reading(), false),
        (TradingState::ExitOnly, healthy_reading(), false),
        (TradingState::ExitOnly, StoreHealth::default(), false),
        (TradingState::ExitOnly, outage_reading(), false),
        (TradingState::Disabled, healthy_reading(), false),
        (TradingState::Disabled, StoreHealth::default(), false),
        (TradingState::Disabled, outage_reading(), false),
    ];
    for (trading, reading, expected) in cases {
        let view = health_view(&shared_with(trading, reading));
        assert_eq!(
            view.allows_entries, expected,
            "entries under {trading:?} with {reading:?}"
        );
    }
}

#[test]
fn exits_route_under_every_state_and_reading() {
    let states = [
        TradingState::Enabled,
        TradingState::ExitOnly,
        TradingState::Disabled,
    ];
    let readings = [healthy_reading(), StoreHealth::default(), outage_reading()];
    for trading in states {
        for reading in readings {
            let view = health_view(&shared_with(trading, reading));
            assert!(
                view.allows_exits,
                "exits must route under {trading:?} with {reading:?}"
            );
        }
    }
}

#[test]
fn trading_state_tag_names_each_state() {
    let cases = [
        (TradingState::Enabled, "Enabled"),
        (TradingState::ExitOnly, "ExitOnly"),
        (TradingState::Disabled, "Disabled"),
    ];
    for (trading, tag) in cases {
        let view = health_view(&shared_with(trading, StoreHealth::default()));
        assert_eq!(view.trading_state, tag);
    }
}
