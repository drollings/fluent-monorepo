//! Shared view builders: every projection the HTTP routes and the in-process
//! [`ServerGateway`](crate::gateway::ServerGateway) read, built once so the
//! route body and the gateway output cannot drift.
//!
//! Nothing here holds order authority: these are read-only folds and the two
//! journaled authority writers (`apply_arm`, `revoke_grant`) that both the
//! route and the gateway call, so the route and the terminal tighten through
//! one implementation.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use fluent_wvr::prelude::*;

use protopyne_chart_model::rows_of;
use protopyne_kernels::{
    BbIndicator, EmaIndicator, Indicator as _, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
};
use protopyne_store::StoreHealth;
use protopyne_strategy::{
    spread_economics, worst_case_for, Direction, Grant, GrantPatch, LegRequest, LegSelector,
    ProposalEvent, ProposalState, StrikePolicy, StructureKind, WorstCaseUnit,
};
use protopyne_types::{Bar, ChainQuote, Money, Qty};
use protopyne_venue::{
    cancel_all_detail, close_all_detail, decide_proposal, replay_grant, replay_proposals,
    serving_allows_entries, update_from_fill, AccountSummary, BrokerBundle, BrokerDeal,
    ConfirmedFill, DriftKind, ImportedPosition, JournalEvent, ManualTicket, OrderIntent,
    OrderRecord, OrderSide, Position, PositionFill, PositionSide, SpreadPosition, TicketDrift,
    TicketRead, TradingState, COMMAND_ARM_GRANT_KIND, COMMAND_ATTEST_KIND, COMMAND_CANCEL_ALL_KIND,
    COMMAND_CLOSE_ALL_KIND, COMMAND_REVOKE_GRANT_KIND, OPERATOR_CLIENT_ID,
};
use protopyne_view::{
    ArmResponse, BacktestMarkerView, BacktestMetricView, BacktestRunParams, BacktestRunRef,
    BacktestRunView, BacktestSchemaView, BrokerAccountView, BrokerCapabilityView, BrokerDealView,
    BrokerLegView, BrokerOrderView, ChartBars, EquityPointView, HealthView, JournalRowView,
    OverlaySeries, PortfolioView, PositionView, ProposalView, ReplayStepView, ReplayView,
    TicketDraft, TicketDriftView, TicketSide, TicketView, WorstCase, PORTFOLIO_SOURCE_BOOKED,
    PORTFOLIO_SOURCE_REHEARSAL,
};

use crate::boot::{AppState, Shared, TapeMissKind, TapeServeSnapshot};
use crate::portfolio::{aggregate, BookedFill, PAPER_PORTFOLIO};
use crate::runner::{build_replay, complete_replay, side_for_direction, BacktestRun, BarDecision};

/// Display-enum bridges: the one home mapping domain enums to their stable
/// wire tags. Exhaustive matches, so a new variant breaks the build until
/// its tag is decided — tags can never silently drift. The cast is one-way:
/// nothing parses tags back into domain enums. Crate-visible so the gateway's
/// interactive replay state reads the same tags as the preview publisher.
pub(crate) fn direction_tag(direction: Direction) -> &'static str {
    match direction {
        Direction::Long => "Long",
        Direction::Short => "Short",
    }
}

/// See [`direction_tag`]: option-structure tags.
pub(crate) fn structure_tag(structure: StructureKind) -> &'static str {
    match structure {
        StructureKind::Single => "Single",
        StructureKind::Vertical => "Vertical",
        StructureKind::Calendar => "Calendar",
        StructureKind::Condor => "Condor",
        StructureKind::NakedShort => "NakedShort",
    }
}

/// See [`direction_tag`]: proposal-lifecycle tags.
pub(crate) fn proposal_state_tag(state: ProposalState) -> &'static str {
    match state {
        ProposalState::Proposed => "Proposed",
        ProposalState::RiskChecked => "RiskChecked",
        ProposalState::AutoApproved => "AutoApproved",
        ProposalState::PendingHuman => "PendingHuman",
        ProposalState::Approved => "Approved",
        ProposalState::Rejected => "Rejected",
        ProposalState::Expired => "Expired",
        ProposalState::Submitted => "Submitted",
        ProposalState::Working => "Working",
        ProposalState::Filled => "Filled",
        ProposalState::Partial => "Partial",
        ProposalState::Cancelled => "Cancelled",
    }
}

/// See [`direction_tag`]: trading-state tags.
pub(crate) fn trading_state_tag(state: TradingState) -> &'static str {
    match state {
        TradingState::Enabled => "Enabled",
        TradingState::Disabled => "Disabled",
        TradingState::ExitOnly => "ExitOnly",
    }
}

/// Map one position to its view: strings restored at the boundary so the
/// wire shape is unchanged.
fn position_view(position: &Position) -> PositionView {
    PositionView {
        underlying: position.underlying.clone(),
        quantity: position.quantity,
        avg_price: position.avg_price,
    }
}

/// Map the serving state's store reading onto the view's optional fields.
/// An unobserved reading reports nothing, so a server that never observed
/// the store serves the pre-store payload byte-identically; any observed
/// reading reports all three facts together — even one that happens to
/// equal the defaults — because a half-reported reading would be worse
/// than none, and a genuine reading must never read as no reading.
fn store_fields(reading: StoreHealth) -> (Option<bool>, Option<u64>, Option<u64>) {
    if reading.observed {
        (
            Some(reading.reachable),
            Some(reading.open_gaps),
            reading.watermark_age_secs,
        )
    } else {
        (None, None, None)
    }
}

/// Map one tape-miss kind onto its stable wire tag. See [`direction_tag`]:
/// the cast is one-way, and a new kind breaks the build until its tag is
/// decided — tags can never silently drift.
fn tape_miss_tag(kind: TapeMissKind) -> &'static str {
    match kind {
        TapeMissKind::NoTape => "NoTape",
        TapeMissKind::UnsupportedTimeframe => "UnsupportedTimeframe",
        TapeMissKind::UnknownSymbol => "UnknownSymbol",
        TapeMissKind::Denied => "Denied",
        TapeMissKind::Store => "Store",
    }
}

/// Map the tape-serve counters onto the view's optional fields. An
/// unobserved snapshot reports nothing, so a server that never read through
/// its tape serves the pre-tape payload byte-identically; any observed
/// snapshot reports every counter together — even ones that happen to be
/// zero — because a half-reported tally would be worse than none. The last
/// miss rides only when a miss was observed: a serve-only window names no
/// fault. Field order follows the view's alphabetical field order.
struct TapeFieldSet {
    denied: Option<u64>,
    last_miss: Option<String>,
    no_tape: Option<u64>,
    served: Option<u64>,
    store_error: Option<u64>,
    unknown_symbol: Option<u64>,
    unsupported_timeframe: Option<u64>,
}

fn tape_fields(snapshot: TapeServeSnapshot) -> TapeFieldSet {
    if snapshot == TapeServeSnapshot::default() {
        TapeFieldSet {
            denied: None,
            last_miss: None,
            no_tape: None,
            served: None,
            store_error: None,
            unknown_symbol: None,
            unsupported_timeframe: None,
        }
    } else {
        TapeFieldSet {
            denied: Some(snapshot.denied),
            last_miss: snapshot
                .last_miss
                .map(|kind| tape_miss_tag(kind).to_owned()),
            no_tape: Some(snapshot.no_tape),
            served: Some(snapshot.served),
            store_error: Some(snapshot.store_error),
            unknown_symbol: Some(snapshot.unknown_symbol),
            unsupported_timeframe: Some(snapshot.unsupported_timeframe),
        }
    }
}

