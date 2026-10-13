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
