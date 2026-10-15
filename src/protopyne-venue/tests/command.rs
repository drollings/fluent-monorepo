//! Command-plane calibration: registry grouping, state gating, atomic
//! close-all, active-only cancel-all, supervised cancellation, and replica
//! fidelity.
//!
//! The portfolio registry is the only grouping authority (no second
//! instrument index); the trading state is the only entry gate besides the
//! risk engine (no second kill switch); close-all emits one intent per
//! leg-group (never leg-by-leg); and the read-only replica folds the same
//! journal events monitors render, holding no order authority itself.

use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use fluent_concurrency::batch::{
    CancelReason, SupervisedBatch, SupervisedBatchEvent, SupervisedBatchSummary,
};
use rust_decimal::Decimal;

use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderRecord, OrderSide, OrderStatus};
use crate::paper::{PaperConfig, PaperVenue};
use crate::position::PositionSide;
use crate::quote::Quote;
use crate::venue::{LegQuoteSource, Venue};
use protopyne_types::{
    ContractId, LegSide, Money, OptionContract, OptionKind, Portfolio, PortfolioId, SpreadLeg,
    SpreadLegs,
};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn portfolio(name: &str) -> Portfolio {
    Portfolio::new()
        .id(PortfolioId::new(format!("pf-{name}")))
        .name(name.to_owned())
        .strategies(vec!["ribbon".to_owned()])
        .accounts(vec![format!("{name}-acct")])
        .build()
}

fn contract(strike: &str, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date"))
        .strike(money(strike))
        .kind(kind)
        .build()
}

fn vertical() -> SpreadLegs {
    SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract("600", OptionKind::Call))
            .side(LegSide::Long)
            .ratio(1)
            .build(),
        SpreadLeg::new()
            .contract(contract("610", OptionKind::Call))
            .side(LegSide::Short)
            .ratio(1)
            .build(),
    ])
    .expect("vertical must build")
}

fn fresh_quote(bid: &str, ask: &str) -> Quote {
    Quote::new()
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(50)
        .ask_size(50)
        .ts(now())
        .build()
}

/// Deterministic per-leg market data: OCC symbol → quote.
struct MapLegQuotes {
    map: HashMap<String, Quote>,
}

impl LegQuoteSource for MapLegQuotes {
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote> {
        self.map.get(&contract.occ_symbol()).copied()
    }
}

fn spread_intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(money("5.00"))
        .maybe_legs(Some(vertical()))
        .build()
}

/// A dependent that stays pending on an unmet upstream asset: the
/// deterministic vehicle for supervised cancellation (a fast unit would
/// finish before the failure lands, making the test a race).
struct PendingStub {
    name: String,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl WorkUnit for PendingStub {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Err(WorkError::Dependency(format!(
            "{} waits for an upstream asset",
            self.name
        )))
    }
}

impl_fieldless!(PendingStub);

impl Describable for PendingStub {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(&self.name, &self.depends, &self.provides, None)
    }
}

impl_component!(PendingStub);

#[test]
fn registry_groups_named_portfolios() {
    let registry = PortfolioRegistry::new();
    assert!(registry.is_empty());
    registry.register(portfolio("paper"));
    registry.register(portfolio("backtest"));
    registry.register(portfolio("manual-ticket"));
    assert_eq!(registry.len(), 3);
    let paper = registry.get("paper").expect("paper registered");
    assert_eq!(paper.name, "paper");
    assert!(registry.get("live").is_none(), "unknown portfolio is absence, not error");
    let mut names = registry.names();
    names.sort();
    assert_eq!(names, vec!["backtest", "manual-ticket", "paper"]);
}

