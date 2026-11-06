//! `params` pins: generation defaults and the MaskGIT schedule
//! cover every codebook exactly once.

use crate::params::{maskgit_schedule_covers_codebooks, GenerationDefaults};
use crate::arch::{CODEBOOKS, MASKGIT_STEPS};

#[test]
fn defaults_match_the_reference_configs() {
    // Bit-exact pins (transcribed literals, never computed floats).
    let defaults = GenerationDefaults::default();
    assert!(!defaults.text.do_sample, "text greedy");
    assert_eq!(defaults.text.temperature.to_bits(), 0.0f32.to_bits());
    assert_eq!(defaults.text.top_k, 1);
    assert_eq!(defaults.text.repetition_window, 32);
    assert_eq!(defaults.audio.temperature.to_bits(), 1.0f32.to_bits());
    assert_eq!(defaults.audio.top_k, 128);
    assert_eq!(
        defaults.audio.repetition_penalty.to_bits(),
        1.1f32.to_bits()
    );
    assert_eq!(defaults.audio.repetition_window, 4_096);
    assert_eq!(defaults.interleave.text_slots, 6);
    assert_eq!(defaults.interleave.audio_frames, 12);
    assert_eq!(defaults.maskgit_steps, MASKGIT_STEPS);
}

#[test]
fn maskgit_schedule_covers_all_codebooks_once() {
    let defaults = GenerationDefaults::default();
    assert!(maskgit_schedule_covers_codebooks(
        &defaults.maskgit_schedule
    ));
    assert_eq!(
        defaults.maskgit_schedule.iter().sum::<usize>(),
        CODEBOOKS,
        "4+4+4 reveals all 12 codebooks"
    );
    assert!(!maskgit_schedule_covers_codebooks(&[4, 4, 3]));
}
