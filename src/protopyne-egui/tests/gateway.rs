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

use std::path::{Path, PathBuf};

use protopyne_view::{MockGateway, TerminalGateway};

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

    let overlay = gateway.chart_overlays("sma", 20).expect("mock overlay reads");
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
}

#[tokio::test]
async fn server_gateway_matches_http_routes() {
    let history =
        protopyne_server::load_bars_csv(&history_path()).expect("vendored history loads");
    let config =
        protopyne_server::ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = protopyne_server::serve(config, history).await.expect("test server boots");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();
    let gateway: &dyn TerminalGateway = &gateway;

    async fn route<T>(client: &reqwest::Client, url: String) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        client.get(url).send().await.expect("route reachable").json().await.expect("route is JSON")
    }

    let health: protopyne_view::HealthView = route(&client, format!("{base}/health")).await;
    assert_eq!(gateway.health().expect("gateway health reads"), health);

    let proposals: Vec<protopyne_view::ProposalView> =
        route(&client, format!("{base}/proposals")).await;
    assert_eq!(gateway.proposals().expect("gateway proposals read"), proposals);

    let portfolio: protopyne_view::PortfolioView =
        route(&client, format!("{base}/portfolio")).await;
    assert_eq!(gateway.portfolio().expect("gateway portfolio reads"), portfolio);

    // No journal route exists yet: both sides read empty until it lands.
    assert!(
        gateway.journal().expect("gateway journal reads").is_empty(),
        "gateway and (missing) route agree on the empty journal"
    );

    let bars: protopyne_view::ChartBars = route(&client, format!("{base}/chart/bars")).await;
    assert_eq!(gateway.chart_bars().expect("gateway bars read"), bars);

    let overlay: protopyne_view::OverlaySeries =
        route(&client, format!("{base}/chart/overlays?kind=ema&period=10")).await;
    assert_eq!(gateway.chart_overlays("ema", 10).expect("gateway overlay reads"), overlay);

    handle.shutdown().await;
}

/// Non-comment lines: `//`, `//!`, and `///` lines document but never
/// depend, so the direction scan skips them (same rule as the budget ledger).
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('/'))
        .collect()
}

fn collect_rs(dir: &Path, prefix: &Path, out: &mut Vec<String>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display()));
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_rs(&path, &rel, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn render_modules_name_no_trading_token() {
    // Composition-root direction pin: the server becomes an optional
    // dependency in this milestone, but only the example outside `src/` may
    // name it. Render modules keep reading through `protopyne_view` (the
    // abstraction); the trading concretion never enters `src/`.
    let src = manifest_dir().join("src");
    let mut names = Vec::new();
    collect_rs(&src, Path::new(""), &mut names);
    assert!(!names.is_empty(), "src/ scan must find the render modules");
    for name in &names {
        let text = std::fs::read_to_string(src.join(name))
            .unwrap_or_else(|e| panic!("read src/{name}: {e}"));
        for line in code_lines(&text) {
            for token in [
                "protopyne_server",
                "protopyne_strategy",
                "protopyne_venue",
                "protopyne-server",
                "protopyne-strategy",
                "protopyne-venue",
            ] {
                assert!(
                    !line.contains(token),
                    "src/{name} must not name {token:?} (line: {line})"
                );
            }
        }
    }
}