#[test]
fn disarm_halts_entries_while_exits_route() {
    assert!(TradingState::Enabled.allows(false));
    assert!(TradingState::Enabled.allows(true));
    assert!(!TradingState::ExitOnly.allows(false), "exit-only halts entries");
    assert!(TradingState::ExitOnly.allows(true));
    assert!(!TradingState::Disabled.allows(false), "disabled halts entries");

    let venue = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let state = StateHandle::new(TradingState::Enabled);
    let registry = Arc::new(PortfolioRegistry::new());
    registry.register(portfolio("paper"));
    let disarm = CommandUnit::new()
        .name("cmd.disarm".to_owned())
        .command(CommandKind::Disarm)
        .venue(Arc::clone(&venue))
        .state(state.clone())
        .registry(Arc::clone(&registry))
        .build();
    let outcome = disarm
        .execute(&WorkContext::default())
        .expect("disarm executes");
    assert!(outcome.success);
    assert_eq!(state.get(), TradingState::Disabled);
    assert!(!state.get().allows(false), "entries halt after disarm");
    assert!(state.get().allows(true), "exits still route after disarm");
}

#[test]
fn close_all_emits_one_atomic_intent_per_leg_group() {
    let groups = vec![
        LegGroup {
            underlying: "SPY".to_owned(),
            side: PositionSide::Long,
            quantity: 1,
            legs: Some(vertical()),
        },
        LegGroup {
            underlying: "SPY".to_owned(),
            side: PositionSide::Short,
            quantity: 2,
            legs: None,
        },
    ];
    let intents = close_all_intents(&groups, money("1.00"), "prop-9", "ribbon").expect("closable");
    assert_eq!(intents.len(), 2, "one intent per leg-group, never per leg");

    let spread = &intents[0];
    assert_eq!(spread.side, OrderSide::Sell, "long package closes by selling");
    assert_eq!(spread.quantity, 1);
    let legs = spread.legs.as_ref().expect("spread close stays atomic");
    assert_eq!(legs.legs().len(), 2, "both legs ride one intent");
    assert_eq!(legs.legs()[0].side, LegSide::Short, "closing flips the long leg");
    assert_eq!(legs.legs()[1].side, LegSide::Long, "closing flips the short leg");

    let single = &intents[1];
    assert_eq!(single.side, OrderSide::Buy, "short single closes by buying");
    assert!(single.legs.is_none());
    assert_eq!(single.quantity, 2);

    let again = close_all_intents(&groups, money("1.00"), "prop-9", "ribbon").expect("closable");
    assert_eq!(intents, again, "close-all is deterministic");
}

#[test]
fn close_all_refuses_a_flip_into_naked_short() {
    // A lone long leg closes into a lone short leg: undefined risk, so the
    // command refuses instead of emitting it.
    let lone_long = SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract("600", OptionKind::Call))
            .side(LegSide::Long)
            .ratio(1)
            .build(),
    ])
    .expect("lone long is defined-risk");
    let groups = vec![LegGroup {
        underlying: "SPY".to_owned(),
        side: PositionSide::Long,
        quantity: 1,
        legs: Some(lone_long),
    }];
    assert!(
        close_all_intents(&groups, money("1.00"), "prop-9", "ribbon").is_err(),
        "closing into naked-short must refuse"
    );
}

#[test]
fn cancel_all_lists_only_active_orders() {
    let orders = vec![
        record_with_status("a", OrderStatus::Pending),
        record_with_status("b", OrderStatus::Partial),
        record_with_status("c", OrderStatus::Filled),
        record_with_status("d", OrderStatus::Cancelled),
        record_with_status("e", OrderStatus::Expired),
    ];
    assert_eq!(cancel_all_ids(&orders), vec!["a".to_owned(), "b".to_owned()]);
}

fn record_with_status(client_id: &str, status: OrderStatus) -> OrderRecord {
    let mut record = OrderRecord::pending(spread_intent(client_id), now());
    record.status = status;
    record
}

