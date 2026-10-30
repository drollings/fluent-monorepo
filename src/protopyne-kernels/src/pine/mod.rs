//! Pine script surface: source loading now, grammar and execution later.
//!
//! [`source`] reads `.pine` files as data with warn-only version flags. The
//! lexer, parser, import graph, and evaluator arrive in later work and build
//! on the identities recorded here.

#[cfg(feature = "bytecode")]
pub mod bind;
pub mod builtins_core;
pub mod builtins_state;
pub mod builtins_ta;
#[cfg(feature = "bytecode")]
pub mod bytecode;
#[cfg(feature = "bytecode")]
pub mod bytecode_compile;
#[cfg(feature = "bytecode")]
pub mod bytecode_exec;
pub mod closure;
pub mod display;
pub mod drawing;
pub mod egui_parity;
pub mod engine;
pub mod eval;
pub mod home;
pub mod lexer;
pub mod manifest;
pub mod modules;
#[cfg(any(test, feature = "oracle"))]
pub mod oracle;
pub mod projection;
pub mod request;
pub mod semantics;
pub mod source;
pub mod strategy;
pub mod syntax;
/// Shared link-fixture constructors for the Pine suites (test-only single
/// home; suites `use` it instead of including it, so the module exists once).
#[cfg(test)]
#[path = "../../tests/common/fixture.rs"]
pub(crate) mod test_fixture;
pub mod trigger;

/// Differential-test harness for the bytecode milestone track (test-only:
/// fixed corpus, tree-walk runner, canonical outcome hash).
#[cfg(test)]
#[path = "../../tests/pine_bytecode_diff.rs"]
mod bytecode_diff_tests;

/// Red-team contract for the bytecode track (test-only adversarial
/// corpus every execution engine must satisfy).
#[cfg(test)]
#[path = "../../tests/pine_bytecode_adversarial.rs"]
mod bytecode_adversarial_tests;

/// Contiguous series behavior for the bytecode track (test-only hole
/// discipline, offset edges, and tolerance parity for the float columns).
/// Needs the `research` Expr oracle for its float-tolerance assertions;
/// every other gate that runs this suite enables it.
#[cfg(all(test, feature = "research"))]
#[path = "../../tests/pine_bytecode_series.rs"]
mod bytecode_series_tests;

/// Trigger calibration for the bytecode track (test-only must-fire and
/// control-group runs pinning the tier-up and split-brain constants).
/// Needs the `research` Expr oracle for its float-tolerance assertions;
/// every other gate that runs this suite enables it.
#[cfg(all(test, feature = "research"))]
#[path = "../../tests/pine_bytecode_calibration.rs"]
mod bytecode_calibration_tests;

/// Engine selection for the bytecode track (test-only opt-in mode vs
/// default tree-walk byte-identity, plus long-history soak probes).
#[cfg(test)]
#[path = "../../tests/pine_bytecode_integration.rs"]
mod bytecode_integration_tests;

/// Fixed-corpus outcome contract (test-only: checked-in canonical forms,
/// refusal inventory, engine-selection ledger, corpus parity target).
#[cfg(test)]
#[path = "../../tests/pine_bytecode_contract.rs"]
mod bytecode_contract_tests;

pub use builtins_ta::TaBridge;
#[cfg(feature = "bytecode")]
pub use bytecode::{
    decode_block, decode_consts, decode_symbols, encode_block, encode_consts, encode_symbols,
    BinOp, BuiltinNamespace, BuiltinVarKind, CallArgSlot, CallTarget, CaptureKind, CompiledBlock,
    CompiledModule, CompiledProgram, CondContext, ConstId, ConstPool, ConstValue, DecodeError,
    FallbackDisposition, FallbackReason, FillLeg, Instruction, LegKind, PlotTitle, SecurityExpr,
    SlotId, SymbolId, SymbolTable, UnOp, FALLBACK_DISPOSITIONS, INSTRUCTION_NAMES,
};
#[cfg(feature = "bytecode")]
pub use bytecode_compile::compile_module;
#[cfg(feature = "bytecode")]
pub use bytecode_exec::{
    link, run_linked as run_bytecode, run_linked_with_probe, FloatSeries, LinkError, ProbeReport,
    VmConfig, VmProgram, SPLIT_BRAIN_COVERAGE, TIER_STABLE_BARS,
};
pub use closure::{
    closure_data_root, closure_inputs, discover_closures, ClosureOutcome, DiscoveredClosure,
};
pub use display::display_program;
pub use egui_parity::{parity_for, EguiParityRow, ParityVerdict, EGUI_PARITY};
#[cfg(feature = "bytecode")]
pub use eval::BytecodeMode;
pub use eval::{
    BarSnapshot, LinkedModule, LtfDataset, PineEngine, PineEvaluator, RunOutcome,
    MAX_LOOP_ITERATIONS,
};
pub use lexer::{lex, PineLexer, PineToken, TokenKind};
pub use manifest::{
    build_manifest, manifest_id, IndicatorManifestEntry, ManifestFile, ManifestParity,
};
pub use modules::{resolve_imports, ModuleGraph, ModuleId, ModuleInput, ResolvedModule};
pub use projection::{project, OverlayProjection, PlotPrimitive, ProjectionRefusal};
pub use source::{
    load_pine_source, parse_pine_source, ImportDecl, LibraryId, PineSource, SourceKind,
    VersionWarn, SUPPORTED_PINE_VERSION,
};
pub use syntax::{
    parse_program, AssignItem, AssignOp, BinaryOp, BuiltinVar, CallArg, EnumMember, FieldDecl,
    FieldPersistence, MultiDecl, Param, PineExpr, PineParseError, PineParser, PineProgram,
    PineStmt, PineType, SwitchArm, SwitchStmt, UnaryOp,
};
