//! Backtest result contract over HTTP and the gateway: the schema reads the
//! nested composition plus the registry enumerations; a run stores under a
//! deterministic id and reads back identical bytes through both surfaces; a
//! refused spec names its field; an unknown id fails closed on both sides.

use crate::common_fixtures::spy_history;

use protopyne_strategy::integration_lite::{IntegrationLiteSpec, IntegrationLiteStrategyConfig};
use protopyne_view::{
    BacktestRunParams, BacktestRunView, BacktestSchemaView, GatewayError, TerminalGateway,
};

use crate::service::{serve, ServerConfig, ServerHandle};

async fn boot() -> ServerHandle {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve(config, spy_history())
        .await
        .expect("test server boots")
}

fn params() -> BacktestRunParams {
    let spec = IntegrationLiteSpec::from(&IntegrationLiteStrategyConfig::default());
    BacktestRunParams {
        spec: serde_json::to_value(&spec).expect("spec serializes"),
        seed: 7,
        initial_equity: "100000".to_owned(),
        start: None,
        end: None,
        grant: None,
    }
}

#[tokio::test]
async fn the_schema_serves_the_composition_and_the_registries() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let schema: BacktestSchemaView = client
        .get(format!("{base}/backtest/schema"))
        .send()
        .await
        .expect("schema reachable")
        .json()
        .await
        .expect("schema is JSON");
    for section in ["indicator", "entry", "exit", "risk", "session", "display"] {
        assert!(
            schema.schema.get(section).is_some(),
            "the schema carries the {section} section"
        );
    }
    let triggers = schema.schema["triggers"].as_array().expect("trigger enum");
    for id in [
        "momentum",
        "ribbon_flip",
        "osc_cross",
        "exhaustion",
        "dual_exhaustion",
    ] {
        assert!(
            triggers.iter().any(|entry| entry == id),
            "triggers name {id}"
        );
    }
    let filters = schema.schema["filters"].as_array().expect("filter enum");
    assert!(filters.iter().any(|entry| entry == "ribbon"));
    let sizers = schema.schema["sizers"].as_array().expect("sizer enum");
    assert!(sizers.iter().any(|entry| entry == "equity"));
    let strategies = schema.schema["strategies"]
        .as_array()
        .expect("strategy enum");
    assert!(strategies.iter().any(|entry| entry == "momentum"));
    assert_eq!(
        handle
            .gateway()
            .backtest_schema()
            .expect("gateway schema reads"),
        schema,
        "the gateway and the route serve the same schema bytes"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn a_run_stores_and_reads_back_identical_bytes_on_both_surfaces() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let gateway = handle.gateway();

    let first: protopyne_view::BacktestRunRef = client
        .post(format!("{base}/backtest/run"))
        .json(&params())
        .send()
        .await
        .expect("run reachable")
        .json()
        .await
        .expect("run ref is JSON");
    let view: BacktestRunView = client
        .get(format!("{base}/backtest/{}", first.run_id))
        .send()
        .await
        .expect("state reachable")
        .json()
        .await
        .expect("state is JSON");
    assert_eq!(view.run_id, first.run_id);
    assert_eq!(
        gateway
            .backtest_state(&first.run_id)
            .expect("gateway state reads"),
        view,
        "the gateway and the route serve the same stored bytes"
    );
    assert_eq!(view.equity.len(), spy_history().len());
    assert!(
        view.status.is_some(),
        "the final bar carries its status row"
    );
    assert!(!view.metrics.is_empty());
    assert!(!view.warnings.is_empty(), "limits ride beside the metrics");
    for trade in &view.trades {
        let raw = serde_json::to_value(trade).expect("trade serializes");
        assert!(raw["pnl"].is_string(), "trade P&L rides decimal text");
        assert!(raw["entry_price"].is_string());
    }

    // A repeated submit files under the same id with identical bytes: the
    // run is a pure function of the request, never a second stored copy.
    let second: protopyne_view::BacktestRunRef = client
        .post(format!("{base}/backtest/run"))
        .json(&params())
        .send()
        .await
        .expect("rerun reachable")
        .json()
        .await
        .expect("rerun ref is JSON");
    assert_eq!(second.run_id, first.run_id);
    let again: BacktestRunView = client
        .get(format!("{base}/backtest/{}", second.run_id))
        .send()
        .await
        .expect("state reachable")
        .json()
        .await
        .expect("state is JSON");
    assert_eq!(again, view, "a rerun reads back byte-identical");
    handle.shutdown().await;
}

#[tokio::test]
async fn a_refused_spec_names_the_field_and_an_unknown_id_fails_closed() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let mut bad = params();
    bad.spec = serde_json::json!({});
    let refused = client
        .post(format!("{base}/backtest/run"))
        .json(&bad)
        .send()
        .await
        .expect("refusal reachable");
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = refused.json().await.expect("refusal is JSON");
    let reason = body["error"].as_str().expect("refusal names its cause");
    assert!(
        reason.contains("backtest spec refused"),
        "refusal names the surface"
    );
    assert!(
        reason.contains("indicator"),
        "refusal names the missing field, got: {reason}"
    );

    let missing = client
        .get(format!("{base}/backtest/no-such-run"))
        .send()
        .await
        .expect("unknown id reachable");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    let gateway = handle.gateway();
    assert!(matches!(
        gateway.backtest_state("no-such-run"),
        Err(GatewayError::Unavailable(_))
    ));
    assert!(matches!(
        gateway.backtest_stop("no-such-run"),
        Err(GatewayError::Unavailable(_))
    ));
    handle.shutdown().await;
}
