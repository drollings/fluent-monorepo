//! Gateway-equals-routes: `ServerGateway` serves the same views as HTTP.
//!
//! Both sides call the same builders, so this pins the wiring (not the
//! math): every read through the gateway must equal the same read through
//! the route, and an arm through the gateway must be visible through the
//! route's grant read. Journal reads empty on both sides until the journal
//! route lands.

use std::path::PathBuf;

use crate::service::{ServerConfig, ServerHandle, serve};
use chrono::{TimeZone, Utc};
use protopyne_types::Bar;
use protopyne_strategy::{
    BreakerLimits, Direction, Grant, GrantBudgets, GrantPermission, GrantScope,
    PortfolioThresholds, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::Money;
use protopyne_view::TerminalGateway;

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::load_bars_csv(&path).expect("vendored history loads")
}

async fn boot() -> ServerHandle {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve(config, history()).await.expect("test server boots")
}

#[tokio::test]
async fn gateway_reads_equal_route_reads() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();

    let health: protopyne_view::HealthView = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("health reachable")
        .json()
        .await
        .expect("health is JSON");
    assert_eq!(gateway.health().expect("gateway health reads"), health);

    let proposals: Vec<protopyne_view::ProposalView> = client
        .get(format!("{base}/proposals"))
        .send()
        .await
        .expect("proposals reachable")
        .json()
        .await
        .expect("proposals are JSON");
    assert_eq!(gateway.proposals().expect("gateway proposals read"), proposals);

    let portfolio: protopyne_view::PortfolioView = client
        .get(format!("{base}/portfolio"))
        .send()
        .await
        .expect("portfolio reachable")
        .json()
        .await
        .expect("portfolio is JSON");
    assert_eq!(gateway.portfolio().expect("gateway portfolio reads"), portfolio);

    assert!(
        gateway.journal().expect("gateway journal reads").is_empty(),
        "no journal route yet: both sides read empty"
    );

    let bars: protopyne_view::ChartBars = client
        .get(format!("{base}/chart/bars"))
        .send()
        .await
        .expect("bars reachable")
        .json()
        .await
        .expect("bars are JSON");
    assert_eq!(gateway.chart_bars().expect("gateway bars read"), bars);

    let overlay: protopyne_view::OverlaySeries = client
        .get(format!("{base}/chart/overlays?kind=ema&period=10"))
        .send()
        .await
        .expect("overlay reachable")
        .json()
        .await
        .expect("overlay is JSON");
    assert_eq!(
        gateway.chart_overlays("ema", 10).expect("gateway overlay reads"),
        overlay
    );

    handle.shutdown().await;
}

fn arming_grant() -> Grant {
    Grant::new()
        .id("gateway-grant".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long])
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(2)
                .max_contracts(4)
                .max_loss(Money::from(500))
                .max_notional(Money::from(5_000))
                .build(),
        )
        .signal(SignalThresholds::new().min_conviction(0.0).min_activity_score(0.0).build())
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(10))
                .max_spread_width(Money::from(5))
                .min_dte_days(1)
                .max_dte_days(30)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

#[tokio::test]
async fn gateway_arm_is_visible_through_the_route() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();

    let grant = serde_json::to_value(arming_grant()).expect("grant serializes");
    let response = gateway.arm(grant).expect("gateway arm succeeds");
    assert!(response.armed);
    assert_eq!(response.grant_id, "gateway-grant");

    let current: Option<serde_json::Value> = client
        .get(format!("{base}/grants/current"))
        .send()
        .await
        .expect("grant read reachable")
        .json()
        .await
        .expect("grant is JSON");
    assert_eq!(
        current.expect("grant stored").get("id"),
        Some(&serde_json::json!("gateway-grant")),
        "gateway arm lands where the route reads it"
    );

    handle.shutdown().await;
}

async fn current_grant(client: &reqwest::Client, base: &str) -> serde_json::Value {
    client
        .get(format!("{base}/grants/current"))
        .send()
        .await
        .expect("grant read reachable")
        .json()
        .await
        .expect("grant is JSON")
}

#[tokio::test]
async fn gateway_patch_tighten_is_visible_through_the_route() {
    // The form's envelope tightens the live grant through the core path:
    // same grant id, narrowed budgets, visible where the route reads it.
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");

    let patch = serde_json::json!({"patch": {"budgets": {
        "max_trades": 1,
        "max_contracts": 4,
        "max_loss": "500",
        "max_notional": "5000",
    }}});
    let response = gateway.arm(patch).expect("tightening patch arms");
    assert!(response.armed);
    assert_eq!(response.grant_id, "gateway-grant");
    assert_eq!(response.worst_case.max_trades, 1);

    let current = current_grant(&client, &base).await;
    assert_eq!(current.get("id"), Some(&serde_json::json!("gateway-grant")));
    assert_eq!(
        current.pointer("/budgets/max_trades"),
        Some(&serde_json::json!(1)),
        "tightened budgets land where the route reads them"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn gateway_patch_loosening_surfaces_the_core_refusal() {
    // A loosening patch is refused with the core's own tighten message —
    // not a local form refusal — and the live grant is untouched.
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");

    let loosen = serde_json::json!({"patch": {"budgets": {
        "max_trades": 99,
        "max_contracts": 4,
        "max_loss": "500",
        "max_notional": "5000",
    }}});
    let refused = gateway.arm(loosen).expect_err("loosening patch refuses");
    assert!(
        format!("{refused:?}").contains("tightening must not raise any budget"),
        "the core's tighten refusal surfaces, got: {refused:?}"
    );

    let current = current_grant(&client, &base).await;
    assert_eq!(
        current.pointer("/budgets/max_trades"),
        Some(&serde_json::json!(2)),
        "the refused patch mints nothing: the live grant is unchanged"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn gateway_patch_without_live_grant_refuses() {
    let handle = boot().await;
    let gateway = handle.gateway();
    let patch = serde_json::json!({"patch": {"breakers": {"max_consecutive_errors": 1}}});
    let refused = gateway.arm(patch).expect_err("patch with no live grant refuses");
    assert!(
        format!("{refused:?}").contains("no live grant"),
        "got: {refused:?}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn gateway_patch_malformed_refuses_before_the_core() {
    let handle = boot().await;
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");
    let patch = serde_json::json!({"patch": {"budgets": "{not json"}});
    assert!(
        gateway.arm(patch).is_err(),
        "a malformed patch never reaches the core"
    );
    handle.shutdown().await;
}
