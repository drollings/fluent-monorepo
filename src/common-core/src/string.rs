//! 20+ string utilities: case-insensitive search, slug, truncation, identifier detection.

use std::collections::HashSet;
use std::str::Chars;
use std::sync::LazyLock;

pub static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut s = HashSet::new();
    for w in &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
        "need", "dare", "ought", "used", "this", "that", "these", "those", "i", "you", "he", "she",
        "it", "we", "they", "me", "him", "her", "us", "them", "my", "your", "his", "its", "our",
        "their", "mine", "yours", "hers", "ours", "theirs", "and", "but", "or", "nor", "not", "so",
        "yet", "for", "in", "on", "at", "to", "by", "with", "from", "of", "as", "into", "through",
        "during", "before", "after", "above", "below", "between", "out", "off", "over", "under",
        "again", "further", "then", "once",
    ] {
        s.insert(*w);
    }
    s
});

pub fn trim_right<'a>(slice: &'a [u8], pattern: &[u8]) -> &'a [u8] {
    let mut end = slice.len();
    while end > 0 && pattern.contains(&slice[end - 1]) {
        end -= 1;
    }
    &slice[..end]
}

pub fn trim_left<'a>(slice: &'a [u8], pattern: &[u8]) -> &'a [u8] {
    let mut start = 0;
    while start < slice.len() && pattern.contains(&slice[start]) {
        start += 1;
    }
    &slice[start..]
}

