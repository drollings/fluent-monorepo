//! The rehearsal composition: one fixed signal strategy and one fixed grant
//! body, shared by the HTTP preview and the interactive replay opener so the
//! two rehearsal surfaces cannot drift. Everything here is a compile-time
//! constant: no clock read, no environment, so a pinned config is
//! reproducible from the request alone.

use std::sync::Arc;

use protopyne_strategy::{Grant, Strategy, StructureKind};
use protopyne_types::Money;
use protopyne_view::{RehearsalEconomics, StructureKindHint};

use crate::error::ServerError;

/// Which strategy composition a run decides with. The rehearsal default
/// keeps the fixed-signal composition every existing pin was recorded
/// against; momentum selects the kernel-backed deployment composition.
/// Serialized kebab-case; absent in a request body means rehearsal, so an
/// old request runs byte-identically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
/// Which strategy composition a run decides with. The rehearsal default
/// keeps the fixed-signal composition every existing pin was recorded
/// against; momentum selects the kernel-backed deployment composition.
/// Serialized kebab-case; absent in a request body means rehearsal, so an
/// old request runs byte-identically.
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
    let id = match kind {
        StrategyKind::Rehearsal => "rehearsal",
        StrategyKind::Momentum => "momentum",
    };
    protopyne_strategy::slots::composition::strategies()
        .build(id, &serde_json::json!({}))
        .map_err(|refused| ServerError::Service(format!("composition {id} refused: {refused}")))
}

/// The bundled rehearsal signal: fixed long exposure. The composition
/// library owns the builder so the preview, replay, and registry paths cannot
/// drift.
pub fn rehearsal_strategy(structure: Option<StructureKind>) -> Result<Arc<Strategy>, ServerError> {
    protopyne_strategy::slots::composition::rehearsal_strategy(structure).map_err(|refused| {
        ServerError::Service(format!("rehearsal composition refused: {refused}"))
    })
}

/// The fixed preview grant used when the caller supplies none.
pub(crate) fn preview_grant() -> Grant {
    Grant::preset(protopyne_strategy::GrantPreset::Rehearsal {
        id: "preview-permissive".to_owned(),
        structures: vec![StructureKind::Vertical],
    })
}

/// The fixed rehearsal grant an interactive replay runs under when the opener
/// supplies no grant document: the preview shape, scoped to the requested
/// structure. Singles behave exactly as before; a multi-leg scope admits the
/// shape the window's chains cover — uncovered bars are still refused by the
/// session's coverage screen before the grant sees them.
pub(crate) fn replay_grant_for(structure: StructureKind) -> Grant {
    Grant::preset(protopyne_strategy::GrantPreset::Rehearsal {
        id: "replay-permissive".to_owned(),
        structures: vec![structure],
    })
}

/// Map the read boundary's structure hint to the core's own structure kind.
#[must_use]
/// Map the read boundary's structure hint to the core's own structure kind.
pub(crate) fn structure_of_hint(hint: StructureKindHint) -> StructureKind {
    match hint {
        StructureKindHint::Single => StructureKind::Single,
        StructureKindHint::Vertical => StructureKind::Vertical,
        StructureKindHint::Calendar => StructureKind::Calendar,
        StructureKindHint::Condor => StructureKind::Condor,
        StructureKindHint::NakedShort => StructureKind::NakedShort,
    }
}

/// The economics every rehearsal proposal books for `underlying` in
/// `structure`, read off the core's own rehearsal grant — the one place that
/// declares them, so a terminal never holds a copy that can drift.
///
/// The per-proposal numbers are the grant's own authorities: `max_loss` is
/// the per-trade worst case it authorizes, `dte_days` the nearest expiry its
/// trade tier admits, and `notional` the grant's notional budget shared over
/// its trade budget — one trade can never commit more than its average share,
/// so a run that spends its trade budget in full cannot exhaust notional
/// first. An underlying or structure the grant does not cover refuses here,
/// before a session is built around numbers the ledger would refuse bar by
/// bar.
pub(crate) fn rehearsal_economics(
    underlying: &str,
    structure: StructureKind,
) -> Result<RehearsalEconomics, ServerError> {
    let grant = replay_grant_for(structure);
    if !grant
        .scope
        .underlyings
        .iter()
        .any(|name| name == underlying)
    {
        return Err(ServerError::Service(format!(
            "the rehearsal grant covers {} only, not '{underlying}'",
            grant.scope.underlyings.join(", ")
        )));
    }
    let Some(strategy) = grant.scope.strategies.first() else {
        return Err(ServerError::Service(
            "the rehearsal grant names no strategy".to_owned(),
        ));
    };
    let trades = std::num::NonZeroU32::new(grant.budgets.max_trades)
        .ok_or_else(|| ServerError::Service("the rehearsal grant allows no trades".to_owned()))?;
    let notional = Money::new(
        grant.budgets.max_notional.as_decimal() / rust_decimal::Decimal::from(trades.get()),
    );
    Ok(RehearsalEconomics {
        strategy: strategy.clone(),
        max_loss: grant.trade.max_loss_per_trade.to_string(),
        notional: notional.to_string(),
        dte_days: grant.trade.min_dte_days,
        grant_id: grant.id.clone(),
    })
}