/// Liveness plus command-plane status: the trading state (halt/resume) and
/// the last operator command ride the existing health route — additive
/// fields, no new route, no new auth. The DTO's alphabetical field order
/// reproduces the historical sorted-key bytes exactly (golden-pinned). The
/// store facts ride the same builder the gateway reads, so the route and
/// the terminal cannot drift. Entry gating composes the venue's
/// serving-path functions, so the route and the preview refusal read one
/// verdict.
pub(crate) fn health_view(state: &Shared) -> HealthView {
    let trading = state.trading_state.get();
    let reading: StoreHealth = *common_core::sync::lock(&state.store_health);
    let (store_reachable, store_open_gaps, store_watermark_age_secs) = store_fields(reading);
    let tape = tape_fields(state.tape_serve_stats.snapshot());
    let ingest = state.ingest_health();
    HealthView {
        allows_entries: serving_allows_entries(trading, reading),
        allows_exits: trading.allows(true),
        last_command: common_core::sync::lock(&state.last_command).clone(),
        status: "ok".to_owned(),
        store_open_gaps,
        store_reachable,
        store_watermark_age_secs,
        tape_denied: tape.denied,
        tape_last_miss: tape.last_miss,
        tape_no_tape: tape.no_tape,
        tape_served: tape.served,
        tape_store_error: tape.store_error,
        tape_unknown_symbol: tape.unknown_symbol,
        tape_unsupported_timeframe: tape.unsupported_timeframe,
        trading_state: trading_state_tag(trading).to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        ingest_open_gaps: ingest.as_ref().map(|health| health.open_gaps as u64),
        ingest_watermark_age_secs: ingest
            .as_ref()
            .and_then(|health| health.watermark_age_ms)
            .map(|ms| ms / 1_000),
        ingest_last_repair: ingest.and_then(|health| health.last_repair),
    }
}

/// Project an armed grant's worst-case exposure into the view DTO.
///
/// The numbers come from `worst_case_for`: per-structure unit economics when
/// the boot chain history can price every permitted structure, else the
/// budget fields. `basis` names which, so the confirmation screen never shows
/// a budget echo as a computed exposure.
pub(crate) fn worst_case_of(state: &AppState, grant: &Grant) -> WorstCase {
    let units = chain_units_of(state, grant);
    let computed = worst_case_for(
        grant,
        &grant.scope.structures,
        |structure| {
            units
                .iter()
                .find(|entry| entry.0 == structure)
                .map(|entry| entry.1)
        },
        structure_legs,
    );
    WorstCase {
        max_trades: computed.max_trades,
        max_contracts: computed.max_contracts,
        max_loss: computed.max_loss,
        max_notional: computed.max_notional,
        basis: computed.basis.as_str().to_owned(),
    }
}

/// Leg contracts per package unit. The zero that a stray structure could
/// produce is clamped by `worst_case_for`, never divided by here.
pub(crate) fn structure_legs(structure: StructureKind) -> u32 {
    match structure {
        StructureKind::Single | StructureKind::NakedShort => 1,
        StructureKind::Vertical | StructureKind::Calendar => 2,
        StructureKind::Condor => 4,
    }
}

/// Price one representative package per permitted structure from the boot
/// chain history, so an armed screen shows a computed exposure rather than a
/// budget echo. A missing chain or boot history simply leaves the map empty.
fn chain_units_of(state: &AppState, grant: &Grant) -> Vec<(StructureKind, WorstCaseUnit)> {
    let Some(chain) = state.chain_history.as_ref() else {
        return Vec::new();
    };
    let Some(bar) = state.history.last() else {
        return Vec::new();
    };
    chain_units(grant, chain.quotes(), bar)
}

/// The pure core of [`chain_units_of`]: for every permitted structure, the
/// worst-case unit over the grant's directions and both credit intents,
/// priced with the pinned `LegSelector` + `spread_economics`. A structure is
/// omitted unless *every* variant prices — an unpriceable variant could hide
/// a larger loss, so the honest answer is to fall back to the budget echo.
pub(crate) fn chain_units(
    grant: &Grant,
    quotes: &[ChainQuote],
    bar: &Bar,
) -> Vec<(StructureKind, WorstCaseUnit)> {
    let snapshot: Vec<ChainQuote> = quotes
        .iter()
        .filter(|quote| quote.timestamp <= bar.ts)
        .cloned()
        .collect();
    if snapshot.is_empty() {
        return Vec::new();
    }
    let Some(underlying) = grant.scope.underlyings.iter().find(|name| {
        let symbol = name.as_str();
        snapshot
            .iter()
            .any(|quote| quote.contract.underlying.as_str() == symbol)
    }) else {
        return Vec::new();
    };
    let selector = LegSelector::new().build();
    let mut units = Vec::new();
    for &structure in &grant.scope.structures {
        let mut worst: Option<WorstCaseUnit> = None;
        let mut every_variant_priced = true;
        for &direction in &grant.scope.directions {
            for credit in [false, true] {
                let request = LegRequest {
                    structure,
                    direction,
                    credit,
                    strike_policy: StrikePolicy::NearestToTouch,
                    dte_days: grant.trade.min_dte_days,
                    // A worst-case bound prices every permitted structure:
                    // the window that admits a trade is the grant gate's
                    // call at decision time, not this projection's.
                    max_dte_days: None,
                    underlying,
                    touch: Money::new(bar.c),
                    as_of: bar.ts.date_naive(),
                };
                let priced = selector
                    .select_directional(&request, &snapshot)
                    .ok()
                    .and_then(|legs| {
                        spread_economics(
                            &legs,
                            &snapshot,
                            Qty::new(rust_decimal::Decimal::ONE),
                            Money::zero(),
                        )
                        .ok()
                    })
                    .map(|economics| WorstCaseUnit {
                        max_loss: economics.max_loss_per_unit,
                        notional: economics.capital_requirement,
                    });
                match priced {
                    Some(unit) => {
                        if worst.is_none_or(|best| {
                            unit.max_loss.as_decimal() > best.max_loss.as_decimal()
                        }) {
                            worst = Some(unit);
                        }
                    }
                    None => every_variant_priced = false,
                }
            }
        }
        if every_variant_priced {
            if let Some(unit) = worst {
                units.push((structure, unit));
            }
        }
    }
    units
}

/// Whether `next` only tightens `current`. The domain owns the rule set
/// (`Grant::tightens`, mirroring `Grant::tighten` tier for tier); this stays
/// a thin wrapper so the route's refusal carries the route's error mapping
/// and every tier the domain refuses, the route refuses.
fn is_tightening(next: &Grant, current: &Grant) -> bool {
    next.tightens(current)
}

/// Fold booked fills into a portfolio view: the same engine the live book
/// and the portfolio fold use, so same-direction fills re-average by volume
/// weight while opposite-direction fills reduce (never re-average) and full
/// reductions close the book. A submitted order without a priced fill counts
/// toward orders and fees but adds no exposure. Shared by the preview
/// publisher and the interactive replay state — one fold, two readers.
/// `trading_state` is the caller's own tag: the preview reads the live
/// handle, the replay names its mode.
pub(crate) fn portfolio_of_decisions(
    decisions: &[BarDecision],
    trading_state: &str,
) -> PortfolioView {
    let mut books: BTreeMap<String, SpreadPosition> = BTreeMap::new();
    let mut fees = rust_decimal::Decimal::ZERO;
    let mut filled_orders: u64 = 0;
    for decision in decisions {
        if !decision.submitted {
            continue;
        }
        filled_orders += 1;
        fees += decision
            .fee
            .map_or(rust_decimal::Decimal::ZERO, Money::as_decimal);
        let quantity = decision.filled_quantity;
        let (Some(direction), Some(price)) = (decision.direction, decision.fill_price) else {
            continue;
        };
        if quantity == 0 {
            continue;
        }
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(&decision.ts) else {
            continue;
        };
        let fill = PositionFill::new()
            .underlying(decision.underlying.clone())
            .side(side_for_direction(direction))
            .quantity(quantity)
            .price(price)
            .fee(decision.fee.unwrap_or_else(Money::zero))
            .trade_id(protopyne_venue::client_id_for(&decision.proposal_id, 0))
            .at(at.with_timezone(&chrono::Utc))
            .build();
        let current = books.remove(&decision.underlying);
        if let Ok((Some(position), _)) = update_from_fill(current, &fill) {
            books.insert(decision.underlying.clone(), position);
        }
    }
    let positions: Vec<PositionView> = books
        .into_iter()
        .map(|(underlying, book)| {
            let quantity = match book.side {
                PositionSide::Long => i64::from(book.quantity_abs),
                PositionSide::Short => -i64::from(book.quantity_abs),
            };
            position_view(
                &Position::new()
                    .underlying(underlying)
                    .quantity(quantity)
                    .avg_price(book.price_entry_average)
                    .build(),
            )
        })
        .collect();
    let open_contracts = positions.iter().map(|position| position.quantity).sum();
    PortfolioView {
        positions,
        filled_orders,
        total_fees: Money::new(fees),
        open_contracts,
        trading_state: trading_state.to_owned(),
        source: PORTFOLIO_SOURCE_REHEARSAL.to_owned(),
    }
}

