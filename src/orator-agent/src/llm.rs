//! Supervised local inference endpoint and the per-turn session handle over it.

pub mod probe;
pub mod session;

pub use probe::{probe_models, ProbeError, ProbeInfo};
pub use session::{HttpSession, SessionConfig};
