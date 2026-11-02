//! Sentence streamer: LLM tokens in, speakable chunks out.
//!
//! Cuts land only on natural pauses so a boundary never splits a phrase:
//! sentence ends past a minimum length cut hard (a breath follows), the
//! opening chunk may break at an early comma (first word plays sooner),
//! and over-long runs break at a word boundary. A trailing candidate at
//! the buffer end always waits for more tokens — the end of the buffer is
//! never a cut. Period cuts additionally confirm against the sentence
//! oracle and hold on abbreviations; over-long chunks without any pause
//! fall back to budget-bounded re-chunking.

use std::sync::Arc;

use fluent_llm::tokens::{chunk_document, ChunkConfig};

/// Fragments below this (chars) merge into the next clause.
pub const MIN_CHUNK: usize = 16;
/// The opening chunk may break at a comma within this (chars).
pub const FIRST_MAX: usize = 100;
/// Only a sentence longer than this (chars) is broken further.
pub const CAP_CHUNK: usize = 220;
/// Gap after a full sentence: a natural breath.
pub const SENTENCE_GAP_MS: u32 = 140;
/// Gap after a soft comma/clause break: just flows.
pub const SOFT_GAP_MS: u32 = 60;

/// Periods that never end a sentence: titles and Latin abbreviations.
const TITLES: &[&str] = &[
    "mr", "mrs", "ms", "dr", "st", "jr", "sr", "prof", "rev", "hon", "esq", "eg", "ie", "etc",
    "vs",
];

/// One speakable chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Chunk text, trimmed.
    pub text: String,
    /// True after a sentence terminator (a breath follows); false after a
    /// comma/clause break (flows into the next chunk).
    pub hard: bool,
}

/// One streaming cut: byte length into the buffer plus its break kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Byte length of the chunk (leading text through the break).
    pub len: usize,
    /// Whether the break was a sentence terminator.
    pub hard: bool,
}

/// Gap after `chunk`: a breath after sentences, a flow after soft breaks,
/// nothing after the last chunk.
#[must_use]
pub const fn tail_gap_ms(chunk: &Chunk, is_last: bool) -> u32 {
    if is_last {
        0
    } else if chunk.hard {
        SENTENCE_GAP_MS
    } else {
        SOFT_GAP_MS
    }
}

/// Sentence-boundary oracle: the shared sentencizer over real tokenized
/// docs. Built once per stream; consulted per period cut.
pub struct SentenceTools {
    tokenizer: spacy_rs::Tokenizer,
    sentencizer: spacy_rs::Sentencizer,
}

impl SentenceTools {
    /// Build the tokenizer stack once. Fails on resource setup, never on
    /// text (per-text failures fall back to the scan).
    pub fn new() -> Result<Self, String> {
        let vocab = Arc::new(spacy_rs::Vocab::new(spacy_rs::LexiconConfig::default()));
        let tokenizer =
            spacy_rs::lang::en::tokenizer(Arc::clone(&vocab)).map_err(|error| error.to_string())?;
        Ok(Self {
            tokenizer,
            sentencizer: spacy_rs::Sentencizer::new(),
        })
    }

    /// Whether a sentence boundary sits exactly at `byte_pos` in `text`:
    /// some sentence after the first starts with the token there.
    /// Tokenizer failures fall back to true — the scan is the safe default.
    #[must_use]
    pub fn boundary_at(&self, text: &str, byte_pos: usize) -> bool {
        let Ok(mut doc) = self.tokenizer.tokenize(text) else {
            return true;
        };
        self.sentencizer.process(&mut doc);
        spacy_rs::doc::sentence_spans(&doc)
            .into_iter()
            .skip(1)
            .any(|(start, _)| token_offset(&doc, start) == byte_pos)
    }
}

fn token_offset(doc: &spacy_rs::Doc, index: usize) -> usize {
    doc.token(index).idx as usize
}

/// Find the first streaming cut in `buffer`. `first` marks the opening
/// chunk (comma fast-path armed). Period cuts consult `tools` when
/// present; without tools the scan decides alone. Never cuts at the
/// buffer end — a trailing candidate waits for more tokens.
#[must_use]
pub fn find_cut(buffer: &str, first: bool, tools: Option<&SentenceTools>) -> Option<Cut> {
    let (chunks, _) = scan(buffer, first, tools, false);
    chunks
        .into_iter()
        .next()
        .map(|(chunk, end)| Cut { len: end, hard: chunk.hard })
}

/// Split complete text into chunks. The tail always emits whole; chunks
/// past the cap without any pause fall back to budget-bounded re-chunking.
#[must_use]
pub fn split_all(text: &str) -> Vec<Chunk> {
    let (committed, rest) = scan(text, true, None, true);
    let mut chunks: Vec<Chunk> = committed.into_iter().map(|(chunk, _)| chunk).collect();
    let tail = rest.trim();
    if !tail.is_empty() {
        chunks.push(Chunk {
            text: tail.to_string(),
            hard: true,
        });
    }
    if chunks.is_empty() {
        chunks.push(Chunk {
            text: text.trim().to_string(),
            hard: true,
        });
    }
    chunks.into_iter().flat_map(safety_chunk).collect()
}

