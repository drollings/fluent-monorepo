//! Interactive replay controls: start resolution, prefix truncation, and the
//! gateway-driven open/step/stop wiring.
//!
//! Every assertion reads headless state (no paint): the truncation helpers
//! the canvas reads, the recompute the steps drive, and the cached session
//! the dock panes render.

use super::*;

use std::sync::Mutex;

use protopyne_chart_model::ChartRow;
use protopyne_view::{
    GatewayError, RehearsalEconomics, ReplayDecisionView, ReplayPositionView, ReplayStartParams,
    ReplayStateView, StructureKindHint,
};

/// The economics a scripted gateway serves: a fixed booking the terminal
/// asserts against, never a value the terminal could have invented.
fn rehearsal_for_test() -> RehearsalEconomics {
    RehearsalEconomics {
        strategy: "ribbon".to_owned(),
        max_loss: "400".to_owned(),
        notional: "2000".to_owned(),
        dte_days: 30,
        grant_id: "replay-permissive".to_owned(),
    }
}

fn ladder_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|index| {
            let close = 100.0 + index as f64;
            ChartRow {
                time: 1_700_000_000 + index as i64 * 900,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000,
            }
        })
        .collect()
}

fn idle_state(cursor: u64, total: u64) -> ReplayStateView {
    ReplayStateView {
        session_id: "terminal".to_owned(),
        active: true,
        cursor,
        total_bars: total,
        decisions: Vec::new(),
        positions: Vec::new(),
        filled_orders: 0,
        total_fees: Money::from(0),
        journal: Vec::new(),
    }
}

fn seat_live(workspace: &mut ChartWorkspace, cursor: usize) {
    let total = workspace.rows.len() as u64;
    workspace.live_replay = Some(LiveReplay::new(
        "terminal".to_owned(),
        idle_state(cursor as u64, total),
    ));
}

/// A script replay gateway: opens at the requested start bar over a fixed
/// window, steps the cursor, and records every verb. With no history it
/// serves no chart or replay time axis (the legacy identity mapping); with
/// history it serves the full series and a matching replay time axis, so the
/// terminal maps display indices to server indices by timestamp.
struct ScriptReplay {
    total: u64,
    cursor: Mutex<u64>,
    calls: Mutex<Vec<String>>,
    structures: Mutex<Vec<String>>,
    history: Vec<ChartRow>,
    /// Core-side journal: steps append, terminal verbs never rewrite.
    journal: Mutex<Vec<protopyne_view::JournalRowView>>,
    /// Refuse the rehearsal-economics read, for the fail-closed pin.
    refuse_economics: bool,
}

impl ScriptReplay {
    /// A script gateway over `total` ladder bars: chart reads serve the
    /// fixture series and the replay time axis stamps every bar, so opens
    /// map display indices to server indices by timestamp.
    fn new(total: usize) -> Self {
        Self::with_history(ladder_rows(total))
    }

    /// A script gateway over a full server history: chart reads serve the
    /// trailing take of it, and the replay time axis stamps every bar.
    fn with_history(history: Vec<ChartRow>) -> Self {
        let total = history.len() as u64;
        Self {
            total,
            cursor: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
            structures: Mutex::new(Vec::new()),
            history,
            journal: Mutex::new(Vec::new()),
            refuse_economics: false,
        }
    }

    /// The same gateway with the economics read refused.
    fn refusing_economics(mut self) -> Self {
        self.refuse_economics = true;
        self
    }

    fn state_of(&self, session: &str) -> ReplayStateView {
        idle_state(*self.cursor.lock().expect("cursor locks"), self.total).with_session(session)
    }

    fn bars_of(rows: &[ChartRow]) -> protopyne_view::ChartBars {
        protopyne_view::ChartBars {
            rows: rows
                .iter()
                .map(|row| protopyne_view::ChartBar {
                    time: row.time,
                    open: row.open,
                    high: row.high,
                    low: row.low,
                    close: row.close,
                    volume: row.volume,
                })
                .collect(),
            dropped: 0,
        }
    }

    fn stamp_of(time: i64) -> String {
        chrono::DateTime::from_timestamp(time, 0)
            .expect("fixture stamps are valid")
            .to_rfc3339()
    }
}

trait WithSession {
    fn with_session(self, session: &str) -> Self;
}

impl WithSession for ReplayStateView {
    fn with_session(mut self, session: &str) -> Self {
        self.session_id = session.to_owned();
        self
    }
}

