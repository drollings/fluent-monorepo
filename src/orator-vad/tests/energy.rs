//! VAD tests: attack/hangover timing, hysteresis band, silence control.

use crate::energy::EnergyVad;

fn speech_frame() -> Vec<f32> {
    (0..320).map(|index| (index as f32 * 0.3).sin() * 0.2).collect()
}

fn silence_frame() -> Vec<f32> {
    vec![0.0; 320]
}

#[test]
fn silence_never_opens_speech() {
    let mut vad = EnergyVad::default();
    for _ in 0..50 {
        let output = vad.push(&silence_frame());
        assert!(!output.speech);
        assert!(output.probability <= f32::EPSILON);
    }
    assert!(!vad.speaking());
}

#[test]
fn sustained_speech_opens_after_attack() {
    let mut vad = EnergyVad::default();
    let frame = speech_frame();
    assert!(!vad.push(&frame).speech);
    assert!(!vad.push(&frame).speech);
    assert!(vad.push(&frame).speech);
    assert!(vad.speaking());
}

#[test]
fn speech_closes_after_hangover() {
    let mut vad = EnergyVad::default();
    let speech = speech_frame();
    for _ in 0..3 {
        vad.push(&speech);
    }
    assert!(vad.speaking());
    for _ in 0..14 {
        assert!(vad.push(&silence_frame()).speech);
    }
    assert!(!vad.push(&silence_frame()).speech);
}

#[test]
fn band_energy_holds_the_verdict() {
    let mut vad = EnergyVad::default();
    // Just under the high mark: hot streak must not grow.
    let edge: Vec<f32> = vec![0.029; 320];
    for _ in 0..10 {
        assert!(!vad.push(&edge).speech);
    }
}

#[test]
fn reset_drops_held_state() {
    let mut vad = EnergyVad::default();
    let speech = speech_frame();
    for _ in 0..3 {
        vad.push(&speech);
    }
    vad.reset();
    assert!(!vad.speaking());
    assert!(!vad.push(&silence_frame()).speech);
}

#[test]
fn probability_pins_current_rms_mapping() {
    let mut vad = EnergyVad::default();
    // Silence: RMS 0.0 reads as probability 0.0.
    let silent = vad.push(&silence_frame());
    assert_eq!(silent.probability.to_bits(), 0);
    // Constant near-band frame: accumulation order pins these bits.
    let edge = vad.push(&vec![0.029; 320]);
    assert_eq!(edge.probability.to_bits(), 1064514386);
    // Hot harmonic frame: far above the band, probability clamps to 1.0.
    let hot = vad.push(&speech_frame());
    assert_eq!(hot.probability.to_bits(), 1065353216);
}
