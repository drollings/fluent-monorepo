//! Operator command execution: fold journaled close/cancel commands and run
//! them through the session's order path.
//!
//! The route and the gateway journal only (like approve/reject); the live
//! loop calls [`execute_operator_commands`] once per bar, after the quote
//! resolves and before the entry body runs — so a halted book still closes
//! while entries refuse. The cursor is the last executed journal seq: audit
//! rows carry their command's seq, so a restart never re-executes a command.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fluent_concurrency::scope::scoped;

use protopyne_strategy::{
    Direction, FillOutcome, FillStatus, GrantLedger, OrderShape, Proposal, ProposalSide,
    ProposalState, RiskEngine, RiskState, StructureKind,
};
use protopyne_types::{Money, Qty};
use protopyne_venue::{
    cancel_all_ids, close_all_intents, open_groups_of, Journal, JournalEvent, TradingSession,
    Venue, VenueStorage, COMMAND_ATTEST_KIND, COMMAND_CANCEL_ALL_KIND, COMMAND_CLOSE_ALL_KIND,
    OPERATOR_CLIENT_ID,
};
use rust_decimal::Decimal;

/// Fold journaled operator commands newer than `done_through` and execute
/// them in seq order, returning the new cursor. Unknown rows pass through;
/// audit rows (carrying their command's seq) only move the cursor.
///
/// Close-all groups the registry book through the one grouping
/// implementation, emits one atomic intent per group, mints a risk token
/// for the summed exit proposal, submits through the safe path, and books
/// each fill into the ledger's lifetime budgets. A close commits no new
/// loss (the position's loss was booked at entry), so the token proposal
/// carries zero max loss — the per-trade loss cap stays meaningful for
/// entries, never a bar on exiting. Cancel-all sweeps working orders and
/// touches no budget: a cancel consumes nothing, so there is no claim to
/// settle or release.
///
/// Every outcome journals an audit row before the cursor advances: success
/// and refusal alike are durable, so a crash replays to the same cursor.
pub(crate) async fn execute_operator_commands(
    session: &TradingSession,
    journal: &Journal,
    permit: &VenueStorage,
    ledger: &GrantLedger,
    risk: &RiskEngine,
    risk_state: &RiskState,
    quote: &protopyne_venue::Quote,
    now: DateTime<Utc>,
    done_through: i64,
) -> Result<i64, protopyne_venue::VenueError> {
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    scoped(
        caps,
        execute_scoped(
            session,
            journal,
            permit,
            ledger,
            risk,
            risk_state,
            quote,
            now,
            done_through,
        ),
    )
    .await
}

async fn execute_scoped(
    session: &TradingSession,
    journal: &Journal,
    permit: &VenueStorage,
    ledger: &GrantLedger,
    risk: &RiskEngine,
    risk_state: &RiskState,
    quote: &protopyne_venue::Quote,
    now: DateTime<Utc>,
    done_through: i64,
) -> Result<i64, protopyne_venue::VenueError> {
    let replayed = journal
        .replay(permit)
        .map_err(|error| protopyne_venue::VenueError::Journal(error.to_string()))?;
    let mut commands: Vec<_> = replayed
        .events
        .iter()
        .filter(|event| {
            event.seq > done_through
                && (event.kind == COMMAND_CLOSE_ALL_KIND
                    || event.kind == COMMAND_CANCEL_ALL_KIND
                    || event.kind == COMMAND_ATTEST_KIND)
                && !is_audit(&event.detail)
        })
        .collect();
    commands.sort_by_key(|event| event.seq);
    let mut cursor = done_through;
    for command in commands {
        let portfolio = command_portfolio(&command.detail);
        if command.kind == COMMAND_CLOSE_ALL_KIND {
            cursor = execute_close(
                session,
                journal,
                permit,
                ledger,
                risk,
                risk_state,
                quote,
                now,
                command.seq,
                portfolio,
            )
            .await?;
        } else if command.kind == COMMAND_ATTEST_KIND {
            cursor = execute_attest(ledger, command, command.seq);
        } else {
            cursor = execute_cancel(session, journal, permit, now, command.seq, portfolio).await?;
        }
    }
    Ok(cursor)
}