impl TerminalGateway for ScriptReplay {
    fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
        Ok(self.journal.lock().expect("journal locks").clone())
    }

    fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
        if self.history.is_empty() {
            return Err(GatewayError::Unavailable("script replay".to_owned()));
        }
        Ok(Self::bars_of(&self.history))
    }

    fn chart_bars_for_limit(
        &self,
        symbol: &str,
        timeframe: &str,
        limit: Option<usize>,
    ) -> Result<protopyne_view::ChartBars, GatewayError> {
        protopyne_view::validate_symbol_timeframe(symbol, timeframe)?;
        if self.history.is_empty() {
            return Err(GatewayError::Unavailable("script replay".to_owned()));
        }
        let take = limit.unwrap_or(self.history.len()).min(self.history.len());
        Ok(Self::bars_of(&self.history[self.history.len() - take..]))
    }

    fn chart_overlays(
        &self,
        _kind: &str,
        _period: i64,
    ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
        if self.history.is_empty() {
            return Err(GatewayError::Unavailable("script replay".to_owned()));
        }
        let steps = self
            .history
            .iter()
            .enumerate()
            .map(|(bar_index, row)| protopyne_view::ReplayStepView {
                bar_index: bar_index as u64,
                ts: Self::stamp_of(row.time),
                done: true,
                checkpoint: bar_index == 0,
            })
            .collect::<Vec<_>>();
        let total_bars = steps.len() as u64;
        Ok(protopyne_view::ReplayView {
            cursor: total_bars,
            total_bars,
            steps,
        })
    }

    fn preview_arm(
        &self,
        _patch: serde_json::Value,
    ) -> Result<protopyne_view::WorstCase, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn arm(&self, _grant: serde_json::Value) -> Result<protopyne_view::ArmResponse, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn rehearsal_economics(
        &self,
        _underlying: &str,
        _structure: StructureKindHint,
    ) -> Result<RehearsalEconomics, GatewayError> {
        if self.refuse_economics {
            return Err(GatewayError::Unavailable(
                "economics are not served".to_owned(),
            ));
        }
        Ok(rehearsal_for_test())
    }

    fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push(format!("start:{}", params.start_bar));
        self.structures
            .lock()
            .expect("structure log locks")
            .push(params.structure.clone());
        if params.start_bar > self.total {
            return Err(GatewayError::Malformed("start past the end".to_owned()));
        }
        *self.cursor.lock().expect("cursor locks") = params.start_bar;
        Ok(self.state_of(&params.session_id))
    }

    fn replay_step(&self, session_id: &str, bars: u64) -> Result<ReplayStateView, GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push(format!("step:{bars}"));
        let mut cursor = self.cursor.lock().expect("cursor locks");
        let from = *cursor;
        *cursor = (*cursor + bars).min(self.total);
        let stepped = *cursor - from;
        drop(cursor);
        // Core-side booking appends one journal row per decided bar; the
        // sequence never restarts, so a terminal verb can only extend the
        // log, never rewrite it.
        let mut journal = self.journal.lock().expect("journal locks");
        for bar in from..from + stepped {
            let at_ms = self
                .history
                .get(bar as usize)
                .map_or(0, |row| row.time * 1_000);
            let seq = journal.len() as i64 + 1;
            journal.push(protopyne_view::JournalRowView {
                seq,
                client_id: session_id.to_owned(),
                kind: "filled".to_owned(),
                detail: format!("{{\"bar\":{bar}}}"),
                at_ms,
            });
        }
        drop(journal);
        Ok(self.state_of(session_id))
    }

    fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
        Ok(self.state_of(session_id))
    }

    fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push(format!("stop:{session_id}"));
        Ok(())
    }
}

#[test]
fn capture_visible_replay_window_uses_the_displayed_bars() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    assert_eq!(
        workspace.replay_epochs().1,
        None,
        "no capture is the full loaded history"
    );
    assert_eq!(workspace.replay_range_label(), "Full");
    // The window is the chart's own visible bars at the moment of capture:
    // stored as epochs, resolved to indices for the divider markers.
    workspace
        .capture_visible_replay_window()
        .expect("a bound chart captures its visible window");
    let main = workspace.main_viewport().expect("a viewport binds");
    let width = crate::workspace::PANE_WIDTH_PX;
    let (start, end) = main.visible_bar_range(width, 40);
    assert!(start < end, "the seeded chart shows bars");
    let rows = ladder_rows(40);
    assert_eq!(
        workspace.replay_range,
        Some((rows[start].time, rows[end - 1].time)),
        "a captured window stores its edge epochs"
    );
    assert_eq!(
        workspace.replay_marker_indices(),
        Some((start, end - 1)),
        "the divider markers ride the captured edges"
    );
    assert!(
        workspace.replay_range_label().contains('→'),
        "a captured window labels its dates"
    );
    // Panning the display moves the markers with the time axis, never the
    // window: the epochs stay fixed while the view roams.
    let before = workspace.replay_range;
    workspace.rewind_view().expect("the view pans");
    assert_eq!(
        workspace.replay_range, before,
        "panning never moves the captured window"
    );
    assert_eq!(
        workspace.replay_marker_indices(),
        Some((start, end - 1)),
        "the markers keep their time setting after a pan"
    );
}

#[test]
fn replay_prefix_truncates_the_view_never_the_store() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    assert_eq!(
        workspace.replay_prefix_len(),
        None,
        "no run is the live chart"
    );
    assert_eq!(workspace.replay_view_rows(&workspace.rows).len(), 40);
    seat_live(&mut workspace, 12);
    assert_eq!(workspace.replay_prefix_len(), Some(12));
    let view = workspace.replay_view_rows(&workspace.rows);
    assert_eq!(view.len(), 12, "painters read the decided prefix only");
    assert_eq!(view.last().map(|row| row.close), Some(111.0));
    assert_eq!(workspace.rows.len(), 40, "the store keeps the full history");
    // A cursor past the store clamps instead of panicking: the gateway, not
    // the painter, owns cursor validity.
    seat_live(&mut workspace, 400);
    assert_eq!(workspace.replay_view_rows(&workspace.rows).len(), 40);
}

#[test]
fn recompute_over_a_prefix_holds_holes_past_the_cursor() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    seat_live(&mut workspace, 10);
    workspace.recompute().expect("a prefix recomputes");
    // Every plugin series keeps full slot length with holes past the cursor:
    // painters cull the same slot space, and a hole paints as a gap.
    for (id, evaluation) in &workspace.evaluations {
        for series in &evaluation.series {
            assert_eq!(
                series.values.len(),
                40,
                "{id} keeps full slot length under a live replay"
            );
            assert!(
                series.values[10..].iter().all(Option::is_none),
                "{id} carries no values past the cursor (no lookahead)"
            );
        }
    }
    // The sub-pane folds decided bars only: its legs end at the cursor.
    let pane = workspace
        .sub_pane
        .as_ref()
        .expect("the default pane builds");
    assert_eq!(pane.ribbon.upper.len(), 10, "ribbon legs end at the cursor");
    assert_eq!(pane.ribbon.lower.len(), 10);
    assert_eq!(pane.ribbon.bullish.len(), 10);
    assert_eq!(
        workspace.momentum_keys.len(),
        10,
        "tint keys end at the cursor"
    );
    // Stopping returns to the full evaluation without a re-pull.
    workspace.live_replay = None;
    workspace.recompute().expect("the live chart recomputes");
    let pane = workspace.sub_pane.as_ref().expect("the pane rebuilds");
    assert_eq!(
        pane.ribbon.upper.len(),
        40,
        "exiting restores the full fold"
    );
}

