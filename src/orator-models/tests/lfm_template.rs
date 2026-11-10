//! `template` goldens: marker expansion counts, role framing,
//! refusal shapes — no model, no network.

use crate::lfm::template::{
    assemble_prompt, AudioClip, AudioOutClip, TemplateError, TextTurn,
};
use crate::lfm::arch::{AUDIO_OUT_TOKEN_ID, AUDIO_TOKEN_ID};

fn user_turn(text: &str) -> TextTurn {
    TextTurn {
        role: "user".to_string(),
        text: text.to_string(),
        audio: None,
        audio_out: None,
    }
}

#[test]
fn plain_text_turn_frames_chatml() {
    let (text, ids) = assemble_prompt(&user_turn("hello")).expect("assembles");
    assert_eq!(text, "<|im_start|>user\nhello<|im_end|>\n", "{text}");
    assert!(ids.is_empty(), "no placeholders, no ids");
}

#[test]
fn audio_marker_expands_to_encoder_tokens() {
    let turn = TextTurn {
        role: "user".to_string(),
        text: "transcribe <|audio|> please".to_string(),
        audio: Some(AudioClip { mel_frames: 80 }),
        audio_out: None,
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(ids.len(), 10, "ceil(80/8) encoder tokens");
    assert!(ids.iter().all(|id| *id == AUDIO_TOKEN_ID));
    assert_eq!(text.matches("<|audio|>").count(), 10, "{text}");
    assert!(text.starts_with("<|im_start|>user\ntranscribe "));
    assert!(text.ends_with(" please<|im_end|>\n"));
}

#[test]
fn audio_out_expands_in_place_plus_eos_frame() {
    // The reference rewrites the marker where it stands — appended
    // placeholders would misplace the conditioning.
    let turn = TextTurn {
        role: "assistant".to_string(),
        text: "answer <|audio_out|> now".to_string(),
        audio: None,
        audio_out: Some(AudioOutClip { frames: 12 }),
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(ids.len(), 13, "12 frames plus the EOS frame");
    assert!(ids.iter().all(|id| *id == AUDIO_OUT_TOKEN_ID));
    let expanded = "<|audio_out|>".repeat(13);
    assert_eq!(
        text,
        format!("<|im_start|>assistant\nanswer {expanded} now<|im_end|>\n"),
        "{text}"
    );
}

#[test]
fn bad_shapes_refuse_loudly() {
    let two = TextTurn {
        role: "user".to_string(),
        text: "<|audio|> and <|audio|>".to_string(),
        audio: Some(AudioClip { mel_frames: 8 }),
        audio_out: None,
    };
    assert_eq!(
        assemble_prompt(&two),
        Err(TemplateError::MultipleAudioMarkers)
    );
    let orphan_marker = user_turn("hear <|audio|>");
    assert_eq!(assemble_prompt(&orphan_marker), Err(TemplateError::OrphanAudio));
    let orphan_clip = TextTurn {
        audio: Some(AudioClip { mel_frames: 8 }),
        ..user_turn("hear this")
    };
    assert_eq!(assemble_prompt(&orphan_clip), Err(TemplateError::OrphanAudio));
    // Audio-out follows the same one-marker-per-clip rule: a clip
    // without its marker and a marker without its clip both refuse,
    // like the reference's count check.
    let out_no_marker = TextTurn {
        role: "assistant".to_string(),
        text: "answer".to_string(),
        audio: None,
        audio_out: Some(AudioOutClip { frames: 12 }),
    };
    assert_eq!(assemble_prompt(&out_no_marker), Err(TemplateError::OrphanAudio));
    let marker_no_out = TextTurn {
        role: "assistant".to_string(),
        text: "answer <|audio_out|> now".to_string(),
        audio: None,
        audio_out: None,
    };
    assert_eq!(assemble_prompt(&marker_no_out), Err(TemplateError::OrphanAudio));
    let two_markers = TextTurn {
        role: "assistant".to_string(),
        text: "a <|audio_out|> b <|audio_out|>".to_string(),
        audio: None,
        audio_out: Some(AudioOutClip { frames: 12 }),
    };
    assert_eq!(assemble_prompt(&two_markers), Err(TemplateError::OrphanAudio));
}
