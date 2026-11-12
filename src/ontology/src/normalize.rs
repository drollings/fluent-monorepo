//! Single source for YaGO name normalization.
//!
//! Lexicon normalization only (Wikidata `_Q\d+` strip, `_UXXXX` decode,
//! `_`→space, lowercase, WS collapse); only the Rust implementation remains.
//! Also owns the IRI→CURIE compression used by the loader, the view, and
//! the plausibility fallback, so the `schema:`/`yago:` prefix table lives
//! in exactly one place.

#![forbid(unsafe_code)]

use regex::Regex;
use std::sync::LazyLock;

static Q_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_Q\d+$").unwrap());
static UNDERSCORE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_").unwrap());
static UNICODE_ESC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_U([0-9A-Fa-f]{4})_?").unwrap());
static UNICODE_LOWER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"u([0-9a-f]{4})").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static HYPHEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-–—]").unwrap());

/// Apply the local-name pipeline (Wikidata `_Q\d+` strip, `_UXXXX` decode,
/// `_`→space, lowercase, WS collapse) to an already-extracted name.
/// Shared by [`normalize_yago_name`] and the full-IRI branch of the
/// taxonomy converter so the decode logic lives in exactly one place.
#[must_use]
/// Apply the local-name pipeline (Wikidata `_Q\d+` strip, `_UXXXX` decode,
/// `_`→space, lowercase, WS collapse) to an already-extracted name.
/// Shared by [`normalize_yago_name`] and the full-IRI branch of the
/// taxonomy converter so the decode logic lives in exactly one place.
pub fn normalize_extracted_name(name: &str) -> String {
    let mut s = Q_SUFFIX_RE.replace(name, "").to_string();
    s = UNICODE_ESC_RE
        .replace_all(&s, |caps: &regex::Captures| {
            let hex = &caps[1];
            char::from_u32(u32::from_str_radix(hex, 16).unwrap_or(0))
                .unwrap_or('?')
                .to_string()
        })
        .to_string();
    s = s.to_lowercase();
    s = UNICODE_LOWER_RE
        .replace_all(&s, |caps: &regex::Captures| {
            let hex = &caps[1];
            char::from_u32(u32::from_str_radix(hex, 16).unwrap_or(0))
                .unwrap_or('?')
                .to_string()
        })
        .to_string();
    s = UNDERSCORE_RE.replace_all(&s, " ").to_string();
    s = WS_RE.replace_all(&s, " ").to_string();
    s.trim().to_string()
}

/// Normalize a YaGO IRI local name for lexicon matching.
///
/// Extracts the local part (after the last `/`, `#`, or `:`) and runs the
/// [`normalize_extracted_name`] pipeline.
/// `yago:Adult_Video_Game_Q3362070` → `adult video game`.
#[must_use]
/// Normalize a YaGO IRI local name for lexicon matching.
/// 
/// Extracts the local part (after the last `/`, `#`, or `:`) and runs the
/// [`normalize_extracted_name`] pipeline.
/// `yago:Adult_Video_Game_Q3362070` → `adult video game`.
pub fn normalize_yago_name(iri: &str) -> String {
    let local = iri
        .rsplit('/')
        .next()
        .unwrap_or(iri)
        .rsplit('#')
        .next()
        .unwrap_or(iri)
        .rsplit(':')
        .next()
        .unwrap_or(iri);
    normalize_extracted_name(local)
}

/// Normalize a CURIE (`yago:Foo` / `schema:Bar`) — lower, `UXXXX` decode, `_→space`.
#[must_use]
/// Normalize a CURIE (`yago:Foo` / `schema:Bar`) — lower, `UXXXX` decode, `_→space`.
pub fn normalize_lexicon_name(curie: &str) -> String {
    normalize_yago_name(curie)
}