fn contains_word_with_boundary_lowered(
    lowered: &str,
    lowered_word: &str,
    is_boundary: fn(u8) -> bool,
) -> bool {
    let bytes = lowered.as_bytes();
    let wb = lowered_word.as_bytes();
    if wb.is_empty() || wb.len() > bytes.len() {
        return false;
    }
    let mut i = 0;
    while i + wb.len() <= bytes.len() {
        if &bytes[i..i + wb.len()] == wb {
            let left_boundary = i == 0 || is_boundary(bytes[i - 1]);
            let right_boundary = i + wb.len() == bytes.len() || is_boundary(bytes[i + wb.len()]);
            if left_boundary && right_boundary {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn contains_word_with_boundary(text: &str, word: &str, is_boundary: fn(u8) -> bool) -> bool {
    contains_word_with_boundary_lowered(&text.to_lowercase(), &word.to_lowercase(), is_boundary)
}

/// A haystack lowered once and reused across many case-insensitive queries.
///
/// The free functions below each lower the haystack on every call, so a loop
/// over N keywords pays N lowerings. Build one `LoweredHaystack` and run the
/// whole scan against it instead; matching semantics are identical to the
/// free functions (comparison happens on the lowered bytes).
pub struct LoweredHaystack {
    lowered: String,
}

impl LoweredHaystack {
    pub fn new(text: &str) -> Self {
        Self {
            lowered: text.to_lowercase(),
        }
    }

    pub fn lowered(&self) -> &str {
        &self.lowered
    }

    pub fn contains(&self, pattern: &str) -> bool {
        self.lowered.contains(&pattern.to_lowercase())
    }

    pub fn contains_any(&self, keywords: &[&str]) -> bool {
        keywords.iter().any(|k| self.contains(k))
    }

    pub fn contains_word(&self, word: &str) -> bool {
        contains_word_with_boundary_lowered(&self.lowered, &word.to_lowercase(), |c| {
            !c.is_ascii_alphanumeric()
        })
    }

    pub fn contains_any_word(&self, keywords: &[&str]) -> bool {
        keywords.iter().any(|k| self.contains_word(k))
    }

    pub fn contains_ident_word(&self, needle: &str) -> bool {
        contains_word_with_boundary_lowered(
            &self.lowered,
            &needle.to_lowercase(),
            is_ident_boundary,
        )
    }
}

fn is_ident_boundary(c: u8) -> bool {
    !c.is_ascii_alphanumeric()
}

pub fn contains_ident_word(haystack: &str, needle: &str) -> bool {
    contains_word_with_boundary(haystack, needle, is_ident_boundary)
}

pub fn contains_any(text: &str, keywords: &[&str]) -> bool {
    LoweredHaystack::new(text).contains_any(keywords)
}

pub fn contains_any_word(text: &str, keywords: &[&str]) -> bool {
    LoweredHaystack::new(text).contains_any_word(keywords)
}

pub fn contains_ignore_case(text: &str, pattern: &str) -> bool {
    text.to_lowercase().contains(&pattern.to_lowercase())
}

pub fn contains_word(text: &str, word: &str) -> bool {
    contains_word_with_boundary(text, word, |c| !c.is_ascii_alphanumeric())
}

pub fn first_comment_line(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    let trimmed = line
        .trim()
        .trim_start_matches("///")
        .trim()
        .trim_start_matches("//!")
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn has_extension(path: &str, ext: &str) -> bool {
    let ext = ext.trim_start_matches('.');
    path.to_lowercase()
        .ends_with(&format!(".{}", ext.to_lowercase()))
}

pub fn looks_like_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn lower_into<'a>(dst: &'a mut [u8], src: &[u8]) -> &'a [u8] {
    let len = src.len().min(dst.len());
    for i in 0..len {
        dst[i] = src[i].to_ascii_lowercase();
    }
    &dst[..len]
}

/// Lightweight HTML tag stripper — no regex dependency. Strips `<script>`,
/// `<style>`, and all other tags, decodes common entities, replaces
/// block-level tags (`<br>`, `</p>`, `</div>`, `</li>`, `</tr>`) with
/// newlines, and collapses whitespace.
/// Suitable for untrusted text going to an LLM or field validator.
///
/// Single pass: tag-name parsing allocates nothing, entity decoding and
/// whitespace collapsing fold into the main walk.
pub fn strip_html(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_script = false;
    let mut in_style = false;
    // Collapsing state folded into emission: at most one `' '` per
    // whitespace run, no leading space (`true` initially suppresses it),
    // at most one trailing `' '` (popped at the end).
    let mut prev_space = true;
    let mut i = 0;

    // Skip past the closing `>` of a tag starting at byte offset `i`
    // (which is just past the `<` or `/name` already examined).
    let skip_to_tag_end = |i: &mut usize| match bytes[*i..].iter().position(|&b| b == b'>') {
        Some(pos) => *i += pos + 1,
        None => *i = bytes.len(),
    };

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &s[i + 1..];
            let tag_start = rest.trim_start();
            let name_empty = tag_start
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());

            // Check for closing tags first — in_script/in_style flags block the
            // normal tag handler, so we must detect </script>/</style> here.
            if in_script && name_empty {
                if let Some(past_slash) = tag_start.strip_prefix('/') {
                    if run_eq_word(past_slash, "script") {
                        in_script = false;
                        i += 1;
                        skip_to_tag_end(&mut i);
                        continue;
                    }
                }
            }
            if in_style && name_empty {
                if let Some(past_slash) = tag_start.strip_prefix('/') {
                    if run_eq_word(past_slash, "style") {
                        in_style = false;
                        i += 1;
                        skip_to_tag_end(&mut i);
                        continue;
                    }
                }
            }

            if run_starts_word(tag_start, "script") {
                in_script = true;
                i += 1;
                continue;
            }
            if run_starts_word(tag_start, "style") {
                in_style = true;
                i += 1;
                continue;
            }
            // Check for block-level break tags and replace with newline.
            if name_empty {
                if let Some(past_slash) = tag_start.strip_prefix('/') {
                    if matches_block_close(past_slash) {
                        emit_collapsed(&mut out, &mut prev_space, '\n');
                    }
                }
            } else if matches_block_open(tag_start) {
                emit_collapsed(&mut out, &mut prev_space, '\n');
            }
            // Skip everything until '>'
            i += 1;
            skip_to_tag_end(&mut i);
            continue;
        }

        if in_script || in_style {
            // Advance one char (tags inside skipped content are handled
            // by the `<` branch above on the next iteration).
            i += s[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }

        if bytes[i] == b'&' {
            if let Some((decoded, len)) = decode_entity(&s[i..]) {
                for c in decoded.chars() {
                    emit_collapsed(&mut out, &mut prev_space, c);
                }
                i += len;
                continue;
            }
            emit_collapsed(&mut out, &mut prev_space, '&');
            i += 1;
        } else if bytes[i] < 0x80 {
            emit_collapsed(&mut out, &mut prev_space, bytes[i] as char);
            i += 1;
        } else {
            let c = s[i..].chars().next().unwrap();
            emit_collapsed(&mut out, &mut prev_space, c);
            i += c.len_utf8();
        }
    }

    if prev_space {
        out.pop();
    }
    out
}

/// Push `c` with whitespace collapsing: a whitespace run emits at most one
/// `' '`, and leading whitespace is suppressed when `prev_space` starts
/// `true`. A single trailing `' '` (if any) is popped by the caller.
fn emit_collapsed(out: &mut String, prev_space: &mut bool, c: char) {
    if c.is_whitespace() {
        if !*prev_space {
            out.push(' ');
            *prev_space = true;
        }
    } else {
        out.push(c);
        *prev_space = false;
    }
}

/// Byte length of the leading ASCII-alphanumeric run of `s`.
fn alnum_run_len(s: &str) -> usize {
    s.bytes().take_while(u8::is_ascii_alphanumeric).count()
}

/// Exact tag-name match without allocating: the leading run must equal `lit`
/// (ASCII case-insensitive) *and* be the full alphanumeric run, so `<bré>`
/// does not match `"br"`.
fn run_eq_word(s: &str, lit: &str) -> bool {
    let run = alnum_run_len(s);
    run == lit.len()
        && s.as_bytes()[..run].eq_ignore_ascii_case(lit.as_bytes())
        && s[run..].chars().next().is_none_or(|c| !c.is_alphanumeric())
}

/// Tag-name prefix match without allocating (opening `<script…>` / `<style…>`).
fn run_starts_word(s: &str, lit: &str) -> bool {
    let run = alnum_run_len(s);
    run >= lit.len() && s.as_bytes()[..lit.len()].eq_ignore_ascii_case(lit.as_bytes())
}

fn matches_block_close(past_slash: &str) -> bool {
    matches_block_open(past_slash)
}

fn matches_block_open(tag_start: &str) -> bool {
    ["br", "p", "div", "li", "tr"]
        .iter()
        .any(|name| run_eq_word(tag_start, name))
}

/// Decode one entity at the start of `s`, replicating the historical
/// sequential-replace semantics (`&amp;` first, then `&lt;`, `&gt;`,
/// `&quot;`, `&#39;`): `&amp;lt;` decodes twice to `<`, while `&amp;amp;`
/// stays `&amp;` (a single amp pass). Returns the decoded text and the
/// bytes consumed. Unknown entities return `None` (emitted literally).
fn decode_entity(s: &str) -> Option<(&'static str, usize)> {
    if let Some(after) = s.strip_prefix("&amp;") {
        // Chained decode: `&amp;` immediately forming a second entity
        // decodes again, exactly as the old sequential passes did.
        if after.starts_with("lt;") {
            return Some(("<", 5 + 3));
        }
        if after.starts_with("gt;") {
            return Some((">", 5 + 3));
        }
        if after.starts_with("quot;") {
            return Some(("\"", 5 + 5));
        }
        if after.starts_with("#39;") {
            return Some(("'", 5 + 4));
        }
        return Some(("&", 5));
    }
    if s.starts_with("&lt;") {
        return Some(("<", 4));
    }
    if s.starts_with("&gt;") {
        return Some((">", 4));
    }
    if s.starts_with("&quot;") {
        return Some(("\"", 6));
    }
    if s.starts_with("&#39;") {
        return Some(("'", 5));
    }
    None
}

// ── ANSI / control-character sanitizers ───────────────────────────────
// NOTE (ROADMAP_20260903_LLM M11): think-block stripping
// (`strip_thinking_blocks`, `strip_think_block`, `StreamingThinkFilter`)
// and the SSE drainer (`drain_sse_lines`) lived here through M10 as
// deprecated byte-identical shims of `fluent_llm::{thinking, sse}`; M11
// deleted them. The generic `find_subseq` stays (composed by
// `fluent_llm::thinking`).

/// Remove unsafe control / formatting characters: C0/C1 controls, bidi
/// overrides (U+202A–U+202E), line/paragraph separators (U+2028/U+2029),
/// and Plane-14 tags (U+E0000–U+E007F).
pub fn filter_unsafe_chars(text: &str) -> String {
    text.chars().filter(|&c| is_safe_char(c)).collect()
}

fn is_safe_char(c: char) -> bool {
    !matches!(
        c,
        '\u{0000}'
            | '\u{007F}'..='\u{009F}'
            | '\u{0001}'..='\u{0008}'
            | '\u{000B}'..='\u{000C}'
            | '\u{000E}'..='\u{001F}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// Iterator that strips ANSI escape sequences (CSI control sequences, e.g.
/// SGR color codes) from text. A lone ESC not followed by `[` is preserved.
pub struct AnsiStripper<'a> {
    chars: Chars<'a>,
}

impl<'a> AnsiStripper<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars(),
        }
    }
}

