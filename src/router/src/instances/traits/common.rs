//! Shared llama-adapter error and residency-row mapping helpers.

use std::str::FromStr;

use fluent_llm::runtime::{LlmResidencyRow, LlmRuntime, LlmRuntimeError, ResidencyState};

use crate::instances::client::{InstanceError, InstanceInfo};
use crate::instances::instance_aliases;
use crate::instances::manager::instance_name_from_server_id;

/// Map a llama management-client error onto the runtime-agnostic error type.
pub(crate) fn map_instance_err(e: InstanceError) -> LlmRuntimeError {
    match e {
        InstanceError::Transient { status, body } => LlmRuntimeError::Transient { status, body },
        InstanceError::Duplicate => LlmRuntimeError::Duplicate,
        InstanceError::Rejected { status, body } => LlmRuntimeError::Rejected { status, body },
        InstanceError::Network(s) => LlmRuntimeError::Network(s),
        InstanceError::Other(s) => LlmRuntimeError::Parse(s),
    }
}

/// Map a supervisor error (`String` from `ensure_running`) onto the runtime
/// error type, naming the model.
pub(crate) fn map_server_err(model: &str, e: &str) -> LlmRuntimeError {
    LlmRuntimeError::Other(format!("model '{model}': {e}"))
}

/// One `InstanceInfo` (the fork's envelope row) rendered as a shared
/// [`LlmResidencyRow`]. The fork's `last_used` clock is seconds — kept
/// pool-native (the shared engine's ordering expects it).
///
/// The fork emits `"loaded"` for every allocated instance (see the
/// `InstanceInfo::state` contract in `instances::client`); the parse
/// measures correctness of the wire string for the task (a known lifecycle
/// state), and an envelope row that names no known state still describes an
/// allocated instance, so it falls back to `Loaded` rather than inventing a
/// residency the engine cannot order.
pub(crate) fn info_to_row(model_key: &str, info: &InstanceInfo) -> LlmResidencyRow {
    LlmResidencyRow {
        context_key: format!("{model_key}:{}", instance_name_from_server_id(&info.id)),
        group: info.group.clone(),
        n_ctx: info.n_ctx,
        parallel: info.parallel,
        pinned: info.pinned,
        resume: info.resume,
        state: ResidencyState::from_str(&info.state).unwrap_or(ResidencyState::Loaded),
        runtime: LlmRuntime::Llama,
        model_bytes: info.model_bytes,
        context_bytes: info.context_bytes,
        compute_bytes: info.compute_bytes,
        total_bytes: info.total_bytes,
        vram_bytes: info.vram_bytes,
        last_used: info.last_used,
    }
}

/// A shared [`LlmResidencyRow`] rendered back as a fork `InstanceInfo` for the
/// unified `/instances` envelope. `is_default` is derived from the context
/// name (`default` — the synthesized plain-model / single-context shape); the
/// onnx implementors mark their rows `is_default` the same way.
pub(crate) fn row_to_info(model_key: &str, row: &LlmResidencyRow) -> InstanceInfo {
    let name = row
        .context_key
        .strip_prefix(&format!("{model_key}:"))
        .unwrap_or(&row.context_key)
        .to_string();
    InstanceInfo {
        id: row.context_key.clone(),
        aliases: instance_aliases(model_key, &row.context_key, &row.group, name == "default"),
        group: row.group.clone(),
        n_ctx: row.n_ctx,
        parallel: row.parallel,
        pinned: row.pinned,
        is_default: name == "default",
        resume: row.resume,
        state: row.state.to_string(),
        model_bytes: row.model_bytes,
        context_bytes: row.context_bytes,
        compute_bytes: row.compute_bytes,
        total_bytes: row.total_bytes,
        vram_bytes: row.vram_bytes,
        last_used: row.last_used,
    }
}
