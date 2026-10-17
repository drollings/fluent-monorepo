//! Pine script surface: source loading now, grammar and execution later.
//!
//! [`source`] reads `.pine` files as data with warn-only version flags. The
//! lexer, parser, import graph, and evaluator arrive in later work and build
//! on the identities recorded here.

pub mod source;
pub mod lexer;
pub mod syntax;
pub mod display;
pub mod modules;
pub mod closure;
pub mod manifest;
/// Shared link-fixture constructors for the Pine suites (test-only single
/// home; suites `use` it instead of including it, so the module exists once).
#[cfg(test)]
#[path = "../../tests/common/fixture.rs"]
pub(crate) mod test_fixture;
pub mod egui_parity;
pub mod eval;
pub mod projection;
pub mod builtins_ta;
pub mod builtins_core;

pub use source::{
    ImportDecl, LibraryId, PineSource, SourceKind, VersionWarn, SUPPORTED_PINE_VERSION,
    load_pine_source, parse_pine_source,
};
pub use display::display_program;
pub use lexer::{PineLexer, PineToken, TokenKind, lex};
pub use modules::{ModuleGraph, ModuleId, ModuleInput, ResolvedModule, resolve_imports};
pub use manifest::{IndicatorManifestEntry, ManifestFile, ManifestParity, PROVEN_STEMS, build_manifest, manifest_id};
pub use closure::{
    ClosureEntry, INDICATOR_PATH, REAL_LIBRARIES, TASK_INDICATORS, closure_data_root,
    indicator_library_id, load_closure_sources, load_listed_closure,
};
pub use egui_parity::{EguiParityRow, EGUI_PARITY, ParityVerdict, parity_for};
pub use eval::{BarSnapshot, LinkedModule, LtfDataset, PineEngine, RunOutcome, MAX_LOOP_ITERATIONS};
pub use projection::{OverlayProjection, PlotPrimitive, ProjectionRefusal, project};
pub use builtins_ta::TaBridge;
pub use syntax::{
    AssignItem, AssignOp, BinaryOp, BuiltinVar, CallArg, EnumMember, FieldDecl, FieldPersistence,
    MultiDecl, Param, PineExpr, PineParseError, PineParser, PineProgram, PineStmt, PineType,
    SwitchArm, SwitchStmt, UnaryOp, parse_program,
};
