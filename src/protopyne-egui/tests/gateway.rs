//! Composition-root parity from the terminal side.
//!
//! The terminal renders over `&dyn TerminalGateway`, never over the trading
//! path: [`MockGateway`](protopyne_view::MockGateway) covers every trait
//! method headlessly, and a CSV replayed through
//! [`ServerGateway`](protopyne_server::ServerGateway) matches the same reads
//! through the HTTP routes on a loopback server. The ledger test pins the
//! direction this composition relies on: render modules under `src/` name
//! the gateway abstraction, never the trading concretion — the server enters
//! only as an optional dependency for the composition-root example outside
//! `src/`.

use std::path::PathBuf;

use protopyne_view::{MockGateway, TerminalGateway};

use crate::{ReplayScrub, ScrubOutcome};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The server owns the single trimmed history fixture; the terminal reads it
/// at dev time only (never ships it, never a main dependency), so parity
/// runs against the same bars the server's own suites pin.
fn history_path() -> PathBuf {
    manifest_dir().join("../protopyne-server/tests/data/SPY_15M.csv")
}

#[test]
fn mock_gateway_covers_every_trait_method() {
    let gateway = MockGateway::new();
    let gateway: &dyn TerminalGateway = &gateway;

    let health = gateway.health().expect("mock health reads");
    assert_eq!(health.status, "ok");
    assert_eq!(health.trading_state, "Enabled");
    assert!(health.allows_entries && health.allows_exits);

    let proposals = gateway.proposals().expect("mock proposals read");
    assert_eq!(proposals.len(), 1, "mock inbox carries one canned row");
    assert_eq!(proposals[0].id, "mock-prop-1");
    assert_eq!(proposals[0].state, "Proposed");

    let portfolio = gateway.portfolio().expect("mock portfolio reads");
    assert_eq!(portfolio.positions.len(), 1);
    assert_eq!(portfolio.positions[0].underlying, "MOCK");
    assert_eq!(portfolio.filled_orders, 1);

    let journal = gateway.journal().expect("mock journal reads");
    assert_eq!(journal.len(), 2, "mock journal carries intent then fill");
    assert_eq!(journal[0].seq, 1);
    assert_eq!(journal[1].seq, 2);

    let bars = gateway.chart_bars().expect("mock bars read");
    assert_eq!(bars.rows.len(), 3);
    assert_eq!(bars.dropped, 0);

    let overlay = gateway
        .chart_overlays("sma", 20)
        .expect("mock overlay reads");
    assert_eq!(overlay.kind, "sma");
    assert_eq!(overlay.period, 20);
    assert_eq!(overlay.values.len(), bars.rows.len());

    let armed = gateway
        .arm(serde_json::json!({"grant_id": "g1"}))
        .expect("mock arm accepts a named grant");
    assert!(armed.armed);
    assert_eq!(armed.grant_id, "g1");
    assert!(
        gateway.arm(serde_json::json!({})).is_err(),
        "mock arm refuses a grant with no id before any core could see it"
    );

    assert!(
        gateway.attest_grant("g1").is_err(),
        "the mock refuses attestation honestly: no journal behind it"
    );

    let replay = gateway.replay().expect("mock replay reads");
    assert_eq!(
        replay.total_bars,
        replay.steps.len() as u64,
        "the mock replay window spans its own steps"
    );
    assert_eq!(
        replay.cursor, replay.total_bars,
        "a fresh read stands at the head"
    );
    assert!(
        replay.steps.iter().any(|step| step.checkpoint),
        "the mock window carries anchors to scrub to"
    );
}

#[tokio::test]
async fn server_gateway_matches_http_routes() {
    let history =
        protopyne_server::load_bars_file(&history_path()).expect("vendored history loads");
    let config = protopyne_server::ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .build();
    let handle = protopyne_server::serve(config, history)
        .await
        .expect("test server boots");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();
    let gateway: &dyn TerminalGateway = &gateway;

    async fn route<T>(client: &reqwest::Client, url: String) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        client
            .get(url)
            .send()
            .await
            .expect("route reachable")
            .json()
            .await
            .expect("route is JSON")
    }

    let health: protopyne_view::HealthView = route(&client, format!("{base}/health")).await;
    assert_eq!(gateway.health().expect("gateway health reads"), health);

    let proposals: Vec<protopyne_view::ProposalView> =
        route(&client, format!("{base}/proposals")).await;
    assert_eq!(
        gateway.proposals().expect("gateway proposals read"),
        proposals
    );

    let portfolio: protopyne_view::PortfolioView =
        route(&client, format!("{base}/portfolio")).await;
    assert_eq!(
        gateway.portfolio().expect("gateway portfolio reads"),
        portfolio
    );

    // The journal route answers now: the same rows through the gateway and
    // through the route.
    let journal: Vec<protopyne_view::JournalRowView> =
        route(&client, format!("{base}/journal")).await;
    assert_eq!(gateway.journal().expect("gateway journal reads"), journal);

    let bars: protopyne_view::ChartBars = route(&client, format!("{base}/chart/bars")).await;
    assert_eq!(gateway.chart_bars().expect("gateway bars read"), bars);

    let overlay: protopyne_view::OverlaySeries =
        route(&client, format!("{base}/chart/overlays?kind=ema&period=10")).await;
    assert_eq!(
        gateway
            .chart_overlays("ema", 10)
            .expect("gateway overlay reads"),
        overlay
    );

    // Replay parity: the gateway reads the route's default cadence, so the
    // terminal's scrub sees the same window through either side.
    let replay: protopyne_view::ReplayView = route(&client, format!("{base}/replay")).await;
    assert_eq!(gateway.replay().expect("gateway replay reads"), replay);
    assert!(
        replay.steps.iter().any(|step| step.checkpoint),
        "the published window carries the core's own anchors to scrub to"
    );

    // Backtest parity: the schema, the stored run, and the state read read
    // the same bytes through the gateway and through the route.
    let schema: protopyne_view::BacktestSchemaView =
        route(&client, format!("{base}/backtest/schema")).await;
    assert_eq!(
        gateway.backtest_schema().expect("gateway schema reads"),
        schema
    );
    let spec = protopyne_strategy::integration_lite::IntegrationLiteSpec::from(
        &protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig::default(),
    );
    let run: protopyne_view::BacktestRunRef = client
        .post(format!("{base}/backtest/run"))
        .json(&protopyne_view::BacktestRunParams {
            spec: serde_json::to_value(&spec).expect("spec serializes"),
            seed: 7,
            initial_equity: "100000".to_owned(),
            start: None,
            end: None,
            grant: None,
        })
        .send()
        .await
        .expect("run reachable")
        .json()
        .await
        .expect("run ref is JSON");
    let state: protopyne_view::BacktestRunView =
        route(&client, format!("{base}/backtest/{}", run.run_id)).await;
    assert_eq!(
        gateway
            .backtest_state(&run.run_id)
            .expect("gateway state reads"),
        state
    );

    handle.shutdown().await;
}

