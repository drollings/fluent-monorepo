//! Streaming audio codec seam: PCM edges, codes on the wire.
//!
//! The PCM adapter below is the supported implementation — it frames
//! 24 kHz float samples as little-endian i16, the standard PCM wire
//! format, with a bounded half-LSB rounding contract. A neural backend
//! adopts this seam (same rates, same turn shape) without touching
//! callers; until weights and an in-workspace runtime exist for one,
//! the adapter is the transport and the gap stays explicitly open.

/// Streaming codec: one PCM step in, wire codes out, and back.
/// Stateful backends reset between turns; the PCM adapter below is
/// stateless and `reset` is a deliberate no-op it still honors.
pub trait Codec: Send {
    /// PCM sample rate in Hz, both directions.
    fn sample_rate(&self) -> u32;
    /// Wire code rate in Hz.
    fn code_rate(&self) -> u32;
    /// Encode one PCM step to wire codes.
    fn encode_step(&mut self, pcm: &[f32]) -> Vec<u8>;
    /// Decode one wire step to PCM.
    fn decode_step(&mut self, codes: &[u8]) -> Vec<f32>;
    /// Drop cross-turn state. No turn leaks into the next.
    fn reset(&mut self);
}

/// PCM wire adapter: f32 samples framed as little-endian i16. The
/// round-trip contract is half an LSB per conversion; energy drift
/// stays under 1e-4 on voiced fixtures (pinned by golden).
#[derive(Debug, Default)]
pub struct PcmCodec;

impl PcmCodec {
    /// Codec PCM rate in Hz.
    pub const RATE: u32 = 24_000;

    /// Wire adapter with no cross-step state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Codec for PcmCodec {
    fn sample_rate(&self) -> u32 {
        Self::RATE
    }

    fn code_rate(&self) -> u32 {
        Self::RATE
    }

    fn encode_step(&mut self, pcm: &[f32]) -> Vec<u8> {
        // One i16 per sample, little-endian: the standard PCM wire
        // framing. Rounding costs at most half an LSB per conversion.
        // Shared scalar home: `common_core::pcm` (Nearest = `round`).
        common_core::pcm::encode_i16_le(
            pcm,
            common_core::pcm::Scale16::Symmetric32767,
            common_core::pcm::Rounding::Nearest,
        )
    }

    fn decode_step(&mut self, codes: &[u8]) -> Vec<f32> {
        common_core::pcm::decode_i16_le(codes, common_core::pcm::Scale16::Symmetric32767)
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
#[path = "../tests/codec.rs"]
mod tests;
