//! DAG: Directed Acyclic Graph executor for guidance target orchestration.
//!
//! ## Modules
//! - `target` — Target metadata: names, deps, kinds (File, Phony, Abstract)
//! - `resolver` — Dependency resolution: topological sort, cycle detection
//! - `executor` — Parallel DAG executor with Native/Docker/WASM dispatch
//! - `middleware` — Middleware chain: `TimingMiddleware`, `RetryMiddleware`
//! - `adapter` — Runtime `ComponentAdapter` for name/execute/schema override
//! - `work_unit` — `CommandUnit` implementing `WorkUnit` for CLI command execution
pub mod target;
pub mod resolver;
pub mod executor;
pub mod middleware;
pub mod adapter;
pub mod work_unit;

pub use target::{ExecutorKind, Target, TargetType};
pub use guidance_common::registry::TargetRegistry;
pub use resolver::{DependencyResolver, ExecutionPlan};
pub use guidance_common::error::ResolverError;
pub use executor::{DagExecutor, ExecutionError};
