//! Syntactic call graph: unique, homonym, and unresolved tiers.

/// Resolution tiers are syntactic: unique, homonym, or unresolved.
/// Never a correctness verdict.
pub struct CallEdge {
    /// Raw callee name.
    pub callee: String,
}

/// Callers of one symbol and callees of one definition.
pub fn callers_of(symbol: &str) -> Vec<String> {
    let _ = symbol;
    vec![]
}
