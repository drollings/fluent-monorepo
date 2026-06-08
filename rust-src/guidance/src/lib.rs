#![allow(clippy::too_many_arguments)]
pub mod ast_parser;
pub mod plugin;
pub mod query_engine;
pub mod sync_engine;

pub mod query {
    pub mod identifier;
    pub mod strategy;
    pub mod llm_filter;
    pub mod llm_filter_batch;
    pub mod synthesize;
}

pub mod sync {
    pub mod comments;
    pub mod json_store;
    pub mod json_writer;
    pub mod staleness;
}

pub mod vector {
    pub mod math;
    pub mod quantized_embedding;
    pub mod semantic_aliases;
    pub mod vector_db;
}