/// Fold venue-booked fills into the portfolio view through the one aggregate
/// the report uses. Positions merge by underlying (a symbol in two attributed
/// books is one net position); `filled_orders` is the booked fill count, so
/// the view can never claim an order the venue did not book.
pub(crate) fn portfolio_of_booked(
    fills: &[BookedFill],
    trading_state: &str,
) -> Result<PortfolioView, crate::error::ServerError> {
    let aggregation = aggregate(fills)?;
    let mut merged: BTreeMap<String, (i64, rust_decimal::Decimal, rust_decimal::Decimal)> =
        BTreeMap::new();
    for open in aggregation.open_positions() {
        let quantity = match open.side {
            PositionSide::Long => i64::from(open.quantity),
            PositionSide::Short => -i64::from(open.quantity),
        };
        let weight = rust_decimal::Decimal::from(open.quantity);
        let entry = merged.entry(open.underlying).or_default();
        entry.0 += quantity;
        entry.1 += weight;
        entry.2 += open.price_entry_average.as_decimal() * weight;
    }
    let positions = merged
        .into_iter()
        .map(|(underlying, (quantity, weight, notional))| PositionView {
            underlying,
            quantity,
            avg_price: Money::new(if weight.is_zero() {
                rust_decimal::Decimal::ZERO
            } else {
                notional / weight
            }),
        })
        .collect::<Vec<_>>();
    let buckets = aggregation.buckets();
    let filled_orders = buckets.iter().map(|bucket| bucket.fills).sum();
    let fees = buckets
        .iter()
        .fold(rust_decimal::Decimal::ZERO, |total, bucket| {
            total + bucket.fees.as_decimal()
        });
    let open_contracts = positions.iter().map(|position| position.quantity).sum();
    Ok(PortfolioView {
        positions,
        filled_orders,
        total_fees: Money::new(fees),
        open_contracts,
        trading_state: trading_state.to_owned(),
        source: PORTFOLIO_SOURCE_BOOKED.to_owned(),
    })
}

/// The booked portfolio read: the venue-booked fills held in the state, folded
/// through [`portfolio_of_booked`]. An empty book is an empty view, never a
/// rehearsal projection standing in for it.
pub(crate) fn booked_portfolio_of(
    state: &Shared,
) -> Result<PortfolioView, crate::error::ServerError> {
    let fills = common_core::sync::lock(&state.booked).clone();
    let trading_state = trading_state_tag(state.trading_state.get()).to_owned();
    portfolio_of_booked(&fills, &trading_state)
}

/// Fold the journal's arm/revoke rows into the live grant. The journal is the
/// authority; the in-memory `armed` cache only mirrors this fold.
fn folded_grant(state: &AppState) -> Option<Grant> {
    state
        .journal
        .replay(&state.journal_permit)
        .ok()
        .and_then(|replayed| replay_grant(&replayed.events))
        .filter(|grant| !grant.revoked)
}

/// The live armed grant, if any. Reads the journal fold (not the cache) and
/// refreshes `armed` from it, so authority never outlives the row that
/// granted it. The gateway's patch path and the routes read through here.
pub(crate) fn live_grant(state: &Shared) -> Option<Grant> {
    let grant = folded_grant(state);
    common_core::sync::lock(&state.armed).clone_from(&grant);
    grant
}

/// Journal one authority row through the server's command journal. The
/// rehearsal journal is ephemeral, so this needs no capability scope; a
/// durable journal would demand the session permit and refuse here.
fn journal_grant(state: &AppState, kind: &str, detail: &str) -> Result<(), (StatusCode, String)> {
    state
        .journal
        .record_transition(
            &state.journal_permit,
            OPERATOR_CLIENT_ID,
            kind,
            detail,
            chrono::Utc::now(),
        )
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("grant journal refused: {error}"),
            )
        })
}

/// Journal an operator re-attestation row for the loop to fold: re-arms
/// entries for another cadence. A wrong or absent id refuses, exactly like
/// revoke — attestation names the authority it extends.
pub(crate) fn journal_attest(state: &Shared, grant_id: &str) -> Result<(), (StatusCode, String)> {
    let Some(current) = live_grant(state) else {
        return Err((StatusCode::CONFLICT, "no live grant to attest".to_owned()));
    };
    if current.id.as_str() != grant_id {
        return Err((
            StatusCode::CONFLICT,
            format!("live grant {} is not {grant_id}", current.id),
        ));
    }
    let detail = serde_json::json!({ "grant_id": grant_id }).to_string();
    journal_grant(state, COMMAND_ATTEST_KIND, &detail)
}

/// Shared arm logic for the route and the gateway: refuse a revoked grant,
/// enforce tighten-only against the live grant, journal the arm row first,
/// then let the fold install it. The `Err` status rides along so both callers
/// map the same refusal the same way.
pub(crate) fn apply_arm(
    state: &Shared,
    grant: &Grant,
) -> Result<ArmResponse, (StatusCode, String)> {
    grant
        .validate_armable()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if let Some(current) = live_grant(state) {
        if !is_tightening(grant, &current) {
            return Err((
                StatusCode::CONFLICT,
                "arming must tighten or revoke: loosening needs a new grant after revocation"
                    .to_owned(),
            ));
        }
    }
    let response = ArmResponse {
        grant_id: grant.id.clone(),
        worst_case: worst_case_of(state, grant),
        armed: true,
    };
    let detail = serde_json::to_string(grant)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    journal_grant(state, COMMAND_ARM_GRANT_KIND, &detail)?;
    live_grant(state);
    Ok(response)
}

/// Revoke the live grant by id: journal the revoke row, then re-fold. A wrong
/// or absent id refuses, so revocation names exactly the authority it ends.
pub(crate) fn revoke_grant(state: &Shared, grant_id: &str) -> Result<(), (StatusCode, String)> {
    let Some(current) = live_grant(state) else {
        return Err((StatusCode::CONFLICT, "no live grant to revoke".to_owned()));
    };
    if current.id.as_str() != grant_id {
        return Err((
            StatusCode::CONFLICT,
            format!("live grant {} is not {grant_id}", current.id),
        ));
    }
    let detail = serde_json::json!({ "grant_id": grant_id }).to_string();
    journal_grant(state, COMMAND_REVOKE_GRANT_KIND, &detail)?;
    live_grant(state);
    Ok(())
}

