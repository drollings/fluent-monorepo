//! Pine script surface: source loading now, grammar and execution later.
//!
//! [`source`] reads `.pine` files as data with warn-only version flags. The
//! lexer, parser, import graph, and evaluator arrive in later work and build
//! on the identities recorded here.

pub mod source;
pub mod lexer;
pub mod syntax;
pub mod modules;
pub mod eval;
pub mod builtins_ta;
pub mod builtins_core;

pub use source::{
    ImportDecl, LibraryId, PineSource, SourceKind, VersionWarn, SUPPORTED_PINE_VERSION,
    load_pine_source, parse_pine_source,
};
pub use lexer::{PineLexer, PineToken, TokenKind, lex};
pub use modules::{ModuleGraph, ModuleId, ModuleInput, ResolvedModule, resolve_imports};
pub use eval::{BarSnapshot, LinkedModule, PineEngine, RunOutcome, MAX_LOOP_ITERATIONS};
pub use builtins_ta::TaBridge;
pub use syntax::{
    BinaryOp, BuiltinVar, CallArg, EnumMember, FieldDecl, Param, PineExpr, PineParseError,
    PineParser, PineProgram, PineStmt, PineType, SwitchArm, SwitchStmt, UnaryOp, parse_program,
};
