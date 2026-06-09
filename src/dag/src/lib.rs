//! DAG: thin re-export crate for the DAG executor and registry types.
//! All implementation lives in `guidance-dag-executor` and `guidance-registry`.

pub use guidance_dag_executor::{adapter, executor, middleware, resolver, work_unit};
pub use guidance_dag_executor::{DagExecutor, DependencyResolver, ExecutionError, ExecutionPlan};
pub use guidance_registry::{CapabilityRegistry, ExecutorKind, RegistryError, ResolverError, Target, TargetRegistry, TargetType};
