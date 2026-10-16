//! Schema-driven grant arming form, tested from the terminal side.
//!
//! The form renders one typed editor per patch leaf from the nested
//! descriptor, assembles drafted leaves into per-tier boundary strings, and
//! submits a `{"patch": ...}` envelope of tier texts through
//! `TerminalGateway::arm` — it never mints a grant locally, and the core
//! alone validates, tightens, and refuses. Submit stays unactionable until
//! a successful preview covers exactly the drafts, so the worst case on
//! screen is the core's own number before confirmation.

use super::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use fluent_egui::FieldKind;
use fluent_wvr::Describable;
use protopyne_strategy::{
    BreakerLimits, Direction, Grant, GrantBudgets, GrantPatch, GrantPermission, GrantScope,
    PortfolioThresholds, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::Money;
use protopyne_view::{ArmResponse, GatewayError, MockGateway, TerminalGateway, WorstCase};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn descriptor() -> serde_json::Value {
    GrantPatch::default().describe()
}

fn descriptor_tier_names() -> Vec<String> {
    let properties = descriptor()["properties"]
        .as_object()
        .expect("descriptor carries properties")
        .clone();
    let mut names: Vec<String> = properties.keys().cloned().collect();
    names.sort();
    names
}

/// Leaf paths of one tier, in schema order (`tier.leaf`, or the bare tier
/// when the tier itself is the leaf).
fn tier_leaves(tier: &str) -> Vec<String> {
    let hint = descriptor()["properties"][tier].clone();
    match hint.get("properties").and_then(serde_json::Value::as_object) {
        Some(leaves) => leaves.keys().map(|leaf| format!("{tier}.{leaf}")).collect(),
        None => vec![tier.to_owned()],
    }
}

/// A recording test double: every read delegates to the mock, `preview_arm`
/// records the patch and answers the canned worst case, while `arm`
/// records the submitted document and answers canned. It proves what the
/// form sends (and whether it sends at all) without involving the core.
struct RecordGateway {
    mock: MockGateway,
    previewed: std::sync::Mutex<Option<serde_json::Value>>,
    preview_refusal: std::sync::Mutex<Option<String>>,
    sent: std::sync::Mutex<Option<serde_json::Value>>,
}

impl RecordGateway {
    fn new() -> Self {
        Self {
            mock: MockGateway::new(),
            previewed: std::sync::Mutex::new(None),
            preview_refusal: std::sync::Mutex::new(None),
            sent: std::sync::Mutex::new(None),
        }
    }

    /// Make the next previews refuse with the core-shaped reason, so the
    /// form's refused-preview gate is testable without a live server.
    fn refuse_previews(&self, reason: &str) {
        *self.preview_refusal.lock().expect("record lock") = Some(reason.to_owned());
    }

    fn previewed(&self) -> Option<serde_json::Value> {
        self.previewed.lock().expect("record lock").clone()
    }

    fn sent(&self) -> Option<serde_json::Value> {
        self.sent.lock().expect("record lock").clone()
    }
}

impl TerminalGateway for RecordGateway {
    fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
        self.mock.health()
    }

    fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
        self.mock.proposals()
    }

    fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
        self.mock.portfolio()
    }

    fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
        self.mock.journal()
    }

    fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
        self.mock.chart_bars()
    }

    fn chart_overlays(
        &self,
        kind: &str,
        period: i64,
    ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
        self.mock.chart_overlays(kind, period)
    }

    fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
        self.mock.replay()
    }

    fn preview_arm(&self, patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        *self.previewed.lock().expect("record lock") = Some(patch);
        if let Some(reason) = self.preview_refusal.lock().expect("record lock").clone() {
            return Err(GatewayError::InvalidGrant(reason));
        }
        self.mock.preview_arm(serde_json::Value::Null)
    }

    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        self.mock.grant_tiers()
    }

    fn arm(&self, grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        *self.sent.lock().expect("record lock") = Some(grant);
        Ok(ArmResponse {
            grant_id: "recorded".to_owned(),
            worst_case: protopyne_view::WorstCase {
                max_trades: 1,
                max_contracts: 1,
                max_loss: Money::from(100),
                max_notional: Money::from(1_000),
            },
            armed: true,
        })
    }
}

