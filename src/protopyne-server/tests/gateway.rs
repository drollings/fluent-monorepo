//! Gateway-equals-routes: `ServerGateway` serves the same views as HTTP.
//!
//! Both sides call the same builders, so this pins the wiring (not the
//! math): every read through the gateway must equal the same read through
//! the route, and an arm through the gateway must be visible through the
//! route's grant read. Journal reads empty on both sides until the journal
//! route lands.

use protopyne_strategy::fixtures::grant_with;
use std::path::PathBuf;

use crate::service::{serve, ServerConfig, ServerHandle};
use chrono::{TimeZone, Utc};
use protopyne_strategy::{Direction, Grant, GrantBudgets, StructureKind};
use protopyne_types::Bar;
use protopyne_types::Money;
use protopyne_view::TerminalGateway;

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
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
    assert_eq!(
        gateway.proposals().expect("gateway proposals read"),
        proposals
    );

    let portfolio: protopyne_view::PortfolioView = client
        .get(format!("{base}/portfolio"))
        .send()
        .await
        .expect("portfolio reachable")
        .json()
        .await
        .expect("portfolio is JSON");
    assert_eq!(
        gateway.portfolio().expect("gateway portfolio reads"),
        portfolio
    );

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
        gateway
            .chart_overlays("ema", 10)
            .expect("gateway overlay reads"),
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
    assert_eq!(
        view.steps.len(),
        history().len(),
        "one step per history bar"
    );
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
        refused
            .error
            .contains("checkpoint_every must be at least 1"),
        "the core's own refusal surfaces, got: {}",
        refused.error
    );
    assert!(
        !gateway
            .replay()
            .expect("gateway replay still reads")
            .steps
            .is_empty(),
        "the refused route read leaves the gateway read intact"
    );

    handle.shutdown().await;
}

