//! Audio-endpoint configuration and session selection.
//!
//! The schema lives in [`orator_core::config`] (single home, shared
//! with the config file's `s2s.*` section); this module re-exports it
//! so session code keeps its paths.

pub use orator_core::{
    bundle_from_s2s, AudioBundle, S2sConfig, AUDIO_BUNDLE_DIR, AUDIO_IN_RATE, AUDIO_OUT_RATE,
};

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
