//! Whole-composition slots: the strategy catalogs as registry rows.
//!
//! A composition factory owns only *selection*: which signal, structure, and
//! loss set make up a run. The elements themselves are unchanged, so a
//! registry row is a migration path, not a second strategy implementation.

use std::sync::{Arc, OnceLock};

use crate::elements::{ExplicitStructure, FixedSignal, Sizing, Strategy};
use crate::error::StrategyError;
use crate::legs::LegSelector;
use crate::proposal::{Direction, StructureKind};
use crate::slots::{FnFactory, SlotError, SlotRegistry};

/// The bundled rehearsal signal: fixed long exposure. Constants sit inside
/// `[0, 1]` by construction; a refusal is a bug, never a fallback.
#[allow(clippy::needless_pass_by_value)] // a builder signature; the enum is Copy
pub fn rehearsal_strategy(
    structure: Option<StructureKind>,
) -> Result<Arc<Strategy>, StrategyError> {
    let signal = FixedSignal::new(Direction::Long, 0.8, 0.8)?;
    let strategy = Strategy::with_signal(Arc::new(signal));
    Ok(Arc::new(match structure {
        Some(structure) if structure.needs_chain_data() => {
            strategy.with_structure(Arc::new(LegSelector::new().build()))
        }
        Some(structure) => strategy.with_structure(Arc::new(ExplicitStructure { structure })),
        None => strategy,
    }))
}

/// The deployment composition (kernel trend signal, buying-power sizing,
/// stop/target/time exits, chain-aware legs).
pub fn momentum_strategy() -> Result<Arc<Strategy>, StrategyError> {
    Strategy::momentum().map(Arc::new)
}

fn composition_registry() -> &'static SlotRegistry<Arc<Strategy>> {
    static REGISTRY: OnceLock<SlotRegistry<Arc<Strategy>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = SlotRegistry::new();
        for (id, build) in [
            (
                "rehearsal",
                rehearsal_factory as fn(&serde_json::Value) -> Result<Arc<Strategy>, SlotError>,
            ),
            ("momentum", momentum_factory),
        ] {
            registry
                .register(Arc::new(FnFactory::new(id, build)))
                .expect("unique composition id");
        }
        registry
    })
}

#[allow(clippy::needless_pass_by_value)] // owned from map_err; the message is all that survives
fn translate(id: &'static str, error: StrategyError) -> SlotError {
    SlotError::invalid(id, error.to_string())
}

fn rehearsal_factory(params: &serde_json::Value) -> Result<Arc<Strategy>, SlotError> {
    let structure = match params.get("structure").and_then(serde_json::Value::as_str) {
        Some("single") => Some(StructureKind::Single),
        Some("vertical") => Some(StructureKind::Vertical),
        Some("calendar") => Some(StructureKind::Calendar),
        Some("condor") => Some(StructureKind::Condor),
        Some("naked_short" | "naked-short") => Some(StructureKind::NakedShort),
        Some(other) => {
            return Err(SlotError::invalid(
                "rehearsal",
                format!("unknown structure {other:?}"),
            ))
        }
        None => None,
    };
    rehearsal_strategy(structure).map_err(|error| translate("rehearsal", error))
}

fn momentum_factory(_: &serde_json::Value) -> Result<Arc<Strategy>, SlotError> {
    momentum_strategy().map_err(|error| translate("momentum", error))
}

/// The composition catalog.
#[must_use]
pub fn strategies() -> &'static SlotRegistry<Arc<Strategy>> {
    composition_registry()
}

/// The catalog ids, in schema order.
#[must_use]
pub fn strategy_ids() -> Vec<&'static str> {
    strategies().ids()
}

/// A sizing slot's trait-object type, re-exported so a composition factory
/// can name it without reaching into `elements`.
pub type AnySizer = Arc<dyn Sizing>;
