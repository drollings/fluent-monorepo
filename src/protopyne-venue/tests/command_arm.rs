//! Journaled arming: `CommandUnit` arm/revoke rows fold back into the live
//! grant, and a revoked grant is representable but never armable.

use super::*;
use crate::paper::PaperConfig;
use chrono::TimeZone;
use protopyne_strategy::{
    BreakerLimits, Direction, GrantBudgets, GrantPermission, GrantScope, PortfolioThresholds,
    SignalThresholds, StructureKind, TradeThresholds,
};
use rust_decimal::Decimal;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn grant(id: &str) -> Grant {
    Grant::new()
        .id(id.to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(10)
                .max_contracts(100)
                .max_loss(money("5000"))
                .max_notional(money("100000"))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money("1000"))
                .max_spread_width(money("50"))
                .min_dte_days(7)
                .max_dte_days(60)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money("10000"))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn ctx_with(permit: &VenueStorage) -> WorkContext {
    WorkContext {
        caps: fluent_wvr::CapabilitySet::new().with(permit.clone()),
        ..WorkContext::default()
    }
}

async fn scoped<F, T>(permit: VenueStorage, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new().with(permit);
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

fn journal_at(path: &std::path::Path) -> (Journal, VenueStorage) {
    let journal = Journal::open(path).expect("open");
    let permit = VenueStorage::new(path.to_owned());
    (journal, permit)
}

fn unit(grants: &GrantHandle, journal: &Arc<Journal>, command: CommandKind) -> CommandUnit {
    CommandUnit::new()
        .name("cmd.grant".to_owned())
        .command(command)
        .venue(Arc::new(PaperVenue::new(PaperConfig::new().build())))
        .state(StateHandle::new(TradingState::Enabled))
        .registry(Arc::new(PortfolioRegistry::new()))
        .grants(grants.clone())
        .journal(Arc::clone(journal))
        .build()
}

/// Read the whole journal inside the capability scope replay demands.
async fn events_of(journal: &Arc<Journal>, permit: &VenueStorage) -> Vec<JournalEvent> {
    let permit = permit.clone();
    let journal = Arc::clone(journal);
    scoped(permit.clone(), async move {
        journal.replay(&permit).expect("replay").events
    })
    .await
}

#[tokio::test]
async fn an_arm_row_replays_into_a_fresh_handle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let journal = Arc::new(journal);
    let grants = GrantHandle::default();

    scoped(permit.clone(), async {
        unit(
            &grants,
            &journal,
            CommandKind::ArmGrant {
                grant: Box::new(grant("g1")),
            },
        )
        .execute(&ctx_with(&permit))
        .expect("arm executes");
    })
    .await;
    assert_eq!(grants.get(), Some(grant("g1")));

    // Restart simulation: fold the journal into a fresh handle.
    let events = events_of(&journal, &permit).await;
    assert!(
        events
            .iter()
            .any(|event| event.kind == COMMAND_ARM_GRANT_KIND),
        "the arm journaled its row"
    );
    let rebuilt = GrantHandle::default();
    rebuilt.set(replay_grant(&events));
    assert_eq!(
        rebuilt.get(),
        Some(grant("g1")),
        "replay rebuilds the armed grant from rows alone"
    );
}

#[tokio::test]
async fn a_revoke_row_replays_into_nothing_live() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let journal = Arc::new(journal);
    let grants = GrantHandle::default();

    scoped(permit.clone(), async {
        let ctx = ctx_with(&permit);
        unit(
            &grants,
            &journal,
            CommandKind::ArmGrant {
                grant: Box::new(grant("g1")),
            },
        )
        .execute(&ctx)
        .expect("arm executes");
        unit(
            &grants,
            &journal,
            CommandKind::RevokeGrant {
                grant_id: "g1".to_owned(),
            },
        )
        .execute(&ctx)
        .expect("revoke executes");
    })
    .await;

    assert!(
        grants.get().expect("representable").revoked,
        "the revoked grant stays representable"
    );
    let events = events_of(&journal, &permit).await;
    assert!(events
        .iter()
        .any(|event| event.kind == COMMAND_REVOKE_GRANT_KIND));
    let folded = replay_grant(&events).expect("the revoked grant is representable");
    assert!(folded.revoked);
    assert!(
        !folded.is_live(Utc::now()),
        "nothing is live after a revoke"
    );
}

#[tokio::test]
async fn arming_a_revoked_grant_refuses_and_journals_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let journal = Arc::new(journal);
    let grants = GrantHandle::default();

    scoped(permit.clone(), async {
        let arm = unit(
            &grants,
            &journal,
            CommandKind::ArmGrant {
                grant: Box::new(grant("g1").revoked_copy()),
            },
        );
        assert!(arm.execute(&ctx_with(&permit)).is_err());
    })
    .await;

    assert_eq!(grants.get(), None, "a refused arm installs nothing");
    let events = events_of(&journal, &permit).await;
    assert!(
        events
            .iter()
            .all(|event| event.kind != COMMAND_ARM_GRANT_KIND),
        "a refused arm leaves no row"
    );
}

#[tokio::test]
async fn an_unmatched_revoke_refuses_without_a_revoke_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let journal = Arc::new(journal);
    let grants = GrantHandle::default();

    scoped(permit.clone(), async {
        let ctx = ctx_with(&permit);
        let revoke = unit(
            &grants,
            &journal,
            CommandKind::RevokeGrant {
                grant_id: "g1".to_owned(),
            },
        );
        // Nothing armed: revoke refuses.
        assert!(revoke.execute(&ctx).is_err());

        unit(
            &grants,
            &journal,
            CommandKind::ArmGrant {
                grant: Box::new(grant("g1")),
            },
        )
        .execute(&ctx)
        .expect("arm executes");

        revoke.execute(&ctx).expect("first revoke executes");
        // Re-revoking the already-revoked grant refuses.
        assert!(revoke.execute(&ctx).is_err());
    })
    .await;

    let events = events_of(&journal, &permit).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == COMMAND_REVOKE_GRANT_KIND)
            .count(),
        1,
        "only the effective revoke writes a row"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == COMMAND_ARM_GRANT_KIND)
            .count(),
        1
    );
    assert!(events
        .iter()
        .all(|event| event.client_id == OPERATOR_CLIENT_ID));
}
