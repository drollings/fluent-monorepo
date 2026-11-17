pub(crate) mod schema;

pub use crate::vector_store::{SearchResult, VectorDbError, VectorDocStore};

#[cfg(test)]
#[path = "../tests/error_hops.rs"]
mod error_hops;

#[cfg(test)]
#[path = "../tests/hnsw_dispatch.rs"]
mod hnsw_dispatch_tests;

#[cfg(test)]
#[path = "../tests/hnsw_calibration.rs"]
mod hnsw_calibration_tests;
#[cfg(test)]
#[path = "../tests/db.rs"]
mod tests;
