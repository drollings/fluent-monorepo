//! Adjustment rules: drafts, not exits.
//!
//! An adjustment is a **proposal draft** — structure, legs, target DTE,
//! credit intent, and the priced economics the rule read off the mark — never
//! an `OrderIntent` and never a `LossAction`. The strategy exposes two
//! separate review paths so the two authorities cannot collapse into one.

use super::*;

use chrono::{TimeZone, Utc};
use protopyne_types::chain::LegSide;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn marked(price: i64) -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::ChainMid,
        mark: Some(money(price)),
    }
}

fn expiry() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid test date")
}

fn legs() -> SpreadLegs {
    use protopyne_types::{OptionContract, OptionKind, SpreadLeg};
    SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(
                OptionContract::new()
                    .underlying("SPY".to_owned())
                    .expiry(expiry())
                    .strike(money(600))
                    .kind(OptionKind::Call)
                    .build(),
            )
            .side(LegSide::Long)
            .build(),
        SpreadLeg::new()
            .contract(
                OptionContract::new()
                    .underlying("SPY".to_owned())
                    .expiry(expiry())
                    .strike(money(610))
                    .kind(OptionKind::Call)
                    .build(),
            )
            .side(LegSide::Short)
            .build(),
    ])
    .expect("a long/short vertical is defined-risk")
}

fn position() -> PositionView<'static> {
    PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 2,
        entry_avg: money(250),
        max_loss: money(600),
        dte_days: Some(30),
        legs: None,
        events: None,
    }
}

fn draft() -> Adjustment {
    Adjustment {
        structure: StructureKind::Vertical,
        legs: legs(),
        target_dte_days: 45,
        credit: true,
        underlying: "SPY".to_owned(),
        direction: Direction::Long,
        quantity: Qty::new(Decimal::from(2)),
        limit_price: money(5),
        max_loss: money(300),
        notional: money(1_000),
        conviction: 0.8,
        activity_score: 0.7,
    }
}

/// A rule that holds every book.
struct NoAdjust;

impl AdjustRule for NoAdjust {
    fn name(&self) -> &str {
        "test-hold"
    }

    fn review(&self, _position: &PositionView<'_>, _mark: &MarkContext) -> Vec<Adjustment> {
        Vec::new()
    }
}

/// A rule that rolls every marked book into the test draft.
struct RollOut;

impl AdjustRule for RollOut {
    fn name(&self) -> &str {
        "test-roll"
    }

    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<Adjustment> {
        if mark.mark.is_none() {
            return Vec::new();
        }
        let mut adjustment = draft();
        adjustment.underlying = position.underlying.clone();
        adjustment.direction = position.side;
        vec![adjustment]
    }
}

// The two channels accept only their own type: an `Adjustment` never becomes
// a `LossAction` and vice versa. If the types ever unified, this fails to
// compile — which is the point.
fn only_loss_actions(_: &[LossAction]) {}
fn only_drafts(_: &[Adjustment]) {}

// ── Separate paths: loss exits and roll drafts never cross ─────────────────

#[test]
fn loss_exits_and_roll_drafts_travel_separate_paths() {
    let signal: Arc<dyn Signal> =
        Arc::new(FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"));
    // A firing stop with a holding adjust rule: the exit travels the loss
    // path, and the draft path stays empty.
    let stop_strategy = Strategy::with_signal(Arc::clone(&signal))
        .with_loss(Arc::new(StopLoss::new(money(200)).expect("valid stop")))
        .with_adjust_rule(Arc::new(NoAdjust));
    let beaten = PositionView {
        quantity: 2,
        entry_avg: money(400),
        ..position()
    };
    let exits = stop_strategy.review_position(&beaten, &marked(100));
    only_loss_actions(&exits);
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].kind, "stop");
    let drafts = stop_strategy.review_adjustments(&beaten, &marked(100));
    only_drafts(&drafts);
    assert!(drafts.is_empty());
    // A holding loss element with a rolling adjust rule: the draft travels
    // the adjustment path, and the loss path stays empty.
    let roll_strategy = Strategy::with_signal(signal).with_adjust_rule(Arc::new(RollOut));
    let held = roll_strategy.review_position(&position(), &marked(250));
    only_loss_actions(&held);
    assert!(held.is_empty());
    let drafts = roll_strategy.review_adjustments(&position(), &marked(250));
    only_drafts(&drafts);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].structure, StructureKind::Vertical);
}

// ── Default empty: no adjust rule, no drafts ────────────────────────────────

#[test]
fn the_default_strategy_carries_no_adjust_rules() {
    let strategy = Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ));
    assert!(strategy
        .review_adjustments(&position(), &marked(250))
        .is_empty());
}

// ── Rules compose in installation order ─────────────────────────────────────

#[test]
fn adjust_rules_compose_in_installation_order() {
    struct Second(RollOut);
    impl AdjustRule for Second {
        fn name(&self) -> &str {
            "test-roll-2"
        }

        fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<Adjustment> {
            self.0.review(position, mark)
        }
    }
    let strategy = Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ))
    .with_adjust_rule(Arc::new(RollOut))
    .with_adjust_rule(Arc::new(Second(RollOut)));
    let drafts = strategy.review_adjustments(&position(), &marked(250));
    assert_eq!(drafts.len(), 2);
}

// ── A draft mints a Roll proposal, never an exit ────────────────────────────

#[test]
fn a_draft_mints_a_roll_proposal_with_its_legs() {
    let created_at = Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap();
    let proposal = draft()
        .propose("roll-1", "ribbon", created_at)
        .expect("a priced draft mints");
    assert_eq!(proposal.side, ProposalSide::Roll);
    assert_eq!(proposal.structure, StructureKind::Vertical);
    assert_eq!(proposal.dte_days, 45);
    assert_eq!(proposal.quantity, Qty::new(Decimal::from(2)));
    assert_eq!(proposal.legs.expect("one multi-leg order").legs().len(), 2);
    assert_eq!(proposal.state, ProposalState::Proposed);
}

// ── A draft that cannot be priced refuses, never an unpriced order ──────────

#[test]
fn an_unpriceable_draft_refuses_rather_than_minting() {
    let created_at = Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap();
    let mut zero = draft();
    zero.quantity = Qty::zero();
    assert!(
        zero.propose("roll-1", "ribbon", created_at).is_err(),
        "a roll of nothing mints nothing"
    );
    let mut wild = draft();
    wild.conviction = 2.0;
    assert!(
        wild.propose("roll-1", "ribbon", created_at).is_err(),
        "a conviction outside [0, 1] is refused, never clamped"
    );
    let mut negative = draft();
    negative.limit_price = money(-5);
    assert!(
        negative.propose("roll-1", "ribbon", created_at).is_err(),
        "a negative limit is refused"
    );
}