impl Iterator for AnsiStripper<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        if c == '\u{1B}' {
            // Check for '[' following ESC — that starts a CSI sequence
            if let Some('[') = self.chars.clone().next() {
                self.chars.next(); // consume '['
                skip_csi_params(&mut self.chars);
                skip_csi_final(&mut self.chars);
                // Recurse to get the next visible character
                self.next()
            } else {
                // Lone ESC, not part of a CSI sequence
                Some('\u{1B}')
            }
        } else {
            Some(c)
        }
    }
}

fn skip_csi_params(chars: &mut Chars<'_>) {
    // Skip parameter bytes (0x30-0x3F) and intermediate bytes (0x20-0x2F)
    loop {
        let mut peek = chars.clone();
        match peek.next() {
            Some(p)
                if ('\u{0030}'..='\u{003F}').contains(&p)
                    || ('\u{0020}'..='\u{002F}').contains(&p) =>
            {
                chars.next();
            }
            _ => break,
        }
    }
}

fn skip_csi_final(chars: &mut Chars<'_>) {
    // Skip the final byte (0x40-0x7E) if present
    let mut peek = chars.clone();
    if let Some(f) = peek.next() {
        if ('\u{0040}'..='\u{007E}').contains(&f) {
            chars.next();
        }
    }
}

// ── Doc/identifier helpers ─────────────────────────────────────────────

