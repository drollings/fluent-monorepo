//! Resampling: device rates to the 16 kHz canonical analysis format.
//!
//! Linear interpolation covers integer-friendly ratios exactly and is fully
//! testable without hardware. The FFT resampler covers the rest.

use rubato::Resampler;

/// Linear-interpolated resample from `from_hz` to `to_hz`. Empty input
/// yields empty output; rates must be nonzero.
#[must_use]
pub fn resample_linear(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    assert!(from_hz != 0 && to_hz != 0, "rates must be nonzero");
    if input.is_empty() || from_hz == to_hz {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * u64::from(to_hz) / u64::from(from_hz)) as usize;
    if out_len == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * f64::from(from_hz) / f64::from(to_hz);
        let before = position.floor() as usize;
        let after = (before + 1).min(input.len() - 1);
        let fraction = (position - before as f64) as f32;
        out.push(input[before].mul_add(1.0 - fraction, input[after] * fraction));
    }
    out
}

pub struct RubatoResampler {
    inner: rubato::FftFixedIn<f32>,
    /// Input chunk length in samples per channel.
    pub chunk_len: usize,
}

impl RubatoResampler {
    /// Build for mono `from_hz` → `to_hz` with `chunk_len`-sample input
    /// chunks.
    pub fn new(from_hz: u32, to_hz: u32, chunk_len: usize) -> Result<Self, String> {
        let inner =
            rubato::FftFixedIn::<f32>::new(from_hz as usize, to_hz as usize, chunk_len, 2, 1)
                .map_err(|error| format!("rubato setup failed: {error:?}"))?;
        Ok(Self { inner, chunk_len })
    }

    /// Resample exactly one `chunk_len` input block into fresh output.
    pub fn process(&mut self, block: &[f32]) -> Result<Vec<f32>, String> {
        let output = self
            .inner
            .process(&[block], None)
            .map_err(|error| format!("rubato process failed: {error:?}"))?;
        Ok(output.into_iter().flatten().collect())
    }
}

#[cfg(test)]
#[path = "../tests/resample.rs"]
mod tests;
