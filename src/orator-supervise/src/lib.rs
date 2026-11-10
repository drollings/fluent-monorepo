#![forbid(unsafe_code)]

//! orator-supervise: the single supervised-child home.
//!
//! One `Supervisor` owns one local inference server process per
//! served model: resolve the binary from the environment or `$PATH`
//! with a fail-fast actionable error, bind a free localhost port,
//! wait for the readiness probe before serving, and reap boundedly
//! on shutdown. Both the LLM fleet (`orator-agent`) and the native
//! audio endpoint (`orator-voice`) compose this crate — neither owns
//! a second supervisor, and neither depends on the other, so the
//! voice↔agent edge cannot form in either direction.

pub mod supervisor;

pub use supervisor::{
    boot_with_binary, build_args, find_all_on_path, find_on_path, free_port, probe_voice_ready,
    recovery_action, resolve_binary, resolve_binary_candidates, resolve_binary_validated,
    resolve_with_override, slot_save_flag, HealthProbe, RecoveryAction, Supervisor,
    SupervisorConfig, SupervisorError, GPU_OFFLOAD_LAYERS, LLAMA_SERVER_BIN, LLAMA_SERVER_ENV,
};