/// Strip Rust `///`, `//!`, and `#` doc-comment prefixes from every line,
/// preserving inner indentation. The `#` arm also strips the Markdown-hidden
/// `# ` prefix on doc examples.
pub fn trim_doc_prefix(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    for line in &mut lines {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("///") {
            *line = rest.strip_prefix(' ').unwrap_or(rest);
        } else if let Some(rest) = trimmed.strip_prefix("//!") {
            *line = rest.strip_prefix(' ').unwrap_or(rest);
        } else if let Some(rest) = trimmed.strip_prefix('#') {
            *line = rest.strip_prefix(' ').unwrap_or(rest);
        }
    }
    lines.join("\n")
}

/// Identifier case-style classification for a query string.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Identifier case-style classification for a query string.
pub enum IdentifierKind {
    CamelCase,
    PascalCase,
    SnakeCase,
    KebabCase,
    DottedPath,
    Other,
}

/// Detect the case-style of `query` without a regex. Returns `None` for empty
/// or whitespace-only input, `Some(kind)` otherwise. The pure classification
/// logic — callers that additionally require a syntactically valid identifier
/// (e.g. guidance's `detect_identifier_pattern`) apply their own gate on top.
pub fn detect_identifier_kind(query: &str) -> Option<IdentifierKind> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains('.') && !trimmed.contains(' ') {
        return Some(IdentifierKind::DottedPath);
    }

    let kind = if trimmed.contains('-') && !trimmed.contains(' ') {
        IdentifierKind::KebabCase
    } else if trimmed.contains('_')
        && trimmed
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
    {
        IdentifierKind::SnakeCase
    } else if trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
        && !trimmed.contains('_')
        && !trimmed.contains('-')
    {
        IdentifierKind::PascalCase
    } else if trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        IdentifierKind::CamelCase
    } else {
        IdentifierKind::Other
    };
    Some(kind)
}

pub fn slugify(text: &str) -> String {
    slugify_with(text, &SlugOptions::legacy_default())
}

/// Options for the parameterized slug primitive (P4).
///
/// `slugify(text) == slugify_with(text, &SlugOptions::legacy_default())`
/// byte-for-byte (locked in by `slugify_characterization_table`); the
/// router's chart-name slug is `CHART_OPTIONS` (separator `'_'`, run
/// collapsing, trailing-separator strip, char-bounded length cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Four independent option flags; a bitflags struct would break the public
// `CHART_OPTIONS` / `legacy_default()` field-init API for no gain.
#[allow(clippy::struct_excessive_bools)]
/// Options for the parameterized slug primitive (P4).
/// 
/// `slugify(text) == slugify_with(text, &SlugOptions::legacy_default())`
/// byte-for-byte (locked in by `slugify_characterization_table`); the
/// router's chart-name slug is `CHART_OPTIONS` (separator `'_'`, run
/// collapsing, trailing-separator strip, char-bounded length cap).
/// Four independent option flags; a bitflags struct would break the public
/// `CHART_OPTIONS` / `legacy_default()` field-init API for no gain.
pub struct SlugOptions {
    /// `true`: ASCII-lowercase and map every other non-kept char to the
    /// separator. `false` (legacy): Unicode-lowercase, map only `' '` to the
    /// separator, drop all other non-kept chars.
    pub ascii_only: bool,
    /// The separator emitted for blanks/other chars (`'-'` legacy, `'_'` charts).
    pub separator: char,
    /// Collapse consecutive separators into one (charts; legacy keeps runs).
    pub collapse_runs: bool,
    /// Strip leading separators (legacy trims `'-'` ends; charts never strip
    /// leading — a leading kept char is preserved and a leading separator is
    /// never emitted thanks to collapsing).
    pub trim_leading: bool,
    /// Strip trailing separators.
    pub trim_trailing: bool,
    /// Char-boundary truncation (charts cap at 64; legacy is uncapped).
    pub max_chars: Option<usize>,
}

impl SlugOptions {
    /// Options reproducing the legacy [`slugify`] byte-for-byte.
    pub fn legacy_default() -> Self {
        Self {
            ascii_only: false,
            separator: '-',
            collapse_runs: false,
            trim_leading: true,
            trim_trailing: true,
            max_chars: None,
        }
    }
}

