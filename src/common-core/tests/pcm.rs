//! `common_core::pcm` unit tests: scales, rounding modes, edge samples,
//! framing drops, and base64 trimming — independent of any call site.

use common_core::pcm::{
    decode_base64_trimmed, decode_f32_le, decode_i16_le, encode_base64, encode_f32_le,
    encode_i16_le, f32_to_i16_sample, i16_to_f32_sample, root_mean_square_f32, scale_divisor,
    wav_frame_header, Rounding, Scale16, WAV_HEADER_LEN,
};

#[test]
fn scales_name_their_divisors() {
    assert!((scale_divisor(Scale16::Symmetric32767) - 32767.0).abs() < f32::EPSILON);
    assert!((scale_divisor(Scale16::Symmetric32768) - 32768.0).abs() < f32::EPSILON);
}

#[test]
fn rails_and_clamps_match() {
    use Rounding::{Nearest, TowardZero};
    use Scale16::Symmetric32767;
    assert_eq!(f32_to_i16_sample(1.0, Symmetric32767, Nearest), 32767);
    assert_eq!(f32_to_i16_sample(-1.0, Symmetric32767, Nearest), -32767);
    assert_eq!(f32_to_i16_sample(1.0, Symmetric32767, TowardZero), 32767);
    assert_eq!(f32_to_i16_sample(-1.0, Symmetric32767, TowardZero), -32767);
    assert_eq!(f32_to_i16_sample(3.0, Symmetric32767, Nearest), 32767);
    assert_eq!(f32_to_i16_sample(-3.0, Symmetric32767, TowardZero), -32767);
    assert_eq!(f32_to_i16_sample(f32::NAN, Symmetric32767, Nearest), 0);
    assert_eq!(f32_to_i16_sample(f32::NAN, Symmetric32767, TowardZero), 0);
}

#[test]
fn rounding_modes_diverge_at_the_half_lsb() {
    // 0.5 * 32767 = 16383.5: Nearest rounds up, TowardZero truncates.
    assert_eq!(
        f32_to_i16_sample(0.5, Scale16::Symmetric32767, Rounding::Nearest),
        16384
    );
    assert_eq!(
        f32_to_i16_sample(0.5, Scale16::Symmetric32767, Rounding::TowardZero),
        16383
    );
    assert_eq!(
        f32_to_i16_sample(-0.5, Scale16::Symmetric32767, Rounding::Nearest),
        -16384
    );
    assert_eq!(
        f32_to_i16_sample(-0.5, Scale16::Symmetric32767, Rounding::TowardZero),
        -16383
    );
}

#[test]
fn decode_scales_are_exact_where_expected() {
    assert!((i16_to_f32_sample(32767, Scale16::Symmetric32767) - 1.0).abs() < f32::EPSILON);
    assert!((i16_to_f32_sample(16384, Scale16::Symmetric32768) - 0.5).abs() < f32::EPSILON);
    assert!((i16_to_f32_sample(0, Scale16::Symmetric32767)).abs() < f32::EPSILON);
}

#[test]
fn i16_framing_round_trips_and_drops_partial_tails() {
    use Rounding::Nearest;
    let back = decode_i16_le(
        &encode_i16_le(&[0.0, 1.0, -1.0], Scale16::Symmetric32767, Nearest),
        Scale16::Symmetric32767,
    );
    assert_eq!(back, vec![0.0, 1.0, -1.0]);
    assert!(decode_i16_le(&[], Scale16::Symmetric32767).is_empty());
    assert!(decode_i16_le(&[0x01], Scale16::Symmetric32767).is_empty());
    assert_eq!(
        decode_i16_le(&[0xFF, 0x7F, 0x01], Scale16::Symmetric32767).len(),
        1
    );
    assert!(encode_i16_le(&[], Scale16::Symmetric32767, Nearest).is_empty());
}

#[test]
fn f32_framing_round_trips_and_drops_partial_tails() {
    let samples = vec![0.0f32, 1.0, -1.0, 0.25];
    let back = decode_f32_le(&encode_f32_le(&samples));
    assert_eq!(back, samples);
    assert!(decode_f32_le(&[]).is_empty());
    assert!(decode_f32_le(&[0, 1, 2]).is_empty());
    assert_eq!(decode_f32_le(&[0, 0, 128, 63, 9]).len(), 1);
}