/// Core scan shared by streaming and offline paths. Returns the committed
/// chunks (each with its end byte into `text`) plus the uncut remainder.
/// With `flush_end` a sentence end at the text end commits; otherwise it
/// waits for more tokens.
fn scan(
    text: &str,
    first: bool,
    tools: Option<&SentenceTools>,
    flush_end: bool,
) -> (Vec<(Chunk, usize)>, String) {
    let mut chunks: Vec<(Chunk, usize)> = Vec::new();
    let mut opened = usize::from(!first);
    let mut cur = String::new();
    // Byte offset where `cur` starts; committed cuts record absolute ends.
    let mut cur_base = 0usize;
    let mut committed_upto = 0usize;
    let mut last_comma = 0usize;
    let mut last_space = 0usize;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let (byte, ch) = chars[index];
        if cur.is_empty() {
            cur_base = byte;
        }
        cur.push(ch);
        match ch {
            ',' => last_comma = cur.len(),
            ' ' => last_space = cur.len(),
            _ => {}
        }
        let at_end = index + 1 == chars.len();
        // Lookahead past whitespace: a real cut needs more text after the
        // break, or an explicit flush at the text end.
        let mut look = index + 1;
        while chars.get(look).is_some_and(|(_, c)| c.is_whitespace()) {
            look += 1;
        }
        let more_text = look < chars.len();
        let end_byte = chars.get(look).map_or(text.len(), |(b, _)| *b);
        let sentence_end = matches!(ch, '.' | '!' | '?' | '\n' | ';' | ':');
        if sentence_end
            && (more_text || (flush_end && at_end))
            && cur.trim_end().chars().count() >= MIN_CHUNK
        {
            if ch == '.' && !period_confirmed(text, byte, end_byte, tools) {
                // Abbreviation: hold, keep scanning.
            } else {
                let piece = cur.trim().to_string();
                if !piece.is_empty() {
                    chunks.push((Chunk { text: piece, hard: true }, end_byte));
                }
                opened += 1;
                committed_upto = end_byte;
                cur.clear();
                last_comma = 0;
                last_space = 0;
                index = look;
                continue;
            }
        } else if opened == 0
            && ch == ','
            && (more_text || (flush_end && at_end))
            && cur.trim_end().chars().count() >= MIN_CHUNK
            && cur.len() <= FIRST_MAX
        {
            let piece = cur.trim().to_string();
            if !piece.is_empty() {
                chunks.push((Chunk { text: piece, hard: false }, end_byte));
            }
            opened += 1;
            committed_upto = end_byte;
            cur.clear();
            last_comma = 0;
            last_space = 0;
            index = look;
            continue;
        } else if cur.chars().count() >= CAP_CHUNK {
            let at = if last_comma > MIN_CHUNK {
                last_comma
            } else {
                last_space
            };
            if at > MIN_CHUNK && at < cur.len() {
                let carry = cur.split_off(at);
                let piece = cur.trim().to_string();
                if !piece.is_empty() {
                    chunks.push((Chunk {
                        text: piece,
                        hard: false,
                    }, cur_base + at));
                }
                opened += 1;
                committed_upto = cur_base + at;
                let trimmed = carry.trim_start();
                cur_base = committed_upto + (carry.len() - trimmed.len());
                cur = trimmed.to_string();
                last_comma = 0;
                last_space = 0;
            } else if flush_end {
                let end_byte = byte + ch.len_utf8();
                let piece = cur.trim().to_string();
                if !piece.is_empty() {
                    chunks.push((Chunk { text: piece, hard: false }, end_byte));
                }
                opened += 1;
                committed_upto = end_byte;
                cur.clear();
                last_comma = 0;
                last_space = 0;
            }
            // Else: hold. Tracked boundaries stay valid — a later char may
            // separate `at` from the end, or bring a fresh boundary.
        }
        index += 1;
    }
    let only = chunks;
    // The scan consumes greedily, so the remainder is exactly the unflushed
    // tail from the last committed cut.
    (only, text[committed_upto..].trim_start().to_string())
}

/// Whether the period at `period_byte` ends a sentence: not a known title
/// or initial, and either oracle-confirmed at the cut end or tool-less.
fn period_confirmed(
    text: &str,
    period_byte: usize,
    cut_end: usize,
    tools: Option<&SentenceTools>,
) -> bool {
    let before = text[..period_byte].trim_end();
    let token = before
        .rsplit(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("");
    if token.len() <= 4 && TITLES.contains(&token.to_lowercase().as_str()) {
        return false;
    }
    // A period followed by a lowercase letter continues the sentence
    // ("e.g. apples", "v1.2 release").
    let after = text[period_byte + 1..].chars().find(|c| !c.is_whitespace());
    if after.is_some_and(char::is_lowercase) {
        return false;
    }
    match tools {
        Some(oracle) => oracle.boundary_at(text, cut_end),
        None => true,
    }
}

/// Overflow safety: chunks past the cap without any pause re-chunk through
/// the shared budget-bounded chunker (newline/unit boundaries). Normal
/// chunks pass through untouched.
fn safety_chunk(chunk: Chunk) -> Vec<Chunk> {
    if chunk.text.chars().count() <= CAP_CHUNK {
        return vec![chunk];
    }
    let budget = ChunkConfig::new((CAP_CHUNK / 4) as u64, 0);
    let pieces = chunk_document(&chunk.text, &budget);
    if pieces.len() <= 1 {
        return vec![chunk];
    }
    pieces
        .into_iter()
        .map(|text| Chunk { text, hard: false })
        .collect()
}

#[cfg(test)]
#[path = "../tests/splitter.rs"]
mod tests;