#[test]
fn open_step_and_stop_drive_the_cached_session() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    let rows = ladder_rows(40);
    workspace.open_live_replay(&gateway, rows[5].time, None, 7, None, "Single".to_owned());
    let live = workspace.live_replay.as_ref().expect("open seats a run");
    assert_eq!(live.state.cursor, 5, "the run opens at the requested bar");
    assert_eq!(live.state.total_bars, 40);
    assert_eq!(workspace.replay_prefix_len(), Some(5));
    assert!(live.error.is_none());

    workspace.step_live_replay(&gateway, 3);
    let live = workspace.live_replay.as_ref().expect("the run stays live");
    assert_eq!(live.state.cursor, 8, "a step advances the cached cursor");

    workspace.stop_live_replay(&gateway);
    assert!(workspace.live_replay.is_none(), "stop clears the run");
    assert_eq!(
        workspace.replay_prefix_len(),
        None,
        "the chart is live again"
    );
    assert_eq!(workspace.rows.len(), 40, "no re-pull was needed");
    let calls = gateway.calls.lock().expect("call log locks").clone();
    assert_eq!(calls, vec!["start:5", "step:3", "stop:terminal"]);
}

#[test]
fn a_refused_economics_read_is_visible_and_sends_nothing() {
    // The core owns the booking: when it refuses to state one, the terminal
    // refuses the open visibly instead of inventing a number of its own.
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(20)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(20).refusing_economics();
    let rows = ladder_rows(20);
    workspace.open_live_replay(&gateway, rows[2].time, None, 7, None, "Single".to_owned());
    assert!(
        workspace.live_replay.is_none(),
        "a refused read seats no run"
    );
    assert!(
        workspace
            .replay_error
            .as_deref()
            .unwrap_or_default()
            .contains("economics are not served"),
        "the refusal is visible: {:?}",
        workspace.replay_error
    );
    assert!(
        gateway.calls.lock().expect("call log locks").is_empty(),
        "nothing reaches a session when the core refuses its own economics"
    );
}

#[test]
fn a_refused_step_stops_play_with_the_reason_kept() {
    struct RefusingStart(ScriptReplay);
    impl TerminalGateway for RefusingStart {
        fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
            self.0.chart_bars()
        }

        fn chart_overlays(
            &self,
            _kind: &str,
            _period: i64,
        ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn preview_arm(
            &self,
            _patch: serde_json::Value,
        ) -> Result<protopyne_view::WorstCase, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn arm(
            &self,
            _grant: serde_json::Value,
        ) -> Result<protopyne_view::ArmResponse, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn rehearsal_economics(
            &self,
            underlying: &str,
            structure: StructureKindHint,
        ) -> Result<RehearsalEconomics, GatewayError> {
            self.0.rehearsal_economics(underlying, structure)
        }
        fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_start(params)
        }

        fn replay_step(
            &self,
            _session_id: &str,
            _bars: u64,
        ) -> Result<ReplayStateView, GatewayError> {
            Err(GatewayError::Unavailable("venue halted mid-run".to_owned()))
        }

        fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_state(session_id)
        }

        fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
            self.0.replay_stop(session_id)
        }
    }

    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = RefusingStart(ScriptReplay::new(40));
    let rows = ladder_rows(40);
    workspace.open_live_replay(&gateway, rows[0].time, None, 7, None, "Single".to_owned());
    let live = workspace.live_replay.as_mut().expect("open seats a run");
    live.playing = true;
    workspace.step_live_replay(&gateway, 1);
    let live = workspace
        .live_replay
        .as_ref()
        .expect("the session stays live for retry");
    assert!(!live.playing, "a refused step stops the play loop");
    assert_eq!(live.state.cursor, 0, "a refused step decides nothing");
    assert!(
        live.error
            .as_deref()
            .unwrap_or_default()
            .contains("venue halted"),
        "the refusal reason stays on the section"
    );
}

#[test]
fn chain_gated_verdicts_stay_distinct_from_proposals() {
    // The dock renders direction-bearing verdicts as proposals and counts
    // the chain-gated remainder beside them; the flag (never reason text)
    // is what separates the two.
    let gated = ReplayDecisionView {
        bar_index: 2,
        proposal_id: "run-7-2".to_owned(),
        ts: "2023-11-14T22:13:20+00:00".to_owned(),
        direction: None,
        structure: None,
        state: "options spreads need an option-chain feed (none configured)".to_owned(),
        conviction: 0.0,
        submitted: false,
        filled_quantity: 0,
        fill_price: None,
        skipped: Some("options spreads need an option-chain feed (none configured)".to_owned()),
        chain_gated: true,
        legs: Vec::new(),
    };
    let filled = ReplayDecisionView {
        bar_index: 3,
        proposal_id: "run-7-3".to_owned(),
        ts: "2023-11-14T22:28:20+00:00".to_owned(),
        direction: Some("Long".to_owned()),
        structure: Some("Single".to_owned()),
        state: "Filled".to_owned(),
        conviction: 0.8,
        submitted: true,
        filled_quantity: 1,
        fill_price: Some(Money::from(100)),
        skipped: None,
        chain_gated: false,
        legs: Vec::new(),
    };
    let state = ReplayStateView {
        decisions: vec![gated, filled],
        positions: vec![ReplayPositionView {
            underlying: "SPY".to_owned(),
            quantity: 1,
            avg_price: Money::from(100),
        }],
        ..idle_state(4, 40)
    };
    let proposed: Vec<_> = state
        .decisions
        .iter()
        .filter(|decision| decision.direction.is_some())
        .collect();
    assert_eq!(proposed.len(), 1, "only proposal-bearing verdicts propose");
    assert_eq!(
        state
            .decisions
            .iter()
            .filter(|decision| decision.chain_gated)
            .count(),
        1,
        "the gate count rides the flag, never reason text"
    );
}

