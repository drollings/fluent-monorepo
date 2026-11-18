//! Cross-crate magic numbers (size caps, dimension limits, HNSW defaults).

pub const MAX_VALUE_LEN: usize = 128;
pub const MAX_FILE_SIZE: usize = 100 * 1024 * 1024;
pub const MAX_JSON_DEPTH: usize = 100;

/// Upper bound on KNN candidates before routing through the HNSW index.
///
/// Deprecated shim: the canonical home is
/// `search_vector::hnsw::MAX_KNN_CANDIDATES`. Kept value-identical so
/// existing import paths keep resolving; new code imports the new home.
/// (A plain `pub use` shim would need a `common-core → search-vector`
/// edge against the existing reverse dependency.)
pub const MAX_KNN_CANDIDATES: usize = 100_000;
/// Maximum accepted MCP request payload size, in bytes (promoted from coral's
/// `mcp.rs` alongside `MAX_KNN_CANDIDATES`).
pub const MAX_MCP_REQUEST_SIZE: usize = 10 * 1024 * 1024;

/// Default node count threshold above which `ContentNodeStore::knn_search`
/// switches from brute-force to HNSW.
///
/// Deprecated shim: the canonical home is
/// `search_vector::hnsw::DEFAULT_HNSW_THRESHOLD`. Kept value-identical
/// so existing import paths keep resolving; new code imports the new home.
pub const DEFAULT_HNSW_THRESHOLD: usize = 512;

/// Serde default helper that returns `true`.
pub const fn default_true() -> bool {
    true
}

/// HNSW graph tunables.
///
/// Deprecated shim: the canonical home is
/// `search_vector::hnsw::HnswParams`. Kept value-identical so existing
/// import paths keep resolving; new code imports the new home.
///
/// ```rust
/// let p = common_core::constants::HnswParams::default();
/// assert_eq!((p.max_nb_connection, p.ef_construction), (16, 200));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswParams {
    pub max_nb_connection: usize,
    pub max_layer: usize,
    pub ef_construction: usize,
    pub initial_capacity: usize,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            max_nb_connection: 16,
            max_layer: 16,
            ef_construction: 200,
            initial_capacity: 1024,
        }
    }
}
