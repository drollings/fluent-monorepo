#![forbid(unsafe_code)]

//! orator-agent: thinking — supervised local inference and the tool-using agent loop.
//!
//! Two halves of agency in one crate: `llm` owns the supervised
//! `llama-server` child and the per-turn session over it; `tools`
//! owns the tool trait/registry, the agentic turn loop, the sandbox,
//! approvals, and policy. Neither half knows about audio pipelines,
//! durability, or desktop surfaces — those compose this crate from
//! the daemon and the CLI fallback.

pub mod llm;
pub mod tools;
