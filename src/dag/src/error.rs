use thiserror::Error;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("target '{name}' already exists")]
    DuplicateTarget { name: String },
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("invalid capability reference: {0}")]
    InvalidCapability(String),
    #[error("target id must be non-negative, got {0}")]
    NegativeTargetId(i64),
    #[error("target bit index {index} already registered by '{name}'")]
    DuplicateBitIndex { index: usize, name: String },
}

/// Dependency-resolution failures: cycles, unknown targets, unmet version
/// requirements, ambiguous providers. Returned by the resolver, narrowing,
/// and version-selection passes; callers match on the variant to decide
/// whether to fail closed or suggest alternatives.
#[derive(Error, Debug)]
pub enum ResolverError {
    #[error("circular dependency detected")]
    CircularDependency,
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("missing dependency: {0}")]
    MissingDependency(String),
    #[error("ambiguous dependency: '{name}' could be provided by {}", candidates.join(", "))]
    AmbiguousDependency {
        name: String,
        candidates: Vec<String>,
    },
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;
