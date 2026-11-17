//! Read-plane retrieval planes: symbols, graph, orientation, docs,
//! and code. Planes read through the `GuidanceSearchDb` row methods — never
//! a second index.

pub mod code;
pub mod docs;
pub mod graph;
pub mod orientation;
pub mod symbol;

#[cfg(test)]
#[path = "../../../tests/read_graph.rs"]
mod read_graph_tests;

#[cfg(test)]
#[path = "../../../tests/read_orientation.rs"]
mod read_orientation_tests;

#[cfg(test)]
#[path = "../../../tests/read_docs.rs"]
mod read_docs_tests;

#[cfg(test)]
#[path = "../../../tests/read_code.rs"]
mod read_code_tests;
