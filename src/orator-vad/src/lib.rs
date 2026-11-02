#![forbid(unsafe_code)]

//! orator-vad: Voice activity detection: the zero-dependency energy detector first, nightly-model backends later.

pub mod energy;

pub use energy::{EnergyVad, VadOutput};
