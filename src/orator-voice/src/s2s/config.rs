//! Audio-endpoint configuration and session selection.
//!
//! The schema lives in [`orator::config`] (single home, shared
//! with the config file's `s2s.*` section); this module re-exports it
//! so session code keeps its paths.

pub use orator::{
    audio_bundle_dir, bundle_from_s2s, default_bundle, default_bundle_in, resolve_bundle_path,
    AudioBundle, S2sConfig, AUDIO_IN_RATE, AUDIO_OUT_RATE,
};

#[cfg(test)]
#[path = "../../tests/s2s_config.rs"]
mod tests;
