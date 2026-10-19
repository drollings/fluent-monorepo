//! Server arming through the command journal: the route journals first, the
//! fold is the authority, and a revoked grant is representable but neither
//! live nor tightenable.

use super::*;
use protopyne_strategy::{
    BreakerLimits, GrantBudgets, GrantPermission, GrantScope, PortfolioThresholds,
    SignalThresholds, TradeThresholds,
};

fn history() -> Vec<Bar> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

async fn boot() -> ServerHandle {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve(config, history()).await.expect("test server boots")
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
                .max_loss(Money::from(5_000))
                .max_notional(Money::from(100_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(1_000))
                .max_spread_width(Money::from(50))
                .min_dte_days(7)
                .max_dte_days(60)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid stamp"))
        .valid_until(chrono::DateTime::from_timestamp(1_900_000_000, 0).expect("valid stamp"))
        .build()
}

fn journal_kinds(state: &Shared) -> Vec<String> {
    state
        .journal
        .replay(&state.journal_permit)
        .expect("ephemeral journal replays")
        .events
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

#[tokio::test]
async fn arming_journals_and_the_fold_rebuilds_it() {
    let handle = boot().await;
    let state = Shared::clone(&handle.state);

    apply_arm(&state, &grant("g1")).expect("arm succeeds");
    assert_eq!(
        live_grant(&state).map(|grant| grant.id),
        Some("g1".to_owned()),
        "the fold reports the armed grant"
    );
    assert!(
        journal_kinds(&state).contains(&COMMAND_ARM_GRANT_KIND.to_owned()),
        "arming journaled its authority row"
    );

    // Restart simulation: the armed grant is rebuilt from rows alone.
    let events = state
        .journal
        .replay(&state.journal_permit)
        .expect("replay")
        .events;
    assert_eq!(
        replay_grant(&events).map(|grant| grant.id),
        Some("g1".to_owned())
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn revoking_leaves_nothing_live_and_tightening_requires_a_new_grant() {
    let handle = boot().await;
    let state = Shared::clone(&handle.state);

    apply_arm(&state, &grant("g1")).expect("arm succeeds");
    revoke_grant(&state, "g1").expect("revoke succeeds");
    assert!(
        live_grant(&state).is_none(),
        "nothing is live after a revoke"
    );
    assert!(journal_kinds(&state).contains(&COMMAND_REVOKE_GRANT_KIND.to_owned()));

    // Tightening is a patch against a live grant: with none, it refuses.
    let patch = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":1,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
    assert!(
        preview_arm_of(&state, &patch).is_err(),
        "a revoke leaves nothing to tighten"
    );

    // A fresh grant is the only way forward.
    apply_arm(&state, &grant("g2")).expect("a new grant arms");
    assert_eq!(
        live_grant(&state).map(|grant| grant.id),
        Some("g2".to_owned())
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn re_arming_a_revoked_grant_refuses() {
    let handle = boot().await;
    let state = Shared::clone(&handle.state);

    let refused = apply_arm(&state, &grant("g1").revoked_copy());
    assert!(refused.is_err(), "a revoked grant is not armable");
    assert!(live_grant(&state).is_none());
    assert!(
        !journal_kinds(&state).contains(&COMMAND_ARM_GRANT_KIND.to_owned()),
        "a refused arm journals nothing"
    );

    // Revoking an id that is not live refuses too.
    assert!(revoke_grant(&state, "missing").is_err());

    handle.shutdown().await;
}