#[test]
fn form_field_set_fans_out_to_typed_leaves() {
    // One typed editor per leaf, grouped under its tier: the tiers match
    // the descriptor, every leaf carries its kind and format, and money
    // leaves read decimal (never a freeform box).
    let form = GrantFormState::from_schema(&descriptor());
    assert_eq!(
        form.field_paths(),
        descriptor_tier_names(),
        "the form exposes exactly the descriptor's tiers"
    );
    assert!(!form.field_paths().is_empty(), "the descriptor is not empty");
    let mut decimal_leaves = 0;
    for field in form.fields() {
        let path = field.path();
        assert!(
            path == field.tier || path.starts_with(&format!("{}.", field.tier)),
            "leaf {path} belongs to its tier"
        );
        assert!(!field.description.is_empty(), "leaf {path} carries its schema hint");
        if field.format.as_deref() == Some("decimal") {
            decimal_leaves += 1;
            assert_eq!(field.kind, FieldKind::Text, "decimal is text on the wire");
        }
        if field.format.as_deref() == Some("date-time") {
            assert_eq!(field.kind, FieldKind::Text, "date-time is text on the wire");
        }
    }
    assert!(
        decimal_leaves >= 5,
        "every money leaf edits as decimal text, found {decimal_leaves}"
    );
}

/// Non-comment lines of the form module: the no-hand-written-field-list
/// rule is pinned as text — no tier name may appear as a code literal.
/// (`//` lines document but never depend, same rule as the ledger scans.)
#[test]
fn form_module_lists_no_tier_by_name() {
    let text = std::fs::read_to_string(manifest_dir().join("src").join("grant_form.rs"))
        .expect("form module reads");
    let code: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('/'))
        .collect();
    assert!(!code.is_empty(), "form module must exist");
    for tier in descriptor_tier_names() {
        let literal = format!("\"{tier}\"");
        for line in &code {
            assert!(
                !line.contains(&literal),
                "tier {tier} must come from the descriptor, not a literal (line: {line})"
            );
        }
    }
}

#[test]
fn submit_sends_string_tiers_and_mints_no_grant() {
    // A fully drafted tier assembles to boundary-string text: the envelope
    // carries strings, money rides decimal text, and no grant identity
    // rides anywhere.
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    for leaf in tier_leaves(&descriptor_tier_names()[0]) {
        form.set_draft(&leaf, "1".to_owned());
    }
    form.refresh_preview(&gateway);
    assert!(form.is_preview_fresh(), "a drafted tier previews");
    let response = form.submit(&gateway).expect("tightening tier submits");
    assert!(response.armed);

    for document in [gateway.previewed(), gateway.sent()].into_iter().flatten() {
        let patch = document.get("patch").expect("the document is a patch envelope");
        assert!(
            document.get("id").is_none(),
            "the form mints no grant identity, sent: {document}"
        );
        let tiers = patch.as_object().expect("patch is an object");
        assert_eq!(tiers.len(), 1, "only drafted tiers ride the patch, sent: {document}");
        for (tier, text) in tiers {
            assert!(
                text.is_string(),
                "tier {tier} rides boundary-string text, sent: {document}"
            );
        }
    }
}

#[test]
fn money_leaves_ride_decimal_text_never_json_numbers() {
    // Exactness by construction: a money leaf assembles to a JSON string,
    // so the core's codec parses decimal text and no float ever intervenes.
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    let money_leaf = form
        .fields()
        .iter()
        .find(|field| field.format.as_deref() == Some("decimal"))
        .expect("a decimal leaf exists")
        .path();
    let tier = money_leaf.split('.').next().expect("leaf has a tier").to_owned();
    for leaf in tier_leaves(&tier) {
        let draft = if leaf == money_leaf { "100.05" } else { "1" };
        form.set_draft(&leaf, draft.to_owned());
    }
    form.refresh_preview(&gateway);
    form.submit(&gateway).expect("decimal tier submits");
    let sent = gateway.sent().expect("the form sent exactly one document");
    let text = sent["patch"][&tier].as_str().expect("tier rides text");
    let document: serde_json::Value = serde_json::from_str(text).expect("tier text is a document");
    let leaf = money_leaf.split('.').nth(1).expect("leaf has a name");
    assert_eq!(
        document[leaf],
        serde_json::json!("100.05"),
        "money rides decimal text in the tier document"
    );
}

#[test]
fn empty_form_refuses_without_calling_the_gateway() {
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    let refused = form.submit(&gateway).expect_err("an empty patch refuses locally");
    assert!(
        matches!(refused, GrantFormError::Empty),
        "got: {refused:?}"
    );
    assert!(
        gateway.sent().is_none(),
        "a local refusal sends nothing"
    );
}