#[test]
fn base64_trims_and_round_trips() {
    assert_eq!(encode_base64(&[0, 1, 2, 250, 255]), "AAEC+v8=");
    assert_eq!(
        decode_base64_trimmed("AAEC+v8=").expect("decodes"),
        vec![0, 1, 2, 250, 255]
    );
    assert_eq!(
        decode_base64_trimmed("  AAEC+v8=  ").expect("trims"),
        vec![0, 1, 2, 250, 255]
    );
    assert_eq!(decode_base64_trimmed("").expect("empty"), Vec::<u8>::new());
    assert!(decode_base64_trimmed("!!!not-base64!!!").is_err());
}

/// M2-S1 characterization: the Transcribe pair contract. The CLI
/// encoder quantizes with (32767, TowardZero) while the daemon decoder
/// divides by 32768, so `decode(encode(x))` is within one LSB of `x`
/// but never exact: pins the worst case the two pinned scales imply.
#[test]
fn transcribe_pair_scales_round_trip_within_one_lsb() {
    use Rounding::TowardZero;
    let probe: Vec<f32> = (-100..=100).map(|i: i32| i as f32 / 100.0).collect();
    let wire = encode_i16_le(&probe, Scale16::Symmetric32767, TowardZero);
    let back = decode_i16_le(&wire, Scale16::Symmetric32768);
    let worst = probe
        .iter()
        .zip(back.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "pair within one LSB: {worst}");
    // The mismatch is real, not a test artifact: exact rails drift.
    assert!(
        (back[200] - 1.0).abs() > 1e-9,
        "1.0 does not survive the 32767/32768 pair"
    );
    assert!(
        (back[200] - 32767.0 / 32768.0).abs() < 1e-6,
        "rail maps to 32767/32768"
    );
}

#[test]
fn wav_frame_header_matches_the_pinned_layout() {
    assert_eq!(WAV_HEADER_LEN, 44);
    let header = wav_frame_header(16_000, 4);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(header[4..8].try_into().expect("len")),
        40
    );
    assert_eq!(&header[8..16], b"WAVEfmt ");
    assert_eq!(
        u16::from_le_bytes(header[20..22].try_into().expect("fmt")),
        1
    );
    assert_eq!(
        u16::from_le_bytes(header[22..24].try_into().expect("ch")),
        1
    );
    assert_eq!(
        u32::from_le_bytes(header[24..28].try_into().expect("rate")),
        16_000
    );
    assert_eq!(
        u32::from_le_bytes(header[28..32].try_into().expect("byte rate")),
        32_000
    );
    assert_eq!(&header[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(header[40..44].try_into().expect("data len")),
        4
    );
    let empty = wav_frame_header(24_000, 0);
    assert_eq!(
        u32::from_le_bytes(empty[40..44].try_into().expect("data len")),
        0
    );
    assert_eq!(
        u32::from_le_bytes(empty[24..28].try_into().expect("rate")),
        24_000
    );
}

#[test]
fn root_mean_square_f32_pins_exact_bit_patterns() {
    // Empty reads as digital silence.
    assert_eq!(root_mean_square_f32(&[]).to_bits(), 0);
    // Singleton: sqrt(0.25) = 0.5.
    assert_eq!(root_mean_square_f32(&[0.5]).to_bits(), 1056964608);
    // Mixed signs: squares discard the sign before averaging.
    assert_eq!(
        root_mean_square_f32(&[0.5, -0.5, 0.25, -0.25]).to_bits(),
        1053450946
    );
    // Textbook 3-4-5 triangle scaled: sqrt((9 + 16) / 2).
    assert_eq!(root_mean_square_f32(&[3.0, 4.0]).to_bits(), 1080182320);
    // Long accumulation-sensitive vector: order and f32 rounding matter.
    let long: Vec<f32> = (0..1000).map(|i| (i % 7) as f32 * 0.13 - 0.4).collect();
    assert_eq!(root_mean_square_f32(&long).to_bits(), 1048913045);
}

