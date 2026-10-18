use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use crate::journal::{Journal, VenueStorage, LEG_FILL_KIND};
use crate::order::{OrderIntent, OrderSide, OrderStatus};
use crate::reconcile::{journal_leg_net, reconcile_legs, ReconcileGate};
use crate::venue::{LegExecution, LegQuoteSource, Venue};
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn quote(bid: &str, ask: &str) -> Quote {
    Quote::new()
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(50)
        .ask_size(50)
        .ts(now())
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

fn single_intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn spread_intent(id: &str, legs: SpreadLegs) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(money("5.00"))
        .maybe_legs(Some(legs))
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

fn liquid_vertical_quotes() -> MapLegQuotes {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), quote("2.50", "2.70"));
    map.insert(legs.legs()[1].contract.occ_symbol(), quote("1.20", "1.40"));
    MapLegQuotes { map }
}

fn venue_with(source: MapLegQuotes) -> PaperVenue {
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(source));
    venue
}

#[tokio::test]
async fn single_leg_keeps_legacy_fill_economics() {
    let venue = PaperVenue::new(PaperConfig::new().build());
    // 600.10 ask slipped 10 bps = 600.7001, capped at the 600.50 limit.
    let record = venue
        .submit(single_intent("parity#0"), &quote("600.00", "600.10"), now())
        .await
        .expect("liquid single leg fills");
    assert_eq!(record.status, OrderStatus::Filled);
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(record.avg_price, Some(money("600.50")));
    assert_eq!(record.fees_paid, money("1.30"));
    assert!(venue.executed_legs("parity#0").is_empty());
    assert!(venue.spread_positions().is_empty());
    // Single-leg replay carries no leg rows: pre-migration shape preserved.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path);
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            journal
                .record_intent(&permit, &single_intent("parity#0"), now())
                .expect("journal intent");
            let state = journal.replay(&permit).expect("replay");
            assert_eq!(state.intents, vec![single_intent("parity#0")]);
            assert!(
                state.events.iter().all(|e| e.kind != LEG_FILL_KIND),
                "single-leg journaling writes no leg rows"
            );
        })
        .await;
}

#[tokio::test]
async fn vertical_fills_atomically_per_leg() {
    let venue = venue_with(liquid_vertical_quotes());
    let legs = vertical();
    let record = venue
        .submit(
            spread_intent("v#0", legs.clone()),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("liquid vertical fills");
    // Long 2.70 ask slipped up (2.7027), short 1.20 bid slipped down (1.1988).
    assert_eq!(record.status, OrderStatus::Filled);
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(record.avg_price, Some(money("1.5039")));
    assert_eq!(record.fees_paid, money("1.30"));

    let executions = venue.executed_legs("v#0");
    assert_eq!(executions.len(), 2);
    assert_eq!(
        executions
            .iter()
            .map(|e| e.contract.as_str())
            .collect::<Vec<_>>(),
        vec![
            legs.legs()[0].contract.occ_symbol(),
            legs.legs()[1].contract.occ_symbol(),
        ]
    );
    let positions = venue.spread_positions();
    assert_eq!(positions.len(), 2);
    let long = &positions[&legs.legs()[0].contract.occ_symbol()];
    assert_eq!(long.side, crate::position::PositionSide::Long);
    assert_eq!(long.quantity_abs, 1);
    let short = &positions[&legs.legs()[1].contract.occ_symbol()];
    assert_eq!(short.side, crate::position::PositionSide::Short);
}

#[tokio::test]
async fn one_illiquid_leg_refuses_the_whole_spread() {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), quote("2.50", "2.70"));
    // 60.00 wide against the 50.00 maximum: this leg cannot trade.
    map.insert(legs.legs()[1].contract.occ_symbol(), quote("1.00", "61.00"));
    let venue = venue_with(MapLegQuotes { map });

    let err = venue
        .submit(
            spread_intent("w#0", legs.clone()),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect_err("wide leg must refuse the spread");
    let reason = err.to_string();
    assert!(
        reason.contains(&legs.legs()[1].contract.occ_symbol()),
        "refusal names the offending leg, got: {reason}"
    );
    // Atomicity: no partial-leg state survives the refusal.
    assert!(venue.query("w#0").await.is_none());
    assert!(venue.spread_positions().is_empty());
    assert!(venue.executed_legs("w#0").is_empty());
}

