//! Server-backed gateway: the same views the routes serve, no HTTP hop.
//!
//! [`ServerGateway`] implements [`TerminalGateway`](protopyne_view::TerminalGateway)
//! over the live [`Shared`](crate::service::Shared) state by calling the
//! same pure builders the route handlers call (`health_view`,
//! `chart_bars_of`, `overlay_series_of`, `apply_arm`) — one code path per
//! view, so the gateway and the route cannot drift. Mutation stays
//! core-side: `arm` runs the identical tighten-only logic through
//! `apply_arm`, including its refusal messages.

use protopyne_store::{fold_bars_edges, FoldEdges};
use protopyne_strategy::{Grant, ProposalEvent, RiskEngine, RiskLimits};
use protopyne_types::{Bar, Timeframe};
use protopyne_view::{
    validate_symbol_timeframe, ArmResponse, BrokerAccountView, BrokerCapabilityView,
    BrokerDealView, BrokerOrderView, ChartBars, GatewayError, HealthView, JournalRowView,
    OverlaySeries, PortfolioView, ProposalView, ReplayDecisionView, ReplayLegView,
    ReplayPositionView, ReplayStartParams, ReplayStateView, ReplayView, SymbolInfoView,
    TerminalGateway, WorstCase,
};
use std::collections::BTreeMap;

use crate::backtest::BacktestConfig;
use crate::replay_session::ReplayDriver;
use crate::runner::RunnerConfig;
use crate::service::Shared;
use crate::service::{
    apply_arm, chart_bars_of, direction_tag, grant_tiers_of, health_view, live_grant,
    overlay_series_of, parse_patch, portfolio_of_decisions, preview_arm_of, proposal_state_tag,
    replay_grant_for, replay_view_of, structure_tag, tape_serve, TapeMiss, TapeMissKind, TapeServe,
    DEFAULT_REPLAY_CHECKPOINT_EVERY,
};
use crate::views::{
    booked_portfolio_of, broker_account_of, broker_capabilities_of, broker_deal_of,
    broker_order_of, confirm_ticket, decide_journaled, import_tickets, issue_ticket,
    journal_cancel_all, journal_close_all, journal_rows_of, journaled_proposals_of, revoke_grant,
    ticket_drifts_of, waiting_tickets,
};

/// The fallback latch: edge-triggered, so a sustained fallback warns once and
/// a recovery logs once instead of one line per read. Observation-axis only —
/// the read path decides its outcome before consulting it, and nothing about
/// serving depends on the latch's state.
#[derive(Debug, Default)]
pub struct FallbackLatch {
    in_fallback: bool,
}

/// What a latch observation crossed, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatchEvent {
    /// This observation entered fallback (a served read followed by a miss).
    EnteredFallback,
    /// This observation recovered (a miss followed by a served read).
    Recovered,
    /// The state did not change, or the miss is not a fault.
    None,
}

impl FallbackLatch {
    /// Observe one read outcome: `None` for a served read, `Some(kind)` for a
    /// miss, and return the crossed edge. `NoTape` is not a fault — a server
    /// with no tape configured falls back by design — so it neither warns nor
    /// toggles the latch.
    pub fn observe(&mut self, miss: Option<TapeMissKind>) -> LatchEvent {
        match miss {
            None => {
                if self.in_fallback {
                    self.in_fallback = false;
                    LatchEvent::Recovered
                } else {
                    LatchEvent::None
                }
            }
            Some(TapeMissKind::NoTape) => LatchEvent::None,
            Some(_) => {
                if self.in_fallback {
                    LatchEvent::None
                } else {
                    self.in_fallback = true;
                    LatchEvent::EnteredFallback
                }
            }
        }
    }
}

/// Map the overlay builder's `(status, reason)` refusal onto the gateway's
/// error vocabulary: an unavailable dataset is transport-shaped, everything
/// else is a malformed request. Shared by the slot-agnostic and symbol-scoped
/// overlay reads so both refuse identically.
fn overlay_error((status, reason): (axum::http::StatusCode, String)) -> GatewayError {
    if status == axum::http::StatusCode::SERVICE_UNAVAILABLE {
        GatewayError::Unavailable(reason)
    } else {
        GatewayError::Malformed(reason)
    }
}