/// Journal a close-all command row for the loop to execute: one atomic close
/// intent per open group, submitted as exits (so they route even while
/// entries halt). The route and the gateway share this builder, so the two
/// surfaces journal identical rows; execution belongs to the loop, never to
/// the read path. A blank portfolio refuses before anything journals.
pub(crate) fn journal_close_all(
    state: &Shared,
    portfolio: &str,
) -> Result<(), (StatusCode, String)> {
    if portfolio.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "close-all needs a portfolio name".to_owned(),
        ));
    }
    journal_grant(state, COMMAND_CLOSE_ALL_KIND, &close_all_detail(portfolio))
}

/// Journal a cancel-all command row for the loop to execute: every working
/// order cancels, terminal orders stay untouched. Same shared-builder shape
/// as [`journal_close_all`].
pub(crate) fn journal_cancel_all(
    state: &Shared,
    portfolio: &str,
) -> Result<(), (StatusCode, String)> {
    if portfolio.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "cancel-all needs a portfolio name".to_owned(),
        ));
    }
    journal_grant(
        state,
        COMMAND_CANCEL_ALL_KIND,
        &cancel_all_detail(portfolio),
    )
}

/// One manual ticket awaiting the human's fill report, in display
/// vocabulary. Side rides the domain variant name; the tag is one-way —
/// nothing parses it back.
fn ticket_view_of(ticket: &ManualTicket) -> TicketView {
    TicketView {
        ticket_id: ticket.ticket_id().to_owned(),
        rendered: ticket.render(),
        underlying: ticket.intent.underlying.clone(),
        side: match ticket.intent.side {
            OrderSide::Buy => "Buy".to_owned(),
            OrderSide::Sell => "Sell".to_owned(),
        },
        quantity: ticket.intent.quantity,
        limit_price: ticket.intent.limit_price,
    }
}

/// One flagged ticket disagreement, in display vocabulary. The kind rides
/// the core variant name verbatim — the same one-way tag rule as every
/// other display enum.
fn ticket_drift_view_of(drift: &TicketDrift) -> TicketDriftView {
    TicketDriftView {
        ticket_id: drift.ticket_id.clone(),
        kind: match drift.kind {
            DriftKind::QuantityMismatch => "QuantityMismatch".to_owned(),
            DriftKind::PriceOutsideLimit => "PriceOutsideLimit".to_owned(),
            DriftKind::ImportMismatch => "ImportMismatch".to_owned(),
        },
        detail: drift.detail.clone(),
    }
}

/// Tickets still waiting for the human's fill report, in ticket order.
pub(crate) fn waiting_tickets(state: &Shared) -> Vec<TicketView> {
    common_core::sync::lock(&state.tickets)
        .unconfirmed()
        .iter()
        .map(|ticket| ticket_view_of(ticket))
        .collect()
}

/// Flagged ticket disagreements, oldest first.
pub(crate) fn ticket_drifts_of(state: &Shared) -> Vec<TicketDriftView> {
    common_core::sync::lock(&state.tickets)
        .drifts()
        .iter()
        .map(ticket_drift_view_of)
        .collect()
}

/// Issue a manual ticket through the tracker's journal-first path: validate
/// and journal the issued row, then track. A duplicate wait or a malformed
/// intent refuses before anything journals. Shared by the route and the
/// gateway, so both surfaces land the same row.
pub(crate) fn issue_ticket(
    state: &Shared,
    ticket: &serde_json::Value,
) -> Result<(), (StatusCode, String)> {
    let intent: OrderIntent = serde_json::from_value(ticket.clone()).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            format!("ticket intent malformed: {error}"),
        )
    })?;
    issue_intent(state, intent)
}

/// Journal one already-parsed intent as a waiting ticket: the tracker refuses
/// a duplicate id before anything is journaled.
fn issue_intent(state: &Shared, intent: OrderIntent) -> Result<(), (StatusCode, String)> {
    common_core::sync::lock(&state.tickets)
        .issue_journaled(
            &state.journal,
            &state.journal_permit,
            intent,
            chrono::Utc::now(),
        )
        .map(|_| ())
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))
}

/// The venue intent one draft describes, under a client id the caller minted.
///
/// This is the one place a [`TicketDraft`] becomes an [`OrderIntent`]: the
/// price is parsed by the venue's own scale-guarded money rule (the same one
/// the wire parser applies), the side and quantity carry through, and the
/// attribution names the manual surface. A blank symbol or an unparseable
/// price refuses here, naming the field — never at the terminal.
pub(crate) fn intent_of_draft(
    draft: &TicketDraft,
    client_id: String,
) -> Result<OrderIntent, (StatusCode, String)> {
    let symbol = draft.symbol.trim();
    if symbol.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "ticket intent malformed: symbol is required".to_owned(),
        ));
    }
    let limit_price: Money = draft.limit_price.trim().parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "ticket intent malformed: limit_price is not a decimal".to_owned(),
        )
    })?;
    Ok(OrderIntent::new()
        .client_id(client_id)
        .proposal_id("manual".to_owned())
        .strategy("manual".to_owned())
        .underlying(symbol.to_owned())
        .side(match draft.side {
            TicketSide::Buy => OrderSide::Buy,
            TicketSide::Sell => OrderSide::Sell,
        })
        .quantity(draft.quantity)
        .limit_price(limit_price)
        .build())
}

/// Issue one draft as a manual ticket: the core mints the identity, builds
/// the intent, and journals the waiting row.
pub(crate) fn issue_draft(state: &Shared, draft: &TicketDraft) -> Result<(), (StatusCode, String)> {
    let client_id = common_core::sync::lock(&state.tickets).next_client_id();
    let intent = intent_of_draft(draft, client_id)?;
    issue_intent(state, intent)
}

/// Confirm a ticket against the human's fill report through the tracker's
/// journal-first path: an unknown ticket refuses with no row journaled, and
/// anything the fill disagrees on flags drift on the drifts read.
pub(crate) fn confirm_ticket(
    state: &Shared,
    ticket_id: &str,
    fill: &serde_json::Value,
) -> Result<(), (StatusCode, String)> {
    let fill: ConfirmedFill = serde_json::from_value(fill.clone()).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            format!("ticket fill malformed: {error}"),
        )
    })?;
    let mut tracker = common_core::sync::lock(&state.tickets);
    if tracker
        .unconfirmed()
        .iter()
        .all(|ticket| ticket.ticket_id() != ticket_id)
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no waiting ticket {ticket_id}"),
        ));
    }
    tracker
        .confirm_journaled(
            &state.journal,
            &state.journal_permit,
            ticket_id,
            fill,
            chrono::Utc::now(),
        )
        .map(|_| ())
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
    Ok(())
}

/// One posted read-only import snapshot: data, never authority — the tracker
/// never moves on a failed read, and parsing the snapshot never touches it.
struct PostedImport {
    /// Position lines the operator posted.
    lines: Vec<ImportedPosition>,
}

impl TicketRead for PostedImport {
    fn snapshot(&self) -> Result<Vec<ImportedPosition>, protopyne_venue::VenueError> {
        Ok(self.lines.clone())
    }
}

/// Apply one read-only import snapshot through the tracker's import seam and
/// report the resulting drifts. A malformed snapshot refuses before the
/// tracker is touched; a failing read reports without mutating ticket state
/// (the tracker's own guarantee, reused here — no second import path).
pub(crate) fn import_tickets(
    state: &Shared,
    snapshot: &serde_json::Value,
) -> Result<Vec<TicketDriftView>, (StatusCode, String)> {
    let lines: Vec<ImportedPosition> =
        serde_json::from_value(snapshot.clone()).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("ticket import malformed: {error}"),
            )
        })?;
    let mut tracker = common_core::sync::lock(&state.tickets);
    tracker
        .apply_import(&PostedImport { lines })
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(tracker.drifts().iter().map(ticket_drift_view_of).collect())
}

