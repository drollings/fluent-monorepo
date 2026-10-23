//! Journaled arming: `CommandUnit` arm/revoke rows fold back into the live
//! grant, and a revoked grant is representable but never armable.

use super::*;
use crate::paper::PaperConfig;
use protopyne_strategy::fixtures::grant_with;
use protopyne_strategy::GrantBudgets;
fn grant(id: &str) -> Grant {
    // The shared fixture's shape with the arm suite's tighter budgets and DTE
    // window; the id is what tells two arms apart.
    grant_with(|grant| {
        grant.id = id.to_owned();
        grant.budgets = GrantBudgets::new()
            .max_trades(10)
            .max_contracts(100)
            .max_loss(Money::from(5_000))
            .max_notional(Money::from(100_000))
            .build();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
        grant.trade.min_dte_days = 7;
        grant.trade.max_dte_days = 60;
        grant.portfolio.max_open_risk = Money::from(10_000);
    })
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

// ── Attest rows re-arm entries without consuming anything ───────────────────

fn attest_event(seq: i64, grant_id: &str, at_ms: i64) -> JournalEvent {
    JournalEvent {
        seq,
        client_id: OPERATOR_CLIENT_ID.to_owned(),
        kind: COMMAND_ATTEST_KIND.to_owned(),
        detail: serde_json::json!({ "grant_id": grant_id }).to_string(),
        at_ms,
    }
}

#[test]
fn attest_rows_fold_last_wins_and_consume_nothing() {
    let first = attest_event(1, "g1", 1_000);
    let second = attest_event(2, "g1", 2_000);
    let folded = replay_attestation(&[first, second]).expect("attest rows fold");
    assert_eq!(
        folded,
        DateTime::from_timestamp_millis(2_000).expect("valid instant"),
        "last row wins"
    );
    assert!(
        replay_attestation(&[]).is_none(),
        "no rows means never attested"
    );
}

#[tokio::test]
async fn an_attest_row_journals_and_replays() {
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
        // A wrong id refuses before journaling: attestation names its grant.
        unit(
            &grants,
            &journal,
            CommandKind::Attest {
                grant_id: "nope".to_owned(),
            },
        )
        .execute(&ctx)
        .expect_err("a mismatched attest refuses");
        unit(
            &grants,
            &journal,
            CommandKind::Attest {
                grant_id: "g1".to_owned(),
            },
        )
        .execute(&ctx)
        .expect("attest executes");
    })
    .await;

    let events = events_of(&journal, &permit).await;
    let attests: Vec<_> = events
        .iter()
        .filter(|event| event.kind == COMMAND_ATTEST_KIND)
        .collect();
    assert_eq!(attests.len(), 1, "only the matching attest journaled");
    // The folded instant is the row's own time: replay carries authority.
    let folded = replay_attestation(&events).expect("the attest row folds");
    assert_eq!(
        folded.timestamp_millis(),
        attests[0].at_ms,
        "replay carries the journaled instant"
    );
}