/// Parameterized slugify: lowercase alphanumerics plus `_`/`-` kept, blanks
/// and (in `ascii_only` mode) every other non-kept char folded to
/// [`SlugOptions::separator`], optional run-collapsing, end-trimming, and
/// char-boundary truncation.
pub fn slugify_with(text: &str, opts: &SlugOptions) -> String {
    let lowered = if opts.ascii_only {
        text.to_ascii_lowercase()
    } else {
        text.to_lowercase()
    };
    let mut out = String::with_capacity(lowered.len());
    // Byte offset of the first pushed char that is not the separator.
    // Every byte before it is separator-valued by construction (`-` is both
    // kept and separator-valued), so draining the prefix replaces the old
    // `len() - trim_start_matches().len()` + `drain` recomputation.
    let mut first_live: Option<usize> = None;
    // Chars pushed before the first live char (the trim_leading count).
    let mut leading_seps: usize = 0;
    // Total chars pushed (for the `max_chars` cap).
    let mut pushed: usize = 0;
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            if c != opts.separator && first_live.is_none() {
                first_live = Some(out.len());
            }
            out.push(c);
            pushed += 1;
            if first_live.is_none() {
                leading_seps += 1;
            }
        } else if c == ' ' || opts.ascii_only {
            if opts.collapse_runs && (out.is_empty() || out.ends_with(opts.separator)) {
                continue;
            }
            out.push(opts.separator);
            pushed += 1;
            if first_live.is_none() {
                leading_seps += 1;
            }
        }
        // Else (!ascii_only, non-space, non-kept): drop (legacy behavior).
    }
    if opts.trim_leading {
        match first_live {
            Some(off) => {
                out.drain(..off);
            }
            None => out.clear(),
        }
    }
    let mut trailing_popped: usize = 0;
    if opts.trim_trailing {
        while out.ends_with(opts.separator) {
            out.pop();
            trailing_popped += 1;
        }
    }
    if let Some(max) = opts.max_chars {
        // Chars surviving the trims, counted during the single build walk:
        // separators before the first kept char are gone iff trim_leading
        // ran, and the popped tail is gone iff trim_trailing ran. The cap
        // walk from the trimmed start is bounded by `max`, not by the
        // input length.
        let trimmed_away = trailing_popped + leading_seps * usize::from(opts.trim_leading);
        if pushed - trimmed_away > max {
            let byte_offset = out.char_indices().nth(max).map_or(out.len(), |(i, _)| i);
            out.truncate(byte_offset);
        }
    }
    out
}

pub fn truncate_at_sentence(text: &str, max_chars: usize) -> String {
    // Single walk: count chars, remember the cut point and the last `.`
    // (byte offset + char index) seen inside the first `max_chars` chars.
    let mut cut_byte = text.len();
    let mut last_period: Option<(usize, usize)> = None;
    let mut fits = true;
    for (count, (i, c)) in text.char_indices().enumerate() {
        if count >= max_chars {
            cut_byte = i;
            fits = false;
            break;
        }
        if c == '.' {
            last_period = Some((i, count));
        }
    }
    if fits {
        return text.to_string();
    }
    let truncated = &text[..cut_byte];
    if let Some((period_byte, period_chars)) = last_period {
        if period_chars > max_chars / 2 {
            return text[..=period_byte].to_string();
        }
    }
    truncated.to_string()
}

/// Truncate to `max_bytes` at a UTF-8 char boundary, appending `…` if the
/// input was truncated. Never panics on mid-character boundaries (unlike a
/// raw `&s[..n]` byte slice) and never exceeds `max_bytes`.
pub fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = s[..end].to_string();
    truncated.push('…');
    truncated
}

/// Return the first sentence of `text` — trimmed text up to and including
/// the first `.`, `!`, or `?`. If no sentence-ending punctuation is found,
/// returns up to 120 characters.
pub fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(idx) = trimmed.find(['.', '!', '?']) {
        trimmed[..=idx].trim().to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

pub fn is_path_token(s: &str) -> bool {
    s.len() >= 3 && (s.contains('/') || s.contains('\\'))
}

pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("test")
        || lower.contains("spec")
        || lower.ends_with("_test.zig")
        || lower.ends_with("_tests.zig")
}

pub fn strip_boilerplate(text: &str, prefix: &str) -> String {
    if let Some(stripped) = text.strip_prefix(prefix) {
        stripped.trim_start().to_string()
    } else {
        text.to_string()
    }
}

const NL_PREFIXES: &[&str] = &[
    "what is ",
    "what are ",
    "what does ",
    "what's ",
    "where is ",
    "where are ",
    "where does ",
    "where can i find ",
    "how does ",
    "how do ",
    "how can i ",
    "how to ",
    "why is ",
    "why does ",
    "why do ",
    "when is ",
    "when does ",
    "when do ",
    "who is ",
    "who are ",
    "who does ",
    "which is ",
    "which are ",
    "which does ",
    "explain ",
    "define ",
    "describe ",
    "tell me about ",
];

pub fn strip_nl_prefix(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    for &prefix in NL_PREFIXES {
        if lower.starts_with(prefix) {
            return text[prefix.len()..].to_string();
        }
    }
    text.to_string()
}

pub fn is_noisy_comment(comment: &str) -> bool {
    if comment.len() < 10 {
        return true;
    }
    let non_alpha: usize = comment
        .chars()
        .filter(|c| !c.is_alphanumeric() && !c.is_whitespace())
        .count();
    non_alpha as f64 / comment.len() as f64 > 0.5
}