#[test]
fn open_forwards_an_explicit_grant_document() {
    struct GrantRecording(ScriptReplay);
    impl TerminalGateway for GrantRecording {
        fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
            self.0.chart_bars()
        }
        fn chart_overlays(
            &self,
            _kind: &str,
            _period: i64,
        ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn preview_arm(
            &self,
            _patch: serde_json::Value,
        ) -> Result<protopyne_view::WorstCase, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn arm(
            &self,
            _grant: serde_json::Value,
        ) -> Result<protopyne_view::ArmResponse, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn rehearsal_economics(
            &self,
            underlying: &str,
            structure: StructureKindHint,
        ) -> Result<RehearsalEconomics, GatewayError> {
            self.0.rehearsal_economics(underlying, structure)
        }
        fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
            assert!(
                params.grant == Some(serde_json::json!({"grant_id": "desk-1"})),
                "the explicit document reaches the core untouched"
            );
            self.0.replay_start(params)
        }
        fn replay_step(
            &self,
            session_id: &str,
            bars: u64,
        ) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_step(session_id, bars)
        }
        fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_state(session_id)
        }
        fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
            self.0.replay_stop(session_id)
        }
    }

    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(20)).expect("seeded workspace builds");
    let gateway = GrantRecording(ScriptReplay::new(20));
    let rows = ladder_rows(20);
    workspace.open_live_replay(
        &gateway,
        rows[2].time,
        None,
        7,
        Some(serde_json::json!({"grant_id": "desk-1"})),
        "Single".to_owned(),
    );
    assert!(
        workspace.live_replay.is_some(),
        "open with a grant document seats a run"
    );
    assert!(
        workspace.replay_error.is_none(),
        "a well-formed document opens cleanly"
    );
}

#[test]
fn picked_replay_opens_rehearsal_singles() {
    // The dock offers no structure picker: every transport open rehearses
    // the single-leg strategy under the rehearsal grant, the same economics
    // the preview agrees on.
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    workspace.open_captured_replay(&gateway);
    assert_eq!(
        gateway
            .structures
            .lock()
            .expect("structure log locks")
            .as_slice(),
        ["Single"],
        "the transport open rehearses singles"
    );
    assert!(workspace.live_replay.is_some(), "open seats a run");
}

#[test]
fn spread_decision_formats_one_row_per_leg_and_singles_format_none() {
    use protopyne_view::{ReplayDecisionView, ReplayLegView};
    let legged = ReplayDecisionView {
        bar_index: 3,
        proposal_id: "run-7-3".to_owned(),
        ts: "2026-01-09T22:00:00Z".to_owned(),
        direction: Some("Long".to_owned()),
        structure: Some("Vertical".to_owned()),
        state: "Filled".to_owned(),
        conviction: 0.8,
        submitted: true,
        filled_quantity: 2,
        fill_price: None,
        skipped: None,
        chain_gated: false,
        legs: vec![
            ReplayLegView {
                contract: "SPY   260717C00690000".to_owned(),
                side: "buy".to_owned(),
                quantity: 1,
                price: protopyne_types::Money::from(400),
                fee: protopyne_types::Money::from(65),
            },
            ReplayLegView {
                contract: "SPY   260717C00695000".to_owned(),
                side: "sell".to_owned(),
                quantity: 1,
                price: protopyne_types::Money::from(199),
                fee: protopyne_types::Money::from(65),
            },
        ],
    };
    assert_eq!(
        ChartWorkspace::replay_leg_lines(&legged),
        vec![
            "SPY   260717C00690000 buy 1 @ 400 (fee 65)".to_owned(),
            "SPY   260717C00695000 sell 1 @ 199 (fee 65)".to_owned(),
        ],
        "each executed leg reads as one display row"
    );
    let single = ReplayDecisionView {
        legs: Vec::new(),
        ..legged
    };
    assert!(
        ChartWorkspace::replay_leg_lines(&single).is_empty(),
        "a fill with no leg detail adds no rows"
    );
}

#[test]
fn replay_epochs_default_to_full_history_and_apply_a_captured_window() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let rows = ladder_rows(40);
    assert_eq!(
        workspace.replay_epochs(),
        (rows[0].time, None),
        "no captured window replays the full loaded history"
    );
    assert_eq!(workspace.replay_range_label(), "Full");
    // The window is captured from the visible chart, stored as epochs;
    // setting it directly is the headless equivalent of a capture.
    workspace.replay_range = Some((rows[10].time, rows[19].time));
    assert_eq!(
        workspace.replay_epochs(),
        (rows[10].time, Some(rows[20].time)),
        "epochs resolve back to the captured display window"
    );
    assert!(
        workspace.replay_range_label().contains('→'),
        "a captured window labels its dates"
    );
    assert_eq!(
        workspace.replay_marker_indices(),
        Some((10, 19)),
        "the divider markers ride the captured edges"
    );
    workspace.replay_range = None;
    assert_eq!(workspace.replay_epochs(), (rows[0].time, None));
    assert_eq!(workspace.replay_marker_indices(), None);
}

#[test]
fn exiting_to_live_keeps_the_captured_window_markers() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let rows = ladder_rows(40);
    // The markers name times, not the run: exiting to live keeps them so the
    // divider still shows where the backtest ran.
    workspace.replay_range = Some((rows[5].time, rows[15].time));
    let gateway = ScriptReplay::new(40);
    workspace.open_captured_replay(&gateway);
    assert!(
        workspace.live_replay.is_some(),
        "a run opens over the captured window"
    );
    workspace.stop_live_replay(&gateway);
    assert!(workspace.live_replay.is_none(), "exit closes the run");
    assert_eq!(
        workspace.replay_range,
        Some((rows[5].time, rows[15].time)),
        "exit keeps the captured window — only a symbol switch clears it"
    );
    assert_eq!(
        workspace.replay_marker_indices(),
        Some((5, 15)),
        "the divider markers outlive the run"
    );
}

#[test]
fn menu_play_opens_bounded_and_steps_stop_at_the_end() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    assert!(
        workspace.live_replay.is_none(),
        "nothing auto-plays on boot"
    );
    workspace.toggle_replay_play(Some(&gateway));
    let live = workspace.live_replay.as_ref().expect("play opens a run");
    assert!(live.playing, "play starts the loop");
    assert_eq!(live.state.cursor, 0, "full-history play starts at the head");
    assert_eq!(live.end(), 40);
    // Step to the end: the clamp stops play exactly there.
    workspace.step_live_replay(&gateway, 100);
    let live = workspace.live_replay.as_ref().expect("the run stays live");
    assert_eq!(live.state.cursor, 40);
    assert!(!live.playing, "arriving at the end stops play");
    // Toggling again restarts at the head and plays.
    workspace.toggle_replay_play(Some(&gateway));
    let live = workspace.live_replay.as_ref().expect("replay restarts");
    assert!(live.playing);
    assert_eq!(live.state.cursor, 0);
}