/// Upward-resample factor for a requested timeframe, over the boot
/// history's actual base.
///
/// Fold the boot history to the requested timeframe when it holds at least
/// one full group, through the bucket-anchored fold under complete-only
/// edges: whole buckets emit stamped at the bucket start, short or torn
/// buckets are omitted rather than fabricated into partial bars. A request
/// the base history cannot express — an unknown label, a finer spacing, or
/// a non-multiple — refuses naming the base spacing instead of silently
/// serving base-spacing bars. A short history that cannot fill a coarser bar
/// serves the base unchanged; a long history with no whole bucket serves an
/// empty window, and the overlay builders refuse that window the same way
/// they refuse any empty history. Boot history is validated at load; an
/// unvalidated row serves the base unchanged instead of stranding the read.
/// `limit` narrows the resampled history to its trailing bars (the same
/// latest-N semantics the tape read plans through), so load-more growth is
/// never a silent no-op in the fallback; `None` and oversize serve the
/// resampled history untouched.
fn resampled_history(
    history: &[Bar],
    base: Timeframe,
    timeframe: &str,
    limit: Option<usize>,
) -> Result<Vec<Bar>, GatewayError> {
    let Some(requested) = Timeframe::from_chart_label(timeframe) else {
        return Err(GatewayError::Malformed(format!(
            "cannot serve {timeframe:?} bars from {base} history: unknown timeframe"
        )));
    };
    let resampled = if requested == base {
        history.to_vec()
    } else if !requested.is_whole_multiple_of(base) {
        return Err(GatewayError::Malformed(format!(
            "cannot serve {requested} bars from {base} history: {requested} is not a whole multiple of one {base} bar"
        )));
    } else {
        let group = (requested.duration_ms() / base.duration_ms()) as usize;
        if history.len() < group {
            history.to_vec()
        } else {
            fold_bars_edges(history, requested, FoldEdges::CompleteOnly).map_err(|error| {
                GatewayError::Malformed(format!(
                    "cannot fold {base} history into {requested}: {error}"
                ))
            })?
        }
    };
    Ok(match limit {
        Some(count) => {
            let skip = resampled.len().saturating_sub(count);
            resampled[skip..].to_vec()
        }
        None => resampled,
    })
}

/// The interactive replay strategy: the shared rehearsal signal under the
/// caller-requested structure (see [`crate::rehearsal`]). Singles behave
/// exactly as before; a multi-leg shape flows to the venue only when the
/// window's chains cover the bar (the session's coverage screen still refuses
/// uncovered bars first).
fn replay_strategy(
    structure: protopyne_strategy::StructureKind,
) -> Result<protopyne_strategy::Strategy, GatewayError> {
    crate::rehearsal::rehearsal_strategy(Some(structure))
        .map(|strategy| (*strategy).clone())
        .map_err(|error| GatewayError::Unavailable(error.to_string()))
}

/// Parse the terminal's structure tag into the core vocabulary. Only
/// defined-risk shapes open: an unknown tag or a naked short refuses at the
/// open, before any driver constructs.
fn replay_structure_of(tag: &str) -> Result<protopyne_strategy::StructureKind, GatewayError> {
    use protopyne_strategy::StructureKind;
    match tag {
        "Single" => Ok(StructureKind::Single),
        "Vertical" => Ok(StructureKind::Vertical),
        "Calendar" => Ok(StructureKind::Calendar),
        "Condor" => Ok(StructureKind::Condor),
        "NakedShort" => Err(GatewayError::Malformed(
            "replay structure NakedShort is outside the defined-risk envelope".to_owned(),
        )),
        _ => Err(GatewayError::Malformed(format!(
            "unknown replay structure {tag:?}"
        ))),
    }
}

/// A step call moves at most this many bars: one synchronous gateway call
/// stays a frame-scale operation, and a play loop wanting more steps again
/// instead of widening one call.
const MAX_REPLAY_STEP_BARS: u64 = 256;

