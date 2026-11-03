#![forbid(unsafe_code)]

//! orator-llm: Supervised local inference endpoint and the per-turn session handle over it.

pub mod probe;
pub mod session;
pub mod supervisor;

pub use probe::{probe_models, ProbeError, ProbeInfo};
pub use session::{HttpSession, SessionConfig};
pub use supervisor::{
    boot_with_binary, build_args, find_all_on_path, find_on_path, free_port, probe_voice_ready,
    recovery_action, resolve_binary, resolve_binary_candidates, resolve_binary_validated,
    resolve_with_override, slot_save_flag, HealthProbe, RecoveryAction, Supervisor, SupervisorConfig,
    SupervisorError, GPU_OFFLOAD_LAYERS, LLAMA_SERVER_BIN, LLAMA_SERVER_ENV,
};