#[test]
fn extend_mono_from_interleaved_covers_channel_shapes() {
    use common_core::pcm::extend_mono_from_interleaved;

    // Empty input appends nothing and reports no remainder.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved(std::iter::empty::<f32>(), 2, |x| x, &mut out),
        0
    );
    assert!(out.is_empty());

    // Mono and zero channels pass every converted input through.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([0.1f32, -0.2, 0.3], 1, |x| x, &mut out),
        0
    );
    assert_eq!(out, vec![0.1f32, -0.2, 0.3]);
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([0.1f32, -0.2], 0, |x| x, &mut out),
        0
    );
    assert_eq!(out, vec![0.1f32, -0.2]);

    // Stereo averages complete frames in order.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([0.5f32, 0.5, -0.25, -0.25, 0.0, 0.0], 2, |x| x, &mut out),
        0
    );
    assert_eq!(out, vec![0.5, -0.25, 0.0]);

    // More than two channels sum left to right before dividing.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([1.0f32, 1e16, 1.0, -1e16], 4, |x| x, &mut out),
        0
    );
    assert_eq!(out, vec![0.0]);

    // Slice inputs work as well as arrays.
    let mut out = Vec::new();
    let stereo = [0.8f32, 0.0, 0.4, 0.0];
    assert_eq!(
        extend_mono_from_interleaved(stereo.iter().copied(), 2, |x| x, &mut out),
        0
    );
    assert_eq!(out, vec![0.4, 0.2]);
}

#[test]
fn extend_mono_from_interleaved_discards_partial_frames() {
    use common_core::pcm::extend_mono_from_interleaved;

    // One trailing sample cannot form a frame: counted, never emitted.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([0.5f32, 0.5, 0.25], 2, |x| x, &mut out),
        1
    );
    assert_eq!(out, vec![0.5]);

    // A loud trailing sample never leaks into the output.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([0.5f32, 0.25, 99.0], 2, |x| x, &mut out),
        1
    );
    assert_eq!(out, vec![0.375]);

    // Input shorter than one frame: everything is remainder.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([7.0f32], 4, |x| x, &mut out),
        1
    );
    assert!(out.is_empty());

    // Converters run per item, including a scaling conversion.
    let mut out = Vec::new();
    assert_eq!(
        extend_mono_from_interleaved([32767i16, 32767], 2, |s| f32::from(s) / 32767.0, &mut out),
        0
    );
    assert_eq!(out, vec![1.0]);
}

#[test]
fn encode_wav_mono_pcm16_pins_header_and_payload_bytes() {
    use common_core::pcm::{encode_wav_mono_pcm16, Rounding, Scale16, WAV_HEADER_LEN};
    use Rounding::TowardZero;
    use Scale16::Symmetric32767;

    // Empty input is header-only.
    let empty = encode_wav_mono_pcm16(&[], 16_000, Symmetric32767, TowardZero);
    assert_eq!(empty.len(), WAV_HEADER_LEN);
    assert_eq!(&empty[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(empty[40..44].try_into().expect("data len")),
        0
    );

    // Ordinary samples: header framing plus truncating payload.
    let bytes = encode_wav_mono_pcm16(&[0.0, 1.0, -1.0, 0.5], 16_000, Symmetric32767, TowardZero);
    assert_eq!(bytes.len(), WAV_HEADER_LEN + 8);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(bytes[24..28].try_into().expect("rate")),
        16_000
    );
    assert_eq!(
        u32::from_le_bytes(bytes[28..32].try_into().expect("byte rate")),
        32_000
    );
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), 0);
    assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), 32767);
    assert_eq!(i16::from_le_bytes([bytes[48], bytes[49]]), -32767);
    // 0.5 * 32767 = 16383.5 truncates toward zero.
    assert_eq!(i16::from_le_bytes([bytes[50], bytes[51]]), 16383);

    // Clipped input clamps to the rails; NaN saturates to silence.
    let bytes = encode_wav_mono_pcm16(&[1.5, f32::NAN], 24_000, Symmetric32767, TowardZero);
    assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), 32767);
    assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), 0);
    assert_eq!(
        u32::from_le_bytes(bytes[24..28].try_into().expect("rate")),
        24_000
    );
}

#[test]
fn root_mean_square_f32_propagates_nan() {
    let nan = root_mean_square_f32(&[f32::NAN]);
    assert!(nan.is_nan(), "NaN input stays NaN");
    assert_eq!(nan.to_bits(), 2143289344);
    let mixed = root_mean_square_f32(&[1.0, f32::NAN]);
    assert!(mixed.is_nan(), "one NaN poisons the frame");
}