#[tokio::test]
async fn spread_without_leg_market_data_refuses_fail_closed() {
    let venue = PaperVenue::new(PaperConfig::new().build());
    let err = venue
        .submit(
            spread_intent("d#0", vertical()),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect_err("legs without quotes must not fill");
    assert!(matches!(err, VenueError::Invalid(_)));
    assert!(venue.query("d#0").await.is_none());
}

#[tokio::test]
async fn spread_lose_ack_resolves_through_query_by_id() {
    let venue = venue_with(liquid_vertical_quotes());
    venue.inject(Fault::LoseAck);
    let err = venue
        .submit(
            spread_intent("l#0", vertical()),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect_err("lost ack errors");
    assert!(matches!(err, VenueError::Transport(_)));
    let record = venue.query("l#0").await.expect("query finds the fill");
    assert_eq!(record.status, OrderStatus::Filled);
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(venue.executed_legs("l#0").len(), 2);
    assert_eq!(venue.spread_positions().len(), 2);
    // Retry after the lost ack dedups: still exactly one order, one leg set.
    let again = venue
        .submit(
            spread_intent("l#0", vertical()),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("idempotent resubmit");
    assert_eq!(again.filled_quantity, 2);
    assert_eq!(venue.orders().len(), 1);
    assert_eq!(venue.executed_legs("l#0").len(), 2);
}

fn approved(token_proposal: &str) -> protopyne_strategy::RiskApproved {
    let engine = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::default());
    let proposal = protopyne_strategy::Proposal::new()
        .id(token_proposal.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(protopyne_strategy::Direction::Long)
        .side(protopyne_strategy::ProposalSide::Entry)
        .structure(protopyne_strategy::StructureKind::Vertical)
        .quantity(protopyne_types::Qty::new(Decimal::from(1)))
        .limit_price(money("5.00"))
        .max_loss(money("300"))
        .notional(money("1_000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build();
    let state = protopyne_strategy::RiskState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        reconciliation_healthy: true,
        is_limit_order: true,
        is_spread: true,
    };
    engine.check(&proposal, &state).expect("healthy")
}

#[tokio::test]
async fn spread_journal_reconciles_leg_aware() {
    use crate::submit::safe_submit;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path);
    let venue = venue_with(liquid_vertical_quotes());
    let token = approved("prop-1");
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            let record = safe_submit(
                &permit,
                &journal,
                &venue,
                &token,
                spread_intent("r#0", vertical()),
                &quote("600.00", "600.10"),
                now(),
            )
            .await
            .expect("spread submits");
            assert_eq!(record.status, OrderStatus::Filled);

            let state = journal.replay(&permit).expect("replay");
            let leg_events: Vec<_> = state
                .events
                .iter()
                .filter(|e| e.kind == LEG_FILL_KIND)
                .collect();
            assert_eq!(leg_events.len(), 2, "one journal row per executed leg");
            // Leg detail rows parse back to the executed legs.
            let parsed: Vec<LegExecution> = leg_events
                .iter()
                .map(|e| serde_json::from_str(&e.detail).expect("leg detail parses"))
                .collect();
            assert_eq!(parsed, venue.executed_legs("r#0"));

            // Leg-aware journal vs venue truth agree leg by leg.
            let journal_net = journal_leg_net(&state.events).expect("leg net builds");
            let mut venue_net = HashMap::new();
            for (occ, position) in venue.spread_positions() {
                let sign = match position.side {
                    crate::position::PositionSide::Long => 1,
                    crate::position::PositionSide::Short => -1,
                };
                *venue_net.entry(occ).or_insert(0) += sign * i64::from(position.quantity_abs);
            }
            let report = reconcile_legs(&journal_net, &venue_net);
            assert!(report.matched);

            // Dropping a leg row breaks agreement and halts entries.
            let mut tampered = journal_net.clone();
            tampered.remove(&vertical().legs()[0].contract.occ_symbol());
            let bad = reconcile_legs(&tampered, &venue_net);
            assert!(!bad.matched);
            assert!(!bad.alerts.is_empty());
            let mut gate = ReconcileGate::default();
            gate.observe(&bad);
            assert!(gate.entries_halted());
        })
        .await;
}

#[tokio::test]
async fn pre_migration_rows_replay_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path);
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            // Pre-migration shape: intent row plus status marks, no leg rows.
            journal
                .record_intent(&permit, &single_intent("old#0"), now())
                .expect("journal intent");
            journal
                .mark_status(&permit, "old#0", OrderStatus::Submitted, now())
                .expect("mark submitted");
            journal
                .mark_status(&permit, "old#0", OrderStatus::Filled, now())
                .expect("mark filled");
            let state = journal.replay(&permit).expect("replay");
            assert_eq!(state.intents, vec![single_intent("old#0")]);
            let kinds: Vec<_> = state.events.iter().map(|e| e.kind.as_str()).collect();
            assert_eq!(kinds, vec!["intent", "submitted", "filled"]);
            assert!(journal_leg_net(&state.events).expect("leg net").is_empty());
        })
        .await;
}