/// Parse a patch envelope's tier texts into a [`GrantPatch`]: each tier
/// rides as a boundary string and parses through its own `FromStr` codec
/// (via `FieldAccess::set_field`), so the whole-patch `from_value` re-parse
/// is gone and there is exactly one parser per tier. Anything else —
/// a non-object patch, a non-string tier, an unknown tier, an unparsable
/// document — refuses naming the tier, before the core's tighten rule.
pub(crate) fn parse_patch(
    document: &serde_json::Value,
) -> Result<GrantPatch, (StatusCode, String)> {
    let patch = document.get("patch").ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "grant patch envelope names no patch".to_owned(),
        )
    })?;
    let tiers = patch.as_object().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "grant patch must be an object of tier texts".to_owned(),
        )
    })?;
    let mut assembled = GrantPatch::default();
    for (tier, text) in tiers {
        let text = text.as_str().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("grant patch tier {tier} must be document text, not JSON"),
            )
        })?;
        assembled.set_field(tier, text).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("grant patch tier {tier} refuses: {error}"),
            )
        })?;
    }
    Ok(assembled)
}

/// Preview a patch envelope's worst case without storing anything: parse
/// the tiers, read the live grant, tighten in memory, and return the worst
/// case the patched grant would authorize. The confirmation surface renders
/// this before submit is actionable; a preview never arms, and the live
/// grant is untouched whether the patch tightens or refuses.
pub(crate) fn preview_arm_of(
    state: &Shared,
    document: &serde_json::Value,
) -> Result<WorstCase, (StatusCode, String)> {
    let patch = parse_patch(document)?;
    let current = live_grant(state)
        .ok_or_else(|| (StatusCode::CONFLICT, "no live grant to tighten".to_owned()))?;
    let next = current
        .tighten(patch)
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
    Ok(worst_case_of(state, &next))
}

/// Current tier texts of the live grant, for display beside the form's
/// editors: tier name to boundary-string document, composite tiers as JSON
/// document text and the window end as bare RFC 3339 — the same encoding a
/// patch carries back. Read from the grant's own serialized shape through
/// the patch's field list, so no tier is named twice and a new tier appears
/// with no edit here. Empty when no grant is live: placeholders stay blank
/// and the preview refuses, rather than a missing grant looking armed.
pub(crate) fn grant_tiers_of(
    state: &Shared,
) -> Result<BTreeMap<String, String>, (StatusCode, String)> {
    let Some(grant) = live_grant(state) else {
        return Ok(BTreeMap::new());
    };
    let document = serde_json::to_value(&grant)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let mut tiers = BTreeMap::new();
    for tier in GrantPatch::default().field_names() {
        let Some(value) = document.get(*tier) else {
            continue;
        };
        let text = match value.as_str() {
            Some(bare) => bare.to_owned(),
            None => serde_json::to_string(value)
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
        };
        tiers.insert((*tier).to_owned(), text);
    }
    Ok(tiers)
}

/// The one `Decimal → f64` display fence for the service: it delegates to the
/// chart-model conversion, so the route and the chart model cannot disagree
/// about what a display float means.
fn decimal_to_chart(value: rust_decimal::Decimal) -> Option<f64> {
    protopyne_chart_model::to_display_float(value)
}

/// Chart bars over history: the one headless conversion to the wire
/// container, with its shared skip policy counting drops. Shared by the
/// route and the gateway — no second field-wise mapping.
pub(crate) fn chart_bars_of(history: &[Bar]) -> ChartBars {
    rows_of(history)
}

/// Bar frame published to feed subscribers. `cycle` counts completed passes
/// over history so a resuming subscriber can tell a repeat from a stall.
pub(crate) fn bar_frame(cycle: u64, index: usize, bar: &Bar) -> Option<String> {
    Some(
        serde_json::json!({
            "type": "bar",
            "cycle": cycle,
            "index": index,
            "time": bar.ts.timestamp(),
            "open": decimal_to_chart(bar.o)?,
            "high": decimal_to_chart(bar.h)?,
            "low": decimal_to_chart(bar.l)?,
            "close": decimal_to_chart(bar.c)?,
            "volume": bar.volume,
        })
        .to_string(),
    )
}

/// Scatter compacted kernel values back to bar indices: every bar keeps its
/// index with `None` where its close had no chart float, so overlay vectors
/// always agree positionally with the bar rows. `positions[slot]` is the bar
/// index of compacted slot `slot`; the dropped count is the caller's
/// `history_len - positions.len()`.
pub(crate) fn align_overlay(
    history_len: usize,
    positions: &[usize],
    computed: &[Option<f64>],
) -> Vec<Option<f64>> {
    let mut values: Vec<Option<f64>> = vec![None; history_len];
    for (slot, bar_index) in positions.iter().enumerate() {
        values[*bar_index] = computed.get(slot).copied().flatten();
    }
    values
}

/// Overlay series over history, shared by the route and the gateway.
/// Refuses unknown kinds, out-of-range periods, and unchartable closes.
/// Per-bar closes run through the shared display-float policy: a bar whose
/// close has no chart float keeps its index with a `None` value instead of
/// shifting every later bar. The `Err` status rides along so both callers
/// map the same refusal the same way.
pub(crate) fn overlay_series_of(
    history: &[Bar],
    kind: &str,
    period: i64,
    mult: f64,
) -> Result<OverlaySeries, (StatusCode, String)> {
    if !(1..=500).contains(&period) {
        return Err((
            StatusCode::BAD_REQUEST,
            "period must be within [1, 500]".to_owned(),
        ));
    }
    let mut closes: Vec<f64> = Vec::with_capacity(history.len());
    let mut positions: Vec<usize> = Vec::with_capacity(history.len());
    for (index, bar) in history.iter().enumerate() {
        if let Some(close) = decimal_to_chart(bar.c) {
            positions.push(index);
            closes.push(close);
        }
    }
    if closes.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no chartable closes".to_owned(),
        ));
    }
    let frame = Ohlcv::close_only(&closes);
    let params = ParamSet::new().with_int("length", period);
    let computed = match kind {
        "sma" => SmaIndicator::new()
            .batch(&frame, &params)
            .map(|result| result.values),
        "ema" => EmaIndicator::new()
            .batch(&frame, &params)
            .map(|result| result.values),
        // Chartable overlaps only: `bb` serves its mid (the primary leg),
        // `trima` its single output. Momentum, volume, and statistic kernels
        // are not price-chartable and stay refused below.
        "bb" => BbIndicator::new()
            .batch(&frame, &params.clone().with_float("mult", mult))
            .map(|result| result.values),
        "trima" => TrimaIndicator::new()
            .batch(&frame, &params)
            .map(|result| result.values),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "kind must be one of 'sma', 'ema', 'bb', 'trima'".to_owned(),
            ));
        }
    }
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let values = align_overlay(history.len(), &positions, &computed);
    let dropped = (history.len() - closes.len()) as u64;
    debug_assert_eq!(values.len(), history.len());
    Ok(OverlaySeries {
        kind: kind.to_owned(),
        period,
        values,
        dropped,
    })
}

/// Bars between replay checkpoints when the route is asked for no cadence.
/// One anchor per pane-window of a session's history keeps the published step
/// list short enough to scrub while every bar stays individually reachable.
pub const DEFAULT_REPLAY_CHECKPOINT_EVERY: usize = 50;

/// Replay read over history, shared by the route and the gateway.
///
/// Composes the same research replay graph the runner builds
/// ([`build_replay`]) instead of a second step walk, so the published order,
/// the anchors, and the "reached" flags are the graph's own answers lifted
/// field-for-field. The read stands at the head: a fresh window has nothing
/// rewound, and a terminal that scrubs holds the moved cursor itself.
///
/// The `Err` status rides along so both callers map the same refusal the
/// same way.
pub(crate) fn replay_view_of(
    history: &[Bar],
    checkpoint_every: usize,
) -> Result<ReplayView, (StatusCode, String)> {
    let mut graph = build_replay(history, checkpoint_every)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    complete_replay(&mut graph);
    let keys: Vec<String> = graph.step_ids().to_vec();
    let mut steps = Vec::with_capacity(keys.len());
    for key in &keys {
        let step = graph.status(key).ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("replay step missing: {key:?}"),
            )
        })?;
        steps.push(ReplayStepView {
            bar_index: step.bar_index as u64,
            ts: step.ts.clone(),
            done: step.done,
            checkpoint: graph.is_checkpoint(key),
        });
    }
    let total_bars = steps.len() as u64;
    Ok(ReplayView {
        cursor: total_bars,
        total_bars,
        steps,
    })
}