#[tokio::test]
async fn close_all_through_venue_round_trip_is_atomic() {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), fresh_quote("2.50", "2.70"));
    map.insert(legs.legs()[1].contract.occ_symbol(), fresh_quote("1.20", "1.40"));
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(MapLegQuotes { map }));
    let venue = Arc::new(venue);
    venue
        .submit(spread_intent("round-trip"), &fresh_quote("600.00", "600.10"), now())
        .await
        .expect("spread fills");
    assert_eq!(venue.spread_positions().len(), 2, "two leg positions open");

    let registry = Arc::new(PortfolioRegistry::new());
    registry.register(portfolio("paper"));
    let close = CommandUnit::new()
        .name("cmd.close.paper".to_owned())
        .command(CommandKind::CloseAll { portfolio: "paper".to_owned() })
        .venue(Arc::clone(&venue))
        .state(StateHandle::new(TradingState::Enabled))
        .registry(Arc::clone(&registry))
        .limit(money("1.00"))
        .proposal_id("close-1".to_owned())
        .strategy("ribbon".to_owned())
        .build();
    let output = close.execute(&WorkContext::default()).expect("close-all executes");
    let intents: Vec<OrderIntent> =
        serde_json::from_value(output.data.clone()).expect("intents round-trip as data");
    assert_eq!(intents.len(), 1, "one execution closes with one intent");
    let legs = intents[0].legs.as_ref().expect("close stays atomic");
    assert_eq!(legs.legs().len(), 2);
}