/// Play replays a set boundary window instead of recapturing the visible
/// chart: context-menu boundaries arm the replay button until cleared.
#[test]
fn menu_play_reuses_set_boundaries_instead_of_recapturing() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    let rows = workspace.rows().to_vec();
    workspace.set_replay_boundary(rows[10].time);
    workspace.set_replay_boundary(rows[20].time);
    assert_eq!(
        workspace.replay_range,
        Some((rows[10].time, rows[20].time)),
        "two boundary picks arm a window"
    );
    workspace.toggle_replay_play(Some(&gateway));
    let live = workspace.live_replay.as_ref().expect("play opens a run");
    assert!(live.playing, "play starts the loop");
    assert_eq!(
        live.state.cursor, 10,
        "a bounded play starts at the boundary, not the head"
    );
    assert_eq!(live.end(), 21, "a bounded play ends past the boundary bar");
}

#[test]
fn open_maps_a_trailing_window_to_server_history_by_timestamp() {
    // The terminal holds the trailing 20 of a 60-bar server history. Opening
    // a replay over display bars 5..15 must start the core session at server
    // bar 45, not display bar 5 — and the in-fill fetch grows the loaded
    // rows back to the full 60 first.
    let full = ladder_rows(60);
    let gateway = ScriptReplay::with_history(full.clone());
    let mut workspace =
        ChartWorkspace::with_rows(full[40..].to_vec()).expect("seeded workspace builds");
    assert_eq!(workspace.rows().len(), 20);
    let rows = workspace.rows().to_vec();
    workspace.replay_range = Some((rows[5].time, rows[14].time));
    workspace.open_captured_replay(&gateway);
    assert!(
        workspace.replay_error.is_none(),
        "open over a trailing window succeeds: {:?}",
        workspace.replay_error
    );
    assert_eq!(
        workspace.rows().len(),
        60,
        "the in-fill fetch grows the loaded rows to the full history"
    );
    let live = workspace.live_replay.as_ref().expect("open seats a run");
    assert_eq!(
        live.state.cursor, 45,
        "the session opens at the server bar stamping the display start"
    );
    assert_eq!(live.end(), 55, "the server end stamps the display end");
    assert!(
        !live.server_times.is_empty(),
        "a served time axis maps cursors by timestamp"
    );
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(45),
        "the decided prefix covers the in-filled context before the start"
    );
    assert!(
        workspace
            .evaluations
            .values()
            .any(|evaluation| !evaluation.series.is_empty()),
        "indicators evaluate over the decided prefix"
    );
    // Stepping three server bars reveals three display bars.
    workspace.step_live_replay(&gateway, 3);
    let live = workspace.live_replay.as_ref().expect("the run stays live");
    assert_eq!(live.state.cursor, 48);
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(48),
        "future bars stay hidden until the run decides them"
    );
    let calls = gateway.calls.lock().expect("call log locks").clone();
    assert_eq!(
        calls.first().map(String::as_str),
        Some("start:45"),
        "the session opens in server coordinates: {calls:?}"
    );
}

#[test]
fn open_refuses_an_empty_window() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    let rows = ladder_rows(40);
    workspace.open_live_replay(
        &gateway,
        rows[20].time,
        Some(rows[20].time),
        7,
        None,
        "Single".to_owned(),
    );
    assert!(
        workspace.live_replay.is_none(),
        "an empty window seats no run"
    );
    assert!(
        workspace
            .replay_error
            .as_deref()
            .unwrap_or_default()
            .contains("empty"),
        "the refusal names the empty window: {:?}",
        workspace.replay_error
    );
}

#[test]
fn open_refuses_when_the_in_fill_fetch_fails() {
    struct NoCharts(ScriptReplay);
    impl TerminalGateway for NoCharts {
        fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn chart_overlays(
            &self,
            _kind: &str,
            _period: i64,
        ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn preview_arm(
            &self,
            _patch: serde_json::Value,
        ) -> Result<protopyne_view::WorstCase, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn arm(
            &self,
            _grant: serde_json::Value,
        ) -> Result<protopyne_view::ArmResponse, GatewayError> {
            Err(GatewayError::Unavailable("no charts".to_owned()))
        }
        fn rehearsal_economics(
            &self,
            underlying: &str,
            structure: StructureKindHint,
        ) -> Result<RehearsalEconomics, GatewayError> {
            self.0.rehearsal_economics(underlying, structure)
        }
        fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_start(params)
        }
        fn replay_step(
            &self,
            session_id: &str,
            bars: u64,
        ) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_step(session_id, bars)
        }
        fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_state(session_id)
        }
        fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
            self.0.replay_stop(session_id)
        }
    }

    // Ten loaded bars can never carry the warmup context, and the gateway
    // serves no in-fill: the open refuses instead of replaying over a gapped
    // context.
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(10)).expect("seeded workspace builds");
    let gateway = NoCharts(ScriptReplay::new(40));
    let rows = ladder_rows(10);
    workspace.open_live_replay(&gateway, rows[5].time, None, 7, None, "Single".to_owned());
    assert!(
        workspace.live_replay.is_none(),
        "a refused in-fill seats no run"
    );
    assert!(
        workspace.replay_error.is_some(),
        "the refusal names the failed in-fill"
    );
}

#[test]
fn failed_capture_leaves_the_applied_window() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let rows = ladder_rows(40);
    workspace.replay_range = Some((rows[10].time, rows[19].time));
    // Shove the viewport far off the data so nothing is visible: the capture
    // refuses instead of applying an empty window, and the applied window —
    // and any open run — stays exactly as it was.
    let main = workspace.main_viewport().expect("a viewport binds");
    workspace.main = Some(
        main.with_x_offset(1_000_000.0)
            .expect("an off-window offset sets"),
    );
    assert!(
        workspace.capture_visible_replay_window().is_err(),
        "nothing visible refuses the capture"
    );
    assert_eq!(
        workspace.replay_range,
        Some((rows[10].time, rows[19].time)),
        "a refused capture leaves the applied window untouched"
    );
}

