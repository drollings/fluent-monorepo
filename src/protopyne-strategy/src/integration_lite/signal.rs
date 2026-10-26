//! The signal element of the IntegrationLite composition.
//!
//! The implementation lives beside the leg batch it reads
//! (`signals_lite`), and this module is the composition-facing name: a
//! consumer that composes the nested spec imports `integration_lite::signal`
//! without knowing where the evaluator was filed.

pub use crate::signals_lite::{
    build_filters, build_filters_from_entry, build_triggers, build_triggers_from_entry,
    entry_triggers, IntegrationLiteSignal, LiteIndicatorParams, LiteLegs,
};
