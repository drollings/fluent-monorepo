//! Spoken-text normalizer: markdown and code never reach the voice.
//!
//! Fenced code blocks vanish (the reply text on screen keeps them; the
//! voice says nothing there), inline code and identifiers split into
//! spoken words, markdown markup strips to plain text, and whitespace
//! collapses. Numbers, dates, and units pass through untouched — full
//! expansion is later work, and a half-expander mispronounces more than
//! it fixes.

/// Normalize `text` for speech. Idempotent on plain prose.
#[must_use]
pub fn normalize_for_speech(text: &str) -> String {
    let no_code = strip_fenced_code(text);
    let no_markup = strip_markdown(&no_code);
    let words = split_identifiers(&no_markup);
    common_core::string::collapse_whitespace(&words)
}

/// Drop fenced code blocks (``` … ```) wholesale. An unclosed fence
/// drops to the text end — speaking half a listing is worse than none.
fn strip_fenced_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        out.push_str(&rest[..open]);
        out.push(' ');
        let after = &rest[open + 3..];
        if let Some(close) = after.find("```") {
            rest = &after[close + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Strip markdown markup to plain text: headings, quotes, list markers,
/// emphasis, links keep their label, images drop, inline code keeps its
/// content (split into words later), HTML strips to text, bare URLs drop
/// (a read-aloud URL is noise, never information).
fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '#' | '>' if line_start(&out) => {
                // Heading/quote marker: skip to content.
                while chars
                    .peek()
                    .is_some_and(|c| *c == '#' || *c == '>' || *c == ' ')
                {
                    chars.next();
                }
            }
            '*' => {
                // Emphasis marker: drop (paired or stray alike).
            }
            '`' => {
                // Inline code: keep the content, mark word edges.
                out.push(' ');
                for inner in chars.by_ref() {
                    if inner == '`' {
                        break;
                    }
                    out.push(inner);
                }
                out.push(' ');
            }
            '[' => {
                // Link [label](url) or image ![alt](url): keep label, drop
                // the target. Images (preceded by !) drop the alt too.
                let mut label = String::new();
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == ']' {
                        closed = true;
                        break;
                    }
                    label.push(inner);
                }
                let is_link = closed && chars.peek() == Some(&'(');
                if is_link {
                    for inner in chars.by_ref() {
                        if inner == ')' {
                            break;
                        }
                    }
                }
                let image = out.ends_with('!');
                if image {
                    out.pop();
                } else {
                    out.push_str(&label);
                }
            }
            '-' | '+' if line_start(&out) => {
                // List marker: drop, keep the item.
            }
            '<' => {
                // Raw HTML: strip the tag, keep surrounding text.
                for inner in chars.by_ref() {
                    if inner == '>' {
                        break;
                    }
                }
                out.push(' ');
            }
            _ => out.push(ch),
        }
    }
    // Bare URLs: drop whole tokens starting with a scheme or www.
    out.split_whitespace()
        .filter(|token| {
            !token.starts_with("http://")
                && !token.starts_with("https://")
                && !token.starts_with("www.")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn line_start(out: &str) -> bool {
    out.is_empty() || out.ends_with('\n')
}

/// Split run-together identifiers into spoken words: `camelCase` and
/// `PascalCase` split at case edges, `snake_case` and `kebab-case` at
/// separators, acronym runs stay whole until a new word starts.
fn split_identifiers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (index, &ch) in chars.iter().enumerate() {
        if (ch == '_' || ch == '-')
            && index > 0
            && chars[index - 1].is_alphanumeric()
            && chars
                .get(index + 1)
                .is_some_and(|next| next.is_alphanumeric())
        {
            out.push(' ');
            continue;
        }
        if index > 0 {
            let prev = chars[index - 1];
            let next = chars.get(index + 1).copied();
            let lower_to_upper =
                (prev.is_lowercase() || prev.is_ascii_digit()) && ch.is_uppercase();
            let acronym_to_word =
                prev.is_uppercase() && ch.is_uppercase() && next.is_some_and(char::is_lowercase);
            if lower_to_upper || acronym_to_word {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
#[path = "../../tests/tts_normalize.rs"]
mod tests;