/// Fold one journaled attest row into the ledger: the row's own time becomes
/// the usage's last attestation (last-wins through [`observe_attested`](GrantLedger::observe_attested),
/// so a restart replays to the same instant), and only rows naming the
/// ledger's current grant apply — a stale row for a retired grant generation
/// is skipped, never misattributed.
pub(crate) fn execute_attest(ledger: &GrantLedger, command: &JournalEvent, seq: i64) -> i64 {
    #[derive(serde::Deserialize)]
    struct AttestDetail {
        grant_id: String,
    }
    let matches_live = serde_json::from_str::<AttestDetail>(&command.detail)
        .ok()
        .is_some_and(|body| body.grant_id == ledger.grant().id);
    if matches_live {
        if let Some(at) = DateTime::from_timestamp_millis(command.at_ms) {
            ledger.observe_attested(at);
        }
    }
    seq
}

/// An audit row carries its command's seq; command rows never do.
fn is_audit(detail: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(detail)
        .ok()
        .and_then(|value| value.get("command_seq").cloned())
        .is_some_and(|seq| seq.is_number())
}

/// The portfolio a command row scopes to; blank or malformed rows read as
/// absent and refuse at execution.
fn command_portfolio(detail: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(detail)
        .ok()
        .and_then(|value| value.get("portfolio")?.as_str().map(str::to_owned))
        .filter(|portfolio| !portfolio.trim().is_empty())
}

async fn execute_close(
    session: &TradingSession,
    journal: &Journal,
    permit: &VenueStorage,
    ledger: &GrantLedger,
    risk: &RiskEngine,
    risk_state: &RiskState,
    quote: &protopyne_venue::Quote,
    now: DateTime<Utc>,
    seq: i64,
    portfolio: Option<String>,
) -> Result<i64, protopyne_venue::VenueError> {
    let Some(portfolio) = portfolio else {
        return refuse(
            journal,
            permit,
            COMMAND_CLOSE_ALL_KIND,
            seq,
            "close-all",
            "no portfolio named",
            now,
        );
    };
    let Some(spread) = session.registry().open_spread_positions() else {
        return refuse(
            journal,
            permit,
            COMMAND_CLOSE_ALL_KIND,
            seq,
            &portfolio,
            "spread positions unreported: cannot group closes fail-closed",
            now,
        );
    };
    let indexed: HashMap<String, protopyne_venue::SpreadPosition> = spread
        .into_iter()
        .enumerate()
        .map(|(index, position)| (index.to_string(), position))
        .collect();
    let groups = open_groups_of(session.registry().positions(), &indexed);
    if groups.is_empty() {
        audit(
            journal,
            permit,
            COMMAND_CLOSE_ALL_KIND,
            seq,
            &portfolio,
            &serde_json::json!({ "closed": 0, "filled": 0 }).to_string(),
            now,
        )?;
        return Ok(seq);
    }
    let Some(mid) = quote.mid() else {
        return refuse(
            journal,
            permit,
            COMMAND_CLOSE_ALL_KIND,
            seq,
            &portfolio,
            "quote crossed: no price to rest closes at",
            now,
        );
    };
    let proposal_id = format!("operator-close-{seq}");
    // Emission shapes the order (one intent per group, deterministic ids,
    // flipped sides, re-validated structure); the loop prices each intent
    // after: singles rest at the touch (bid for sells, ask for buys —
    // exactly where the paper fill checks them), spreads rest a
    // marketable-by-construction extreme (a package has no single price,
    // and a flattening close must not fail on its own limit). The fill
    // price is the venue's slipped touch, reported back — the limit never
    // prices the package, it only authorizes flattening. Pricing after
    // emission keeps one emitter.
    let mut intents = match close_all_intents(&groups, mid, &proposal_id, "operator") {
        Ok(intents) => intents,
        Err(error) => {
            return refuse(
                journal,
                permit,
                COMMAND_CLOSE_ALL_KIND,
                seq,
                &portfolio,
                &error.to_string(),
                now,
            );
        }
    };
    for intent in &mut intents {
        if intent.legs.is_some() {
            intent.limit_price = match intent.side {
                protopyne_venue::OrderSide::Sell => Money::new(-Decimal::MAX),
                protopyne_venue::OrderSide::Buy => Money::new(Decimal::MAX),
            };
        } else {
            intent.limit_price = match intent.side {
                protopyne_venue::OrderSide::Sell => quote.bid,
                protopyne_venue::OrderSide::Buy => quote.ask,
            };
        }
    }
    // One token for the command's summed exit: a flattening close commits
    // no new loss and no new exposure (the book only shrinks), so the
    // per-trade loss and notional caps — entry-sized bounds — stay vacuous
    // while every health and correctness gate still runs. Action size still
    // binds through the contracts cap.
    let total_qty: u32 = intents.iter().map(|intent| intent.quantity).sum();
    let first = &intents[0];
    let exit = Proposal::new()
        .id(proposal_id)
        .strategy("operator".to_owned())
        .underlying(first.underlying.clone())
        .direction(match first.side {
            protopyne_venue::OrderSide::Sell => Direction::Short,
            protopyne_venue::OrderSide::Buy => Direction::Long,
        })
        .side(ProposalSide::Exit)
        .structure(if first.legs.is_some() {
            StructureKind::Vertical
        } else {
            StructureKind::Single
        })
        .quantity(Qty::new(Decimal::from(total_qty)))
        .limit_price(first.limit_price)
        .max_loss(Money::zero())
        .notional(Money::zero())
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(ProposalState::Proposed)
        .created_at(now)
        .build();
    let shape = OrderShape::new(first.legs.is_some(), true);
    let approved = match risk.check(&exit, shape, risk_state) {
        Ok(approved) => approved,
        Err(refused) => {
            return refuse(
                journal,
                permit,
                COMMAND_CLOSE_ALL_KIND,
                seq,
                &portfolio,
                &refused.detail,
                now,
            );
        }
    };
    let records = match session
        .submit_close_intents(journal, &approved, intents, quote, now)
        .await
    {
        Ok(records) => records,
        Err(error) => {
            return refuse(
                journal,
                permit,
                COMMAND_CLOSE_ALL_KIND,
                seq,
                &portfolio,
                &error.to_string(),
                now,
            );
        }
    };
    let mut filled = 0u32;
    for record in &records {
        let status = match record.status {
            protopyne_venue::OrderStatus::Filled => FillStatus::Filled,
            protopyne_venue::OrderStatus::Partial => FillStatus::Partial,
            _ => FillStatus::Refused,
        };
        let price = record
            .avg_price
            .unwrap_or(record.intent.limit_price)
            .as_decimal()
            .abs();
        ledger.record_operator_fill(&FillOutcome {
            status,
            contracts: record.filled_quantity,
            loss: Money::zero(),
            notional: Money::new(price * Decimal::from(record.filled_quantity)),
        });
        filled += record.filled_quantity;
    }
    audit(
        journal,
        permit,
        COMMAND_CLOSE_ALL_KIND,
        seq,
        &portfolio,
        &serde_json::json!({ "closed": records.len(), "filled": filled }).to_string(),
        now,
    )?;
    Ok(seq)
}