pub fn skill_name_from_ref(ref_path: &str) -> String {
    let normalized = ref_path.replace('\\', "/");
    let basename = normalized.rsplit('/').next().unwrap_or(ref_path);
    if basename.eq_ignore_ascii_case("SKILL.md") {
        normalized
            .rsplit('/')
            .nth(1)
            .unwrap_or(basename)
            .to_string()
    } else {
        basename.to_string()
    }
}

/// Find the first occurrence of a byte subsequence in a slice starting from `start`.
pub fn find_subseq(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| start + i)
}

/// Collapse runs of whitespace (including newlines) into a single space,
/// trimming leading/trailing whitespace.
///
/// # Examples
///
/// ```
/// use common_core::string::collapse_whitespace;
///
/// assert_eq!(collapse_whitespace("a  b\tc\n d"), "a b c d");
/// assert_eq!(collapse_whitespace("  padded  "), "padded");
/// assert_eq!(collapse_whitespace(""), "");
/// ```
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Truncate `s` to at most `max_chars` Unicode scalar values, never cutting
/// a code point. Unlike `truncate_utf8` (which is byte-based and appends a
/// `…` ellipsis when truncating), this is a hard char cap with no suffix —
/// the caller decides whether to append anything.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Whether a char is CJK (Han, Hiragana, Katakana, Hangul syllables +
/// Jamo). Letter/syllable blocks only — CJK punctuation is excluded so
/// bigram indexes carry no junk pairs. Shared by the FTS5 application-level
/// CJK bigram expansion (guidance index + query sides).
#[must_use]
/// Whether a char is CJK (Han, Hiragana, Katakana, Hangul syllables +
/// Jamo). Letter/syllable blocks only — CJK punctuation is excluded so
/// bigram indexes carry no junk pairs. Shared by the FTS5 application-level
/// CJK bigram expansion (guidance index + query sides).
pub fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}' // Hangul Jamo
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{3130}'..='\u{318F}' // Hangul compatibility Jamo
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
        | '\u{20000}'..='\u{2A6DF}' // CJK Extension B
    )
}

