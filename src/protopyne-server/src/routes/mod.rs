//! HTTP routes: thin wrappers over the shared state and the view builders in
//! [`crate::views`]. No route re-derives a projection or re-implements a gate;
//! the body it serves is the builder's own bytes.

use axum::{
    extract::{Path, Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use tokio::sync::broadcast;

use protopyne_store::StoreHealth;
use protopyne_strategy::{Grant, ProposalEvent, RiskEngine, RiskLimits};
use protopyne_types::Money;
use protopyne_venue::{serving_verdict, AccountRange, HealthVerdict};
use protopyne_view::{
    BrokerAccountView, BrokerCapabilityView, BrokerDealView, BrokerOrderView, ChartBars,
    HealthView, JournalRowView, OverlaySeries, PortfolioView, ProposalView, ReplayView,
    TicketDriftView, TicketView,
};

use crate::backtest::BacktestConfig;
use crate::boot::Shared;
use crate::error::ServerError;
use crate::rehearsal::{preview_grant, select_strategy, StrategyKind};
use crate::runner::{run_backtest, BacktestRun, RunnerConfig};
use crate::views::{
    apply_arm, booked_portfolio_of, broker_account_of, broker_capabilities_of, broker_deal_of,
    broker_order_of, chart_bars_of, confirm_ticket, decide_journaled, deny_histogram_of,
    health_view, import_tickets, issue_ticket, journal_cancel_all, journal_close_all,
    journal_rows_of, journaled_proposals_of, live_grant, overlay_series_of, pending_proposals_of,
    publish_run, replay_view_of, revoke_grant, ticket_drifts_of, waiting_tickets,
    DEFAULT_REPLAY_CHECKPOINT_EVERY,
};
use protopyne_view::{ArmResponse, DenyCount};

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorBody>)>;

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        status,
        Json(ErrorBody {
            error: message.into(),
        }),
    )
}

/// Machine-readable error body.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ErrorBody {
    /// What failed.
    pub error: String,
}

/// Preview request parameters.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PreviewParams {
    /// Seed for deterministic proposal identifiers.
    #[serde(default)]
    pub seed: u64,
    /// The grant to evaluate under. `None` uses the fixed rehearsal grant; a
    /// caller that wants a live or altered grant to apply supplies it here, so
    /// the manifest is a pure function of the request rather than of an
    /// ambient clock read.
    #[serde(default)]
    pub grant: Option<Grant>,
    /// Which strategy composition decides the run. Absent means rehearsal,
    /// so a request written before the deployment composition existed runs
    /// byte-identically.
    #[serde(default)]
    pub strategy: StrategyKind,
}

/// Preview outcome: the run plus why bars did not trade.
///
/// The histogram reports outcome counts (a property of the run), never a
/// confidence score: nothing here measures a component's self-doubt, and
/// nothing fires on these counts. Authorization behavior is unchanged — the
/// same grants produce the same verdicts, now with visible reasons.
///
/// The response type stays server-local on purpose: the typed run tree
/// (manifest, per-bar verdicts, report) is core-owned, and a neutral copy
/// would be a second implementation. The histogram legs ride the neutral
/// [`DenyCount`] DTO; the run serializes exactly as before, which the
/// golden proves byte-for-byte.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreviewResponse {
    /// The backtest run, unchanged in shape.
    pub run: BacktestRun,
    /// Deny reasons across the run, most frequent first.
    #[serde(default)]
    pub deny_histogram: Vec<DenyCount>,
}

/// Revoke request body: the id of the live grant to end.
#[derive(Debug, serde::Deserialize)]
pub struct RevokeParams {
    /// Grant id to revoke.
    pub grant_id: String,
}

/// Operator command body: the portfolio the command scopes to. The loop
/// executes against its single-book session; the name rides the audit row.
#[derive(Debug, serde::Deserialize)]
pub struct OperatorParams {
    /// Portfolio name the command scopes to.
    pub portfolio: String,
}

/// Overlay query: `kind` is `sma`, `ema`, `bb` (mid), or `trima`; `period`
/// is the averaging length; `mult` is the band multiplier for `bb` only
/// (default 2.0, matching the calibration parameterization).
#[derive(Debug, serde::Deserialize)]
struct OverlayQuery {
    kind: String,
    period: i64,
    mult: Option<f64>,
}

/// Replay query: `checkpoint_every` is the bar cadence the core records
/// anchors at. Absent means [`DEFAULT_REPLAY_CHECKPOINT_EVERY`].
#[derive(Debug, serde::Deserialize)]
struct ReplayQuery {
    checkpoint_every: Option<usize>,
}

async fn health(State(state): State<Shared>) -> Json<HealthView> {
    Json(health_view(&state))
}

