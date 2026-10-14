use super::*;

#[test]
fn mock_gateway_serves_every_read_method() {
    let gateway = MockGateway::new();
    let health = gateway.health().expect("mock health reads");
    assert_eq!(health.status, "ok", "mock health is live");
    let proposals = gateway.proposals().expect("mock proposals read");
    assert_eq!(proposals.len(), 1, "mock inbox holds one row");
    assert_eq!(proposals[0].id, "mock-prop-1");
    let portfolio = gateway.portfolio().expect("mock portfolio reads");
    assert_eq!(portfolio.positions.len(), 1, "mock book holds one position");
    let journal = gateway.journal().expect("mock journal reads");
    assert_eq!(journal.len(), 2, "mock journal holds two rows");
    assert!(journal.windows(2).all(|pair| pair[0].seq < pair[1].seq), "journal reads in sequence order");
    let bars = gateway.chart_bars().expect("mock bars read");
    assert_eq!(bars.rows.len(), 3, "mock chart holds three bars");
    let overlay =
        gateway.chart_overlays("sma", 14).expect("mock overlay reads");
    assert_eq!(overlay.kind, "sma");
    assert_eq!(overlay.period, 14);
    assert_eq!(overlay.values.len(), bars.rows.len(), "overlay agrees with bars positionally");
    let replay = gateway.replay().expect("mock replay reads");
    assert_eq!(replay.steps.len(), bars.rows.len(), "replay window matches the chart window");
    assert_eq!(replay.total_bars, replay.steps.len() as u64);
    assert_eq!(replay.cursor, replay.total_bars, "a fresh read stands at the head");
    assert!(
        replay.steps.iter().enumerate().all(|(position, step)| {
            step.bar_index == position as u64 && step.done
        }),
        "steps are in bar order and every one was reached"
    );
    assert_eq!(
        replay.steps.iter().filter(|step| step.checkpoint).count(),
        2,
        "the mock publishes named anchors on the even bars"
    );
}

#[test]
fn mock_gateway_arms_explicit_grants_and_refuses_vague_ones() {
    let gateway = MockGateway::new();
    let grant = serde_json::json!({"grant_id": "grant-1", "scope": "exit-only"});
    let response = gateway.arm(grant).expect("explicit grant arms");
    assert!(response.armed);
    assert_eq!(response.grant_id, "grant-1");
    let vague = serde_json::json!({"scope": "everything"});
    let error = gateway.arm(vague).expect_err("grant without an id refuses");
    assert_eq!(error, GatewayError::InvalidGrant("grant document names no grant_id".to_owned()));
}

#[test]
fn gateway_is_object_safe_behind_a_trait_object() {
    let gateway: Box<dyn TerminalGateway> = Box::new(MockGateway::new());
    assert_eq!(gateway.health().expect("object health reads").status, "ok");
    assert_eq!(gateway.proposals().expect("object proposals read").len(), 1);
}

#[test]
fn mock_gateway_previews_the_canned_worst_case() {
    // The preview is a read, never an arm: it answers the worst case for
    // any patch without storing anything.
    let gateway = MockGateway::new();
    let patch = serde_json::json!({"patch": {"budgets": r#"{"max_trades":1}"#}});
    let preview = gateway.preview_arm(patch).expect("mock preview reads");
    assert_eq!(preview.max_trades, 1);
    assert_eq!(preview.max_loss, Money::from(100));
    assert_eq!(preview.max_notional, Money::from(1_000));
}

#[test]
fn mock_gateway_serves_current_tier_texts() {
    let gateway = MockGateway::new();
    let tiers = gateway.grant_tiers().expect("mock tiers read");
    let budgets: serde_json::Value =
        serde_json::from_str(&tiers["budgets"]).expect("budgets tier is document text");
    assert_eq!(budgets["max_trades"], serde_json::json!(1));
    assert_eq!(
        budgets["max_loss"],
        serde_json::json!("100"),
        "money rides decimal text, never a JSON number"
    );
    assert_eq!(
        tiers["valid_until"], "2027-01-01T00:00:00Z",
        "the window end is bare RFC 3339, not a quoted JSON string"
    );
}