/// Read a live driver's cursor, verdicts, fills, and journal without
/// deciding anything. Decisions and journal rows cover decided bars only;
/// positions fold through the shared [`portfolio_of_decisions`] fold, tagged
/// `Replay` so the mode is never mistaken for the live book.
fn replay_state_view_of(session_id: &str, driver: &ReplayDriver) -> ReplayStateView {
    let mut decisions = Vec::with_capacity(driver.decisions().len());
    let mut journal = Vec::new();
    let mut seq: i64 = 1;
    for decision in driver.decisions() {
        // The last journaled target is the served lifecycle state; a bar
        // that proposed nothing reports its skip reason verbatim instead of
        // a fabricated lifecycle tag.
        let state = decision.transitions.last().map_or_else(
            || decision.skipped.clone().unwrap_or_default(),
            |transition| proposal_state_tag(transition.to).to_owned(),
        );
        decisions.push(ReplayDecisionView {
            bar_index: decision.bar_index as u64,
            proposal_id: decision.proposal_id.clone(),
            ts: decision.ts.clone(),
            direction: decision.direction.map(direction_tag).map(str::to_owned),
            structure: decision.structure.map(structure_tag).map(str::to_owned),
            state,
            conviction: decision.conviction,
            submitted: decision.submitted,
            filled_quantity: decision.filled_quantity,
            fill_price: decision.fill_price,
            skipped: decision.skipped.clone(),
            chain_gated: decision.skipped.as_deref()
                == Some(crate::replay_session::CHAIN_GATE_NOTE),
            // Per-leg truth off the decision's own recorded executions, in
            // fill order: display rows for the terminal, never authority.
            legs: decision
                .legs
                .iter()
                .map(|leg| ReplayLegView {
                    contract: leg.contract.as_str().to_owned(),
                    side: leg.side.to_string(),
                    quantity: leg.quantity,
                    price: leg.price,
                    fee: leg.fee,
                })
                .collect(),
        });
        for transition in &decision.transitions {
            journal.push(JournalRowView {
                seq,
                client_id: transition.proposal_id.clone(),
                kind: "proposal-transition".to_owned(),
                detail: serde_json::json!({
                    "from": proposal_state_tag(transition.from),
                    "to": proposal_state_tag(transition.to),
                    "reason": transition.reason,
                })
                .to_string(),
                at_ms: transition.at.timestamp_millis(),
            });
            seq += 1;
        }
    }
    let portfolio = portfolio_of_decisions(driver.decisions(), "Replay");
    ReplayStateView {
        session_id: session_id.to_owned(),
        active: true,
        cursor: driver.cursor() as u64,
        total_bars: driver.total_bars() as u64,
        decisions,
        positions: portfolio
            .positions
            .into_iter()
            .map(|position| ReplayPositionView {
                underlying: position.underlying,
                quantity: position.quantity,
                avg_price: position.avg_price,
            })
            .collect(),
        filled_orders: portfolio.filled_orders,
        total_fees: portfolio.total_fees,
        journal,
    }
}

/// Read gateway over live server state (`AppState` is not `Debug`, so the
/// gateway isn't either — it prints as its views, never its locks).
#[derive(Clone)]
pub struct ServerGateway {
    state: Shared,
}

impl ServerGateway {
    /// Wrap the shared state (clone the `Arc`, never the state).
    #[must_use]
    pub fn new(state: Shared) -> Self {
        Self { state }
    }

    /// Feed one read outcome to the fallback latch and emit at most one log
    /// line for the crossed edge: a `warn!` naming the miss on entry into
    /// fallback, an `info!` on recovery, and nothing while the state is
    /// unchanged. `NoTape` never warns.
    fn observe_fallback(&self, miss: Option<TapeMissKind>) {
        let event = common_core::sync::lock(&self.state.fallback_latch).observe(miss);
        match event {
            LatchEvent::EnteredFallback => {
                tracing::warn!(miss = ?miss, "chart read fell back to the boot history");
            }
            LatchEvent::Recovered => {
                tracing::info!("chart read recovered; the tape is serving again");
            }
            LatchEvent::None => {}
        }
    }
}