/// The authoritative inbox: every journaled proposal with its latest state.
async fn proposals(
    State(state): State<Shared>,
) -> Result<Json<Vec<ProposalView>>, (StatusCode, Json<ErrorBody>)> {
    let replayed = state
        .journal
        .replay(&state.journal_permit)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(journaled_proposals_of(&replayed.events)))
}

/// The rehearsal inbox a preview run publishes: a clearly named route, never
/// the authoritative inbox above.
async fn proposals_rehearsal(State(state): State<Shared>) -> Json<Vec<ProposalView>> {
    Json(common_core::sync::lock(&state.inbox).clone())
}

/// Reject request body: the human's reason, journaled with the decision.
#[derive(Debug, serde::Deserialize)]
pub struct RejectBody {
    /// Why the human rejected the proposal.
    pub reason: String,
}

/// The journaled pending-approval fold: bodies awaiting a human, never the
/// rehearsal inbox. A thin wrapper over [`pending_proposals_of`].
async fn pending(
    State(state): State<Shared>,
) -> Result<Json<Vec<ProposalView>>, (StatusCode, Json<ErrorBody>)> {
    let replayed = state
        .journal
        .replay(&state.journal_permit)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(pending_proposals_of(&replayed.events)))
}

async fn approve(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    decide_journaled(&state, &id, ProposalEvent::Approve, "operator approved")
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

async fn reject(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<RejectBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    decide_journaled(&state, &id, ProposalEvent::Reject, &body.reason)
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// The journaled lifecycle tail, folded through the same builder the gateway
/// reads. An empty journal returns an empty list, never an error.
async fn journal(
    State(state): State<Shared>,
) -> Result<Json<Vec<JournalRowView>>, (StatusCode, Json<ErrorBody>)> {
    let replayed = state
        .journal
        .replay(&state.journal_permit)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(journal_rows_of(&replayed.events)))
}

async fn preview(
    State(state): State<Shared>,
    Json(params): Json<PreviewParams>,
) -> ApiResult<PreviewResponse> {
    // A store outage halts entries here, naming every failing gate, before
    // any run executes. It is the same pure verdict `/health` reports, so
    // the route a preview reads and the route an operator reads cannot
    // disagree. Exits are unaffected at this layer because a preview never
    // exits a position.
    let reading: StoreHealth = *common_core::sync::lock(&state.store_health);
    if let HealthVerdict::Halt { reasons } = serving_verdict(reading) {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("halted: {}", reasons.join("; ")),
        ));
    }
    // The grant is an explicit input: a supplied one, else the fixed rehearsal
    // grant. Nothing here reads the clock or the armed state, so the pinned
    // config is reproducible from the request alone.
    let PreviewParams {
        seed,
        grant,
        strategy,
    } = params;
    let grant = grant.unwrap_or_else(preview_grant);
    let run = state
        .limiter
        .run(|| async {
            let strategy = select_strategy(strategy)?;
            let runner = RunnerConfig::new(
                "SPY".to_owned(),
                "ribbon".to_owned(),
                Money::from(400),
                Money::from(2_000),
                30,
                0,
            );
            let venue = BacktestConfig::new()
                .underlying("SPY".to_owned())
                .data_version("http-preview".to_owned())
                .build();
            let chain = venue.chain_history.clone();
            let run = run_backtest(
                &strategy,
                &RiskEngine::new(RiskLimits::new().build()),
                &grant,
                &venue,
                &runner,
                &state.history,
                seed,
            )
            .await?;
            Ok((run, chain))
        })
        .await
        .map_err(|e: ServerError| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    let (run, chain) = run;
    publish_run(&state, &run, &grant.id, &chain);
    let deny_histogram = deny_histogram_of(&run);
    Ok(Json(PreviewResponse {
        run,
        deny_histogram,
    }))
}

async fn arm(
    State(state): State<Shared>,
    Json(grant): Json<Grant>,
) -> Result<Json<ArmResponse>, (StatusCode, Json<ErrorBody>)> {
    apply_arm(&state, &grant)
        .map(Json)
        .map_err(|(status, reason)| api_error(status, reason))
}

