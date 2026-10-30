//! Compatibility surface for the split evaluation modules.
//!
//! The tree interpreter lives in [`oracle`](super::oracle), the engine facade
//! in [`engine`](super::engine), and the shared leaves in
//! [`semantics`](super::semantics). This module re-exports every moved path
//! so no caller changes.
//!
//! The re-exports below are intentionally total: different build shapes use
//! different subsets, so no subset is pruned per shape.

#![allow(unused_imports)]

// Test-only glob surface: the suites below join with `use super::*`, which
// sees these private aliases exactly as it saw the previous module's own
// imports. Non-test builds compile this block out.
#[cfg(test)]
use crate::value::PineValue;
#[cfg(test)]
use crate::{inputs::InputSet, runtime::PineRuntimeConfig};
#[cfg(test)]
use protopyne_types::{Bar, EpochMs};
#[cfg(test)]
use std::collections::HashMap;

pub(crate) use super::engine::module_key;
pub use super::engine::BarColorCapture;
pub use super::engine::BarSnapshot;
#[cfg(feature = "bytecode")]
pub use super::engine::BytecodeMode;
pub use super::engine::DrawingArg;
pub use super::engine::DrawingEvent;
pub use super::engine::DrawingEventKind;
pub use super::engine::FillCapture;
pub use super::engine::HlineCapture;
pub use super::engine::InjectedDataset;
pub use super::engine::LinkedModule;
pub use super::engine::LtfDataset;
pub use super::engine::PineEngine;
pub use super::engine::PineEvaluator;
pub use super::engine::PlotCandleCapture;
pub use super::engine::PlotCapture;
pub use super::engine::PlotShapeCapture;
pub use super::engine::RunOutcome;
pub use super::engine::StrategyCall;
pub use super::engine::MAX_CALL_DEPTH;
pub use super::engine::MAX_LOOP_ITERATIONS;
pub(crate) use super::semantics::walk_stmt;
pub(crate) use super::semantics::walk_stmts;
pub(crate) use super::semantics::PartitionVisit;
pub(crate) use super::semantics::WalkVisit;
pub(crate) use super::semantics::array_index_out_of_bounds;
pub(crate) use super::semantics::array_negative_index;
pub(crate) use super::semantics::barstate_flag_at;
pub(crate) use super::semantics::binary;
#[cfg(feature = "bytecode")]
pub(crate) use super::semantics::binary_op_of;
pub(crate) use super::semantics::branch_of;
pub(crate) use super::semantics::builtin_name;
pub(crate) use super::semantics::call_depth_over_limit;
pub(crate) use super::semantics::call_wall_part;
pub(crate) use super::semantics::cannot_rebind_this;
pub(crate) use super::semantics::capture_fallback_title;
pub(crate) use super::semantics::chart_ms;
#[cfg(feature = "bytecode")]
pub(crate) use super::semantics::color_cast;
pub(crate) use super::semantics::confirmed_prefix;
pub(crate) use super::semantics::destructure_arity_mismatch;
pub(crate) use super::semantics::destructure_non_array;
pub(crate) use super::semantics::field_needs_object;
pub(crate) use super::semantics::field_read_on_na;
pub(crate) use super::semantics::fill_plot_site_without_capture;
pub(crate) use super::semantics::fill_variable_without_plot_tag;
pub(crate) use super::semantics::fixnan_call;
pub(crate) use super::semantics::float_binary;
pub(crate) use super::semantics::float_cast;
pub(crate) use super::semantics::for_in_absent;
pub(crate) use super::semantics::for_in_not_array;
pub(crate) use super::semantics::for_in_over_limit;
pub(crate) use super::semantics::for_range_over_limit;
pub(crate) use super::semantics::for_step_non_positive;
pub(crate) use super::semantics::history_negative_offset;
pub(crate) use super::semantics::iff_of;
pub(crate) use super::semantics::int_binary;
pub(crate) use super::semantics::int_cast;
pub(crate) use super::semantics::is_hidden_series;
pub(crate) use super::semantics::kleene_absent;
pub(crate) use super::semantics::leg_or_nan;
#[cfg(test)]
pub(crate) use super::semantics::legs_of_for_test;
pub(crate) use super::semantics::method_arg_missing;
pub(crate) use super::semantics::method_called_on_na;pub(crate) use super::semantics::method_needs_object;
pub(crate) use super::semantics::method_too_many_positionals;
pub(crate) use super::semantics::method_too_many_positionals_message;
pub(crate) use super::semantics::method_unknown_argument;
pub(crate) use super::semantics::method_unknown_argument_message;
pub(crate) use super::semantics::reassign_undeclared;
pub(crate) use super::semantics::security_column;
pub(crate) use super::semantics::security_leg;
pub(crate) use super::semantics::serve_security;
pub(crate) use super::semantics::session_time_of;
pub(crate) use super::semantics::switch_eq;
pub(crate) use super::semantics::this_unbound;
pub(crate) use super::semantics::trailing_type;
pub(crate) use super::semantics::true_range_at;
pub(crate) use super::semantics::unary;
pub(crate) use super::semantics::undefined_variable;
pub(crate) use super::semantics::unknown_dotted_field;
pub(crate) use super::semantics::unknown_field_on;
pub(crate) use super::semantics::unknown_function;
pub(crate) use super::semantics::unknown_function_message;
pub(crate) use super::semantics::unknown_method_on;
pub(crate) use super::semantics::unknown_this_field;
pub(crate) use super::semantics::while_over_limit;
pub(crate) use super::semantics::whole_number;
pub(crate) use super::semantics::writes_back_collection;
#[cfg_attr(test, allow(dead_code))]
pub(crate) use super::semantics::Legs;
pub(crate) use super::semantics::SecurityTail;
pub(crate) use super::semantics::INPUT_MISSING_DEFAULT_MESSAGE;
pub(crate) use super::semantics::LIVE_REQUEST_TAIL;
pub(crate) use super::semantics::NA_ARITY_MESSAGE;
pub(crate) use super::semantics::NZ_ARITY_MESSAGE;
pub(crate) use super::semantics::PLOT_MISSING_SERIES_MESSAGE;