/// Overlapping character bigrams over CJK runs — the application-level half
/// of the FTS CJK contract (`FTS_CJK_STRATEGY = "cjk-bigram-expansion"`).
/// SQLite FTS5 has no CJK dictionary tokenizer, so index and query sides
/// both expand CJK runs to space-joined bigrams; non-CJK text breaks runs
/// and runs shorter than two chars emit nothing.
#[must_use]
/// Overlapping character bigrams over CJK runs — the application-level half
/// of the FTS CJK contract (`FTS_CJK_STRATEGY = "cjk-bigram-expansion"`).
/// SQLite FTS5 has no CJK dictionary tokenizer, so index and query sides
/// both expand CJK runs to space-joined bigrams; non-CJK text breaks runs
/// and runs shorter than two chars emit nothing.
pub fn cjk_bigrams(text: &str) -> Vec<String> {
    fn flush(run: &mut Vec<char>, out: &mut Vec<String>) {
        for pair in run.windows(2) {
            out.push(pair.iter().collect());
        }
        run.clear();
    }
    let mut out = Vec::new();
    let mut run: Vec<char> = Vec::new();
    for c in text.chars() {
        if is_cjk_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Extract the first `<tag>...</tag>` body, trimmed (`None` when either
/// delimiter is missing or the body trims to empty). Callers fall back to
/// the raw response on `None` — the tag is advisory, never load-bearing.
///
/// # Examples
///
/// ```
/// use common_core::string::extract_tag;
///
/// assert_eq!(extract_tag("<comment> hi </comment>", "comment"), Some("hi"));
/// assert_eq!(extract_tag("no tags", "comment"), None);
/// assert_eq!(extract_tag("<comment></comment>", "comment"), None);
/// ```
#[must_use]
/// Extract the first `<tag>...</tag>` body, trimmed (`None` when either
/// delimiter is missing or the body trims to empty). Callers fall back to
/// the raw response on `None` — the tag is advisory, never load-bearing.
/// 
/// # Examples
/// 
/// ```
/// use common_core::string::extract_tag;
/// 
/// assert_eq!(extract_tag("<comment> hi </comment>", "comment"), Some("hi"));
/// assert_eq!(extract_tag("no tags", "comment"), None);
/// assert_eq!(extract_tag("<comment></comment>", "comment"), None);
/// ```
pub fn extract_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(open.as_str())?;
    let after = &text[start + open.len()..];
    let end = after.find(close.as_str())?;
    let content = after[..end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

/// Split a name on non-ASCII-alphanumeric separators, dropping empties.
///
/// Case-preserving (unlike [`split_tokens`]-style lowercasing tokenizers in
/// `search-vector::tokens`): `bug_triage` → `["bug", "triage"]`, so a query
/// naming only a fragment still matches. Moved verbatim from the router's
/// chart store (M7.2); the indexed-doc-text join stays with the chart type.
///
/// # Examples
///
/// ```
/// use common_core::string::ascii_alnum_tokens;
///
/// assert_eq!(ascii_alnum_tokens("bug_triage"), vec!["bug", "triage"]);
/// assert_eq!(ascii_alnum_tokens(""), Vec::<String>::new());
/// ```
#[must_use]
/// Split a name on non-ASCII-alphanumeric separators, dropping empties.
/// 
/// Case-preserving (unlike [`split_tokens`]-style lowercasing tokenizers in
/// `search-vector::tokens`): `bug_triage` → `["bug", "triage"]`, so a query
/// naming only a fragment still matches. Moved verbatim from the router's
/// chart store (M7.2); the indexed-doc-text join stays with the chart type.
/// 
/// # Examples
/// 
/// ```
/// use common_core::string::ascii_alnum_tokens;
/// 
/// assert_eq!(ascii_alnum_tokens("bug_triage"), vec!["bug", "triage"]);
/// assert_eq!(ascii_alnum_tokens(""), Vec::<String>::new());
/// ```
pub fn ascii_alnum_tokens(name: &str) -> Vec<String> {
    AsciiAlnumTokens::new(name).map(str::to_string).collect()
}

/// Streaming ASCII-alphanumeric tokenizer: the zero-allocation walk behind
/// [`ascii_alnum_tokens`], borrowing the input and yielding one case-
/// preserved token per separator-delimited run (same contract, no `String`
/// per token). Skips leading/trailing/repeated separators; empty input
/// yields nothing.
///
/// # Examples
///
/// ```
/// use common_core::string::AsciiAlnumTokens;
///
/// let tokens: Vec<&str> = AsciiAlnumTokens::new("Foo.Bar-baz").collect();
/// assert_eq!(tokens, vec!["Foo", "Bar", "baz"]);
/// ```
#[derive(Debug, Clone)]
/// Streaming ASCII-alphanumeric tokenizer: the zero-allocation walk behind
/// [`ascii_alnum_tokens`], borrowing the input and yielding one case-
/// preserved token per separator-delimited run (same contract, no `String`
/// per token). Skips leading/trailing/repeated separators; empty input
/// yields nothing.
/// 
/// # Examples
/// 
/// ```
/// use common_core::string::AsciiAlnumTokens;
/// 
/// let tokens: Vec<&str> = AsciiAlnumTokens::new("Foo.Bar-baz").collect();
/// assert_eq!(tokens, vec!["Foo", "Bar", "baz"]);
/// ```
pub struct AsciiAlnumTokens<'a> {
    rest: &'a str,
}

impl<'a> AsciiAlnumTokens<'a> {
    /// Tokenizer over `text` (no work until iterated).
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self { rest: text }
    }
}

impl<'a> Iterator for AsciiAlnumTokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        // `find` yields byte indices at char boundaries, so both splits
        // below are boundary-safe for any UTF-8 input.
        let start = self.rest.find(|c: char| c.is_ascii_alphanumeric())?;
        let (_, from_token) = self.rest.split_at(start);
        let len = from_token
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(from_token.len());
        let (token, rest) = from_token.split_at(len);
        self.rest = rest;
        Some(token)
    }
}

