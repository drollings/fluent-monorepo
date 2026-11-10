//! Generation defaults for LFM3-Audio turns, transcribed from
//! `generation_config.json` and the `generate`/`sample_audio_frame`
//! defaults. Single home for every future sampler call.

use crate::lfm::arch::{CODEBOOKS, MASKGIT_SCHEDULE, MASKGIT_STEPS};

/// Text-side sampling (greedy by default).
#[derive(Debug, Clone, PartialEq)]
pub struct TextGen {
    /// Whether to sample (`false` = greedy).
    pub do_sample: bool,
    /// Text temperature (0.0 = greedy).
    pub temperature: f32,
    /// Text top-k (1 = greedy).
    pub top_k: u32,
    /// Text repetition penalty window.
    pub repetition_window: usize,
}

/// Audio-side sampling (MaskGIT over codebooks).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioGen {
    /// Audio temperature.
    pub temperature: f32,
    /// Audio top-k per slot.
    pub top_k: u32,
    /// Audio repetition penalty.
    pub repetition_penalty: f32,
    /// Audio repetition window.
    pub repetition_window: usize,
}

/// One interleaved turn's budget (slots the mode toggles between).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterleavedBudget {
    /// Text slots before switching to audio.
    pub text_slots: usize,
    /// Audio frames before switching to text.
    pub audio_frames: usize,
}

/// Full generation defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationDefaults {
    /// Text side.
    pub text: TextGen,
    /// Audio side.
    pub audio: AudioGen,
    /// Interleaved budget.
    pub interleave: InterleavedBudget,
    /// MaskGIT passes per frame.
    pub maskgit_steps: usize,
    /// Codes revealed per MaskGIT pass (sums to `CODEBOOKS`).
    pub maskgit_schedule: [usize; 3],
}

impl Default for GenerationDefaults {
    fn default() -> Self {
        Self {
            text: TextGen {
                do_sample: false,
                temperature: 0.0,
                top_k: 1,
                repetition_window: 32,
            },
            audio: AudioGen {
                temperature: 1.0,
                top_k: 128,
                repetition_penalty: 1.1,
                repetition_window: 4_096,
            },
            interleave: InterleavedBudget {
                text_slots: crate::lfm::arch::INTERLEAVED_N_TEXT,
                audio_frames: crate::lfm::arch::INTERLEAVED_N_AUDIO,
            },
            maskgit_steps: MASKGIT_STEPS,
            maskgit_schedule: MASKGIT_SCHEDULE,
        }
    }
}

/// Whether the MaskGIT schedule covers every codebook exactly once.
#[must_use]
pub fn maskgit_schedule_covers_codebooks(schedule: &[usize; 3]) -> bool {
    schedule.iter().sum::<usize>() == CODEBOOKS
}

#[cfg(test)]
#[path = "../../tests/lfm_params.rs"]
mod tests;