/// Time-varying leg quotes: each contract reads from a stamped ladder, so a
/// submission prices legs as of its own `now`.
struct LadderLegQuotes {
    early: HashMap<String, Quote>,
    late: HashMap<String, Quote>,
    switch_at: chrono::DateTime<Utc>,
}

impl LegQuoteSource for LadderLegQuotes {
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote> {
        self.late.get(&contract.occ_symbol()).copied()
    }

    fn leg_quote_at(&self, contract: &OptionContract, at: chrono::DateTime<Utc>) -> Option<Quote> {
        let ladder = if at < self.switch_at {
            &self.early
        } else {
            &self.late
        };
        ladder.get(&contract.occ_symbol()).copied()
    }
}

#[tokio::test]
async fn spread_prices_legs_as_of_submission_time() {
    let legs = vertical();
    let early_now = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap();
    let late_now = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 5).unwrap();
    // Ladder quotes carry their own instant's stamp, so the age gate passes
    // at the submission that reads them.
    let stamped = |bid: &str, ask: &str, ts: chrono::DateTime<Utc>| {
        Quote::new()
            .bid(money(bid))
            .ask(money(ask))
            .bid_size(50)
            .ask_size(50)
            .ts(ts)
            .build()
    };
    let early = HashMap::from([
        (
            legs.legs()[0].contract.occ_symbol(),
            stamped("2.50", "2.70", early_now),
        ),
        (
            legs.legs()[1].contract.occ_symbol(),
            stamped("1.20", "1.40", early_now),
        ),
    ]);
    // Late ladder is exactly double the early one: any as-of pricing shows
    // up as a doubled net debit.
    let late = HashMap::from([
        (
            legs.legs()[0].contract.occ_symbol(),
            stamped("5.00", "5.40", late_now),
        ),
        (
            legs.legs()[1].contract.occ_symbol(),
            stamped("2.40", "2.80", late_now),
        ),
    ]);
    let switch_at = Utc.with_ymd_and_hms(2026, 6, 2, 12, 0, 0).unwrap();
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(LadderLegQuotes {
        early,
        late,
        switch_at,
    }));

    // Same intent, same limit, two submission instants straddling the switch.
    let early_record = venue
        .submit(
            spread_intent("v#early", legs.clone()),
            &quote("600.00", "600.10"),
            early_now,
        )
        .await
        .expect("early vertical fills");
    let late_record = venue
        .submit(
            spread_intent("v#late", legs.clone()),
            &quote("600.00", "600.10"),
            late_now,
        )
        .await
        .expect("late vertical fills");
    // Early net debit 1.5039 (pinned above); late legs double, so the net
    // doubles exactly — the submission read its own instant's ladder.
    assert_eq!(early_record.avg_price, Some(money("1.5039")));
    assert_eq!(
        late_record.avg_price,
        Some(money("3.0078")),
        "the late submission prices the late ladder, never the early one"
    );
}

#[tokio::test]
async fn timeless_stubs_keep_newest_only_behavior() {
    // A stub implementing only `leg_quote` (the pre-time default) serves the
    // same quote at every instant: backward compatibility is behavior, not
    // just compilation.
    let venue = venue_with(liquid_vertical_quotes());
    let legs = vertical();
    let past = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    let record = venue
        .submit(
            spread_intent("v#past", legs),
            &quote("600.00", "600.10"),
            past,
        )
        .await
        .expect("timeless stub still fills");
    assert_eq!(
        record.avg_price,
        Some(money("1.5039")),
        "the default leg_quote_at delegates to leg_quote at any instant"
    );
}
