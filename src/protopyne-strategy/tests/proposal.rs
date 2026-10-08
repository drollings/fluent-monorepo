use super::*;
use chrono::Utc;
use protopyne_types::{Money, Qty};
use rust_decimal::Decimal;

fn proposal() -> Proposal {
    Proposal::new()
        .id("p1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(Money::new(Decimal::from(250)))
        .max_loss(Money::new(Decimal::from(300)))
        .notional(Money::new(Decimal::from(1_000)))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(Utc::now())
        .build()
}

#[test]
fn happy_path_walks_the_full_lifecycle() {
    let now = Utc::now();
    let mut p = proposal();
    let mut journal = Vec::new();
    let steps = [
        (ProposalEvent::PassRisk, ProposalState::RiskChecked),
        (ProposalEvent::AutoApprove, ProposalState::AutoApproved),
        (ProposalEvent::Submit, ProposalState::Submitted),
        (ProposalEvent::StartWork, ProposalState::Working),
        (ProposalEvent::Fill, ProposalState::Filled),
    ];
    for (event, expected) in steps {
        let entry = p.apply(event, "test", now).expect("legal move");
        assert_eq!(p.state, expected);
        assert_eq!(entry.to, expected);
        journal.push(entry);
    }
    assert_eq!(journal.len(), 5);
    for entry in &journal {
        assert_eq!(entry.proposal_id, "p1");
        assert_eq!(entry.at, now);
    }
}

#[test]
fn human_path_routes_through_approval() {
    let now = Utc::now();
    let mut p = proposal();
    p.apply(ProposalEvent::PassRisk, "risk ok", now)
        .expect("legal move");
    p.apply(ProposalEvent::RequireHuman, "no grant", now)
        .expect("legal move");
    assert_eq!(p.state, ProposalState::PendingHuman);
    p.apply(ProposalEvent::Approve, "human yes", now)
        .expect("legal move");
    assert_eq!(p.state, ProposalState::Approved);
}

#[test]
fn illegal_moves_are_errors_never_silent_noops() {
    assert!(transition(ProposalState::Proposed, ProposalEvent::Submit).is_err());
    assert!(transition(ProposalState::Filled, ProposalEvent::Fill).is_err());
    assert!(transition(ProposalState::Rejected, ProposalEvent::Approve).is_err());
    assert!(transition(ProposalState::Proposed, ProposalEvent::Approve).is_err());
}

#[test]
fn terminal_states_accept_no_further_events() {
    for terminal in [
        ProposalState::Filled,
        ProposalState::Partial,
        ProposalState::Cancelled,
        ProposalState::Rejected,
        ProposalState::Expired,
    ] {
        for event in [
            ProposalEvent::Approve,
            ProposalEvent::Submit,
            ProposalEvent::Fill,
            ProposalEvent::Cancel,
        ] {
            assert!(
                transition(terminal, event).is_err(),
                "{terminal:?} must reject {event:?}"
            );
        }
    }
}

#[test]
fn defined_risk_covers_everything_but_naked_short() {
    for structure in [
        StructureKind::Single,
        StructureKind::Vertical,
        StructureKind::Calendar,
        StructureKind::Condor,
    ] {
        assert!(structure.is_defined_risk(), "{structure:?} is bounded");
    }
    assert!(!StructureKind::NakedShort.is_defined_risk());
}

#[test]
fn contracts_are_none_for_fractional_quantities() {
    let mut p = proposal();
    p.quantity = Qty::new(Decimal::new(15, 1));
    assert_eq!(p.contracts(), None);
    p.quantity = Qty::new(Decimal::from(3));
    assert_eq!(p.contracts(), Some(3));
}
