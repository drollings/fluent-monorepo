//! Pine script surface: source loading now, grammar and execution later.
//!
//! [`source`] reads `.pine` files as data with warn-only version flags. The
//! lexer, parser, import graph, and evaluator arrive in later work and build
//! on the identities recorded here.

pub mod builtins_core;
pub mod builtins_ta;
pub mod closure;
pub mod display;
pub mod egui_parity;
pub mod eval;
pub mod lexer;
pub mod manifest;
pub mod modules;
pub mod projection;
pub mod source;
pub mod syntax;
/// Shared link-fixture constructors for the Pine suites (test-only single
/// home; suites `use` it instead of including it, so the module exists once).
#[cfg(test)]
#[path = "../../tests/common/fixture.rs"]
pub(crate) mod test_fixture;

pub use builtins_ta::TaBridge;
pub use closure::{
    closure_data_root, indicator_library_id, load_closure_sources, load_listed_closure,
    ClosureEntry, INDICATOR_PATH, REAL_LIBRARIES, TASK_INDICATORS,
};
pub use display::display_program;
pub use egui_parity::{parity_for, EguiParityRow, ParityVerdict, EGUI_PARITY};
pub use eval::{
    BarSnapshot, LinkedModule, LtfDataset, PineEngine, RunOutcome, MAX_LOOP_ITERATIONS,
};
pub use lexer::{lex, PineLexer, PineToken, TokenKind};
pub use manifest::{
    build_manifest, manifest_id, IndicatorManifestEntry, ManifestFile, ManifestParity, PROVEN_STEMS,
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