#[cfg(test)]
#[path = "../../tests/pine_eval.rs"]
mod tests;

/// Injection-proof suite: the DVP higher-timeframe fetch served from
/// injected datasets, with look-ahead/gap calibration and refusal pins.
#[cfg(test)]
#[path = "../../tests/pine_dvp_security.rs"]
mod dvp_security_tests;

/// Task-indicator cross-checks: indicator outputs that reduce to kernel
/// expressions assert against `polars::Expr` references; the rest are
/// named golden-only with reasons.
#[cfg(all(test, feature = "research"))]
#[path = "../../tests/pine_task_parity.rs"]
mod task_parity_tests;
/// v6 surface suite: switch, enums, user functions/methods, tuples, plots.
#[cfg(test)]
#[path = "../../tests/pine_v6.rs"]
mod v6_tests;

/// Headless indicator proof: the linked library triple over real history.
#[cfg(test)]
#[path = "../../tests/pine_fastmas.rs"]
mod fastmas_tests;

/// Calibration gate: bridge coverage, accept/reject tallies, controls.
#[cfg(all(test, feature = "research"))]
#[path = "../../tests/pine_calibration.rs"]
mod calibration_tests;

/// Persistent-binding scoping: declarations own cells per call instance.
#[cfg(test)]
#[path = "../../tests/pine_scope.rs"]
mod scope_tests;

/// Headless runtime suite: cursor flags, injected time, chart scope, session
/// and source inputs, and live-fetch refusal.
#[cfg(test)]
#[path = "../../tests/pine_runtime_shims.rs"]
mod runtime_shim_tests;

/// Real-library suite: the shipped utility library linked with drivers.
#[cfg(test)]
#[path = "../../tests/pine_libutil.rs"]
mod libutil_tests;

/// Real-library suite: the shipped moving-average library linked with
/// drivers, pinned against kernel twins and hand steps.
#[cfg(all(test, feature = "research"))]
#[path = "../../tests/pine_libmas.rs"]
mod libmas_tests;

// Real-library suite: the shipped tick-engine library linked with
// drivers, pinned against hand-computed expectations.
#[cfg(test)]
#[path = "../../tests/pine_libbars.rs"]
mod libbars_tests;

// Drawing-capture suite: plotcandle/barcolor/fill captures plus the
// label/table/box/line lifecycle recorders, pinned as data.
#[cfg(test)]
#[path = "../../tests/pine_draw_captures.rs"]
mod draw_tests;

// Market-core suite: the shipped apex library linked with drivers,
// pinned against kernel twins, hand-computed branches, and a labeled
// regime fixture set.
#[cfg(test)]
#[path = "../../tests/pine_libapex.rs"]
mod libapex_tests;

// Buffer-library suite: the shipped buffers library linked with drivers,
// pinned against hand-computed ring/array/statistics expectations.
#[cfg(test)]
#[path = "../../tests/pine_libbuffers.rs"]
mod libbuffers_tests;
