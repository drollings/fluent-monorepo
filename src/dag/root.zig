//! dag — DAG execution engine for build systems.
//!
//! Provides:
//!   Target       — Target/TargetType/ExecutorKind value types
//!   TargetRegistry — DAG registry with TargetBuilder (fluent DSL)
//!   DependencyResolver — Topological sort with cycle detection
//!   DagExecutor  — Parallel DAG execution
//!   BuildContext — Build execution context
//!   Repl         — Interactive REPL
//!   json_parser  — Target file parser

const std = @import("std");

pub const target = @import("target.zig");
pub const registry = @import("registry.zig");
pub const resolver = @import("resolver.zig");
pub const dag_executor = @import("dag_executor.zig");
pub const context = @import("context.zig");
pub const repl = @import("common").repl;
pub const json_parser = @import("common").json_parser;

pub const Target = target.Target;
pub const TargetType = target.TargetType;
pub const ExecutorKind = target.ExecutorKind;
pub const WasmExecutor = target.WasmExecutor;

pub const TargetRegistry = registry.TargetRegistry;
pub const BuilderError = registry.BuilderError;
pub const BuilderPhase = registry.BuilderPhase;
pub const logIfError = registry.logIfError;

pub const DependencyResolver = resolver.DependencyResolver;
pub const ResolverError = resolver.ResolverError;
pub const ResolverOptions = resolver.ResolverOptions;
pub const ResolvedBuild = resolver.ResolvedBuild;

pub const DagNode = dag_executor.DagNode;
pub const DagResult = dag_executor.DagResult;
pub const DagCallbacks = dag_executor.DagCallbacks;
pub const DagExecutor = dag_executor.DagExecutor;

pub const BuildContext = context.BuildContext;
pub const BuildResult = context.BuildResult;
pub const BuildError = context.BuildError;

pub const Repl = repl.Repl;
pub const parseFile = json_parser.parseFile;
pub const ParseError = json_parser.ParseError;
