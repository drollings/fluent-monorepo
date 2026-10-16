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
use protopyne_strategy::Grant;
use protopyne_types::{Bar, Timeframe};
use protopyne_view::{
    validate_symbol_timeframe, ArmResponse, ChartBars, GatewayError, HealthView, JournalRowView,
    OverlaySeries, PortfolioView, ProposalView, ReplayView, SymbolInfoView, TerminalGateway,
    WorstCase,
};
use std::collections::BTreeMap;

use crate::service::Shared;
use crate::service::{
    apply_arm, chart_bars_of, grant_tiers_of, health_view, live_grant, overlay_series_of,
    parse_patch, preview_arm_of, replay_view_of, tape_serve, TapeMiss, TapeMissKind, TapeServe,
    DEFAULT_REPLAY_CHECKPOINT_EVERY,
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
fn resampled_history(
    history: &[Bar],
    base: Timeframe,
    timeframe: &str,
) -> Result<Vec<Bar>, GatewayError> {
    let Some(requested) = Timeframe::from_chart_label(timeframe) else {
        return Err(GatewayError::Malformed(format!(
            "cannot serve {timeframe:?} bars from {base} history: unknown timeframe"
        )));
    };
    if requested == base {
        return Ok(history.to_vec());
    }
    if !requested.is_whole_multiple_of(base) {
        return Err(GatewayError::Malformed(format!(
            "cannot serve {requested} bars from {base} history: {requested} is not a whole multiple of one {base} bar"
        )));
    }
    let group = (requested.duration_ms() / base.duration_ms()) as usize;
    if history.len() < group {
        return Ok(history.to_vec());
    }
    fold_bars_edges(history, requested, FoldEdges::CompleteOnly).map_err(|error| {
        GatewayError::Malformed(format!(
            "cannot fold {base} history into {requested}: {error}"
        ))
    })
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
        Ok(common_core::sync::lock(&self.state.inbox).clone())
    }

    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Ok(common_core::sync::lock(&self.state.portfolio).clone())
    }

    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        // No journal route exists yet: the gateway returns the same empty
        // read the routes would serve, until the route lands and both sides
        // share its builder like every other view here.
        Ok(Vec::new())
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
        // The server serves its boot history for any requested symbol and
        // curates no instrument master, so the catalog reads empty; the
        // terminal drives its Symbol Selection Modal from its own catalog.
        Ok(Vec::new())
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
            &resampled_history(&self.state.history, self.state.history_timeframe, timeframe)?,
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
            return apply_arm(&self.state, next)
                .map_err(|(_, reason)| GatewayError::InvalidGrant(reason));
        }
        let grant: Grant = serde_json::from_value(grant)
            .map_err(|e| GatewayError::Malformed(format!("grant document malformed: {e}")))?;
        apply_arm(&self.state, grant).map_err(|(_, reason)| GatewayError::InvalidGrant(reason))
    }
}

#[cfg(test)]
#[path = "../tests/gateway.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/gateway_tape_parity.rs"]
mod gateway_tape_parity_tests;

#[cfg(test)]
#[path = "../tests/fallback_latch.rs"]
mod fallback_latch_tests;