/// Compress a full IRI to its canonical short name: `schema:Person` for
/// schema.org, `yago:Person` for the YaGO resource namespace, else the IRI
/// unchanged. The single shared prefix table behind the loader
/// (`canonical_class_name`), the view's curie keys, and [`lemma_to_curies`].
/// Matches on the closed [`crate::prefix::CuriePrefix`] IRI bases so new
/// code never repeats the raw base strings.
#[must_use]
/// Compress a full IRI to its canonical short name: `schema:Person` for
/// schema.org, `yago:Person` for the YaGO resource namespace, else the IRI
/// unchanged. The single shared prefix table behind the loader
/// (`canonical_class_name`), the view's curie keys, and [`lemma_to_curies`].
/// Matches on the closed [`crate::prefix::CuriePrefix`] IRI bases so new
/// code never repeats the raw base strings.
pub fn iri_to_curie(iri: &str) -> String {
    if let Some(local) = iri.strip_prefix(crate::prefix::SCHEMA_IRI_BASE) {
        format!("{}:{local}", crate::prefix::CuriePrefix::Schema)
    } else if let Some(local) = iri.strip_prefix(crate::prefix::YAGO_IRI_BASE) {
        format!("{}:{local}", crate::prefix::CuriePrefix::Yago)
    } else {
        iri.to_string()
    }
}

/// Capitalize the first character (ASCII-aware): `dog` → `Dog`.
#[must_use]
/// Capitalize the first character (ASCII-aware): `dog` → `Dog`.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
    }
}

/// The `yago:`/`schema:` curie fallbacks for a raw lemma (`dog` →
/// `yago:Dog` + `schema:Dog`), sharing [`iri_to_curie`]'s prefix table and
/// [`capitalize`]. Used by the plausibility fallback when a lemma id is not
/// directly in the concept store.
#[must_use]
/// The `yago:`/`schema:` curie fallbacks for a raw lemma (`dog` →
/// `yago:Dog` + `schema:Dog`), sharing [`iri_to_curie`]'s prefix table and
/// [`capitalize`]. Used by the plausibility fallback when a lemma id is not
/// directly in the concept store.
pub fn lemma_to_curies(lemma: &str) -> [String; 2] {
    let capitalized = capitalize(lemma);
    [
        format!("yago:{capitalized}"),
        format!("schema:{capitalized}"),
    ]
}

/// Token variants for plural → singular matching (noun `lemma_rules`-style).
#[must_use]
/// Token variants for plural → singular matching (noun `lemma_rules`-style).
pub fn token_variants(token: &str) -> Vec<String> {
    let t = token.to_lowercase();
    let mut variants = vec![t.clone()];
    if t.ends_with("ies") && t.len() > 3 {
        variants.push(format!("{}y", &t[..t.len() - 3]));
    }
    if t.ends_with("ves") && t.len() > 3 {
        variants.push(format!("{}f", &t[..t.len() - 3]));
        variants.push(format!("{}fe", &t[..t.len() - 3]));
    }
    if t.ends_with("ses") && t.len() > 3 {
        variants.push(t[..t.len() - 2].to_string());
    }
    if t.ends_with('s') && t.len() > 2 && !t.ends_with("ss") {
        variants.push(t[..t.len() - 1].to_string());
    }
    if t.ends_with("men") && t.len() > 3 {
        variants.push(format!("{}man", &t[..t.len() - 3]));
    }
    variants
}

/// Whether normalized multi-word name matches lexicon (exact phrase, any token, last token + variants).
#[must_use]
#[allow(clippy::implicit_hasher)]
/// Whether normalized multi-word name matches lexicon (exact phrase, any token, last token + variants).
pub fn matches_lexicon(normalized: &str, lexicon: &std::collections::HashSet<String>) -> bool {
    if normalized.is_empty() {
        return false;
    }
    if lexicon.contains(normalized) {
        return true;
    }
    let mut tokens: Vec<String> = Vec::new();
    for part in normalized.split_whitespace() {
        for tok in HYPHEN_RE.split(part) {
            let t = tok.trim_matches(|c| " '\".,;:()[]".contains(c)).to_string();
            if !t.is_empty() {
                tokens.push(t);
            }
        }
    }
    if tokens.is_empty() {
        return false;
    }
    for tok in &tokens {
        if lexicon.contains(tok) {
            return true;
        }
        for var in token_variants(tok) {
            if lexicon.contains(&var) {
                return true;
            }
        }
    }
    if let Some(last) = tokens.last() {
        for var in token_variants(last) {
            if lexicon.contains(&var) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
#[path = "../tests/normalize.rs"]
mod normalize_tests;