impl TerminalGateway for ServerGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        Ok(health_view(&self.state))
    }

    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        // The authoritative inbox is the journaled fold, never the rehearsal
        // cache the preview route publishes.
        let replayed = self
            .state
            .journal
            .replay(&self.state.journal_permit)
            .map_err(|error| GatewayError::Unavailable(error.to_string()))?;
        Ok(journaled_proposals_of(&replayed.events))
    }

    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        // The authoritative portfolio is the venue book, never the rehearsal
        // projection.
        booked_portfolio_of(&self.state)
            .map_err(|error| GatewayError::Unavailable(error.to_string()))
    }

    fn portfolio_booked(&self) -> Result<PortfolioView, GatewayError> {
        booked_portfolio_of(&self.state)
            .map_err(|error| GatewayError::Unavailable(error.to_string()))
    }

    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        // The tail of the live command journal, folded through the same
        // builder the `/journal` route calls: a read of the journaled state,
        // never a second in-memory log.
        let replayed = self
            .state
            .journal
            .replay(&self.state.journal_permit)
            .map_err(|error| GatewayError::Unavailable(error.to_string()))?;
        Ok(journal_rows_of(&replayed.events))
    }

    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Ok(chart_bars_of(&self.state.history))
    }

    fn chart_overlays(&self, kind: &str, period: i64) -> Result<OverlaySeries, GatewayError> {
        // The route defaults the `bb` multiplier; the gateway takes its
        // builders' default explicitly so callers state nothing implicit.
        overlay_series_of(&self.state.history, kind, period, 2.0).map_err(overlay_error)
    }

    fn symbols(&self) -> Result<Vec<SymbolInfoView>, GatewayError> {
        // Ticker discovery at the data edge: a directory boot reads its root
        // (shipped rows for shipped tickers, sidecar declarations or Unknown
        // for the rest). A file boot has no root and serves the shipped
        // offline list — never a second truth about a root it doesn't have.
        match &self.state.history_root {
            Some(root) => root.symbol_rows().map_err(|error| {
                GatewayError::Unavailable(format!("history root unreadable: {error}"))
            }),
            None => Ok(protopyne_view::symbol_catalog()),
        }
    }

    fn chart_bars_for(&self, symbol: &str, timeframe: &str) -> Result<ChartBars, GatewayError> {
        self.chart_bars_for_limit(symbol, timeframe, None)
    }

    fn chart_bars_for_limit(
        &self,
        symbol: &str,
        timeframe: &str,
        limit: Option<usize>,
    ) -> Result<ChartBars, GatewayError> {
        validate_symbol_timeframe(symbol, timeframe)?;
        // Decide the outcome first, then record it and consult the latch: the
        // store is additive and never strands a read, but a miss is observable
        // (counted) and a sustained fallback is legible (one warn per entry)
        // rather than a silent substitution. A store-less server's `NoTape`
        // counts and never warns — a CSV-only boot is not a fault.
        let serve = match &self.state.tape {
            Some(reader) => tape_serve(reader, symbol, timeframe, limit),
            None => TapeServe::Missing(TapeMiss::NoTape),
        };
        self.state.tape_serve_stats.record(&serve);
        match &serve {
            TapeServe::Served(bars) => {
                self.observe_fallback(None);
                Ok(chart_bars_of(bars))
            }
            TapeServe::Missing(miss) => {
                self.observe_fallback(Some(miss.kind()));
                Ok(chart_bars_of(&resampled_history(
                    &self.state.history,
                    self.state.history_timeframe,
                    timeframe,
                    limit,
                )?))
            }
        }
    }

    fn chart_overlays_for(
        &self,
        symbol: &str,
        timeframe: &str,
        kind: &str,
        period: i64,
    ) -> Result<OverlaySeries, GatewayError> {
        validate_symbol_timeframe(symbol, timeframe)?;
        overlay_series_of(
            &resampled_history(
                &self.state.history,
                self.state.history_timeframe,
                timeframe,
                None,
            )?,
            kind,
            period,
            2.0,
        )
        .map_err(overlay_error)
    }

    fn replay(&self) -> Result<ReplayView, GatewayError> {
        // The route's own default cadence, stated explicitly so the
        // gateway read and a bare `/replay` read cannot disagree: both call
        // `replay_view_of`, so neither can drift from the other. Only
        // 400/500 surface today; the 503 arm stays for a future
        // unavailable-shaped refusal without re-editing the mapping.
        replay_view_of(&self.state.history, DEFAULT_REPLAY_CHECKPOINT_EVERY).map_err(
            |(status, reason)| {
                if status == axum::http::StatusCode::SERVICE_UNAVAILABLE {
                    GatewayError::Unavailable(reason)
                } else {
                    GatewayError::Malformed(reason)
                }
            },
        )
    }

    fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
        // Fail closed up front: a blank slot, blank attribution, or a start
        // past the last bar refuses before anything constructs, so a bad
        // open never leaves a half-built driver in the registry.
        if params.session_id.trim().is_empty() {
            return Err(GatewayError::Malformed(
                "session_id must not be empty".to_owned(),
            ));
        }
        if params.underlying.trim().is_empty() {
            return Err(GatewayError::Malformed(
                "underlying must not be empty".to_owned(),
            ));
        }
        if params.strategy_name.trim().is_empty() {
            return Err(GatewayError::Malformed(
                "strategy_name must not be empty".to_owned(),
            ));
        }
        let start_bar = usize::try_from(params.start_bar).map_err(|_| {
            GatewayError::Malformed(format!("start_bar {} is not addressable", params.start_bar))
        })?;
        if start_bar > self.state.history.len() {
            return Err(GatewayError::Malformed(format!(
                "start_bar {start_bar} is past {} bars",
                self.state.history.len()
            )));
        }
        // The requested shape, parsed before anything constructs: a refusal
        // here leaves no half-built driver in the registry.
        let structure = replay_structure_of(&params.structure)?;
        let runner = RunnerConfig::new(
            params.underlying.clone(),
            params.strategy_name.clone(),
            params.max_loss,
            params.notional,
            params.dte_days,
            params.warmup_bars,
        )
        // The run names the shape it was opened for, so the shared body can
        // tell a multi-leg proposal (which needs a chain feed) from a legless
        // one before anything is decided.
        .with_structure(structure);
        runner
            .validate()
            .map_err(|error| GatewayError::Malformed(error.to_string()))?;
        let quote = BacktestConfig::new()
            .underlying(params.underlying.clone())
            .data_version(format!("replay-{}", params.session_id))
            .build();
        quote
            .validate()
            .map_err(|error| GatewayError::Malformed(error.to_string()))?;
        // An explicit grant document, else the replay rehearsal grant scoped
        // to the requested structure: the terminal never mints grants, and
        // the core alone parses what it is given — the same shape as the
        // `arm` refusal, without arming.
        let grant: Grant = match params.grant {
            Some(document) => serde_json::from_value(document).map_err(|error| {
                GatewayError::Malformed(format!("grant document malformed: {error}"))
            })?,
            None => replay_grant_for(structure),
        };
        let mut driver = ReplayDriver::start(
            replay_strategy(structure)?,
            RiskEngine::new(RiskLimits::new().build()),
            grant,
            quote,
            runner,
            self.state.history.clone(),
            params.seed,
            start_bar,
        )
        .map_err(|error| GatewayError::Malformed(error.to_string()))?;
        // Chain slice for the replay window, when a feed is configured: the
        // boot-loaded history narrowed to the replayed bars' `[first, last]`
        // timestamps, so coverage follows the user's chart range without a
        // per-open load. An installed source lifts the coverage screen bar
        // by bar (covered bars flow into leg selection as of each bar);
        // uncovered bars keep the byte-identical refusal.
        if let Some(chains) = &self.state.chain_history {
            if let (Some(first), Some(last)) = (
                self.state.history.first().map(|bar| bar.ts),
                self.state.history.last().map(|bar| bar.ts),
            ) {
                let window = chains.window(first, last);
                if !window.is_empty() {
                    driver.set_leg_quotes(std::sync::Arc::new(window));
                }
            }
        }
        // Opening replaces: an explicit restart, never a merge with whatever
        // the slot held.
        let mut registry = common_core::sync::lock(&self.state.replay);
        registry.start(params.session_id.clone(), driver);
        let live = registry.session(&params.session_id).ok_or_else(|| {
            GatewayError::Unavailable("replay session vanished on open".to_owned())
        })?;
        Ok(replay_state_view_of(&params.session_id, live))
    }

    fn replay_step(&self, session_id: &str, bars: u64) -> Result<ReplayStateView, GatewayError> {
        if session_id.trim().is_empty() {
            return Err(GatewayError::Malformed(
                "session_id must not be empty".to_owned(),
            ));
        }
        if bars == 0 || bars > MAX_REPLAY_STEP_BARS {
            return Err(GatewayError::Malformed(format!(
                "replay_step takes 1..={MAX_REPLAY_STEP_BARS} bars, not {bars}"
            )));
        }
        let count = usize::try_from(bars).map_err(|_| {
            GatewayError::Malformed(format!("step count {bars} is not addressable"))
        })?;
        let mut registry = common_core::sync::lock(&self.state.replay);
        let Some(driver) = registry.session_mut(session_id) else {
            return Err(GatewayError::Unavailable(format!(
                "no live replay session {session_id:?}"
            )));
        };
        // Bridge the async step onto the caller's context: the gateway is sync
        // (the terminal calls from a paint thread with no runtime), and a
        // caller inside a multi-thread runtime parks its worker via
        // `block_in_place` while the future runs. The shared bridge outlives
        // the call and runs concurrent callers in parallel, so a pooled
        // resource the step touches is never stranded on a dropped
        // per-call runtime. Callers on a current-thread runtime must step
        // from `spawn_blocking` instead — nested bridging cannot enter.
        common_core::runtime::block_on_shared(driver.step_n(count));
        Ok(replay_state_view_of(session_id, driver))
    }

    fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
        if session_id.trim().is_empty() {
            return Err(GatewayError::Malformed(
                "session_id must not be empty".to_owned(),
            ));
        }
        let registry = common_core::sync::lock(&self.state.replay);
        let Some(driver) = registry.session(session_id) else {
            return Err(GatewayError::Unavailable(format!(
                "no live replay session {session_id:?}"
            )));
        };
        Ok(replay_state_view_of(session_id, driver))
    }

    fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
        if session_id.trim().is_empty() {
            return Err(GatewayError::Malformed(
                "session_id must not be empty".to_owned(),
            ));
        }
        // Stopping an unknown session refuses rather than succeeding
        // emptily: the terminal only offers the stop while a session reads
        // live, so a refusal names a real desync.
        if common_core::sync::lock(&self.state.replay).remove(session_id) {
            Ok(())
        } else {
            Err(GatewayError::Unavailable(format!(
                "no live replay session {session_id:?}"
            )))
        }
    }

    fn preview_arm(&self, patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        // The confirmation surface's read: same parse and tighten as an
        // arm, but nothing is stored — the live grant the next read sees
        // is bit-identical to the one this call saw.
        preview_arm_of(&self.state, &patch).map_err(|(status, reason)| {
            if status == axum::http::StatusCode::BAD_REQUEST {
                GatewayError::Malformed(reason)
            } else {
                GatewayError::InvalidGrant(reason)
            }
        })
    }

    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        // Only serialization failures surface here (both arms are 500s), so
        // they map as malformed contract output — never as a transport
        // outage, and never as an empty grant.
        grant_tiers_of(&self.state).map_err(|(_, reason)| GatewayError::Malformed(reason))
    }

    fn arm(&self, grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        // Patch envelope `{"patch": {tier: "<tier document text>"}}`: the
        // schema-driven form never mints a grant locally — it tightens the
        // live grant through the core's own `Grant::tighten`, then stores
        // through the same tighten-only gate as a whole-grant arm.
        // Loosening surfaces the core's refusal; a patch with no live grant
        // behind it refuses.
        if grant.get("patch").is_some() {
            let patch =
                parse_patch(&grant).map_err(|(_, reason)| GatewayError::Malformed(reason))?;
            let current = live_grant(&self.state)
                .ok_or_else(|| GatewayError::InvalidGrant("no live grant to tighten".to_owned()))?;
            let next = current
                .tighten(patch)
                .map_err(|e| GatewayError::InvalidGrant(e.to_string()))?;
            return apply_arm(&self.state, &next)
                .map_err(|(_, reason)| GatewayError::InvalidGrant(reason));
        }
        let grant: Grant = serde_json::from_value(grant)
            .map_err(|e| GatewayError::Malformed(format!("grant document malformed: {e}")))?;
        apply_arm(&self.state, &grant).map_err(|(_, reason)| GatewayError::InvalidGrant(reason))
    }

    fn approve(&self, proposal_id: &str) -> Result<(), GatewayError> {
        decide_journaled(
            &self.state,
            proposal_id,
            ProposalEvent::Approve,
            "operator approved",
        )
        .map_err(decision_error)
    }

    fn reject(&self, proposal_id: &str, reason: &str) -> Result<(), GatewayError> {
        decide_journaled(&self.state, proposal_id, ProposalEvent::Reject, reason)
            .map_err(decision_error)
    }

    fn revoke_grant(&self, grant_id: &str) -> Result<(), GatewayError> {
        revoke_grant(&self.state, grant_id).map_err(|(status, reason)| {
            if status.is_server_error() {
                GatewayError::Unavailable(reason)
            } else {
                GatewayError::Malformed(reason)
            }
        })
    }

    fn close_all(&self, portfolio: &str) -> Result<(), GatewayError> {
        journal_close_all(&self.state, portfolio).map_err(decision_error)
    }

    fn cancel_all(&self, portfolio: &str) -> Result<(), GatewayError> {
        journal_cancel_all(&self.state, portfolio).map_err(decision_error)
    }

    fn tickets_waiting(&self) -> Result<Vec<protopyne_view::TicketView>, GatewayError> {
        Ok(waiting_tickets(&self.state))
    }

    fn ticket_drifts(&self) -> Result<Vec<protopyne_view::TicketDriftView>, GatewayError> {
        Ok(ticket_drifts_of(&self.state))
    }

    fn issue_ticket(&self, ticket: serde_json::Value) -> Result<(), GatewayError> {
        issue_ticket(&self.state, &ticket).map_err(decision_error)
    }

    fn confirm_ticket(&self, ticket_id: &str, fill: serde_json::Value) -> Result<(), GatewayError> {
        confirm_ticket(&self.state, ticket_id, &fill).map_err(decision_error)
    }

    fn import_tickets(&self, snapshot: serde_json::Value) -> Result<(), GatewayError> {
        import_tickets(&self.state, &snapshot)
            .map(|_| ())
            .map_err(decision_error)
    }

    fn broker_venues(&self) -> Result<Vec<BrokerCapabilityView>, GatewayError> {
        let bundles = self.state.broker.bundles();
        if bundles.is_empty() {
            return Err(GatewayError::Unavailable(
                "no broker is registered: broker reads are unsupported on this server".to_owned(),
            ));
        }
        Ok(bundles.iter().map(broker_capabilities_of).collect())
    }

    fn broker_accounts(&self) -> Result<Vec<BrokerAccountView>, GatewayError> {
        let state = self.state.clone();
        common_core::runtime::block_on_shared(async move {
            let bundles = state.broker.bundles();
            if bundles.is_empty() {
                return Err(GatewayError::Unavailable(
                    "no broker is registered: broker reads are unsupported on this server"
                        .to_owned(),
                ));
            }
            let mut rows = Vec::new();
            for bundle in bundles {
                let Some(reader) = bundle.account.clone() else {
                    continue;
                };
                let ids = reader.accounts().await.map_err(account_gateway_error)?;
                for id in ids {
                    let summary = reader.summary(&id).await.map_err(account_gateway_error)?;
                    rows.push(broker_account_of(bundle.id.as_str(), &summary));
                }
            }
            Ok(rows)
        })
    }

    fn broker_account(&self, account: &str) -> Result<BrokerAccountView, GatewayError> {
        let state = self.state.clone();
        let account = account.to_owned();
        common_core::runtime::block_on_shared(async move {
            for bundle in state.broker.bundles() {
                let Some(reader) = bundle.account.clone() else {
                    continue;
                };
                let ids = reader.accounts().await.map_err(account_gateway_error)?;
                if !ids.iter().any(|id| id == &account) {
                    continue;
                }
                let summary = reader
                    .summary(&account)
                    .await
                    .map_err(account_gateway_error)?;
                return Ok(broker_account_of(bundle.id.as_str(), &summary));
            }
            Err(GatewayError::Unavailable(format!(
                "no broker serves account {account:?}"
            )))
        })
    }

    fn broker_orders(&self, account: &str) -> Result<Vec<BrokerOrderView>, GatewayError> {
        let state = self.state.clone();
        let account = account.to_owned();
        common_core::runtime::block_on_shared(async move {
            for bundle in state.broker.bundles() {
                let Some(reader) = bundle.account.clone() else {
                    continue;
                };
                let ids = reader.accounts().await.map_err(account_gateway_error)?;
                if !ids.iter().any(|id| id == &account) {
                    continue;
                }
                let orders = reader
                    .orders(&account, protopyne_venue::AccountRange::all())
                    .await
                    .map_err(account_gateway_error)?;
                return Ok(orders
                    .iter()
                    .map(|order| broker_order_of(bundle.id.as_str(), &account, order))
                    .collect());
            }
            Err(GatewayError::Unavailable(format!(
                "no broker serves account {account:?}"
            )))
        })
    }

    fn broker_deals(&self, account: &str) -> Result<Vec<BrokerDealView>, GatewayError> {
        let state = self.state.clone();
        let account = account.to_owned();
        common_core::runtime::block_on_shared(async move {
            for bundle in state.broker.bundles() {
                let Some(reader) = bundle.account.clone() else {
                    continue;
                };
                let ids = reader.accounts().await.map_err(account_gateway_error)?;
                if !ids.iter().any(|id| id == &account) {
                    continue;
                }
                let deals = reader
                    .deals(&account, protopyne_venue::AccountRange::all())
                    .await
                    .map_err(account_gateway_error)?;
                return Ok(deals
                    .iter()
                    .map(|deal| broker_deal_of(bundle.id.as_str(), &account, deal))
                    .collect());
            }
            Err(GatewayError::Unavailable(format!(
                "no broker serves account {account:?}"
            )))
        })
    }
}