#[test]
fn symbol_switch_closes_the_run_and_clears_its_window() {
    use crate::chart::TimeframeKey;
    let full = ladder_rows(60);
    let gateway = ScriptReplay::with_history(full.clone());
    let mut workspace =
        ChartWorkspace::with_rows(full[full.len() - 20..].to_vec()).expect("workspace boots");
    let rows = workspace.rows().to_vec();
    workspace.replay_range = Some((rows[5].time, rows[14].time));
    workspace.open_captured_replay(&gateway);
    assert!(
        workspace.live_replay.is_some(),
        "a run opens over the captured window"
    );
    // A timeframe switch keeps the run: epochs re-resolve and the timestamp
    // mapping is timeframe-agnostic.
    workspace
        .select_timeframe(TimeframeKey::M5)
        .expect("timeframe selects");
    assert!(
        workspace.live_replay.is_some(),
        "a timeframe switch keeps the run"
    );
    // A symbol switch closes it and clears the window, so the next chart can
    // never paint another ticker's decided prefix.
    workspace
        .switch_symbol("GLD", &gateway)
        .expect("symbol switches");
    assert!(
        workspace.live_replay.is_none(),
        "a symbol switch closes the run"
    );
    assert_eq!(
        workspace.replay_range, None,
        "a symbol switch clears the picked window"
    );
    assert!(
        workspace.replay_error.is_some(),
        "the close names itself on the replay section"
    );
}

/// End-to-end replay through a real server gateway on a runtime-free thread
/// — the way the terminal drives it after boot: chart reads resample the
/// 1M base history to the 5M chart, the open maps the display window back to
/// base-history indices by timestamp, and steps bridge onto the shared
/// runtime from outside any runtime (nesting would refuse).
#[cfg(feature = "server-gateway")]
#[test]
fn replay_over_a_real_server_maps_display_bars_to_base_history() {
    use protopyne_types::Bar;

    fn decimal(value: i64) -> rust_decimal::Decimal {
        rust_decimal::Decimal::from(value)
    }
    // Sixty 1M bars opening on a 5M grid line, so the CompleteOnly resample
    // to 5M yields twelve whole buckets (a real 1M tape sits on minute
    // marks; an off-grid fixture would fold to nothing by construction).
    let base = 1_700_000_100_i64;
    let bars: Vec<Bar> = (0..60_i64)
        .map(|index| {
            Bar::new()
                .ts(chrono::DateTime::from_timestamp(base + index * 60, 0).expect("fixture stamp"))
                .o(decimal(100 + index))
                .h(decimal(101 + index))
                .l(decimal(99 + index))
                .c(decimal(100 + index))
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let config = protopyne_server::ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_timeframe("1M".to_owned())
        .build();
    let handle = common_core::runtime::block_on(protopyne_server::serve(config, bars))
        .expect("server boots");
    let gateway = handle.gateway();
    let mut workspace = ChartWorkspace::with_gateway(&gateway).expect("workspace boots");
    assert!(
        workspace.rows().len() >= 8,
        "the resampled chart holds several display bars"
    );
    let end = workspace.rows().len() - 2;
    let rows = workspace.rows().to_vec();
    workspace.replay_range = Some((rows[2].time, rows[end - 1].time));
    workspace.open_captured_replay(&gateway);
    assert!(
        workspace.replay_error.is_none(),
        "open over the real gateway succeeds: {:?}",
        workspace.replay_error
    );
    let live = workspace.live_replay.as_ref().expect("open seats a run");
    assert!(
        !live.server_times.is_empty(),
        "the served time axis maps display candles to base history"
    );
    let server_times = live.server_times.clone();
    let rows = workspace.rows().to_vec();
    let expected_start = server_times.partition_point(|time| *time < rows[2].time) as u64;
    assert_eq!(
        live.state.cursor, expected_start,
        "the session opens at the base bar stamping the display start"
    );
    let expected_end = server_times.partition_point(|time| *time < rows[end].time) as u64;
    assert_eq!(live.end(), expected_end);
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(2),
        "only the decided context shows at open"
    );

    // Step three base bars through the terminal path: no runtime on this
    // thread, so the shared bridge drives the driver without nesting. The
    // third bar still sits inside display candle 2's bucket, so the decided
    // prefix holds until the bucket completes — no half-decided candles.
    workspace.step_live_replay(&gateway, 3);
    let live = workspace.live_replay.as_ref().expect("the run stays live");
    assert_eq!(live.state.cursor, expected_start + 3);
    let decided_through = server_times[live.state.cursor as usize];
    let expected_prefix = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| *index >= 1 && row.time <= decided_through)
        .count();
    assert_eq!(
        expected_prefix, 2,
        "a half-decided bucket reveals nothing new"
    );
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(expected_prefix),
        "future candles stay hidden until the run decides them"
    );
    // Stepping through the bucket's last two bars completes display candle 2.
    workspace.step_live_replay(&gateway, 2);
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(3),
        "a completed bucket reveals its display candle"
    );
    for evaluation in workspace.evaluations.values() {
        for series in &evaluation.series {
            assert!(
                series.values[3..].iter().all(Option::is_none),
                "no indicator reads past the decided prefix"
            );
        }
    }

    // Stepping past the captured end clamps there and stops play.
    workspace.step_live_replay(&gateway, 1_000);
    let live = workspace.live_replay.as_ref().expect("the run stays live");
    assert_eq!(live.state.cursor, expected_end);
    assert!(!live.playing, "arriving at the captured end stops play");

    common_core::runtime::block_on(handle.shutdown());
}