/// Count why bars did not trade: skip reasons plus risk and grant refusal
/// reasons, most frequent first. Pure reporting — nothing authorizes or
/// denies on these counts. Shared with the preview publisher.
pub(crate) const DENY_HISTOGRAM_TOP: usize = 8;

/// See [`DENY_HISTOGRAM_TOP`]: the histogram builder.
pub(crate) fn deny_histogram_of(run: &BacktestRun) -> Vec<protopyne_view::DenyCount> {
    let mut counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for decision in &run.decisions {
        if let Some(reason) = decision.skipped.as_deref() {
            *counts.entry(reason).or_insert(0) += 1;
        }
        for reason in decision
            .risk_reasons
            .iter()
            .chain(decision.grant_merit_reasons.iter())
        {
            *counts.entry(reason.as_str()).or_insert(0) += 1;
        }
    }
    let mut histogram: Vec<protopyne_view::DenyCount> = counts
        .into_iter()
        .map(|(reason, bars)| protopyne_view::DenyCount {
            reason: reason.to_owned(),
            bars,
        })
        .collect();
    histogram.sort_by(|left, right| {
        right
            .bars
            .cmp(&left.bars)
            .then_with(|| left.reason.cmp(&right.reason))
    });
    if histogram.len() > DENY_HISTOGRAM_TOP {
        let rest: u64 = histogram[DENY_HISTOGRAM_TOP..]
            .iter()
            .map(|entry| entry.bars)
            .sum();
        histogram.truncate(DENY_HISTOGRAM_TOP);
        histogram.push(protopyne_view::DenyCount {
            reason: "other".to_owned(),
            bars: rest,
        });
    }
    histogram
}

/// Record a preview run's verdicts in the inbox and portfolio views.
///
/// The inbox is a projection of each decision's carried lifecycle: the
/// attribution comes from the proposal itself, and the state is the final
/// journaled transition's target. Nothing here is fabricated. `chain` is the
/// run's own chain history, so a booked package carries the basis it marks
/// from — the same facts the run's report names.
pub(crate) fn publish_run(
    state: &Shared,
    run: &BacktestRun,
    grant_id: &str,
    chain: &[protopyne_types::ChainQuote],
) {
    let mut inbox = common_core::sync::lock(&state.inbox);
    inbox.clear();
    for decision in &run.decisions {
        let Some(direction) = decision.direction else {
            continue;
        };
        let state_tag = decision
            .transitions
            .last()
            .map_or(ProposalState::Proposed, |transition| transition.to);
        inbox.push(ProposalView {
            id: decision.proposal_id.clone(),
            strategy: decision.strategy.clone(),
            underlying: decision.underlying.clone(),
            direction: direction_tag(direction).to_owned(),
            structure: decision.structure.map_or_else(
                || "unrecorded".to_owned(),
                |kind| structure_tag(kind).to_owned(),
            ),
            limit_price: decision.limit_price,
            conviction: decision.conviction,
            state: proposal_state_tag(state_tag).to_owned(),
        });
    }
    drop(inbox);
    let trading_state = trading_state_tag(state.trading_state.get()).to_owned();
    let folded = portfolio_of_decisions(&run.decisions, &trading_state);
    let mut portfolio = common_core::sync::lock(&state.portfolio);
    *portfolio = folded;
    drop(portfolio);
    // The booked fold is the authoritative portfolio source: the same venue
    // fills the report aggregates. A decision that cannot book leaves the
    // last booked fold intact rather than inventing a fill.
    match crate::runner::booked_fills_of(&run.decisions, PAPER_PORTFOLIO, grant_id, chain) {
        Ok(booked) => *common_core::sync::lock(&state.booked) = booked,
        Err(error) => tracing::error!(%error, "preview booked fold refused"),
    }
}

/// Journaled lifecycle events in sequence order, as the terminal's journal
/// rows. A pure projection: the row is the event's own fields, never
/// re-derived.
pub(crate) fn journal_rows_of(events: &[JournalEvent]) -> Vec<JournalRowView> {
    events
        .iter()
        .map(|event| JournalRowView {
            seq: event.seq,
            client_id: event.client_id.clone(),
            kind: event.kind.clone(),
            detail: event.detail.clone(),
            at_ms: event.at_ms,
        })
        .collect()
}

/// The pending-approval inbox from journaled rows alone: proposal bodies
/// joined with their latest states, filtered to those awaiting a human (the
/// actionable set). A restart serves exactly this, never a second in-memory
/// authority.
pub(crate) fn pending_proposals_of(events: &[JournalEvent]) -> Vec<ProposalView> {
    replay_proposals(events)
        .into_iter()
        .filter(|entry| entry.state == ProposalState::PendingHuman)
        .map(|entry| proposal_view_of(&entry.proposal, entry.state))
        .collect()
}

/// Every journaled proposal, bodies joined with their latest transition
/// states — the authoritative inbox. Terminal rows are included: the inbox
/// split decides affordances, never this fold.
pub(crate) fn journaled_proposals_of(events: &[JournalEvent]) -> Vec<ProposalView> {
    replay_proposals(events)
        .into_iter()
        .map(|entry| proposal_view_of(&entry.proposal, entry.state))
        .collect()
}

/// One proposal body plus its journaled state as the wire row. The single
/// body-to-row projection both journaled inbox folds read.
fn proposal_view_of(proposal: &protopyne_strategy::Proposal, state: ProposalState) -> ProposalView {
    ProposalView {
        id: proposal.id.clone(),
        strategy: proposal.strategy.clone(),
        underlying: proposal.underlying.clone(),
        direction: direction_tag(proposal.direction).to_owned(),
        structure: structure_tag(proposal.structure).to_owned(),
        limit_price: proposal.limit_price,
        conviction: proposal.conviction,
        state: proposal_state_tag(state).to_owned(),
    }
}

/// Journal one human decision on a journaled proposal: rebuild the body,
/// validate the move, and journal the transition through the command plane's
/// own `decide_proposal`. Shared by the gateway verb and the route, so a
/// decision through either lands the same row.
pub(crate) fn decide_journaled(
    state: &Shared,
    proposal_id: &str,
    event: ProposalEvent,
    reason: &str,
) -> Result<(), (StatusCode, String)> {
    let mut proposals = state
        .journal
        .replay_proposals(&state.journal_permit)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let mut proposal = proposals.remove(proposal_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("no journaled proposal {proposal_id}"),
        )
    })?;
    decide_proposal(
        &state.journal,
        &state.journal_permit,
        &mut proposal,
        event,
        reason,
        chrono::Utc::now(),
    )
    .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
    Ok(())
}

/// One broker's capability view.
#[must_use]
pub fn broker_capabilities_of(bundle: &BrokerBundle) -> BrokerCapabilityView {
    BrokerCapabilityView {
        venue: bundle.id.to_string(),
        streams: bundle.capabilities.streams,
        accounts: bundle.capabilities.accounts,
        orders: bundle.capabilities.orders,
        deals: bundle.capabilities.deals,
        auth_refresh: bundle.capabilities.auth_refresh,
        native_bars: bundle.capabilities.native_bars,
    }
}

