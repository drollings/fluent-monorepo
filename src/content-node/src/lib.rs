//! guidance-content-node: Level-of-detail text slicing and file content
//! annotation. Produces annotated nodes from source files.

#![deny(warnings, clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::large_stack_arrays,
    clippy::non_std_lazy_statics,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices,
    clippy::default_trait_access
)]

pub mod doc_node;
pub mod file_node;
pub mod lod;
pub mod node;
pub mod source_node;
pub mod wvr;

pub use doc_node::DocumentContentNode;
pub use file_node::FileContentNode;
pub use lod::generate_lod_slices;
pub use node::{ContentNode, LodLevel, NodeType, NodeTypeInfo};
pub use source_node::SourceCodeContentNode;
pub use wvr::ContentNodeRef;