#[tokio::test]
async fn dependent_cancellation_on_injected_failure() {
    let venue = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(PortfolioRegistry::new());
    registry.register(portfolio("paper"));

    // The injected failure: CloseAll over a portfolio the registry never
    // saw. The command refuses fail-closed instead of closing blind.
    let failer = CommandUnit::new()
        .name("cmd.close.unknown".to_owned())
        .command(CommandKind::CloseAll { portfolio: "no-such".to_owned() })
        .venue(Arc::clone(&venue))
        .state(StateHandle::new(TradingState::Enabled))
        .registry(Arc::clone(&registry))
        .provides(vec!["cmd.seal".into()])
        .build();
    let dependent = PendingStub {
        name: "cmd.report".to_owned(),
        depends: vec!["cmd.seal".into()],
        provides: Vec::new(),
    };
    let independent_state = StateHandle::new(TradingState::Enabled);
    let independent = CommandUnit::new()
        .name("cmd.disarm.ops".to_owned())
        .command(CommandKind::Disarm)
        .venue(Arc::clone(&venue))
        .state(independent_state.clone())
        .registry(Arc::clone(&registry))
        .build();

    let caps = fluent_wvr::CapabilitySet::new();
    let mut batch =
        SupervisedBatch::new(fluent_concurrency::tokio_runtime(), caps);
    batch.register(Arc::new(failer)).expect("register failer");
    batch
        .register_with_context(
            Arc::new(dependent),
            WorkContext { max_retries: 10, ..WorkContext::default() },
        )
        .expect("register dependent");
    batch.register(Arc::new(independent)).expect("register independent");
    let summary: SupervisedBatchSummary = batch.await;

    let failed: Vec<String> = summary
        .failed
        .iter()
        .filter_map(|event| match event {
            SupervisedBatchEvent::Failed { name, .. } => Some(name.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(failed, vec!["cmd.close.unknown".to_owned()]);
    assert_eq!(summary.cancelled.len(), 1, "dependent must be cancelled");
    match &summary.cancelled[0] {
        SupervisedBatchEvent::Cancelled { name, reason } => {
            assert_eq!(&**name, "cmd.report");
            assert!(
                matches!(reason, CancelReason::DependencyFailed),
                "cancellation rides the graph, got {reason:?}"
            );
        }
        other => panic!("expected a cancellation, got {other:?}"),
    }
    let completed: Vec<String> = summary
        .completed
        .iter()
        .filter_map(|event| match event {
            SupervisedBatchEvent::Completed { name, .. } => Some(name.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(completed, vec!["cmd.disarm.ops".to_owned()]);
    assert_eq!(independent_state.get(), TradingState::Disabled);
}

#[tokio::test]
async fn replica_replay_equals_incremental_fold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path);
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let events = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { journal_script(&journal, &permit) })
        .await;

    let full = StateReplica::apply(&events);
    let mut incremental = PortfolioView::default();
    for event in &events {
        StateReplica::apply_one(&mut incremental, event);
    }
    assert_eq!(full, incremental, "replay equals the incremental fold");

    assert_eq!(full.orders.len(), 2);
    let filled: Vec<&str> = full
        .orders
        .iter()
        .filter(|order| order.status == "filled")
        .map(|order| order.client_id.as_str())
        .collect();
    assert_eq!(filled, vec!["cmd-a#0"]);
    assert_eq!(full.leg_fills.len(), 2, "both spread legs replay");
    assert_eq!(
        full.last_seq,
        events.last().expect("events").seq,
        "replica tracks the journal tip"
    );
}

fn journal_script(
    journal: &Journal,
    permit: &VenueStorage,
) -> Vec<crate::journal::JournalEvent> {
    let at = now();
    journal.record_intent(permit, &spread_intent("cmd-a#0"), at).expect("intent a");
    journal.record_intent(permit, &spread_intent("cmd-b#0"), at).expect("intent b");
    journal
        .mark_status(permit, "cmd-a#0", OrderStatus::Filled, at)
        .expect("filled");
    let legs = vertical();
    for (index, leg) in legs.legs().iter().enumerate() {
        journal
            .record_leg_fill(
                permit,
                "cmd-a#0",
                &crate::venue::LegExecution {
                    contract: ContractId::new(leg.contract.occ_symbol()),
                    side: if index == 0 { OrderSide::Buy } else { OrderSide::Sell },
                    quantity: 1,
                    price: money("2.00"),
                    fee: money("0.65"),
                },
                at,
            )
            .expect("leg fill");
    }
    journal.replay(permit).expect("replay").events
}

fn mock_event(seq: i64, client_id: &str, kind: &str) -> crate::journal::JournalEvent {
    crate::journal::JournalEvent {
        seq,
        client_id: client_id.to_owned(),
        kind: kind.to_owned(),
        detail: "{}".to_owned(),
        at_ms: 1_767_225_600_000,
    }
}

#[test]
fn replica_ignores_unknown_command_rows_without_phantom_orders() {
    // Mock feed, no journal: operator rows advance the tip and never read
    // as orders, so an unknown command kind is absence, not a position.
    let events = vec![
        mock_event(1, "operator", "command.disarm"),
        mock_event(2, "operator", "command.resume"),
        mock_event(3, "operator", "command.close_all"),
        mock_event(4, "operator", "command.cancel_all"),
        mock_event(5, "operator", "command.something_new"),
    ];
    let view = StateReplica::apply(&events);
    assert!(view.orders.is_empty(), "no phantom orders, got: {:?}", view.orders);
    assert_eq!(view.last_seq, 5, "the tip still advances past command rows");
}

#[test]
fn replica_fold_is_order_insensitive_at_the_tip() {
    // Out-of-order delivery folds the same tip: the max sequence wins no
    // matter the arrival order.
    let ordered = vec![
        mock_event(1, "a", "intent"),
        mock_event(2, "a", "filled"),
        mock_event(3, "b", "intent"),
    ];
    let mut shuffled = ordered.clone();
    shuffled.reverse();
    let from_ordered = StateReplica::apply(&ordered);
    let from_shuffled = StateReplica::apply(&shuffled);
    assert_eq!(from_shuffled.last_seq, 3);
    assert_eq!(from_shuffled.last_seq, from_ordered.last_seq);
    assert_eq!(from_shuffled.orders.len(), from_ordered.orders.len());
}

#[test]
fn replica_tracks_latest_status_per_client() {
    let events = vec![
        mock_event(1, "a", "intent"),
        mock_event(2, "a", "submitted"),
        mock_event(3, "a", "filled"),
    ];
    let view = StateReplica::apply(&events);
    assert_eq!(view.orders.len(), 1);
    assert_eq!(view.orders[0].status, "filled");
    assert_eq!(view.last_seq, 3);
}
