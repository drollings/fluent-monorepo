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

    // Replay parity: the gateway reads the route's default cadence, so a
    // bare `/replay` must be the same read the gateway answers with.
    let replay: protopyne_view::ReplayView = client
        .get(format!("{base}/replay"))
        .send()
        .await
        .expect("replay reachable")
        .json()
        .await
        .expect("replay is JSON");
    assert_eq!(gateway.replay().expect("gateway replay reads"), replay);

    handle.shutdown().await;
}

/// The replay read is a lift of the runner's replay graph, so the step
/// list it publishes is the graph's own order and anchor cadence — not a
/// second walk that could disagree with it.
#[tokio::test]
async fn replay_route_publishes_the_runner_graph_and_refuses_a_bad_cadence() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();

    let view = gateway.replay().expect("gateway replay reads");
    assert_eq!(view.steps.len(), history().len(), "one step per history bar");
    assert_eq!(view.total_bars, view.steps.len() as u64);
    assert_eq!(
        view.cursor, view.total_bars,
        "an untouched read stands at the head"
    );
    assert!(
        view.steps.iter().enumerate().all(|(position, step)| {
            step.bar_index == position as u64
                && step.done
                && step.checkpoint == (position % crate::DEFAULT_REPLAY_CHECKPOINT_EVERY == 0)
        }),
        "steps are in bar order, all reached, anchored on the recorded cadence"
    );
    assert!(
        !view.steps[0].ts.is_empty(),
        "each step carries the bar timestamp the core recorded"
    );

    // A cadence the core refuses surfaces through the route, and the
    // gateway's own read is unaffected by it.
    let refused: crate::ErrorBody = client
        .get(format!("{base}/replay?checkpoint_every=0"))
        .send()
        .await
        .expect("replay reachable")
        .json()
        .await
        .expect("error body is JSON");
    assert!(
        refused.error.contains("checkpoint_every must be at least 1"),
        "the core's own refusal surfaces, got: {}",
        refused.error
    );
    assert!(
        !gateway.replay().expect("gateway replay still reads").steps.is_empty(),
        "the refused route read leaves the gateway read intact"
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

    let patch = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":1,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
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

    let loosen = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":99,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
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
    let patch =
        serde_json::json!({"patch": {"breakers": r#"{"max_consecutive_errors":1}"#}});
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

#[tokio::test]
async fn preview_arm_returns_the_worst_case_without_storing() {
    // The confirmation surface's read: a tightening patch previews the
    // worst case the patched grant would authorize, and the live grant the
    // next read sees is bit-identical — a preview never arms.
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");

    let patch = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":1,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
    let preview = gateway.preview_arm(patch).expect("tightening patch previews");
    assert_eq!(preview.max_trades, 1);
    assert_eq!(preview.max_loss, Money::from(500));
    assert_eq!(preview.max_notional, Money::from(5_000));

    let current = current_grant(&client, &base).await;
    assert_eq!(
        current.pointer("/budgets/max_trades"),
        Some(&serde_json::json!(2)),
        "the preview stored nothing: the live grant is unchanged"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn preview_arm_refuses_loosening_like_an_arm() {
    // A preview is the same tighten rule with no store: a loosening patch
    // refuses with the core's message, distinguishable from a malformed
    // patch, and mints nothing.
    let handle = boot().await;
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");

    let loosen = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":99,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
    let refused = gateway.preview_arm(loosen).expect_err("loosening preview refuses");
    assert!(
        format!("{refused:?}").contains("tightening must not raise any budget"),
        "the core's tighten refusal surfaces, got: {refused:?}"
    );
    assert!(
        matches!(refused, protopyne_view::GatewayError::InvalidGrant(_)),
        "a tighten refusal is InvalidGrant, not Malformed, got: {refused:?}"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn preview_arm_without_live_grant_refuses() {
    let handle = boot().await;
    let gateway = handle.gateway();
    let patch =
        serde_json::json!({"patch": {"breakers": r#"{"max_consecutive_errors":1}"#}});
    let refused = gateway.preview_arm(patch).expect_err("preview with no live grant refuses");
    assert!(format!("{refused:?}").contains("no live grant"), "got: {refused:?}");
    handle.shutdown().await;
}

#[tokio::test]
async fn patch_tier_riding_json_instead_of_text_refuses() {
    // Tiers ride boundary-string text: a JSON document where text belongs
    // refuses naming the tier, before any codec runs.
    let handle = boot().await;
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");
    let patch = serde_json::json!({"patch": {"budgets": {"max_trades": 1}}});
    let refused = gateway.preview_arm(patch).expect_err("a JSON tier refuses");
    assert!(
        format!("{refused:?}").contains("budgets"),
        "the refusal names the tier, got: {refused:?}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn grant_tiers_read_the_live_grant_as_display_text() {
    // Placeholders for the form's editors: every tier reads back as the
    // boundary string a patch would carry, and money rides decimal text.
    let handle = boot().await;
    let gateway = handle.gateway();
    assert!(
        gateway.grant_tiers().expect("tiers read").is_empty(),
        "no live grant means blank placeholders, not a refusal"
    );
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");
    let tiers = gateway.grant_tiers().expect("tiers read after arming");
    let budgets: serde_json::Value =
        serde_json::from_str(&tiers["budgets"]).expect("budgets tier is document text");
    assert_eq!(budgets["max_trades"], serde_json::json!(2));
    assert_eq!(
        budgets["max_loss"],
        serde_json::json!("500"),
        "money rides decimal text, never a JSON number"
    );
    assert_eq!(tiers["valid_until"], "2027-01-01T00:00:00Z");
    handle.shutdown().await;
}

#[tokio::test]
async fn preview_worst_case_matches_the_checked_in_golden() {
    // Byte-exact preview surface over the fixture grant: the confirmation
    // screen's numbers are pinned, not just shaped.
    let handle = boot().await;
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");
    let patch = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":1,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
    let preview = gateway.preview_arm(patch).expect("tightening patch previews");
    let body = serde_json::to_string(&preview).expect("worst case serializes");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("preview_worst_case.json");
    if std::env::var("OVERWRITE_GOLDENS").is_ok() {
        std::fs::write(&path, &body).expect("golden writes");
    }
    let expected = std::fs::read_to_string(&path).expect("golden checked in");
    assert_eq!(body, expected, "preview worst-case bytes drifted from the golden");
    handle.shutdown().await;
}

#[tokio::test]
async fn patch_negative_integer_leaf_refuses_by_tier() {
    // Contextual refusal, pinned at the tier codec: a negative where the
    // field requires positive never reaches tighten.
    let handle = boot().await;
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");
    let patch = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":-1,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
    let refused = gateway.preview_arm(patch).expect_err("negative trades refuse");
    assert!(
        format!("{refused:?}").contains("budgets"),
        "the refusal names the tier, got: {refused:?}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn patch_zero_trade_budget_refuses_at_core_validation() {
    // Zero parses (it is a u32) but authorizes nothing: the core's grant
    // validation refuses, and the live grant is untouched.
    let handle = boot().await;
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");
    let patch = serde_json::json!({"patch": {"budgets": concat!(
        r#"{"max_trades":0,"max_contracts":4,"#,
        r#""max_loss":"500","max_notional":"5000"}"#,
    )}});
    let refused = gateway.arm(patch).expect_err("zero trades refuse");
    assert!(
        format!("{refused:?}").contains("at least one trade"),
        "the core's validation names the bound, got: {refused:?}"
    );
    handle.shutdown().await;
}
