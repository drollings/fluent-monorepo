#![forbid(unsafe_code)]

//! orator-vad: Voice activity detection: the zero-dependency energy detector first, nightly-model backends later.

pub mod energy;
pub mod gate;

pub use energy::{EnergyVad, VadOutput};
pub use gate::{speech_present_16k, stationary_tone_present};
