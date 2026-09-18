//! fluent-knowledge: Word/trigram index, CSR graph, frequency
//! tables, and tokenizer for project-level knowledge representation.

//! fluent-knowledge: Word/trigram index, CSR graph, frequency
//! tables, and tokenizer for project-level knowledge representation.
//!
//! M10.2/M10.3: LOD mechanics live here too (`lod`, `lod_policy`,
//! `selector`, `assembler`, `views`) — generic over the concept content
//! seam, ledger-free unit-testable over the in-memory double.

pub mod assembler;
pub mod csr_graph;
pub mod freq_table;
pub mod index_header;
pub mod lod;
pub mod lod_policy;
pub mod query_cache;
pub mod selector;
pub mod tokenizer;
pub mod trigram_index;
pub mod views;
pub mod word_index;

pub use assembler::{AssembledPrompt, LedgerPromptAssembler};
pub use csr_graph::CsrGraph;
pub use freq_table::{build_frequency_table, default_frequency_table, pair_weight};
pub use index_header::Header as IndexHeader;
pub use lod::{Lod, LAZY_LOD_RANGE, LOD0_FULL_TEXT, LOD5_LABEL};
pub use lod_policy::{parse_tiers, LodSpec, PromptBudget, RelevanceSignal, WorkerContext};
pub use query_cache::QueryCache;
pub use selector::{CosineRankSelector, FidelitySelector, UniformSelector};
pub use tokenizer::WordTokenizer;
pub use trigram_index::TrigramIndex;
pub use views::{FilteredLedger, LedgerView, ParallelLedger};
pub use word_index::{DocRegistry, WordHit, WordIndex};