#[test]
fn malformed_leaf_refuses_by_path_before_send() {
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    let tier = descriptor_tier_names()[0].clone();
    let leaf = tier_leaves(&tier)[0].clone();
    form.set_draft(&leaf, "{not json".to_owned());
    form.refresh_preview(&gateway);
    assert!(
        gateway.previewed().is_none(),
        "a leaf parse failure previews nothing, let alone arms"
    );
    let refused = form.submit(&gateway).expect_err("malformed leaf refuses");
    assert!(
        matches!(&refused, GrantFormError::FieldParse { field, .. } if field == &leaf),
        "the refusal names the leaf, got: {refused:?}"
    );
    assert!(
        gateway.sent().is_none(),
        "a leaf parse failure sends nothing"
    );
}

#[test]
fn partial_tier_refuses_naming_the_missing_leaves() {
    // A tier submits whole or not at all: drafting one leaf of a
    // multi-leaf tier refuses naming the rest, before any gateway call.
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    // A tier with several leaves, found through the descriptor: drafting
    // one leaf of it must name the rest.
    let tier = descriptor_tier_names()
        .into_iter()
        .find(|tier| tier_leaves(tier).len() > 1)
        .expect("a multi-leaf tier exists");
    let leaves = tier_leaves(&tier);
    assert!(leaves.len() > 1, "the tier under test has several leaves");
    form.set_draft(&leaves[0], "1".to_owned());
    form.refresh_preview(&gateway);
    let refused = form.submit(&gateway).expect_err("a partial tier refuses");
    match &refused {
        GrantFormError::PartialTier { tier: refused_tier, missing } => {
            assert_eq!(refused_tier, &tier);
            assert_eq!(missing.len(), leaves.len() - 1, "every undrafted leaf is named");
        }
        other => panic!("a partial tier names its missing leaves, got: {other:?}"),
    }
    assert!(gateway.sent().is_none(), "a partial tier sends nothing");
    assert!(gateway.previewed().is_none(), "a partial tier previews nothing");
}

#[test]
fn submit_without_preview_refuses_stale() {
    // The freshness gate lives in submit, not just the painter: drafts
    // with no preview behind them refuse without a gateway call.
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    for leaf in tier_leaves(&descriptor_tier_names()[0]) {
        form.set_draft(&leaf, "1".to_owned());
    }
    let refused = form.submit(&gateway).expect_err("unpreviewed drafts refuse");
    assert!(
        matches!(refused, GrantFormError::PreviewStale),
        "got: {refused:?}"
    );
    assert!(gateway.sent().is_none(), "an unpreviewed submit sends nothing");
}

#[test]
fn edited_drafts_invalidate_the_preview() {
    // A preview covers exactly the drafts it ran on: editing afterwards
    // makes submit refuse until a fresh preview runs.
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    let leaves = tier_leaves(&descriptor_tier_names()[0]);
    for leaf in &leaves {
        form.set_draft(leaf, "1".to_owned());
    }
    form.refresh_preview(&gateway);
    assert!(form.is_preview_fresh());
    form.set_draft(&leaves[0], "2".to_owned());
    assert!(!form.is_preview_fresh(), "an edit invalidates the preview");
    let refused = form.submit(&gateway).expect_err("edited drafts refuse");
    assert!(matches!(refused, GrantFormError::PreviewStale), "got: {refused:?}");
    form.refresh_preview(&gateway);
    assert!(form.is_preview_fresh(), "re-previewing covers the edit");
    assert_eq!(gateway.previewed().expect("two previews ran")["patch"].as_object().expect("patch").len(), 1);
}

#[test]
fn failed_preview_blocks_submit() {
    // A refused preview is still a preview — but submit needs a
    // *successful* one, so a loosening refuses at the gate with the core's
    // reason on record.
    let gateway = RecordGateway::new();
    gateway.refuse_previews("tightening must not raise any budget");
    let mut form = GrantFormState::from_schema(&descriptor());
    for leaf in tier_leaves(&descriptor_tier_names()[0]) {
        form.set_draft(&leaf, "1".to_owned());
    }
    form.refresh_preview(&gateway);
    assert!(!form.is_preview_fresh(), "a refused preview is never fresh");
    assert!(
        form.preview_outcome().is_some_and(|outcome| outcome.is_err()),
        "the core's refusal is on record for the painter"
    );
    let refused = form.submit(&gateway).expect_err("a refused preview blocks submit");
    assert!(matches!(refused, GrantFormError::PreviewStale), "got: {refused:?}");
    assert!(gateway.sent().is_none(), "a refused preview sends nothing");
}

