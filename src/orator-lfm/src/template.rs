//! Chat-template assembly for LFM3-Audio turns: ChatML roles with
//! audio placeholders expanded exactly like the reference processor.
//!
//! One `<|audio|>` marker expands to `encoder_tokens_for_frames`
//! repeats (one per encoder token); one `<|audio_out|>` marker
//! expands in place to one token per codec frame plus the appended
//! all-EOS frame. Text turns pass through with role wrappers only. Only
//! verified token ids are emitted (`audio`/`audio_out` placeholders);
//! ChatML framing stays display text — its tokenization belongs to
//! the tokenizer stage, not this assembler.

use crate::arch::{AUDIO_OUT_TOKEN_ID, AUDIO_TOKEN_ID};

/// One audio conditioning clip: mel-frame count (10 ms hop at
/// 16 kHz) the encoder will consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioClip {
    /// Mel frames (`samples / 160`).
    pub mel_frames: usize,
}

/// One codec-frame row of output codes (12 codebooks), used only to
/// count placeholder expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutClip {
    /// Codec frames at 12.5 Hz.
    pub frames: usize,
}

/// One prompt turn: role plus ordered content pieces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTurn {
    /// `system`, `user`, `assistant`, or `tool`.
    pub role: String,
    /// Literal text with at most one `<|audio|>` marker.
    pub text: String,
    /// Conditioning audio (at most one clip per turn, like the reference).
    pub audio: Option<AudioClip>,
    /// Audio-output codes (assistant turns narrating speech).
    pub audio_out: Option<AudioOutClip>,
}

/// Template errors: the reference validates one marker per clip, so
/// assembly refuses anything else instead of guessing. A clip needs
/// exactly its marker in the text (audio and audio_out alike); a
/// marker needs its clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// More than one `<|audio|>` marker in one turn.
    MultipleAudioMarkers,
    /// Marker/clip count mismatch (marker without its clip, clip
    /// without its marker, or a clip facing anything but one
    /// marker) — the reference refuses the same shapes.
    OrphanAudio,
}

/// Assemble one turn: `<|im_start|>role\n…<|im_end|>\n` with markers
/// expanded. Returns the display string plus the placeholder
/// token-id stream in order (`AUDIO_TOKEN_ID` per encoder token,
/// `AUDIO_OUT_TOKEN_ID` per frame plus one EOS frame).
pub fn assemble_prompt(turn: &TextTurn) -> Result<(String, Vec<u32>), TemplateError> {
    let markers = turn.text.matches("<|audio|>").count();
    if markers > 1 {
        return Err(TemplateError::MultipleAudioMarkers);
    }
    if (markers == 1) != turn.audio.is_some() {
        return Err(TemplateError::OrphanAudio);
    }
    let mut text = String::from("<|im_start|>");
    text.push_str(&turn.role);
    text.push('\n');
    let mut ids = Vec::new();
    let mut parts = turn.text.split("<|audio|>");
    // First text span always exists (possibly empty).
    let head = parts.next().unwrap_or("");
    text.push_str(head);
    if let Some(clip) = turn.audio {
        let count = crate::arch::encoder_tokens_for_frames(clip.mel_frames);
        text.push_str(&"<|audio|>".repeat(count));
        ids.extend(std::iter::repeat_n(AUDIO_TOKEN_ID, count));
        text.push_str(parts.next().unwrap_or(""));
    }
    if let Some(out) = &turn.audio_out {
        // One clip needs exactly one marker, expanded in place with
        // one repeat per codec frame plus the EOS frame — the
        // reference rewrites the marker where it stands, so appended
        // placeholders would misplace the conditioning.
        if text.matches("<|audio_out|>").count() != 1 {
            return Err(TemplateError::OrphanAudio);
        }
        let count = out.frames + 1;
        text = text.replacen("<|audio_out|>", &"<|audio_out|>".repeat(count), 1);
        ids.extend(std::iter::repeat_n(AUDIO_OUT_TOKEN_ID, count));
    } else if text.contains("<|audio_out|>") {
        return Err(TemplateError::OrphanAudio);
    }
    text.push_str("<|im_end|>\n");
    Ok((text, ids))
}

#[cfg(test)]
#[path = "../tests/template.rs"]
mod tests;
