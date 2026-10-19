//! The rehearsal composition: one fixed signal strategy and one fixed grant
//! body, shared by the HTTP preview and the interactive replay opener so the
//! two rehearsal surfaces cannot drift. Everything here is a compile-time
//! constant: no clock read, no environment, so a pinned config is
//! reproducible from the request alone.

use std::sync::Arc;

use protopyne_strategy::elements::ExplicitStructure;
use protopyne_strategy::{
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission, GrantScope,
    PortfolioThresholds, SignalThresholds, Strategy, StructureKind, TradeThresholds,
};
use protopyne_types::Money;

use crate::error::ServerError;

/// Which strategy composition a run decides with. The rehearsal default
/// keeps the fixed-signal composition every existing pin was recorded
/// against; momentum selects the kernel-backed deployment composition.
/// Serialized kebab-case; absent in a request body means rehearsal, so an
/// old request runs byte-identically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StrategyKind {
    /// Fixed long signal, explicit structure, one contract, hold.
    #[default]
    Rehearsal,
    /// Kernel trend signal, buying-power sizing, stop/target/time exits,
    /// chain-aware legs.
    Momentum,
}

/// Resolve a configured strategy kind to its composition. Rehearsal is the
/// same builder the preview and replay surfaces already share; momentum is
/// the deployment composition, which refuses at construction exactly as
/// its elements refuse.
pub fn select_strategy(kind: StrategyKind) -> Result<Arc<Strategy>, ServerError> {
    match kind {
        StrategyKind::Rehearsal => rehearsal_strategy(None),
        StrategyKind::Momentum => Strategy::momentum().map(Arc::new).map_err(|refused| {
            ServerError::Service(format!("momentum composition refused: {refused}"))
        }),
    }
}

/// The bundled rehearsal signal: fixed long exposure. Constants sit inside
/// `[0, 1]` by construction; any refusal is a bug the caller sees as a failed
/// run, never a fallback signal. `structure` pins the structure element
/// (interactive replay), or leaves the strategy's default selector (preview)
/// — the *same* signal either way, so the two surfaces share one builder.
///
/// A structure that needs per-leg market data gets the chain-aware selector:
/// config-supplied economics cannot price a package, so a multi-leg replay
/// can only be honest through the chain-derived arm, and only a chain-aware
/// selector produces the legs that arm requires. `Single` keeps the explicit
/// element, which has nothing to price.
pub fn rehearsal_strategy(structure: Option<StructureKind>) -> Result<Arc<Strategy>, ServerError> {
    use protopyne_strategy::LegSelector;

    let signal = FixedSignal::new(Direction::Long, 0.8, 0.8)
        .map_err(|e| ServerError::Service(format!("bundled signal refused: {e}")))?;
    let strategy = Strategy::with_signal(Arc::new(signal));
    Ok(Arc::new(match structure {
        Some(structure) if crate::replay_session::needs_chain_data(structure) => {
            strategy.with_structure(Arc::new(LegSelector::new().build()))
        }
        Some(structure) => strategy.with_structure(Arc::new(ExplicitStructure { structure })),
        None => strategy,
    }))
}

/// The fixed preview grant used when the caller supplies none.
pub(crate) fn preview_grant() -> Grant {
    rehearsal_grant("preview-permissive", vec![StructureKind::Vertical])
}

/// The fixed rehearsal grant an interactive replay runs under when the opener
/// supplies no grant document: the preview shape, scoped to the requested
/// structure. Singles behave exactly as before; a multi-leg scope admits the
/// shape the window's chains cover — uncovered bars are still refused by the
/// session's coverage screen before the grant sees them.
pub(crate) fn replay_grant_for(structure: StructureKind) -> Grant {
    rehearsal_grant("replay-permissive", vec![structure])
}

/// One rehearsal-grant body over an explicit structure scope and id: preview
/// and replay differ only in which structures they admit, never in budgets,
/// thresholds, or breakers. The window is a compile-time constant (no ambient
/// clock), so a pinned config is reproducible; evaluation happens at bar time.
pub(crate) fn rehearsal_grant(id: &str, structures: Vec<StructureKind>) -> Grant {
    Grant::new()
        .id(id.to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(structures)
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(Money::from(1_000_000))
                .max_notional(Money::from(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(1_000))
                .max_spread_width(Money::from(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(rehearsal_stamp(1_700_000_000))
        .valid_until(rehearsal_stamp(1_900_000_000))
        .build()
}

/// A fixed rehearsal-window stamp. The constant is a valid instant, so the
/// `expect` states a construction invariant, never reads a clock.
fn rehearsal_stamp(seconds: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(seconds, 0).expect("fixed rehearsal stamp is valid")
}