fn arming_grant() -> Grant {
    Grant::new()
        .id("form-grant".to_owned())
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
                .max_trades(4)
                .max_contracts(8)
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
async fn loosening_submission_surfaces_the_core_refusal() {
    // End to end through a live server: arm a grant, preview a loosening
    // patch through the form, then submit it. The core's tighten refusal
    // must surface (not a local form refusal), and the live grant must be
    // untouched — the UI minted nothing.
    let history = protopyne_server::load_bars_file(
        &manifest_dir().join("../protopyne-server/tests/data/SPY_15M.csv"),
    )
    .expect("vendored history loads");
    let config = protopyne_server::ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = protopyne_server::serve(config, history).await.expect("test server boots");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");

    let mut form = GrantFormState::from_schema(&descriptor());
    form.set_draft("budgets.max_trades", "99".to_owned());
    form.set_draft("budgets.max_contracts", "8".to_owned());
    form.set_draft("budgets.max_loss", "500".to_owned());
    form.set_draft("budgets.max_notional", "5000".to_owned());
    form.refresh_preview(&gateway);
    assert!(!form.is_preview_fresh(), "the loosening previews refused");
    // The painter would leave submit unactionable here; driving submit
    // directly must meet the same gate.
    let refused = form.submit(&gateway).expect_err("loosening refuses");
    assert!(
        matches!(refused, GrantFormError::PreviewStale),
        "an unconfirmed loosening stops at the freshness gate, got: {refused:?}"
    );

    // A tightening preview succeeds and its submit carries the core's
    // answer; a loosening submitted past the gate (as here, bypassed by
    // refreshing over identical drafts is impossible — so prove the core
    // refusal through the gateway directly).
    let loosening = serde_json::json!({"patch": {
        "budgets": r#"{"max_trades":99,"max_contracts":8,"max_loss":"500","max_notional":"5000"}"#,
    }});
    let core_refused = gateway.arm(loosening).expect_err("the core refuses loosening");
    match &core_refused {
        GatewayError::InvalidGrant(reason) => assert!(
            reason.contains("tightening must not raise any budget"),
            "the core's tighten refusal surfaces, got: {reason}"
        ),
        other => panic!("a loosening must surface the core refusal, got: {other:?}"),
    }

    let current: serde_json::Value = client
        .get(format!("{base}/grants/current"))
        .send()
        .await
        .expect("grant read reachable")
        .json()
        .await
        .expect("grant is JSON");
    assert_eq!(
        current.pointer("/budgets/max_trades"),
        Some(&serde_json::json!(4)),
        "the refused loosening minted nothing: the live grant is unchanged"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn submit_past_preview_meets_the_core_when_the_grant_moves() {
    // The Gateway variant's live case: a preview succeeds, the live grant
    // narrows underneath, and the submit then carries the core's refusal —
    // still distinguishable from local validation.
    let history = protopyne_server::load_bars_file(
        &manifest_dir().join("../protopyne-server/tests/data/SPY_15M.csv"),
    )
    .expect("vendored history loads");
    let config = protopyne_server::ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = protopyne_server::serve(config, history).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway
        .arm(serde_json::to_value(arming_grant()).expect("grant serializes"))
        .expect("whole-grant arm succeeds");

    let mut form = GrantFormState::from_schema(&descriptor());
    form.set_draft("budgets.max_trades", "2".to_owned());
    form.set_draft("budgets.max_contracts", "8".to_owned());
    form.set_draft("budgets.max_loss", "500".to_owned());
    form.set_draft("budgets.max_notional", "5000".to_owned());
    form.refresh_preview(&gateway);
    assert!(form.is_preview_fresh(), "the tightening previews against the live grant");

    // The world moves: a narrower whole grant arms (tightening, allowed),
    // and the previewed patch is now a loosening against it.
    let mut narrower = arming_grant();
    narrower.budgets = GrantBudgets::new()
        .max_trades(1)
        .max_contracts(8)
        .max_loss(Money::from(500))
        .max_notional(Money::from(5_000))
        .build();
    gateway
        .arm(serde_json::to_value(narrower).expect("grant serializes"))
        .expect("narrowing whole-grant arm succeeds");

    // The painter would re-preview on the next frame and show the refusal;
    // driving submit directly must surface the core's answer, not a local one.
    let refused = form.submit(&gateway).expect_err("moved grant refuses the submit");
    match &refused {
        GrantFormError::Gateway(GatewayError::InvalidGrant(reason)) => assert!(
            reason.contains("tightening must not raise any budget"),
            "the core's refusal surfaces through submit, got: {reason}"
        ),
        other => panic!("a moved grant surfaces the core refusal, got: {other:?}"),
    }

    handle.shutdown().await;
}
