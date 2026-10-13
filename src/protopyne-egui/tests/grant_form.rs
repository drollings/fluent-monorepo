//! Schema-driven grant arming form, tested from the terminal side.
//!
//! The form renders from the `GrantPatch` descriptor (`field_names` plus
//! `describe`) with no hand-written field list: tier names, hints, and the
//! submitted envelope all flow from the schema the strategy crate derives.
//! The form submits a `{"patch": ...}` envelope through
//! `TerminalGateway::arm` and never mints a grant locally — the core alone
//! validates, tightens, and refuses (409-on-loosening family).

use super::*;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use fluent_wvr::Describable;
use protopyne_strategy::{
    BreakerLimits, Direction, Grant, GrantBudgets, GrantPatch, GrantPermission, GrantScope,
    PortfolioThresholds, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::Money;
use protopyne_view::{ArmResponse, GatewayError, MockGateway, TerminalGateway};

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

/// A recording test double: every read delegates to the mock, while `arm`
/// records the submitted document and answers canned. It proves what the
/// form sends (and whether it sends at all) without involving the core.
struct RecordGateway {
    mock: MockGateway,
    sent: std::sync::Mutex<Option<serde_json::Value>>,
}

impl RecordGateway {
    fn new() -> Self {
        Self { mock: MockGateway::new(), sent: std::sync::Mutex::new(None) }
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
fn form_field_set_equals_the_descriptor() {
    let form = GrantFormState::from_schema(&descriptor());
    assert_eq!(
        form.field_paths(),
        descriptor_tier_names(),
        "the form exposes exactly the descriptor's tiers"
    );
    assert!(!form.field_paths().is_empty(), "the descriptor is not empty");
    for field in form.fields() {
        assert!(
            !field.description.is_empty(),
            "tier {} carries its schema hint",
            field.path
        );
    }
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
fn submit_sends_a_patch_envelope_and_mints_no_grant() {
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    form.set_draft(
        &descriptor_tier_names()[0],
        serde_json::json!({"max_consecutive_errors": 1}).to_string(),
    );
    let response = form.submit(&gateway).expect("tightening tier submits");
    assert!(response.armed);

    let sent = gateway.sent().expect("the form sent exactly one document");
    let patch = sent.get("patch").expect("the document is a patch envelope");
    assert!(
        sent.get("id").is_none(),
        "the form mints no grant identity, sent: {sent}"
    );
    assert_eq!(
        patch.as_object().expect("patch is an object").len(),
        1,
        "only drafted tiers ride the patch, sent: {sent}"
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
fn malformed_tier_refuses_by_name_before_send() {
    let gateway = RecordGateway::new();
    let mut form = GrantFormState::from_schema(&descriptor());
    let tier = descriptor_tier_names()[0].clone();
    form.set_draft(&tier, "{not json".to_owned());
    let refused = form.submit(&gateway).expect_err("malformed tier refuses");
    assert!(
        matches!(&refused, GrantFormError::FieldParse { field, .. } if field == &tier),
        "the refusal names the tier, got: {refused:?}"
    );
    assert!(
        gateway.sent().is_none(),
        "a tier parse failure sends nothing"
    );
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
    // End to end through a live server: arm a grant, then submit a
    // loosening patch through the form. The core's tighten refusal must
    // surface (not a local form refusal), and the live grant must be
    // untouched — the UI minted nothing.
    let history = protopyne_server::load_bars_csv(
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

    // The budgets tier name comes from the descriptor, never a literal:
    // find the tier whose schema hint names budgets.
    let schema = descriptor();
    let properties = schema["properties"].as_object().expect("properties");
    let budgets_tier = properties
        .iter()
        .find(|(_, hint)| {
            hint["description"].as_str().is_some_and(|d| d.contains("budgets"))
        })
        .map(|(name, _)| name.clone())
        .expect("a budgets tier exists");
    let mut form = GrantFormState::from_schema(&schema);
    form.set_draft(
        &budgets_tier,
        serde_json::json!({
            "max_trades": 99,
            "max_contracts": 8,
            "max_loss": "500",
            "max_notional": "5000",
        })
        .to_string(),
    );
    let refused = form.submit(&gateway).expect_err("loosening refuses");
    match &refused {
        GrantFormError::Gateway(GatewayError::InvalidGrant(reason)) => assert!(
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