async fn revoke(
    State(state): State<Shared>,
    Json(params): Json<RevokeParams>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    revoke_grant(&state, &params.grant_id)
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Journal a close-all command for the loop to execute. The route journals
/// only; the loop's per-bar fold submits the exits, so a halted book stays
/// manageable while entries refuse.
async fn close_all(
    State(state): State<Shared>,
    Json(params): Json<OperatorParams>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    journal_close_all(&state, &params.portfolio)
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Journal a cancel-all command for the loop to execute. Same journal-only
/// shape as [`close_all`]: the loop's fold runs the sweep.
async fn cancel_all(
    State(state): State<Shared>,
    Json(params): Json<OperatorParams>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    journal_cancel_all(&state, &params.portfolio)
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Issue a manual ticket the operator places by hand: the platform never
/// submits it — the row journals the exact ticket and it waits for the
/// human's fill report.
async fn ticket_issue(
    State(state): State<Shared>,
    Json(ticket): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    issue_ticket(&state, &ticket)
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Confirm a waiting ticket against the human's fill report: journals the
/// confirm row, clears the wait, and flags any drift on the drifts read.
async fn ticket_confirm(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(fill): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    confirm_ticket(&state, &id, &fill)
        .map(|()| StatusCode::OK)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Apply one read-only import snapshot over the human-executed account and
/// report the resulting drifts. A failing read reports without touching
/// ticket state; the tracker never moves on bad input.
async fn ticket_import(
    State(state): State<Shared>,
    Json(snapshot): Json<serde_json::Value>,
) -> Result<Json<Vec<TicketDriftView>>, (StatusCode, Json<ErrorBody>)> {
    import_tickets(&state, &snapshot)
        .map(Json)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Tickets still waiting for the human's fill report, in ticket order.
async fn tickets_waiting(State(state): State<Shared>) -> Json<Vec<TicketView>> {
    Json(waiting_tickets(&state))
}

/// Flagged ticket disagreements, oldest first.
async fn tickets_drifts(State(state): State<Shared>) -> Json<Vec<TicketDriftView>> {
    Json(ticket_drifts_of(&state))
}

async fn current_grant(State(state): State<Shared>) -> ApiResult<Option<Grant>> {
    Ok(Json(live_grant(&state)))
}

/// The authoritative portfolio: the venue book.
async fn portfolio(
    State(state): State<Shared>,
) -> Result<Json<PortfolioView>, (StatusCode, Json<ErrorBody>)> {
    booked_portfolio_of(&state)
        .map(Json)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

/// The booked portfolio under its explicit name (the same read `/portfolio`
/// now serves).
async fn portfolio_booked(
    State(state): State<Shared>,
) -> Result<Json<PortfolioView>, (StatusCode, Json<ErrorBody>)> {
    booked_portfolio_of(&state)
        .map(Json)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

/// The rehearsal portfolio a preview run published: a clearly named route.
async fn portfolio_rehearsal(State(state): State<Shared>) -> Json<PortfolioView> {
    Json(common_core::sync::lock(&state.portfolio).clone())
}

async fn chart_bars(State(state): State<Shared>) -> Json<ChartBars> {
    Json(chart_bars_of(&state.history))
}

async fn chart_overlay(
    State(state): State<Shared>,
    Query(query): Query<OverlayQuery>,
) -> Result<Json<OverlaySeries>, (StatusCode, Json<ErrorBody>)> {
    overlay_series_of(
        &state.history,
        &query.kind,
        query.period,
        query.mult.unwrap_or(2.0),
    )
    .map(Json)
    .map_err(|(status, reason)| api_error(status, reason))
}

async fn replay(
    State(state): State<Shared>,
    Query(query): Query<ReplayQuery>,
) -> Result<Json<ReplayView>, (StatusCode, Json<ErrorBody>)> {
    replay_view_of(
        &state.history,
        query
            .checkpoint_every
            .unwrap_or(DEFAULT_REPLAY_CHECKPOINT_EVERY),
    )
    .map(Json)
    .map_err(|(status, reason)| api_error(status, reason))
}

async fn feed(State(state): State<Shared>, upgrade: WebSocketUpgrade) -> impl IntoResponse {
    let feed = state.feed.subscribe();
    upgrade.on_upgrade(move |mut socket| async move {
        let mut feed = feed;
        loop {
            match feed.recv().await {
                Ok(frame) => {
                    let message = axum::extract::ws::Message::Text(frame.into());
                    if socket.send(message).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// The one router: every route is a thin wrapper over a shared builder, so the
/// HTTP body and the in-process gateway read the same view.
pub(crate) fn account_error(
    error: &protopyne_venue::AccountError,
) -> (StatusCode, Json<ErrorBody>) {
    let status = match error {
        protopyne_venue::AccountError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
        protopyne_venue::AccountError::Terminal(_)
        | protopyne_venue::AccountError::Malformed(_) => StatusCode::BAD_GATEWAY,
        protopyne_venue::AccountError::Transient(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    api_error(status, error.to_string())
}

fn no_brokers() -> (StatusCode, Json<ErrorBody>) {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "no broker is registered: broker reads are unsupported on this server",
    )
}

async fn broker_venues(State(state): State<Shared>) -> ApiResult<Vec<BrokerCapabilityView>> {
    let bundles = state.broker.bundles();
    if bundles.is_empty() {
        return Err(no_brokers());
    }
    Ok(Json(bundles.iter().map(broker_capabilities_of).collect()))
}

async fn broker_accounts(State(state): State<Shared>) -> ApiResult<Vec<BrokerAccountView>> {
    let bundles = state.broker.bundles();
    if bundles.is_empty() {
        return Err(no_brokers());
    }
    let mut rows = Vec::new();
    for bundle in bundles {
        let Some(account) = bundle.account.clone() else {
            continue;
        };
        let ids = account
            .accounts()
            .await
            .map_err(|error| account_error(&error))?;
        for id in ids {
            let summary = account
                .summary(&id)
                .await
                .map_err(|error| account_error(&error))?;
            rows.push(broker_account_of(bundle.id.as_str(), &summary));
        }
    }
    Ok(Json(rows))
}

async fn broker_account(
    State(state): State<Shared>,
    Path(account): Path<String>,
) -> ApiResult<BrokerAccountView> {
    for bundle in state.broker.bundles() {
        let Some(reader) = bundle.account.clone() else {
            continue;
        };
        let ids = reader
            .accounts()
            .await
            .map_err(|error| account_error(&error))?;
        if !ids.iter().any(|id| id == &account) {
            continue;
        }
        let summary = reader
            .summary(&account)
            .await
            .map_err(|error| account_error(&error))?;
        return Ok(Json(broker_account_of(bundle.id.as_str(), &summary)));
    }
    Err(api_error(
        StatusCode::NOT_FOUND,
        format!("no broker serves account {account:?}"),
    ))
}

async fn broker_orders(
    State(state): State<Shared>,
    Path(account): Path<String>,
) -> ApiResult<Vec<BrokerOrderView>> {
    for bundle in state.broker.bundles() {
        let Some(reader) = bundle.account.clone() else {
            continue;
        };
        let ids = reader
            .accounts()
            .await
            .map_err(|error| account_error(&error))?;
        if !ids.iter().any(|id| id == &account) {
            continue;
        }
        let orders = reader
            .orders(&account, AccountRange::all())
            .await
            .map_err(|error| account_error(&error))?;
        return Ok(Json(
            orders
                .iter()
                .map(|order| broker_order_of(bundle.id.as_str(), &account, order))
                .collect(),
        ));
    }
    Err(api_error(
        StatusCode::NOT_FOUND,
        format!("no broker serves account {account:?}"),
    ))
}

async fn broker_deals(
    State(state): State<Shared>,
    Path(account): Path<String>,
) -> ApiResult<Vec<BrokerDealView>> {
    for bundle in state.broker.bundles() {
        let Some(reader) = bundle.account.clone() else {
            continue;
        };
        let ids = reader
            .accounts()
            .await
            .map_err(|error| account_error(&error))?;
        if !ids.iter().any(|id| id == &account) {
            continue;
        }
        let deals = reader
            .deals(&account, AccountRange::all())
            .await
            .map_err(|error| account_error(&error))?;
        return Ok(Json(
            deals
                .iter()
                .map(|deal| broker_deal_of(bundle.id.as_str(), &account, deal))
                .collect(),
        ));
    }
    Err(api_error(
        StatusCode::NOT_FOUND,
        format!("no broker serves account {account:?}"),
    ))
}

pub(crate) fn router(state: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/proposals", get(proposals))
        .route("/proposals/rehearsal", get(proposals_rehearsal))
        .route("/proposals/pending", get(pending))
        .route("/proposals/{id}/approve", post(approve))
        .route("/proposals/{id}/reject", post(reject))
        .route("/proposals/preview", post(preview))
        .route("/grants/arm", post(arm))
        .route("/grants/revoke", post(revoke))
        .route("/grants/current", get(current_grant))
        .route("/operator/close-all", post(close_all))
        .route("/operator/cancel-all", post(cancel_all))
        .route("/tickets/issue", post(ticket_issue))
        .route("/tickets/{id}/confirm", post(ticket_confirm))
        .route("/tickets/import", post(ticket_import))
        .route("/tickets/waiting", get(tickets_waiting))
        .route("/tickets/drifts", get(tickets_drifts))
        .route("/portfolio", get(portfolio))
        .route("/portfolio/booked", get(portfolio_booked))
        .route("/portfolio/rehearsal", get(portfolio_rehearsal))
        .route("/chart/bars", get(chart_bars))
        .route("/chart/overlays", get(chart_overlay))
        .route("/replay", get(replay))
        .route("/journal", get(journal))
        .route("/feed", get(feed))
        .route("/broker/venues", get(broker_venues))
        .route("/broker/accounts", get(broker_accounts))
        .route("/broker/accounts/{account}", get(broker_account))
        .route("/broker/accounts/{account}/orders", get(broker_orders))
        .route("/broker/accounts/{account}/deals", get(broker_deals))
        .with_state(state)
}