#[test]
fn replay_speed_menu_positions_map_to_the_documented_candle_rates() {
    assert_eq!(
        crate::widgets::TOP_BAR_REPLAY_SPEEDS,
        [
            ReplaySpeed::Crawl,
            ReplaySpeed::Normal,
            ReplaySpeed::Swift,
            ReplaySpeed::Fast,
            ReplaySpeed::Sprint,
            ReplaySpeed::Full,
        ],
        "menu order runs slowest-first, uncapped last"
    );
    assert_eq!(ReplaySpeed::default(), ReplaySpeed::Normal);
    assert_eq!(ReplaySpeed::Crawl.candles_per_sec(), Some(0.2));
    assert_eq!(ReplaySpeed::Normal.candles_per_sec(), Some(1.0));
    assert_eq!(ReplaySpeed::Swift.candles_per_sec(), Some(3.0));
    assert_eq!(ReplaySpeed::Fast.candles_per_sec(), Some(5.0));
    assert_eq!(ReplaySpeed::Sprint.candles_per_sec(), Some(10.0));
    assert_eq!(ReplaySpeed::Full.candles_per_sec(), None);
    let mut previous = 0.0;
    for speed in crate::widgets::TOP_BAR_REPLAY_SPEEDS.iter().take(5) {
        let rate = speed.candles_per_sec().expect("paced detent");
        assert!(rate > previous, "{speed:?} paces strictly upward");
        previous = rate;
    }
    for speed in crate::widgets::TOP_BAR_REPLAY_SPEEDS {
        assert!(!speed.label().is_empty(), "every pace labels itself");
        assert!(
            !speed.short_label().is_empty(),
            "every pace short-labels itself"
        );
    }
}

