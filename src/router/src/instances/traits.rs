//! The llama half of the shared `LlmWeights`/`LlmContext`/`LlmKVCache`
//! contracts, plus the unified `LlmFleet` facade
//! the server builds from both the llama pool and the onnx registry.
//!
//! These are **thin adapters** (roadmap §3.3): every method is a one-line
//! delegation to the existing concrete types — `ManagedServer` +
//! `InstanceManager` + `InstanceClient` + `SnapshotStore`. The llama machinery
//! is not rewritten; the adapters let the shared residency engine, the
//! `/instances` facade, `/v1/models`, `ps`, and `POST /models/unload` drive
//! both fleets through the same trait surface.
//!
//! The `LlmFleet` holds the llama-only `InstancePool` (whose `aggregate`/
//! `list_models` stay **byte-identical** for llama) alongside the trait-object
//! weights list; onnx rows are appended by rendering `dyn LlmWeights`
//! residency rows. Llama behavior is never changed by the facade.
//!
//! This file is a thin facade: each adapter lives in its own submodule
//! (`weights`/`context`/`kv_cache`/`fleet`/`backend`) with shared error/row
//! mapping in `common`. Re-exports keep every `traits::X` path stable.

pub mod backend;
pub(crate) mod common;
pub mod context;
pub mod fleet;
pub mod kv_cache;
pub mod weights;

pub use self::backend::LlamaBackend;
pub use self::context::LlamaContext;
pub use self::fleet::LlmFleet;
pub use self::kv_cache::LlamaKVCache;
pub(crate) use self::weights::llama_weights_for_pool;
pub use self::weights::LlamaWeights;

#[cfg(test)]
#[cfg(feature = "onnx")]
#[path = "../../tests/instances_traits.rs"]
mod tests;