/// One account summary view.
#[must_use]
pub fn broker_account_of(venue: &str, summary: &AccountSummary) -> BrokerAccountView {
    BrokerAccountView {
        venue: venue.to_owned(),
        account: summary.account.clone(),
        currency: summary.currency.clone(),
        net_liquid: summary.net_liquid,
        buying_power: summary.buying_power,
        margin_used: summary.margin_used,
        observed_at: summary.observed_at,
    }
}

/// One order view, reusing the venue order record.
#[must_use]
pub fn broker_order_of(venue: &str, account: &str, record: &OrderRecord) -> BrokerOrderView {
    let legs = record
        .intent
        .legs
        .as_ref()
        .map(|legs| {
            legs.legs()
                .iter()
                .map(|leg| BrokerLegView {
                    contract: leg.contract.occ_symbol(),
                    side: match leg.side {
                        protopyne_types::LegSide::Long => "Long",
                        protopyne_types::LegSide::Short => "Short",
                    }
                    .to_owned(),
                    quantity: leg.ratio,
                    price: record.intent.limit_price,
                })
                .collect()
        })
        .unwrap_or_default();
    BrokerOrderView {
        venue: venue.to_owned(),
        account: account.to_owned(),
        client_id: record.intent.client_id.clone(),
        proposal_id: record.intent.proposal_id.clone(),
        strategy: record.intent.strategy.clone(),
        underlying: record.intent.underlying.clone(),
        side: format!("{:?}", record.intent.side),
        status: format!("{:?}", record.status),
        quantity: record.intent.quantity,
        filled_quantity: record.filled_quantity,
        limit_price: record.intent.limit_price,
        avg_price: record.avg_price,
        fees_paid: record.fees_paid,
        updated_at: record.updated_at,
        legs,
    }
}

/// One deal view, reusing the deal's own [`Fill`](protopyne_venue::Fill).
#[must_use]
pub fn broker_deal_of(venue: &str, account: &str, deal: &BrokerDeal) -> BrokerDealView {
    BrokerDealView {
        venue: venue.to_owned(),
        account: account.to_owned(),
        deal_id: deal.deal_id.clone(),
        client_id: deal.client_id.clone(),
        order_id: deal.order_id.clone(),
        contract: deal
            .contract
            .as_ref()
            .map(|contract| contract.as_str().to_owned()),
        underlying: deal.underlying.clone(),
        side: format!("{:?}", deal.side),
        quantity: deal.fill.quantity,
        price: deal.fill.price,
        fee: deal.fill.fee,
        executed_at: deal.executed_at,
        observed_at: deal.observed_at,
    }
}

/// Backtest result contract: the terminal's author/run/read surface. The
/// route and the gateway share these builders, so the HTTP body and the
/// in-process read cannot drift. Every money value rides decimal text; every
/// refusal names the field that refused.
///
/// The composition description plus the registry enumerations a form picker
/// must offer. Data only: no strategy type crosses into the view.
#[must_use]
pub fn backtest_schema_of() -> BacktestSchemaView {
    use protopyne_strategy::integration_lite::IntegrationLiteSpec;
    use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
    let spec = IntegrationLiteSpec::from(&IntegrationLiteStrategyConfig::default());
    // The terminal seeds its working document from these defaults and
    // submits the edited document: no field is ever invented client-side,
    // and a refused value names its field on the run call.
    let defaults = serde_json::to_value(&spec).unwrap_or(serde_json::Value::Null);
    BacktestSchemaView {
        schema: serde_json::json!({
            "indicator": spec.indicator.describe(),
            "entry": spec.entry.describe(),
            "exit": spec.exit.describe(),
            "risk": spec.risk.describe(),
            "session": spec.session.describe(),
            "display": spec.display.describe(),
            "triggers": protopyne_strategy::slots::trigger::trigger_ids(),
            "filters": protopyne_strategy::slots::filter::filter_ids(),
            "sizers": protopyne_strategy::slots::sizing::sizing_ids(),
            "strategies": protopyne_strategy::slots::composition::strategy_ids(),
            "defaults": defaults,
        }),
    }
}

/// Deterministic run id over the request: the same spec, seed, equity, and
/// window file under the same id, so a repeated submit reads back the stored
/// run instead of forking a duplicate.
fn backtest_run_id(params: &BacktestRunParams) -> String {
    let digest = common_core::hash::fingerprint_framed(&[
        (
            "spec",
            &serde_json::to_vec(&params.spec).unwrap_or_default(),
        ),
        ("seed", &params.seed.to_le_bytes()),
        ("initial_equity", params.initial_equity.as_bytes()),
        ("start", params.start.as_deref().unwrap_or("").as_bytes()),
        ("end", params.end.as_deref().unwrap_or("").as_bytes()),
    ]);
    format!("backtest-{digest:016x}")
}

/// Parse one RFC3339 window bound, naming the field on refusal.
fn backtest_window_bound(
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, (StatusCode, String)> {
    match value {
        None => Ok(None),
        Some(text) => text
            .parse::<chrono::DateTime<chrono::Utc>>()
            .map(Some)
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("backtest spec refused: {field} is not RFC3339 ({text})"),
                )
            }),
    }
}

fn money_text(
    field: &'static str,
    value: &str,
) -> Result<protopyne_types::Money, (StatusCode, String)> {
    value
        .parse::<rust_decimal::Decimal>()
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("backtest spec refused: {field} is not decimal text ({value})"),
            )
        })
        .map(protopyne_types::Money::new)
}

/// One labelled metric; an unmeasurable value reads `None`, never zero.
fn metric(label: &str, value: Option<f64>) -> BacktestMetricView {
    BacktestMetricView {
        label: label.to_owned(),
        value: value.map(|number| number.to_string()),
    }
}

/// One count metric: integer text, so no float enters and no display fence
/// is needed.
fn metric_of_count(label: &str, value: u64) -> BacktestMetricView {
    BacktestMetricView {
        label: label.to_owned(),
        value: Some(value.to_string()),
    }
}

fn metric_of_money(label: &str, value: protopyne_types::Money) -> BacktestMetricView {
    BacktestMetricView {
        label: label.to_owned(),
        value: Some(value.to_string()),
    }
}

/// Project one stored run's report into labelled metrics.
fn backtest_metrics_of(report: &crate::report::BacktestReport) -> Vec<BacktestMetricView> {
    vec![
        metric_of_count("bars", report.bars),
        metric_of_count("proposals", report.proposals),
        metric_of_count("fills", report.fills),
        metric("fill_rate", report.fill_rate),
        metric_of_money("total_fees", report.total_fees),
        metric_of_money("filled_notional", report.filled_notional),
        metric_of_money("drawdown", report.drawdown),
        metric("cum_return", report.cum_return),
        metric("sharpe", report.sharpe),
        metric("sortino", report.sortino),
        metric("calmar", report.calmar),
        metric("win_rate", report.win_rate),
        metric("profit_factor", report.profit_factor),
        metric(
            "expectancy",
            report.trades.as_ref().and_then(|trades| trades.expectancy),
        ),
        metric(
            "win_rate_trades",
            report
                .trades
                .as_ref()
                .and_then(|trades| trades.win_rate_trades),
        ),
        metric(
            "profit_factor_trades",
            report
                .trades
                .as_ref()
                .and_then(|trades| trades.profit_factor_trades),
        ),
    ]
}