fn account_gateway_error(error: protopyne_venue::AccountError) -> GatewayError {
    match error {
        protopyne_venue::AccountError::Unsupported(reason) => {
            GatewayError::Unavailable(format!("unsupported broker read: {reason}"))
        }
        other => GatewayError::Unavailable(other.to_string()),
    }
}

/// Map a decision's `(status, reason)` refusal onto the gateway vocabulary: a
/// server error is a transport-shaped outage, anything else is a malformed
/// request (unknown proposal, a move its lifecycle refuses).
fn decision_error((status, reason): (axum::http::StatusCode, String)) -> GatewayError {
    if status.is_server_error() {
        GatewayError::Unavailable(reason)
    } else {
        GatewayError::Malformed(reason)
    }
}

#[cfg(test)]
#[path = "../tests/gateway.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/gateway_tape_parity.rs"]
mod gateway_tape_parity_tests;

#[cfg(test)]
#[path = "../tests/gateway_replay.rs"]
mod gateway_replay_tests;

#[cfg(test)]
#[path = "../tests/broker.rs"]
mod broker_tests;

#[cfg(test)]
#[path = "../tests/fallback_latch.rs"]
mod fallback_latch_tests;

#[cfg(test)]
#[path = "../tests/operator_close.rs"]
mod operator_close_tests;

#[cfg(test)]
#[path = "../tests/tickets.rs"]
mod ticket_tests;
