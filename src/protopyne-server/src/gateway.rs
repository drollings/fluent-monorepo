//! Server-backed gateway: the same views the routes serve, no HTTP hop.
//!
//! [`ServerGateway`] implements [`TerminalGateway`](protopyne_view::TerminalGateway)
//! over the live [`Shared`](crate::service::Shared) state by calling the
//! same pure builders the route handlers call (`health_view`,
//! `chart_bars_of`, `overlay_series_of`, `apply_arm`) — one code path per
//! view, so the gateway and the route cannot drift. Mutation stays
//! core-side: `arm` runs the identical tighten-only logic through
//! `apply_arm`, including its refusal messages.

use protopyne_chart_model::aggregate_bars;
use protopyne_strategy::Grant;
use protopyne_types::{Bar, Timeframe};
use protopyne_view::{
    ArmResponse, ChartBars, GatewayError, HealthView, JournalRowView, OverlaySeries, PortfolioView,
    ProposalView, ReplayView, SymbolInfoView, TerminalGateway, WorstCase,
    validate_symbol_timeframe,
};
use std::collections::BTreeMap;

use crate::service::Shared;
use crate::service::{
    DEFAULT_REPLAY_CHECKPOINT_EVERY, apply_arm, chart_bars_of, grant_tiers_of, health_view,
    live_grant, overlay_series_of, parse_patch, preview_arm_of, replay_view_of, tape_bars_for,
    timeframe_of_label,
};

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
/// A coarser requested timeframe that is a whole multiple of the base folds
/// through the shared [`aggregate_bars`]; a finer request, a non-multiple,
/// or an unknown spelling serves the base unchanged — a chart cannot
/// synthesize a finer grid, and folding across a non-multiple would split
/// base bars between buckets.
fn resample_group(base: Timeframe, timeframe: &str) -> usize {
    let Some(requested) = timeframe_of_label(timeframe) else {
        return 1;
    };
    let base_ms = base.duration_ms().max(1);
    let requested_ms = requested.duration_ms();
    if requested_ms <= base_ms || requested_ms % base_ms != 0 {
        return 1;
    }
    (requested_ms / base_ms) as usize
}

/// Fold the boot history to the requested timeframe when it holds at least one
/// full group. A short history that cannot fill a coarser bar serves the base
/// unchanged rather than fabricating a partial bar, so a coarse read never
/// returns an empty window the overlay builders would refuse.
fn resampled_history(history: &[Bar], base: Timeframe, timeframe: &str) -> Vec<Bar> {
    let group = resample_group(base, timeframe);
    if group > 1 && history.len() >= group {
        aggregate_bars(history, group)
    } else {
        history.to_vec()
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
        // A booted tape plans and serves the read; a store-less server (or a
        // symbol/timeframe the tape cannot serve) falls back to the boot
        // history, so the store is additive and never strands a read.
        if let Some(reader) = &self.state.tape {
            if let Some(bars) = tape_bars_for(reader, symbol, timeframe, limit) {
                return Ok(chart_bars_of(&bars));
            }
        }
        Ok(chart_bars_of(&resampled_history(
            &self.state.history,
            self.state.history_timeframe,
            timeframe,
        )))
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
            &resampled_history(&self.state.history, self.state.history_timeframe, timeframe),
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
            let patch = parse_patch(&grant)
                .map_err(|(_, reason)| GatewayError::Malformed(reason))?;
            let current = live_grant(&self.state).ok_or_else(|| {
                GatewayError::InvalidGrant("no live grant to tighten".to_owned())
            })?;
            let next =
                current.tighten(patch).map_err(|e| GatewayError::InvalidGrant(e.to_string()))?;
            return apply_arm(&self.state, next)
                .map_err(|(_, reason)| GatewayError::InvalidGrant(reason));
        }
        let grant: Grant = serde_json::from_value(grant)
            .map_err(|e| GatewayError::Malformed(format!("grant document malformed: {e}")))?;
        apply_arm(&self.state, grant)
            .map_err(|(_, reason)| GatewayError::InvalidGrant(reason))
    }
}

#[cfg(test)]
#[path = "../tests/gateway.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/gateway_tape_parity.rs"]
mod gateway_tape_parity_tests;