/// The terminal's scrub consumes the gateway's replay read end to end: the
/// window supplies the bars, the cursor lands on one of them, and the panes
/// the move makes due come from the terminal's own topology. The gateway
/// stays a read — a scrub never writes back.
#[tokio::test]
async fn a_scrub_over_the_server_window_moves_panes_and_writes_nothing() {
    let history =
        protopyne_server::load_bars_file(&history_path()).expect("vendored history loads");
    let config = protopyne_server::ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .build();
    let handle = protopyne_server::serve(config, history)
        .await
        .expect("test server boots");
    let gateway = handle.gateway();
    let gateway: &dyn TerminalGateway = &gateway;

    let mut scrub = ReplayScrub::standard().expect("standard scrub builds");
    scrub
        .ingest(gateway)
        .expect("the server supplies a replay window");
    let window = gateway.replay().expect("gateway replay reads");
    assert_eq!(
        scrub.state().total_bars,
        window.total_bars,
        "the scrub's window is the core's window"
    );

    let bar = window.total_bars / 2;
    let outcome = scrub.scrub_to(bar).expect("a bar inside the window scrubs");
    assert!(
        matches!(outcome, ScrubOutcome::Rewound { cursor, .. } if cursor == bar),
        "the scrub moved to bar {bar}, got: {outcome:?}"
    );
    assert_eq!(scrub.state().cursor, bar);
    assert!(!scrub.settled(), "the move made the topology due");

    // Read-only on both sides: the same window comes back, cursor at head,
    // because a terminal scrub is the terminal's own state.
    let after = gateway.replay().expect("gateway replay reads");
    assert_eq!(after, window, "a scrub never writes through the gateway");

    handle.shutdown().await;
}

/// The two ledgers enforce one rule, so they hold one copy of it: this file
/// must not re-declare the trading token list or the source walk. A second
/// copy is a rule that agrees only until someone edits one of them, so the
/// duplication is itself gated rather than merely discouraged.
#[test]
fn composition_root_ledger_delegates_to_the_shared_scan() {
    let text = std::fs::read_to_string(manifest_dir().join("tests").join("gateway.rs"))
        .expect("the composition-root ledger is readable");
    let code: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('/'))
        .collect();
    for token in super::budget_tests::TRADING_TOKENS {
        assert!(
            !code
                .iter()
                .any(|line| line.contains(&format!("\"{token}\""))),
            "the trading token list lives in the terminal ledger only; {token:?} \
             must not be restated here"
        );
    }
    for helper in ["fn code_lines", "fn collect_rs", "fn src_file_names"] {
        assert!(
            !code.iter().any(|line| line.starts_with(helper)),
            "{helper} is implemented once, in the terminal ledger"
        );
    }
    assert!(
        code.iter()
            .any(|line| line.contains("forbid_trading_tokens_in_src")),
        "this file must call the shared scan"
    );
}

#[test]
fn render_modules_name_no_trading_token() {
    // Composition-root direction pin: the server becomes an optional
    // dependency, but only the example outside `src/` may name it. Render
    // modules keep reading through `protopyne_view` (the abstraction); the
    // trading concretion never enters `src/`.
    //
    // The scan itself lives in the terminal ledger: the token list and the
    // walk shape are the same rule, so keeping a second copy here would mean
    // two rules that agree only until someone edits one of them.
    super::budget_tests::forbid_trading_tokens_in_src();
}

#[test]
fn an_attest_action_invokes_exactly_the_attest_verb() {
    use crate::grant_form::GrantAction;

    let action = GrantAction::attest("grant-9");
    // The mock's honest refusal names attestation, proving the action routes
    // to `attest_grant` and not to some other verb that also refuses.
    let refused = action
        .apply(&MockGateway::new())
        .expect_err("mock has no journal to attest through");
    assert!(
        refused.to_string().contains("attest"),
        "the refusal names the verb: {refused}"
    );
}