#[test]
fn stepping_never_moves_x_except_to_follow_the_head() {
    use crate::workspace::PANE_WIDTH_PX;
    // A wide history with a mid-history window: the replay head starts far
    // from the trailing edge, so X moves only when the head actually leaves
    // the view — advancing bars never re-zooms or re-anchors the axis.
    let full = ladder_rows(200);
    let gateway = ScriptReplay::with_history(full.clone());
    let mut workspace = ChartWorkspace::with_rows(full.clone()).expect("workspace boots");
    let rows = workspace.rows().to_vec();
    workspace.replay_range = Some((rows[150].time, rows[179].time));
    workspace.open_captured_replay(&gateway);
    assert!(workspace.live_replay.is_some(), "open seats a run");
    assert_eq!(workspace.replay_prefix_len(), Some(150));

    let width = PANE_WIDTH_PX;
    let spacing = workspace
        .main_viewport()
        .expect("a viewport binds")
        .scale()
        .bar_spacing();
    // Step one bar at a time: the offset moves if and only if the new head
    // left the culled view, landing exactly on the place-at-right target;
    // the spacing never changes.
    for _ in 0..10 {
        let main = workspace.main_viewport().expect("a viewport binds");
        let offset = main.x_offset_px();
        assert_eq!(
            main.scale().bar_spacing().to_bits(),
            spacing.to_bits(),
            "advancing a bar never alters the X axis zoom"
        );
        let prefix = workspace.replay_prefix_len().expect("a run is live");
        workspace.step_live_replay(&gateway, 1);
        let next = workspace.main_viewport().expect("a viewport binds");
        assert_eq!(
            next.scale().bar_spacing().to_bits(),
            spacing.to_bits(),
            "advancing a bar never alters the X axis zoom"
        );
        let next_prefix = workspace.replay_prefix_len().expect("a run is live");
        let next_head = next_prefix.saturating_sub(1);
        let (start, end) = main.culled_bar_range(width, prefix + 1);
        let expected = if next_head >= start && next_head < end {
            offset
        } else {
            width - (next_head as f32 + 1.0) * spacing
        };
        assert!(
            (next.x_offset_px() - expected).abs() < 1e-3,
            "X moves only to follow the head out of view"
        );
        // The Y window always contains the full decided candles in view.
        let (cstart, cend) = next.culled_bar_range(width, next_prefix);
        let visible = &workspace.rows()[cstart.min(next_prefix)..cend.min(next_prefix)];
        let lo = visible
            .iter()
            .map(|row| row.low)
            .fold(f64::INFINITY, f64::min);
        let hi = visible
            .iter()
            .map(|row| row.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let range = next.scale().range();
        assert!(
            !visible.is_empty() && range.min <= lo && range.max >= hi,
            "the Y fit contains every decided candle in view"
        );
    }
}

/// The play pump paces in display candles at the selected timeframe: at Fast
/// (5 candles/s) over a 5M chart, one second of play advances five display
/// candles — twenty-five base bars — never five base bars. Slow
/// (1 candle / 3s) holds still over a two-second window. Driven through the
/// terminal's own pump on a runtime-free thread, the way the menu button
/// plays.
#[cfg(feature = "server-gateway")]
#[test]
fn pump_advances_display_bars_not_base_bars() {
    use std::time::Duration;

    use protopyne_types::Bar;
    fn decimal(value: i64) -> rust_decimal::Decimal {
        rust_decimal::Decimal::from(value)
    }
    let base = 1_700_000_100_i64;
    let bars: Vec<Bar> = (0..60_i64)
        .map(|index| {
            Bar::new()
                .ts(chrono::DateTime::from_timestamp(base + index * 60, 0).expect("fixture stamp"))
                .o(decimal(100 + index))
                .h(decimal(101 + index))
                .l(decimal(99 + index))
                .c(decimal(100 + index))
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let config = protopyne_server::ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_timeframe("1M".to_owned())
        .build();
    let handle = common_core::runtime::block_on(protopyne_server::serve(config, bars))
        .expect("server boots");
    let gateway = handle.gateway();
    let mut workspace = ChartWorkspace::with_gateway(&gateway).expect("workspace boots");
    let rows = workspace.rows().to_vec();
    assert!(
        rows.len() >= 8,
        "the resampled chart has several display candles"
    );
    let end = rows.len() - 2;
    workspace.replay_range = Some((rows[2].time, rows[end - 1].time));
    workspace.open_captured_replay(&gateway);
    assert!(
        workspace.live_replay.is_none() || workspace.replay_error.is_none(),
        "open succeeds: {:?}",
        workspace.replay_error
    );
    let live = workspace.live_replay.as_ref().expect("open seats a run");
    assert!(!live.server_times.is_empty(), "the time axis maps");
    let opened = live.state.cursor;

    // Fast for one second, frame by frame: five display candles =
    // twenty-five base bars, delivered steadily — one candle every 0.2 s,
    // never five at once. The frame delta is explicit, so the test owns the
    // clock.
    workspace.replay_speed = ReplaySpeed::Fast;
    workspace
        .live_replay
        .as_mut()
        .expect("run stays live")
        .playing = true;
    let mut seen = Vec::new();
    for _ in 0..10 {
        egui::__run_test_ui(|ui| {
            workspace.pump_replay_play(Some(&gateway), ui, Duration::from_millis(100));
        });
        seen.push(workspace.replay_prefix_len().expect("a run is live"));
    }
    for pair in seen.windows(2) {
        assert!(
            pair[1] - pair[0] <= 1,
            "no frame decides more than one candle: {seen:?}"
        );
    }
    assert_eq!(
        seen[9],
        2 + 5,
        "ten 100 ms frames at Fast decide five 5M candles"
    );
    let live = workspace.live_replay.as_ref().expect("run stays live");
    assert_eq!(
        live.state.cursor,
        opened + 25,
        "one second at Fast decides five 5M candles (25 base bars)"
    );
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(2 + 5),
        "the decided prefix grows in display candles"
    );
    assert!(
        live.playing,
        "the run keeps playing inside its captured window"
    );

    // A one-second stall steps exactly one candle, then the cadence resumes:
    // banked time is dropped, never dumped as a burst.
    let stalled = workspace.replay_prefix_len().expect("a run is live");
    egui::__run_test_ui(|ui| {
        workspace.pump_replay_play(Some(&gateway), ui, Duration::from_secs(1));
    });
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(stalled + 1),
        "a stalled second delivers one candle, not five"
    );
    egui::__run_test_ui(|ui| {
        workspace.pump_replay_play(Some(&gateway), ui, Duration::from_millis(200));
    });
    assert_eq!(
        workspace.replay_prefix_len(),
        Some(stalled + 2),
        "the next interval resumes the cadence"
    );

    common_core::runtime::block_on(handle.shutdown());
}

#[cfg(feature = "server-gateway")]
#[test]
fn pump_holds_slow_speeds_until_a_bar_is_due() {
    use std::time::Duration;

    use protopyne_types::Bar;
    fn decimal(value: i64) -> rust_decimal::Decimal {
        rust_decimal::Decimal::from(value)
    }
    let base = 1_700_000_100_i64;
    let bars: Vec<Bar> = (0..60_i64)
        .map(|index| {
            Bar::new()
                .ts(chrono::DateTime::from_timestamp(base + index * 60, 0).expect("fixture stamp"))
                .o(decimal(100 + index))
                .h(decimal(101 + index))
                .l(decimal(99 + index))
                .c(decimal(100 + index))
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let config = protopyne_server::ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_timeframe("1M".to_owned())
        .build();
    let handle = common_core::runtime::block_on(protopyne_server::serve(config, bars))
        .expect("server boots");
    let gateway = handle.gateway();
    let mut workspace = ChartWorkspace::with_gateway(&gateway).expect("workspace boots");
    // No captured window: the run replays the full loaded history, the old
    // empty-draft default.
    workspace.replay_range = None;
    workspace.open_captured_replay(&gateway);
    assert!(
        workspace.replay_error.is_none(),
        "open succeeds: {:?}",
        workspace.replay_error
    );

    // Crawl (1 candle / 5s) holds still over a two-second frame: nothing is due.
    workspace.replay_speed = ReplaySpeed::Crawl;
    workspace
        .live_replay
        .as_mut()
        .expect("run stays live")
        .playing = true;
    let held = workspace.live_replay.as_ref().expect("run").state.cursor;
    egui::__run_test_ui(|ui| {
        workspace.pump_replay_play(Some(&gateway), ui, Duration::from_secs(2));
    });
    assert_eq!(
        workspace.live_replay.as_ref().expect("run").state.cursor,
        held,
        "Crawl (1 candle / 5s) decides nothing in two seconds"
    );
    // ...and the fifth second decides exactly one display candle (five 1M
    // bars make one 5M candle).
    egui::__run_test_ui(|ui| {
        workspace.pump_replay_play(Some(&gateway), ui, Duration::from_secs(3));
    });
    assert_eq!(
        workspace.live_replay.as_ref().expect("run").state.cursor,
        held + 5,
        "the fifth second decides one 5M candle"
    );
    // The next deadline is a full interval away: one second later holds.
    egui::__run_test_ui(|ui| {
        workspace.pump_replay_play(Some(&gateway), ui, Duration::from_secs(1));
    });
    assert_eq!(
        workspace.live_replay.as_ref().expect("run").state.cursor,
        held + 5,
        "the cadence never steps twice for one deadline"
    );

    common_core::runtime::block_on(handle.shutdown());
}

#[test]
fn stopping_a_replay_never_rewrites_the_journal() {
    // The terminal owns the cursor; the gateway owns the journal. Steps
    // append core-side, and stop/exit only closes the session — rows are
    // never cleared, reordered, or renumbered by a terminal verb.
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    let rows = ladder_rows(40);
    workspace.open_live_replay(&gateway, rows[5].time, None, 7, None, "Single".to_owned());
    workspace.step_live_replay(&gateway, 3);
    let journal = gateway.journal().expect("journal serves stepped rows");
    assert_eq!(journal.len(), 3, "each stepped bar appends one row");
    assert!(
        journal
            .windows(2)
            .all(|pair| pair[0].seq + 1 == pair[1].seq),
        "sequence numbers stay dense and monotonic"
    );
    workspace.stop_live_replay(&gateway);
    assert_eq!(
        gateway.journal().expect("journal serves after stop"),
        journal,
        "stop preserves every row byte-identically"
    );
    workspace.open_live_replay(&gateway, rows[5].time, None, 7, None, "Single".to_owned());
    workspace.step_live_replay(&gateway, 2);
    let resumed = gateway.journal().expect("journal serves the new session");
    assert_eq!(resumed.len(), 5, "the new session appends, never rewrites");
    assert_eq!(
        &resumed[..3],
        journal.as_slice(),
        "earlier rows survive the reopen untouched"
    );
}