async fn execute_cancel(
    session: &TradingSession,
    journal: &Journal,
    permit: &VenueStorage,
    now: DateTime<Utc>,
    seq: i64,
    portfolio: Option<String>,
) -> Result<i64, protopyne_venue::VenueError> {
    let Some(portfolio) = portfolio else {
        return refuse(
            journal,
            permit,
            COMMAND_CANCEL_ALL_KIND,
            seq,
            "cancel-all",
            "no portfolio named",
            now,
        );
    };
    let ids = cancel_all_ids(&session.registry().orders());
    if let Err(error) = session.cancel_orders(&ids, now).await {
        return refuse(
            journal,
            permit,
            COMMAND_CANCEL_ALL_KIND,
            seq,
            &portfolio,
            &error.to_string(),
            now,
        );
    }
    audit(
        journal,
        permit,
        COMMAND_CANCEL_ALL_KIND,
        seq,
        &portfolio,
        &serde_json::json!({ "cancelled": ids.len() }).to_string(),
        now,
    )?;
    Ok(seq)
}

/// Journal a refusal audit and consume the command: a refused command never
/// retries silently — the operator re-issues loudly instead.
fn refuse(
    journal: &Journal,
    permit: &VenueStorage,
    kind: &str,
    seq: i64,
    portfolio: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<i64, protopyne_venue::VenueError> {
    audit(
        journal,
        permit,
        kind,
        seq,
        portfolio,
        &serde_json::json!({ "refused": reason }).to_string(),
        now,
    )?;
    Ok(seq)
}

fn audit(
    journal: &Journal,
    permit: &VenueStorage,
    kind: &str,
    seq: i64,
    portfolio: &str,
    result: &str,
    now: DateTime<Utc>,
) -> Result<(), protopyne_venue::VenueError> {
    let mut detail: serde_json::Value =
        serde_json::from_str(result).unwrap_or(serde_json::Value::Null);
    if let Some(object) = detail.as_object_mut() {
        object.insert(
            "portfolio".to_owned(),
            serde_json::Value::String(portfolio.to_owned()),
        );
        object.insert(
            "command_seq".to_owned(),
            serde_json::Value::Number(seq.into()),
        );
    }
    journal.record_transition(permit, OPERATOR_CLIENT_ID, kind, &detail.to_string(), now)?;
    Ok(())
}