/// Project one run into its stored view: metrics, equity curve, trades,
/// markers, and the final status row. Bars supply the timestamps the run
/// itself does not retain.
fn backtest_run_view_of(
    run_id: &str,
    run: &crate::equity::EquityRun,
    bars: &[protopyne_types::Bar],
) -> BacktestRunView {
    let at = |index: usize| {
        bars.get(index)
            .map(|bar| bar.ts.to_rfc3339())
            .unwrap_or_default()
    };
    let equity = run
        .report
        .equity_curve
        .iter()
        .enumerate()
        .map(|(index, point)| EquityPointView {
            at: at(index),
            equity: point.to_string(),
        })
        .collect();
    let mut markers: Vec<BacktestMarkerView> = Vec::new();
    for trade in &run.trades {
        markers.push(BacktestMarkerView {
            at: at(trade.entry_bar),
            price: protopyne_types::Money::new(
                rust_decimal::Decimal::from_f64_retain(trade.entry_price)
                    .unwrap_or(rust_decimal::Decimal::ZERO),
            )
            .to_string(),
            kind: "entry".to_owned(),
        });
        markers.push(BacktestMarkerView {
            at: at(trade.exit_bar),
            price: protopyne_types::Money::new(
                rust_decimal::Decimal::from_f64_retain(trade.exit_price)
                    .unwrap_or(rust_decimal::Decimal::ZERO),
            )
            .to_string(),
            kind: "exit".to_owned(),
        });
    }
    for partial in &run.partials {
        markers.push(BacktestMarkerView {
            at: at(partial.exit_bar),
            price: protopyne_types::Money::new(
                rust_decimal::Decimal::from_f64_retain(partial.exit_price)
                    .unwrap_or(rust_decimal::Decimal::ZERO),
            )
            .to_string(),
            kind: "exit".to_owned(),
        });
    }
    // Resting stop/target points: one marker per bar while the line rests, so
    // the terminal draws one polyline per kind and breaks it wherever a bar
    // carries no point.
    for level in &run.levels {
        markers.push(BacktestMarkerView {
            at: at(level.bar),
            price: protopyne_types::Money::new(
                rust_decimal::Decimal::from_f64_retain(level.price)
                    .unwrap_or(rust_decimal::Decimal::ZERO),
            )
            .to_string(),
            kind: match level.kind {
                crate::equity::LevelKind::Stop => "stop",
                crate::equity::LevelKind::Target => "target",
            }
            .to_owned(),
        });
    }
    markers.sort_by(|left, right| left.at.cmp(&right.at));
    BacktestRunView {
        run_id: run_id.to_owned(),
        manifest_json: run.report.manifest.to_canonical_json(),
        metrics: backtest_metrics_of(&run.report),
        equity,
        trades: run
            .trades
            .iter()
            .map(|trade| protopyne_view::BacktestTradeView {
                long: trade.long,
                quantity: trade.qty,
                entry_at: at(trade.entry_bar),
                entry_price: protopyne_types::Money::new(
                    rust_decimal::Decimal::from_f64_retain(trade.entry_price)
                        .unwrap_or(rust_decimal::Decimal::ZERO),
                )
                .to_string(),
                exit_at: at(trade.exit_bar),
                exit_price: protopyne_types::Money::new(
                    rust_decimal::Decimal::from_f64_retain(trade.exit_price)
                        .unwrap_or(rust_decimal::Decimal::ZERO),
                )
                .to_string(),
                pnl: protopyne_types::Money::new(
                    rust_decimal::Decimal::from_f64_retain(trade.pnl)
                        .unwrap_or(rust_decimal::Decimal::ZERO),
                )
                .to_string(),
                bars_held: trade.bars_held,
                exit_reason: trade.exit_reason.clone(),
            })
            .collect(),
        markers,
        status: run.status.last().cloned(),
        warnings: run.report.data_limits.clone(),
    }
}

/// The stored-run read one route and the gateway share.
pub(crate) fn backtest_run_view_of_entry(
    entry: &crate::backtest_registry::BacktestEntry,
) -> BacktestRunView {
    entry.view.clone()
}

/// Validate the run params, run the tape, project the view, and store it.
/// The stored bytes are what `GET /backtest/{id}` and the gateway's
/// `backtest_state` both read.
pub(crate) async fn backtest_run_of(
    state: &Shared,
    params: BacktestRunParams,
) -> Result<BacktestRunRef, (StatusCode, String)> {
    use protopyne_strategy::integration_lite::{
        IntegrationLiteSpec, IntegrationLiteStrategyConfig,
    };
    let spec: IntegrationLiteSpec =
        serde_json::from_value(params.spec.clone()).map_err(|refused| {
            (
                StatusCode::BAD_REQUEST,
                format!("backtest spec refused: {refused}"),
            )
        })?;
    let strategy: IntegrationLiteStrategyConfig = spec.legacy.clone();
    let indicator = spec.indicator.clone();
    let grant: protopyne_strategy::Grant = match params.grant.clone() {
        Some(document) => serde_json::from_value(document).map_err(|refused| {
            (
                StatusCode::BAD_REQUEST,
                format!("backtest grant refused: {refused}"),
            )
        })?,
        None => protopyne_strategy::Grant::preset(protopyne_strategy::GrantPreset::Rehearsal {
            id: "backtest-permissive".to_owned(),
            structures: vec![protopyne_strategy::proposal::StructureKind::Single],
        }),
    };
    grant.validate().map_err(|refused| {
        (
            StatusCode::BAD_REQUEST,
            format!("backtest grant refused: {refused}"),
        )
    })?;
    let start = backtest_window_bound("start", params.start.as_deref())?;
    let end = backtest_window_bound("end", params.end.as_deref())?;
    if start.is_some_and(|bound| end.is_some_and(|stop| stop < bound)) {
        return Err((
            StatusCode::BAD_REQUEST,
            "backtest spec refused: end precedes start".to_owned(),
        ));
    }
    let bars: Vec<protopyne_types::Bar> = state
        .history
        .iter()
        .filter(|bar| {
            start.is_none_or(|bound| bar.ts >= bound) && end.is_none_or(|bound| bar.ts <= bound)
        })
        .cloned()
        .collect();
    let underlying = grant
        .scope
        .underlyings
        .first()
        .cloned()
        .unwrap_or_else(|| "SPY".to_owned());
    // Input parsing, not display: decimal text becomes the runner's opening
    // equity through one fallible conversion, and an unrepresentable value
    // refuses naming the field.
    let equity = money_text("initial_equity", &params.initial_equity)?
        .as_decimal()
        .to_string()
        .parse::<f64>()
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "backtest spec refused: initial_equity is not representable".to_owned(),
            )
        })?;
    let run_id = backtest_run_id(&params);
    let venue_config = crate::backtest::BacktestConfig::new()
        .underlying(underlying.clone())
        .data_version(format!("backtest-{run_id}"))
        .build();
    let runner_config = crate::runner::RunnerConfig::new(
        underlying.clone(),
        "integration_lite".to_owned(),
        protopyne_types::Money::from(400),
        protopyne_types::Money::from(2_000),
        0,
        0,
    );
    let timeframe = protopyne_types::Timeframe::infer_or_d1(&bars);
    let run = crate::equity::BacktestRequest::builder()
        .indicator(&indicator)
        .strategy(&strategy)
        .risk(&protopyne_strategy::RiskEngine::new(
            protopyne_strategy::RiskLimits::new().build(),
        ))
        .grant(&grant)
        .venue_config(&venue_config)
        .runner_config(&runner_config)
        .signal_name("integration_lite")
        .underlying(&underlying)
        .timeframe(timeframe)
        .bars(&bars)
        .seed(params.seed)
        .initial_equity(equity)
        .run()
        .await
        .map_err(|error| (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    let view = backtest_run_view_of(&run_id, &run, &bars);
    publish_backtest(
        state,
        crate::backtest_registry::BacktestEntry {
            run_id: run_id.clone(),
            view,
        },
    );
    Ok(BacktestRunRef { run_id })
}

/// Store one projected view under its run id (the registry write behind a
/// run that was projected elsewhere, e.g. a test double).
pub(crate) fn publish_backtest(state: &Shared, entry: crate::backtest_registry::BacktestEntry) {
    common_core::sync::lock(&state.backtests).publish(entry);
}

#[cfg(test)]
#[path = "../tests/conversion_guard.rs"]
mod conversion_guard_tests;
