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
    assert!(
        journal.windows(2).all(|pair| pair[0].seq < pair[1].seq),
        "journal reads in sequence order"
    );
    let bars = gateway.chart_bars().expect("mock bars read");
    assert_eq!(bars.rows.len(), 3, "mock chart holds three bars");
    let overlay = gateway
        .chart_overlays("sma", 14)
        .expect("mock overlay reads");
    assert_eq!(overlay.kind, "sma");
    assert_eq!(overlay.period, 14);
    assert_eq!(
        overlay.values.len(),
        bars.rows.len(),
        "overlay agrees with bars positionally"
    );
    let replay = gateway.replay().expect("mock replay reads");
    assert_eq!(
        replay.steps.len(),
        bars.rows.len(),
        "replay window matches the chart window"
    );
    assert_eq!(replay.total_bars, replay.steps.len() as u64);
    assert_eq!(
        replay.cursor, replay.total_bars,
        "a fresh read stands at the head"
    );
    assert!(
        replay
            .steps
            .iter()
            .enumerate()
            .all(|(position, step)| { step.bar_index == position as u64 && step.done }),
        "steps are in bar order and every one was reached"
    );
    assert_eq!(
        replay.steps.iter().filter(|step| step.checkpoint).count(),
        1,
        "the mock publishes one named anchor on bar 0, matching the core's short-history cadence"
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
    assert_eq!(
        error,
        GatewayError::InvalidGrant("grant document names no grant_id".to_owned())
    );
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

#[test]
fn symbol_info_view_round_trips_through_serde() {
    let entry = SymbolInfoView {
        symbol: "GLD".to_owned(),
        name: "SPDR Gold Shares".to_owned(),
        asset_class: "Funds".to_owned(),
    };
    let text = serde_json::to_string(&entry).expect("symbol info serializes");
    let back: SymbolInfoView = serde_json::from_str(&text).expect("symbol info deserializes");
    assert_eq!(back, entry, "the catalog row round-trips byte-for-byte");
}

#[test]
fn mock_gateway_lists_gld_first_across_every_asset_class() {
    let gateway = MockGateway::new();
    let symbols = gateway.symbols().expect("mock symbols read");
    assert_eq!(
        symbols[0].symbol, "GLD",
        "the terminal's default ticker heads the catalog"
    );
    let classes: std::collections::BTreeSet<&str> = symbols
        .iter()
        .map(|entry| entry.asset_class.as_str())
        .collect();
    assert_eq!(
        classes,
        std::collections::BTreeSet::from(["Crypto", "Funds", "Futures", "Stocks"]),
        "every asset class the Symbol Selection Modal tab-strips on is represented"
    );
    assert!(
        symbols
            .iter()
            .all(|entry| !entry.symbol.is_empty() && !entry.name.is_empty()),
        "no catalog row ships a blank ticker or name"
    );
}

#[test]
fn mock_gateway_symbols_derive_from_the_shared_catalog() {
    let gateway = MockGateway::new();
    let symbols = gateway.symbols().expect("mock symbols read");
    assert_eq!(
        symbols,
        crate::symbol_catalog(),
        "the mock serves the shared catalog, never its own list"
    );
    let tickers: Vec<&str> = symbols.iter().map(|entry| entry.symbol.as_str()).collect();
    assert_eq!(
        tickers,
        vec!["GLD", "SPY", "QQQ", "BTC-USD", "ES"],
        "the shared catalog pins its contents"
    );
}

#[test]
fn mock_gateway_serves_bars_and_overlays_for_symbol_and_timeframe() {
    let gateway = MockGateway::new();
    let bars = gateway
        .chart_bars_for("GLD", "15M")
        .expect("symbol-scoped bars read");
    assert_eq!(
        bars,
        gateway.chart_bars().expect("canned bars read"),
        "the mock's default symbol/timeframe slot is the canned slot"
    );
    let overlay = gateway
        .chart_overlays_for("GLD", "15M", "ema", 10)
        .expect("symbol-scoped overlay reads");
    assert_eq!(
        overlay,
        gateway
            .chart_overlays("ema", 10)
            .expect("canned overlay reads"),
        "the mock's default symbol/timeframe slot is the canned slot"
    );
    assert_eq!(
        overlay.values.len(),
        bars.rows.len(),
        "overlay agrees with bars positionally"
    );
}

#[test]
fn mock_gateway_limit_reads_the_trailing_bars() {
    let gateway = MockGateway::new();
    let full = gateway.chart_bars().expect("canned bars read");
    let unbounded = gateway
        .chart_bars_for_limit("GLD", "15M", None)
        .expect("unbounded limit reads");
    assert_eq!(
        unbounded.rows, full.rows,
        "None serves the canned slot untouched"
    );
    assert_eq!(
        unbounded.dropped, full.dropped,
        "an unbound read drops nothing"
    );
    let oversize = gateway
        .chart_bars_for_limit("GLD", "15M", Some(99))
        .expect("oversize limit reads");
    assert_eq!(
        oversize.rows, full.rows,
        "an oversize limit serves the canned slot untouched"
    );
    let tail = gateway
        .chart_bars_for_limit("GLD", "15M", Some(2))
        .expect("bounded limit reads");
    assert_eq!(
        tail.rows,
        full.rows[1..],
        "limit 2 serves the last two bars"
    );
    assert_eq!(
        tail.dropped, full.dropped,
        "a read bound never counts as dropped"
    );
    let last = gateway
        .chart_bars_for_limit("GLD", "15M", Some(1))
        .expect("limit 1 reads");
    assert_eq!(last.rows, full.rows[2..], "limit 1 serves the newest bar");
}

#[test]
fn mock_gateway_serves_canned_tickets_and_refuses_writes() {
    // Reads are canned like every other mock read; writes stay
    // default-refused — the mock holds no journal, so an issue through it
    // refuses instead of pretending to track.
    let gateway = MockGateway::new();
    let waiting = gateway.tickets_waiting().expect("mock serves waiting");
    assert_eq!(waiting.len(), 1, "mock holds one canned ticket");
    assert_eq!(waiting[0].ticket_id, "mock-ticket-1");
    assert_eq!(waiting[0].underlying, "MOCK");
    assert!(
        waiting[0].rendered.contains("mock-ticket-1"),
        "the canned ticket renders its id: {}",
        waiting[0].rendered
    );
    let drifts = gateway.ticket_drifts().expect("mock serves drifts");
    assert!(drifts.is_empty(), "the canned book agrees with itself");
    let issue = crate::TicketDraft {
        side: crate::TicketSide::Buy,
        quantity: 2,
        limit_price: "600.50".to_owned(),
        symbol: "SPY".to_owned(),
    };
    assert!(
        matches!(
            gateway.issue_ticket(issue),
            Err(GatewayError::Unavailable(_))
        ),
        "an issue with no journal behind it refuses"
    );
    assert!(
        matches!(
            gateway.confirm_ticket("mock-ticket-1", serde_json::json!({})),
            Err(GatewayError::Unavailable(_))
        ),
        "a confirm with no journal behind it refuses"
    );
    assert!(
        matches!(
            gateway.import_tickets(serde_json::json!([])),
            Err(GatewayError::Unavailable(_))
        ),
        "an import with no journal behind it refuses"
    );
}

#[test]
fn symbol_scoped_reads_refuse_blank_identifiers() {
    let gateway = MockGateway::new();
    assert!(
        matches!(
            gateway.chart_bars_for("", "15M"),
            Err(GatewayError::Malformed(_))
        ),
        "a blank symbol refuses as Malformed"
    );
    assert!(
        matches!(
            gateway.chart_bars_for("GLD", "   "),
            Err(GatewayError::Malformed(_))
        ),
        "a whitespace-only timeframe refuses as Malformed"
    );
    assert!(
        matches!(
            gateway.chart_overlays_for("GLD", "", "ema", 10),
            Err(GatewayError::Malformed(_))
        ),
        "a blank timeframe refuses on the overlay read too"
    );
}

#[test]
fn interactive_replay_defaults_refuse_without_a_session_registry() {
    // The mock serves no registry: every interactive verb refuses as
    // Unavailable, so a terminal against the mock shows a refusal line,
    // never an empty session.
    let gateway = MockGateway::new();
    let params = ReplayStartParams {
        session_id: "terminal".to_owned(),
        underlying: "SPY".to_owned(),
        strategy_name: "ribbon".to_owned(),
        max_loss: Money::from(400),
        notional: Money::from(2_000),
        dte_days: 30,
        warmup_bars: 0,
        seed: 7,
        start_bar: 0,
        structure: "Single".to_owned(),
        grant: None,
    };
    for outcome in [
        gateway.replay_start(params).map(|_| ()),
        gateway.replay_step("terminal", 1).map(|_| ()),
        gateway.replay_state("terminal").map(|_| ()),
        gateway.replay_stop("terminal"),
    ] {
        assert_eq!(
            outcome,
            Err(GatewayError::Unavailable(
                "interactive replay is not served by this gateway".to_owned()
            )),
            "a registry-less gateway refuses every interactive verb"
        );
    }
}

#[test]
fn command_plane_verbs_refuse_without_a_journal() {
    // The mock serves no journal-backed command plane: the new decision and
    // revoke verbs refuse as Unavailable, so a terminal against the mock
    // shows a refusal line, never a silent success.
    let gateway = MockGateway::new();
    for outcome in [
        gateway.approve("mock-prop-1").is_err(),
        gateway.reject("mock-prop-1", "too much size").is_err(),
        gateway.revoke_grant("mock-grant").is_err(),
        gateway.close_all("paper").is_err(),
        gateway.cancel_all("paper").is_err(),
    ] {
        assert!(
            outcome,
            "a command-plane-less gateway refuses every decision verb"
        );
    }
}

#[test]
fn booked_portfolio_read_refuses_without_a_venue_book() {
    // The mock holds no venue book: the booked read refuses as Unavailable,
    // so a terminal falls back to the gateway's own portfolio read.
    let gateway = MockGateway::new();
    assert!(
        matches!(
            gateway.portfolio_booked(),
            Err(GatewayError::Unavailable(_))
        ),
        "a book-less gateway refuses the booked portfolio read"
    );
}

#[test]
fn backtest_verbs_refuse_without_a_stored_registry() {
    let gateway = MockGateway::new();
    assert!(
        matches!(gateway.backtest_schema(), Err(GatewayError::Unavailable(_))),
        "no stored schema without a serving gateway"
    );
    let params = super::BacktestRunParams {
        spec: serde_json::json!({}),
        seed: 7,
        initial_equity: "100000".to_owned(),
        start: None,
        end: None,
        grant: None,
    };
    assert!(
        matches!(
            gateway.backtest_run(params),
            Err(GatewayError::Unavailable(_))
        ),
        "no run without a serving gateway"
    );
    assert!(
        matches!(
            gateway.backtest_state("backtest-00"),
            Err(GatewayError::Unavailable(_))
        ),
        "no stored state without a serving gateway"
    );
    assert!(
        matches!(
            gateway.backtest_stop("backtest-00"),
            Err(GatewayError::Unavailable(_))
        ),
        "no stored slot without a serving gateway"
    );
}