/// Replace byte spans rightmost-first so earlier offsets stay valid.
///
/// The one shared span-splicing stanza (scrubbers, anonymizers): sorts by
/// `(start, end)` descending, then applies each span's replacement with
/// [`str::replace_range`]. Total and deterministic — input order is
/// irrelevant; degenerate spans are skipped, never panics:
///
/// - empty/inverted (`start >= end`), past-the-end, and non-`char`-boundary
///   spans are skipped; spans straddling the end of (the current) text are
///   clamped to what exists;
/// - a span starting inside already-handled text is skipped (an exact
///   duplicate, or nested inside a wider applied span);
/// - a span straddling the handled frontier is clamped to the unhandled
///   prefix — the rightmost replacement wins, and matched text is never
///   left in place by an overlap (fail-closed bias: over-scrubbing clean
///   text is safe, under-scrubbing a match is a leak).
///
/// Adjacent spans all apply; each span's replacement is used verbatim (no
/// escaping). Callers with one replacement for every span pass the same
/// `&str` N times.
///
/// # Examples
///
/// ```
/// use common_core::string::apply_spans_rightmost;
///
/// assert_eq!(
///     apply_spans_rightmost("aaaBBBccc", vec![(6, 9, "C"), (0, 3, "A")]),
///     "ABBBC"
/// );
/// assert_eq!(
///     apply_spans_rightmost("abcdef", vec![(0, 6, "O"), (2, 4, "I")]),
///     "OIef"
/// );
/// ```
#[must_use]
/// Replace byte spans rightmost-first so earlier offsets stay valid.
/// 
/// The one shared span-splicing stanza (scrubbers, anonymizers): sorts by
/// `(start, end)` descending, then applies each span's replacement with
/// [`str::replace_range`]. Total and deterministic — input order is
/// irrelevant; degenerate spans are skipped, never panics:
/// 
/// - empty/inverted (`start >= end`), past-the-end, and non-`char`-boundary
/// spans are skipped; spans straddling the end of (the current) text are
/// clamped to what exists;
/// - a span starting inside already-handled text is skipped (an exact
/// duplicate, or nested inside a wider applied span);
/// - a span straddling the handled frontier is clamped to the unhandled
/// prefix — the rightmost replacement wins, and matched text is never
/// left in place by an overlap (fail-closed bias: over-scrubbing clean
/// text is safe, under-scrubbing a match is a leak).
/// 
/// Adjacent spans all apply; each span's replacement is used verbatim (no
/// escaping). Callers with one replacement for every span pass the same
/// `&str` N times.
/// 
/// # Examples
/// 
/// ```
/// use common_core::string::apply_spans_rightmost;
/// 
/// assert_eq!(
/// apply_spans_rightmost("aaaBBBccc", vec![(6, 9, "C"), (0, 3, "A")]),
/// "ABBBC"
/// );
/// assert_eq!(
/// apply_spans_rightmost("abcdef", vec![(0, 6, "O"), (2, 4, "I")]),
/// "OIef"
/// );
/// ```
pub fn apply_spans_rightmost(text: &str, spans: Vec<(usize, usize, &str)>) -> String {
    let mut sorted = spans;
    sorted.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut result = text.to_string();
    let mut cutoff = usize::MAX;
    for (start, end, replacement) in sorted {
        if start >= end {
            continue; // empty or inverted: would insert, never replace
        }
        if start >= cutoff {
            continue; // inside already-handled text (duplicate or nested)
        }
        let mut end = end.min(cutoff);
        end = end.min(result.len());
        if start >= end {
            continue;
        }
        if result.get(start..end).is_none() {
            continue; // out of bounds or splits a `char` boundary
        }
        result.replace_range(start..end, replacement);
        cutoff = start;
    }
    result
}

/// FTS5 match-expression tokenizer shared by every lexical recall arm:
/// Unicode-alphanumeric runs, lowercased, deduped in first-seen order.
/// Differs from [`ascii_alnum_tokens`] on purpose (Unicode-aware,
/// case-folding, deduped): FTS5's `unicode61` tokenizer matches folded, and
/// short match expressions keep the query planner cheap. Extracted from
/// `search-vector`'s match builder so coral's node index composes the same
/// token stream instead of growing a second one.
///
/// # Examples
///
/// ```
/// use common_core::string::fts_match_tokens;
///
/// assert_eq!(fts_match_tokens("find AlphaSymbol"), vec!["find", "alphasymbol"]);
/// assert_eq!(fts_match_tokens("hello hello"), vec!["hello"]);
/// assert_eq!(fts_match_tokens("!!!"), Vec::<String>::new());
/// ```
#[must_use]
/// FTS5 match-expression tokenizer shared by every lexical recall arm:
/// Unicode-alphanumeric runs, lowercased, deduped in first-seen order.
/// Differs from [`ascii_alnum_tokens`] on purpose (Unicode-aware,
/// case-folding, deduped): FTS5's `unicode61` tokenizer matches folded, and
/// short match expressions keep the query planner cheap. Extracted from
/// `search-vector`'s match builder so coral's node index composes the same
/// token stream instead of growing a second one.
/// 
/// # Examples
/// 
/// ```
/// use common_core::string::fts_match_tokens;
/// 
/// assert_eq!(fts_match_tokens("find AlphaSymbol"), vec!["find", "alphasymbol"]);
/// assert_eq!(fts_match_tokens("hello hello"), vec!["hello"]);
/// assert_eq!(fts_match_tokens("!!!"), Vec::<String>::new());
/// ```
pub fn fts_match_tokens(query: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for token in query.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let lower = token.to_lowercase();
        if !tokens.contains(&lower) {
            tokens.push(lower);
        }
    }
    tokens
}

/// Quote one FTS5 match token: wrap in double quotes, doubling embedded
/// quotes so user input can never act as an operator or break the expression.
///
/// # Examples
///
/// ```
/// use common_core::string::fts_quote;
///
/// assert_eq!(fts_quote("hi"), "\"hi\"");
/// assert_eq!(fts_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
/// ```
#[must_use]
/// Quote one FTS5 match token: wrap in double quotes, doubling embedded
/// quotes so user input can never act as an operator or break the expression.
/// 
/// # Examples
/// 
/// ```
/// use common_core::string::fts_quote;
/// 
/// assert_eq!(fts_quote("hi"), "\"hi\"");
/// assert_eq!(fts_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
/// ```
pub fn fts_quote(token: &str) -> String {
    format!("\"{}\"", token.replace('"', "\"\""))
}