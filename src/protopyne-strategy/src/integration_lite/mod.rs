//! The IntegrationLite composition: one strategy, expressed as a nested spec
//! over the flat wire view it is migrating from.
//!
//! `spec` owns the flat configuration (the compatibility view),
//! `migrate` projects it into the nested [`IntegrationLiteSpec`] and back, and
//! `signal` is the decision element built from it. The flat type stays the
//! wire shape for `/proposals/preview` and replay until the flat view is
//! removed; nothing in this tree changes a byte on that wire.

pub mod migrate;
pub mod signal;
pub mod spec;

pub use migrate::{entry_from_config, EntrySpec, FilterSlot, IntegrationLiteSpec, TriggerSlot};
pub use signal::{build_triggers, entry_triggers, IntegrationLiteSignal};
pub use spec::{
    direction_options, exit_action_options, size_mode_options, stop_type_options,
    table_position_options, tp_basis_options, trail_type_options, IntegrationLiteStrategyConfig,
};