fn arming_grant() -> Grant {
    // The shared fixture's shape with the gateway's small armable budgets: this
    // is the grant the arm/preview/tier routes are exercised against.
    grant_with(|grant| {
        grant.id = "gateway-grant".to_owned();
        grant.scope.structures = vec![StructureKind::Vertical];
        grant.scope.directions = vec![Direction::Long];
        grant.budgets = GrantBudgets::new()
            .max_trades(2)
            .max_contracts(4)
            .max_loss(Money::from(500))
            .max_notional(Money::from(5_000))
            .build();
        grant.trade.max_dte_days = 30;
    })
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
    let patch = serde_json::json!({"patch": {"breakers": r#"{"max_consecutive_errors":1}"#}});
    let refused = gateway
        .arm(patch)
        .expect_err("patch with no live grant refuses");
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
    let preview = gateway
        .preview_arm(patch)
        .expect("tightening patch previews");
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
    let refused = gateway
        .preview_arm(loosen)
        .expect_err("loosening preview refuses");
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
    let patch = serde_json::json!({"patch": {"breakers": r#"{"max_consecutive_errors":1}"#}});
    let refused = gateway
        .preview_arm(patch)
        .expect_err("preview with no live grant refuses");
    assert!(
        format!("{refused:?}").contains("no live grant"),
        "got: {refused:?}"
    );
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
async fn gateway_symbols_read_the_history_root_it_booted() {
    let dir = tempfile::tempdir().expect("tempdir");
    for ticker in ["AAA", "ZZZ"] {
        std::fs::create_dir_all(dir.path().join(ticker).join("2026/01"))
            .expect("ticker tree creates");
    }
    let mut config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    config.history_path = Some(dir.path().to_string_lossy().into_owned());
    let handle = serve(config, history()).await.expect("test server boots");
    let rows = handle.gateway().symbols().expect("gateway symbols read");
    let ids: Vec<&str> = rows.iter().map(|row| row.symbol.as_str()).collect();
    assert_eq!(
        ids,
        ["AAA", "ZZZ"],
        "discovery drives the catalog; the shipped rows do not leak in"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn gateway_serves_symbol_scoped_bars_and_overlays_across_timeframes() {
    let handle = boot().await;
    let gateway = handle.gateway();

    assert_eq!(
        gateway.symbols().expect("gateway symbols read"),
        protopyne_view::symbol_catalog(),
        "a server booted without a history root serves the shipped offline list"
    );

    let base = gateway.chart_bars().expect("gateway bars read");
    assert!(
        !base.rows.is_empty(),
        "the fixture history serves bars on the default slot"
    );
    assert_eq!(
        gateway
            .chart_bars_for("SPY", "15M")
            .expect("default-slot bars read"),
        base,
        "the 15M slot is the base: the symbol-scoped read equals the slot-agnostic read"
    );
    assert_eq!(
        gateway
            .chart_overlays_for("SPY", "15M", "ema", 10)
            .expect("default-slot overlay read"),
        gateway
            .chart_overlays("ema", 10)
            .expect("gateway overlay read"),
        "the 15M overlay equals the slot-agnostic read"
    );

    for symbol in ["GLD", "SPY"] {
        for timeframe in ["15M", "1H", "4H"] {
            let bars = gateway
                .chart_bars_for(symbol, timeframe)
                .unwrap_or_else(|e| panic!("{symbol} {timeframe} bars: {e:?}"));
            assert!(!bars.rows.is_empty(), "{symbol} {timeframe} serves rows");
            let overlay = gateway
                .chart_overlays_for(symbol, timeframe, "ema", 10)
                .unwrap_or_else(|e| panic!("{symbol} {timeframe} overlay: {e:?}"));
            assert_eq!(
                overlay.values.len(),
                bars.rows.len(),
                "{symbol} {timeframe} overlay agrees with bars positionally"
            );
        }
    }

    // A timeframe the 15M base cannot express refuses instead of silently
    // serving base-spacing bars: finer grids, non-multiples, and unknown
    // labels alike, each naming the base spacing.
    for timeframe in ["1M", "5M", "10", "10M"] {
        let err = gateway
            .chart_bars_for("SPY", timeframe)
            .expect_err(&format!("{timeframe} refuses over a 15M base"));
        assert!(
            err.to_string().contains("M15"),
            "{timeframe} names the base spacing: {err}"
        );
    }
    assert!(
        gateway.chart_overlays_for("SPY", "5M", "ema", 10).is_err(),
        "overlays refuse an inexpressible timeframe like bars do"
    );

    // A coarser timeframe the base can fill folds upward: 1H is a whole 4x
    // fold of the 15M base, so it serves strictly fewer bars.
    let hour = gateway.chart_bars_for("SPY", "1H").expect("1H bars read");
    assert!(
        hour.rows.len() < base.rows.len(),
        "1H folds the 15M base upward into fewer bars"
    );

    // Blank identifiers refuse before any read.
    assert!(
        matches!(
            gateway.chart_bars_for("", "15M"),
            Err(protopyne_view::GatewayError::Malformed(_))
        ),
        "a blank symbol refuses as Malformed"
    );
    assert!(
        matches!(
            gateway.chart_overlays_for("SPY", "  ", "ema", 10),
            Err(protopyne_view::GatewayError::Malformed(_))
        ),
        "a blank timeframe refuses as Malformed"
    );

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
    let preview = gateway
        .preview_arm(patch)
        .expect("tightening patch previews");
    let body = serde_json::to_string(&preview).expect("worst case serializes");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("preview_worst_case.json");
    if std::env::var("OVERWRITE_GOLDENS").is_ok() {
        std::fs::write(&path, &body).expect("golden writes");
    }
    let expected = std::fs::read_to_string(&path).expect("golden checked in");
    assert_eq!(
        body, expected,
        "preview worst-case bytes drifted from the golden"
    );
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
    let refused = gateway
        .preview_arm(patch)
        .expect_err("negative trades refuse");
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

#[test]
fn resampled_fallback_limit_reads_the_trailing_bars() {
    use protopyne_types::Timeframe;
    // The vendored fixture holds eleven 15M bars aligned to whole hours, so
    // the hourly fold emits exactly two whole bars.
    let bars = history();
    assert_eq!(bars.len(), 11, "the fixture pins its bar count");
    let unbounded =
        super::resampled_history(&bars, Timeframe::M15, "15M", None).expect("unbounded reads");
    assert_eq!(unbounded, bars, "None serves the boot history untouched");
    let oversize = super::resampled_history(&bars, Timeframe::M15, "15M", Some(10_000))
        .expect("oversize reads");
    assert_eq!(
        oversize, bars,
        "an oversize limit serves the boot history untouched"
    );
    let tail =
        super::resampled_history(&bars, Timeframe::M15, "15M", Some(5)).expect("bounded reads");
    assert_eq!(
        tail,
        bars[bars.len() - 5..],
        "limit 5 serves the last five bars"
    );
    let hourly_full =
        super::resampled_history(&bars, Timeframe::M15, "1H", None).expect("hourly folds");
    assert_eq!(
        hourly_full.len(),
        2,
        "eleven aligned 15M bars fold to two whole hours"
    );
    let hourly_one =
        super::resampled_history(&bars, Timeframe::M15, "1H", Some(1)).expect("bounded fold reads");
    assert_eq!(
        hourly_one,
        hourly_full[1..],
        "resample and limit compose: the newest hourly bar"
    );
}

/// A proposal waiting on a human: risk passed, no grant covers it. Ready to be
/// journaled as a body and decided.
fn pending_body(id: &str) -> protopyne_strategy::Proposal {
    use protopyne_strategy::{Proposal, ProposalEvent, ProposalSide};
    let mut proposal = Proposal::new()
        .id(id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(protopyne_types::Qty::new(rust_decimal::Decimal::from(2)))
        .limit_price(Money::from(600))
        .max_loss(Money::from(300))
        .notional(Money::from(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(Utc::now())
        .build();
    proposal
        .apply(ProposalEvent::PassRisk, "risk approved", Utc::now())
        .expect("pass risk");
    proposal
        .apply(
            ProposalEvent::RequireHuman,
            "no grant covers entry",
            Utc::now(),
        )
        .expect("ask human");
    proposal
}

/// Journal a pending proposal the way the run path does: the body, then the
/// `PendingHuman` transition the inbox fold reads. The body's own `state` is
/// `PendingHuman`; the transition row is what `replay_proposals` joins it to.
fn journal_pending(handle: &ServerHandle, id: &str) {
    let proposal = pending_body(id);
    handle
        .state
        .journal
        .record_proposal_body(&handle.state.journal_permit, &proposal, Utc::now())
        .expect("body");
    let entry = protopyne_strategy::JournaledTransition {
        proposal_id: id.to_owned(),
        from: protopyne_strategy::ProposalState::RiskChecked,
        to: protopyne_strategy::ProposalState::PendingHuman,
        reason: "no grant covers entry".to_owned(),
        at: Utc::now(),
    };
    let detail = serde_json::to_string(&entry).expect("transition encodes");
    handle
        .state
        .journal
        .record_transition(
            &handle.state.journal_permit,
            id,
            protopyne_venue::PROPOSAL_TRANSITION_KIND,
            &detail,
            Utc::now(),
        )
        .expect("transition");
}

/// Fold the journal's transition rows into per-proposal states.
fn journaled_states(
    handle: &ServerHandle,
) -> std::collections::BTreeMap<String, protopyne_strategy::ProposalState> {
    let replayed = handle
        .state
        .journal
        .replay(&handle.state.journal_permit)
        .expect("replay");
    protopyne_venue::replay_proposal_states(&replayed.events)
}

#[tokio::test]
async fn gateway_and_route_decide_a_journaled_proposal_the_same_way() {
    use protopyne_strategy::ProposalState;
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    // Gateway path.
    journal_pending(&handle, "gateway-p");
    handle
        .gateway()
        .approve("gateway-p")
        .expect("gateway approves");
    assert_eq!(
        journaled_states(&handle).get("gateway-p"),
        Some(&ProposalState::Approved)
    );

    // Route path: the same decision through HTTP lands the same state.
    journal_pending(&handle, "route-p");
    let path = format!("{base}/proposals/route-p/approve");
    let status = client.post(&path).send().await.expect("reachable").status();
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        journaled_states(&handle).get("route-p"),
        Some(&ProposalState::Approved)
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn route_rejects_with_the_reason_journaled() {
    use protopyne_strategy::ProposalState;
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    journal_pending(&handle, "reject-p");
    let path = format!("{base}/proposals/reject-p/reject");
    let status = client
        .post(&path)
        .json(&serde_json::json!({"reason": "too much size"}))
        .send()
        .await
        .expect("reachable")
        .status();
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        journaled_states(&handle).get("reject-p"),
        Some(&ProposalState::Rejected)
    );

    // An unknown id refuses with NotFound, journaling nothing.
    let missing = client
        .post(format!("{base}/proposals/nope/approve"))
        .send()
        .await
        .expect("reachable")
        .status();
    assert_eq!(missing, reqwest::StatusCode::NOT_FOUND);
    handle.shutdown().await;
}

#[tokio::test]
async fn pending_route_reads_the_journaled_fold() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let empty: Vec<protopyne_view::ProposalView> = client
        .get(format!("{base}/proposals/pending"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert!(empty.is_empty(), "no journaled proposal is pending yet");

    journal_pending(&handle, "pending-p");
    let rows: Vec<protopyne_view::ProposalView> = client
        .get(format!("{base}/proposals/pending"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "pending-p");
    assert_eq!(rows[0].state, "PendingHuman");
    handle.shutdown().await;
}

#[tokio::test]
async fn journal_route_and_gateway_read_the_same_rows() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    // Empty journal: an empty list, not an error, on both sides.
    assert!(handle
        .gateway()
        .journal()
        .expect("gateway journal")
        .is_empty());
    let empty: Vec<protopyne_view::JournalRowView> = client
        .get(format!("{base}/journal"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert!(empty.is_empty());

    // Journal a body: both sides read the same rows.
    handle
        .state
        .journal
        .record_proposal_body(
            &handle.state.journal_permit,
            &pending_body("j-p"),
            Utc::now(),
        )
        .expect("body");
    let via_gateway = handle.gateway().journal().expect("gateway journal");
    let routed: Vec<protopyne_view::JournalRowView> = client
        .get(format!("{base}/journal"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert_eq!(via_gateway, routed);
    assert!(!routed.is_empty());
    handle.shutdown().await;
}

#[tokio::test]
async fn revoke_through_the_gateway_equals_the_route() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let grant = arming_grant();
    let grant_id = grant.id.clone();
    let document = serde_json::to_value(&grant).expect("grant serializes");

    handle
        .gateway()
        .arm(document.clone())
        .expect("first arm succeeds");
    handle
        .gateway()
        .revoke_grant(&grant_id)
        .expect("gateway revokes");
    assert!(
        handle.gateway().grant_tiers().expect("tiers").is_empty(),
        "gateway revoke ends the live grant"
    );

    handle.gateway().arm(document).expect("re-arm succeeds");
    let status = client
        .post(format!("{base}/grants/revoke"))
        .json(&serde_json::json!({ "grant_id": grant_id }))
        .send()
        .await
        .expect("reachable")
        .status();
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        handle.gateway().grant_tiers().expect("tiers").is_empty(),
        "route revoke ends the live grant"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn proposals_route_serves_the_journaled_fold_not_the_rehearsal_cache() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    // No journaled proposal and no preview published: both inboxes are empty.
    let journaled: Vec<protopyne_view::ProposalView> = client
        .get(format!("{base}/proposals"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert!(journaled.is_empty());

    // Journal a pending body: the authoritative inbox serves it, the
    // rehearsal cache stays untouched.
    journal_pending(&handle, "fold-p");
    let journaled: Vec<protopyne_view::ProposalView> = client
        .get(format!("{base}/proposals"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert_eq!(journaled.len(), 1);
    assert_eq!(journaled[0].id, "fold-p");
    assert_eq!(journaled[0].state, "PendingHuman");
    let rehearsal: Vec<protopyne_view::ProposalView> = client
        .get(format!("{base}/proposals/rehearsal"))
        .send()
        .await
        .expect("reachable")
        .json()
        .await
        .expect("json");
    assert!(
        rehearsal.is_empty(),
        "the rehearsal cache is a separate source"
    );
    handle.shutdown().await;
}

#[test]
fn booked_portfolio_folds_two_buckets_and_weights_the_entry_price() {
    use crate::portfolio::{BookedFill, PAPER_PORTFOLIO};
    use protopyne_venue::OrderSide;
    let at = Utc.with_ymd_and_hms(2026, 1, 9, 22, 0, 0).unwrap();
    let mut fills = Vec::new();
    for (index, (grant, quantity, price, fee)) in
        [("g-a", 1u32, 100i64, 1i64), ("g-b", 3u32, 120i64, 2i64)]
            .into_iter()
            .enumerate()
    {
        fills.push(
            BookedFill::new()
                .portfolio(PAPER_PORTFOLIO.to_owned())
                .underlying("SPY".to_owned())
                .structure("Vertical".to_owned())
                .strategy("ribbon".to_owned())
                .grant(grant.to_owned())
                .side(OrderSide::Buy)
                .quantity(quantity)
                .price(Money::from(price))
                .fee(Money::from(fee))
                .trade_id(format!("trade-{index}"))
                .at(at)
                .build(),
        );
    }
    let view =
        crate::service::portfolio_of_booked(&fills, "Enabled").expect("booked fills aggregate");
    assert_eq!(view.source, "booked");
    assert_eq!(
        view.positions.len(),
        1,
        "two attributed books on one underlying merge to one net position"
    );
    assert_eq!(view.positions[0].underlying, "SPY");
    assert_eq!(view.positions[0].quantity, 4);
    assert_eq!(
        view.positions[0].avg_price,
        Money::from(115),
        "the merge is volume-weighted, not the two-price arithmetic mean"
    );
    assert_eq!(view.filled_orders, 2);
    assert_eq!(view.total_fees, Money::from(3));
    assert_eq!(view.open_contracts, 4);
}
