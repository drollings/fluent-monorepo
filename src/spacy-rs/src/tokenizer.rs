//! The deterministic two-pass tokenizer — the port of
//! `spacy/tokenizer.pyx`.
//!
//! **Pass 1 (`_tokenize_affixes`):** the text is split into whitespace
//! delimited runs. A single `' '` between runs is absorbed as the previous
//! token's `spacy` flag; any other whitespace (tab, newline, runs of spaces)
//! becomes its own token. Each run is then tokenized by the
//! prefix/suffix/infix affix rules, the `token_match`/`url_match` regexes, or
//! a special-case rule, with a per-span cache.
//!
//! **Pass 2 (`_apply_special_cases`):** multi-token special cases (rules whose
//! text contains whitespace or an affix, per `faster_heuristics`) are matched
//! as token-orth sequences over the pass-1 doc, the longest non-overlapping
//! spans win, and matched spans are spliced with their rule token sequences
//! (recomputing `idx` in-span, preserving `spacy` of the final token).
//!
//! The tokenizer is immutable after construction and `Sync`: the affix regexes
//! are compiled once, the rule tables freeze into lock-free snapshots at the
//! end of `Tokenizer::new`, and the span cache is lock-striped so concurrent
//! docs share a tokenizer without serializing on a lock.

use fancy_regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use crate::doc::{Doc, TokenRecord};
use crate::error::SpacyError;
use crate::hash::hash_utf8;
use crate::lexeme::Lexeme;
use crate::vocab::Vocab;

/// One token of a special-case rule: an orth lexeme plus the token-level
/// `NORM` override (the only per-token attribute spaCy permits in special
/// cases besides `ORTH` — `tokenizer.pyx:_validate_special_case`).
#[derive(Debug, Clone)]
pub struct SpecialToken {
    /// The orth lexeme.
    pub orth: Arc<Lexeme>,
    /// Token-level `NORM` hash; `0` means "use the lexeme norm".
    pub norm: u64,
}

/// A loaded special-case rule.
#[derive(Debug, Clone)]
pub struct SpecialRule {
    /// The rule text (the `ORTH` concatenation of its tokens).
    pub key: String,
    /// The token sequence the rule expands to.
    pub tokens: Vec<SpecialToken>,
    /// The affix tokenization of `key` without special cases — the phrase the
    /// matcher matches on (`_tokenize_affixes(string, False)`); empty when the
    /// rule is not in the matcher.
    pub phrase: Vec<u64>,
}

/// Tokenizer configuration: language-specific pattern strings and switches.
#[derive(Debug, Clone)]
pub struct TokenizerConfig {
    /// Joined prefix pattern (`^p1|^p2|...`) or `None` to disable prefix
    /// splitting.
    pub prefix_pattern: Option<String>,
    /// Joined suffix pattern (`s1$|s2$|...`) or `None`.
    pub suffix_pattern: Option<String>,
    /// Joined infix pattern (`i1|i2|...`) or `None`.
    pub infix_pattern: Option<String>,
    /// Whole-span `token_match` pattern or `None`.
    pub token_match: Option<String>,
    /// Whole-span `url_match` pattern or `None`.
    pub url_match: Option<String>,
    /// Restrict the matcher pass to rules containing affixes or spaces
    /// (`tokenizer.pyx:138-140`).
    pub faster_heuristics: bool,
    /// Maximum number of tokenization chunks to cache
    /// (`tokenizer.pyx:73`). Add-only once full, matching spaCy.
    pub max_cache_size: usize,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            prefix_pattern: None,
            suffix_pattern: None,
            infix_pattern: None,
            token_match: None,
            url_match: None,
            faster_heuristics: true,
            max_cache_size: 10_000,
        }
    }
}

/// The deterministic tokenizer. The span path (`tokenize` and everything it
/// calls) only reads immutable snapshots, so a shared tokenizer serves
/// concurrent docs with no lock held across spans.
pub struct Tokenizer {
    vocab: Arc<Vocab>,
    prefixes: Option<Regex>,
    suffixes: Option<Regex>,
    infixes: Option<Regex>,
    token_match: Option<Regex>,
    url_match: Option<Regex>,
    faster_heuristics: bool,
    max_cache_size: usize,
    /// Rule tables staging area: written only by `add_special_case` before
    /// the freeze at the end of `new`, drained into `frozen`, never read by
    /// the span path.
    staged_specials: Mutex<HashMap<u64, Arc<SpecialRule>>>,
    /// Rules keyed by text (`_rules`), for the matcher / validation.
    staged_rules: Mutex<HashMap<String, Arc<SpecialRule>>>,
    /// Matcher-eligible rules (`_special_matcher`), in registration order.
    staged_matcher: Mutex<Vec<Arc<SpecialRule>>>,
    /// Frozen rule snapshots, published once at the end of `new`. The span
    /// path reads these without locking.
    frozen: OnceLock<Arc<FrozenTables>>,
    /// Per-span tokenization cache (`_cache`), lock-striped by span hash so
    /// concurrent docs contend only within a shard. Pure memo: same key and
    /// values as the old single map, only the capacity accounting is
    /// per-shard (total bound = shards × per-shard cap).
    cache: [CacheShard; CACHE_SHARDS],
}

/// The frozen rule tables: every rule the constructor registered, published
/// once and read lock-free by the span path.
#[derive(Default)]
struct FrozenTables {
    /// Special-case rules keyed by span hash (`_specials`).
    specials: HashMap<u64, Arc<SpecialRule>>,
    /// Rules keyed by text (`_rules`), for the matcher / validation.
    #[allow(dead_code)]
    rules: HashMap<String, Arc<SpecialRule>>,
    /// Matcher-eligible rules (`_special_matcher`), in registration order.
    matcher: Vec<Arc<SpecialRule>>,
    /// Exact first-orth index over matcher-eligible rules with non-empty
    /// phrases, in registration order per orth: phrase matching only compares
    /// rules whose first token can match at a position. Exact (orth equality),
    /// not heuristic — same matches as the full scan, fewer comparisons.
    first_orth: HashMap<u64, Vec<Arc<SpecialRule>>>,
}

/// Lock-striping shard count for the span cache. Pure contention tuning —
/// any count preserves the exact key-equality memo semantics, so this is a
/// constant, not a threshold: no tuning pass reads it, no behavioral gate
/// depends on it.
const CACHE_SHARDS: usize = 16;

/// One lock-striping shard of the span cache: span hash → token lexemes.
type CacheShard = Mutex<HashMap<u64, Arc<Vec<Arc<Lexeme>>>>>;

/// Shard for `key`: the span hash already mixes well (MurmurHash64A), so the
/// low bits stripe uniformly.
fn shard_of(key: u64) -> usize {
    (key as usize) % CACHE_SHARDS
}

/// `(&key, &[(orth, norm)])` — the exception data shape the language modules
/// provide (norm `None` means "no override").
pub type SpecialCaseData<'a> = (&'a str, &'a [(&'a str, Option<&'a str>)]);

/// The outcome of affix splitting: the untouched core plus the peeled
/// prefix/suffix lexemes (`_split_affixes`).
pub struct AffixSplit {
    /// The remaining string after prefix/suffix removal.
    pub remaining: String,
    /// Prefix lexemes, in peel order.
    pub prefixes: Vec<Arc<Lexeme>>,
    /// Suffix lexemes, in peel order.
    pub suffixes: Vec<Arc<Lexeme>>,
}

impl Tokenizer {
    /// Build a tokenizer from a vocabulary, a configuration, and the language's
    /// special-case rules. Regexes are compiled first, then rules are loaded
    /// (each validated, phrase-tokenized, and cached), mirroring
    /// `Tokenizer.__init__` (`tokenizer.pyx:31-73`). The rule tables freeze
    /// when the builder returns: later `add_special_case` calls fail instead
    /// of mutating tables under concurrent readers (rule genesis rebuilds
    /// the tokenizer). This is the one intentional behavior change.
    pub fn new(
        vocab: Arc<Vocab>,
        config: &TokenizerConfig,
        exceptions: &[SpecialCaseData<'_>],
    ) -> Result<Self, SpacyError> {
        let compile = |p: &str| Regex::new(p).map_err(|e| SpacyError::Regex(e.to_string()));
        let cache: [CacheShard; CACHE_SHARDS] = std::array::from_fn(|_| Mutex::new(HashMap::new()));
        let tokenizer = Self {
            vocab,
            prefixes: config.prefix_pattern.as_deref().map(compile).transpose()?,
            suffixes: config.suffix_pattern.as_deref().map(compile).transpose()?,
            infixes: config.infix_pattern.as_deref().map(compile).transpose()?,
            token_match: config.token_match.as_deref().map(compile).transpose()?,
            url_match: config.url_match.as_deref().map(compile).transpose()?,
            faster_heuristics: config.faster_heuristics,
            max_cache_size: config.max_cache_size,
            staged_specials: Mutex::new(HashMap::new()),
            staged_rules: Mutex::new(HashMap::new()),
            staged_matcher: Mutex::new(Vec::new()),
            frozen: OnceLock::new(),
            cache,
        };
        for (key, tokens) in exceptions {
            tokenizer.add_special_case(key, tokens)?;
        }
        tokenizer.freeze();
        Ok(tokenizer)
    }

    /// Drain the staged tables into the immutable snapshots and publish them.
    /// Runs once at the end of `new`; afterwards the span path reads the
    /// snapshots without locking.
    fn freeze(&self) {
        let specials = std::mem::take(&mut *common_core::sync::lock(&self.staged_specials));
        let rules = common_core::sync::lock(&self.staged_rules)
            .drain()
            .collect::<HashMap<_, _>>();
        let staged = std::mem::take(&mut *common_core::sync::lock(&self.staged_matcher));
        let mut tables = FrozenTables {
            specials,
            rules,
            matcher: Vec::with_capacity(staged.len()),
            first_orth: HashMap::new(),
        };
        // Registration order is preserved in both the matcher vec and each
        // first-orth bucket, so phrase-match resolution (including same-span
        // ties) agrees with the pre-freeze scan order.
        for rule in staged {
            if let Some(first) = rule.phrase.first() {
                tables
                    .first_orth
                    .entry(*first)
                    .or_default()
                    .push(Arc::clone(&rule));
            }
            tables.matcher.push(rule);
        }
        let _ = self.frozen.set(Arc::new(tables));
    }

    /// The frozen rule tables. During construction (before `freeze`)
    /// `add_special_case` stages rules without reading them, and phrase
    /// tokenization runs with special cases disabled, so every span-path
    /// read lands here only after publication.
    fn tables(&self) -> &FrozenTables {
        static EMPTY: std::sync::LazyLock<FrozenTables> =
            std::sync::LazyLock::new(FrozenTables::default);
        self.frozen.get().map_or(&EMPTY, |t| &**t)
    }

    /// Tokenize `text` into a `Doc` (`__call__` — `tokenizer.pyx:152-162`):
    /// the affix pass with special cases, then the matcher pass.
    pub fn tokenize(&self, text: &str) -> Result<Doc, SpacyError> {
        let mut doc = self.tokenize_affixes(text, true)?;
        self.apply_special_cases(&mut doc);
        Ok(doc)
    }

    /// Pass 1 — `_tokenize_affixes` (`tokenizer.pyx:164-210`).
    ///
    /// The text is walked on byte ranges at char boundaries: spans borrow
    /// from `text` (no `Vec<char>`, no per-span `String`), and `idx` stays
    /// char-based exactly as before — `Doc` derives it from lexeme lengths,
    /// untouched here.
    fn tokenize_affixes(&self, text: &str, with_special_cases: bool) -> Result<Doc, SpacyError> {
        if text.len() >= (1 << 30) {
            return Err(SpacyError::TextTooLong(text.len()));
        }
        let mut doc = Doc::new(self.vocab.clone());
        if text.is_empty() {
            return Ok(doc);
        }
        let mut has_special = false;
        let mut run_start = 0usize;
        let mut run_is_ws = text.chars().next().is_some_and(char::is_whitespace);
        for (i, c) in text.char_indices() {
            if c.is_whitespace() != run_is_ws {
                if run_start < i {
                    let span = &text[run_start..i];
                    let key = hash_utf8(span);
                    if !self.try_specials_and_cache(
                        key,
                        &mut doc,
                        &mut has_special,
                        with_special_cases,
                    ) {
                        self.tokenize_span(
                            &mut doc,
                            span,
                            key,
                            &mut has_special,
                            with_special_cases,
                        )?;
                    }
                }
                if c == ' ' {
                    let last = doc
                        .tokens_mut()
                        .last_mut()
                        .ok_or_else(|| SpacyError::Annotation("no token before space".into()))?;
                    last.spacy = true;
                    run_start = i + 1;
                } else {
                    run_start = i;
                }
                run_is_ws = !run_is_ws;
            }
        }
        if run_start < text.len() {
            let span = &text[run_start..];
            let key = hash_utf8(span);
            if !self.try_specials_and_cache(key, &mut doc, &mut has_special, with_special_cases) {
                self.tokenize_span(&mut doc, span, key, &mut has_special, with_special_cases)?;
            }
            let last = doc
                .tokens_mut()
                .last_mut()
                .ok_or_else(|| SpacyError::Annotation("no token after final span".into()))?;
            last.spacy = text.ends_with(' ') && !run_is_ws;
        }
        Ok(doc)
    }

    /// `_try_specials_and_cache` (`tokenizer.pyx:364-391`): push a cached
    /// special-case expansion (setting `has_special`) or a cached affix
    /// tokenization; `true` on hit. Reads the frozen snapshots without
    /// locking; only the shard owning `key` is locked for the cache probe.
    fn try_specials_and_cache(
        &self,
        key: u64,
        doc: &mut Doc,
        has_special: &mut bool,
        with_special_cases: bool,
    ) -> bool {
        if with_special_cases {
            if let Some(rule) = self.tables().specials.get(&key) {
                for st in &rule.tokens {
                    let mut record = TokenRecord::new(st.orth.clone());
                    record.norm = st.norm;
                    doc.push_record(record, false);
                }
                *has_special = true;
                return true;
            }
        }
        if let Some(cached) = common_core::sync::lock(&self.cache[shard_of(key)])
            .get(&key)
            .cloned()
        {
            for lexeme in cached.iter() {
                doc.push_lexeme(lexeme.clone(), false);
            }
            return true;
        }
        false
    }

    /// `_tokenize` (`tokenizer.pyx:393-404`): split affixes, attach tokens,
    /// and cache the fresh span's lexemes when no special was used and the
    /// cache is not full.
    fn tokenize_span(
        &self,
        doc: &mut Doc,
        span: &str,
        key: u64,
        has_special: &mut bool,
        with_special_cases: bool,
    ) -> Result<(), SpacyError> {
        let orig_size = doc.len();
        let AffixSplit {
            remaining,
            prefixes,
            suffixes,
        } = self.split_affixes(span, has_special, with_special_cases)?;
        self.attach_tokens(
            doc,
            &remaining,
            &prefixes,
            &suffixes,
            has_special,
            with_special_cases,
        )?;
        let n = doc.len() - orig_size;
        // Add-only-once-full memo, matching spaCy — accounted per shard so
        // the total bound stays shards × per-shard cap.
        let mut shard = common_core::sync::lock(&self.cache[shard_of(key)]);
        if !*has_special && n > 0 && shard.len() < self.shard_cap() {
            let lexemes: Vec<Arc<Lexeme>> = doc.tokens()[orig_size..]
                .iter()
                .map(|t| t.lexeme.clone())
                .collect();
            shard.insert(key, Arc::new(lexemes));
        }
        Ok(())
    }

    /// Per-shard cache capacity: the total stays bounded by
    /// shards × per-shard cap (≤ `max_cache_size`, exact when divisible).
    fn shard_cap(&self) -> usize {
        self.max_cache_size.div_ceil(CACHE_SHARDS)
    }

    /// `_split_affixes` (`tokenizer.pyx:406-452`): peel prefixes/suffixes off
    /// the span, stopping early when a `token_match` or special-case hit
    /// covers the remainder. Works on byte ranges into `span` — the peel
    /// points are regex byte offsets, so no intermediate `String` is built;
    /// only the surviving core is copied once for the owned return value.
    fn split_affixes(
        &self,
        span: &str,
        _has_special: &mut bool,
        with_special_cases: bool,
    ) -> Result<AffixSplit, SpacyError> {
        let mut lo = 0usize;
        let mut hi = span.len();
        let mut prefixes: Vec<Arc<Lexeme>> = Vec::new();
        let mut suffixes: Vec<Arc<Lexeme>> = Vec::new();
        let mut last_size = 0usize;
        while lo < hi && hi - lo != last_size {
            let string = &span[lo..hi];
            if let Some(tm) = &self.token_match {
                if tm
                    .is_match(string)
                    .map_err(|e| SpacyError::Regex(e.to_string()))?
                {
                    break;
                }
            }
            if with_special_cases && self.has_special(string) {
                break;
            }
            last_size = string.len();
            let pre_len = self.find_prefix(string)?;
            if pre_len != 0 {
                let minus_pre = &string[pre_len..];
                if !minus_pre.is_empty() && with_special_cases && self.has_special(minus_pre) {
                    prefixes.push(self.lex(&string[..pre_len]));
                    lo += pre_len;
                    break;
                }
            }
            let suf_len = self.find_suffix(&string[pre_len..])?;
            if suf_len != 0 {
                let minus_suf = &string[..string.len() - suf_len];
                if !minus_suf.is_empty() && with_special_cases && self.has_special(minus_suf) {
                    suffixes.push(self.lex(&string[string.len() - suf_len..]));
                    hi -= suf_len;
                    break;
                }
            }
            if pre_len != 0 && suf_len != 0 && pre_len + suf_len <= string.len() {
                prefixes.push(self.lex(&string[..pre_len]));
                suffixes.push(self.lex(&string[string.len() - suf_len..]));
                lo += pre_len;
                hi -= suf_len;
            } else if pre_len != 0 {
                prefixes.push(self.lex(&string[..pre_len]));
                lo += pre_len;
            } else if suf_len != 0 {
                suffixes.push(self.lex(&string[string.len() - suf_len..]));
                hi -= suf_len;
            }
        }
        Ok(AffixSplit {
            remaining: span[lo..hi].to_string(),
            prefixes,
            suffixes,
        })
    }

    /// `_attach_tokens` (`tokenizer.pyx:454-512`): prefixes, then the core
    /// (specials / token-match / URL-match / infixes / single token), then
    /// suffixes in reverse order.
    #[allow(clippy::too_many_arguments)]
    fn attach_tokens(
        &self,
        doc: &mut Doc,
        string: &str,
        prefixes: &[Arc<Lexeme>],
        suffixes: &[Arc<Lexeme>],
        has_special: &mut bool,
        with_special_cases: bool,
    ) -> Result<(), SpacyError> {
        for p in prefixes {
            doc.push_lexeme(p.clone(), false);
        }
        if !string.is_empty() {
            let key = hash_utf8(string);
            if !self.try_specials_and_cache(key, doc, has_special, with_special_cases) {
                let token_match_hit = match &self.token_match {
                    Some(tm) => tm
                        .is_match(string)
                        .map_err(|e| SpacyError::Regex(e.to_string()))?,
                    None => false,
                };
                let url_match_hit = match &self.url_match {
                    Some(um) => um
                        .is_match(string)
                        .map_err(|e| SpacyError::Regex(e.to_string()))?,
                    None => false,
                };
                if token_match_hit || url_match_hit {
                    doc.push_lexeme(self.lex(string), false);
                } else if let Some(matches) = self.find_infix(string)? {
                    if matches.is_empty() {
                        doc.push_lexeme(self.lex(string), false);
                    } else {
                        let mut start = 0usize;
                        for (infix_start, infix_end) in matches {
                            if infix_start == 0 {
                                continue;
                            }
                            if infix_start != start {
                                doc.push_lexeme(self.lex(&string[start..infix_start]), false);
                            }
                            if infix_start != infix_end {
                                doc.push_lexeme(self.lex(&string[infix_start..infix_end]), false);
                            }
                            start = infix_end;
                        }
                        let trailing = &string[start..];
                        if !trailing.is_empty() {
                            doc.push_lexeme(self.lex(trailing), false);
                        }
                    }
                } else {
                    doc.push_lexeme(self.lex(string), false);
                }
            }
        }
        for s in suffixes.iter().rev() {
            doc.push_lexeme(s.clone(), false);
        }
        Ok(())
    }

    /// Pass 2 — `_apply_special_cases` (`tokenizer.pyx:243-285`).
    fn apply_special_cases(&self, doc: &mut Doc) {
        let matches = self.find_phrase_matches(doc);
        if matches.is_empty() {
            return;
        }
        let filtered = filter_special_spans(matches);
        let mut span_data: HashMap<usize, (Arc<SpecialRule>, usize)> = HashMap::new();
        for (rule, start, end) in filtered {
            span_data.insert(start, (rule, end));
        }
        let mut new_tokens: Vec<TokenRecord> = Vec::with_capacity(doc.len());
        let mut i = 0usize;
        while i < doc.len() {
            if let Some((rule, end)) = span_data.get(&i).cloned() {
                let orig_final_spacy = doc.token(end - 1).spacy;
                let orig_idx = doc.token(i).idx;
                let mut idx_offset = 0u32;
                let token_count = rule.tokens.len();
                for (j, st) in rule.tokens.iter().enumerate() {
                    let mut record = TokenRecord::new(st.orth.clone());
                    record.norm = st.norm;
                    record.idx = orig_idx + idx_offset;
                    idx_offset += st.orth.length;
                    if j + 1 == token_count {
                        record.spacy = orig_final_spacy;
                    }
                    new_tokens.push(record);
                }
                i = end;
            } else {
                new_tokens.push(doc.token(i).clone());
                i += 1;
            }
        }
        *doc.tokens_mut() = new_tokens;
    }

    /// `PhraseMatcher.find_matches` over the doc: every occurrence of every
    /// matcher-eligible rule's affix-tokenized orth sequence in the doc's
    /// token orth sequence (matcher runs on `ORTH`, `PhraseMatcher` default).
    /// Positions consult the exact first-orth index, so only rules whose
    /// first token matches at a position are compared — same matches as the
    /// full rule scan, fewer comparisons. Spans are visited start-ascending
    /// with rules in registration order per start, which resolves to the same
    /// spans as the old rule-major scan (including same-span ties).
    fn find_phrase_matches(&self, doc: &Doc) -> Vec<(Arc<SpecialRule>, usize, usize)> {
        let tables = self.tables();
        if tables.matcher.is_empty() {
            return Vec::new();
        }
        let orth: Vec<u64> = doc.tokens().iter().map(|t| t.lexeme.orth).collect();
        let n = orth.len();
        let mut out = Vec::new();
        for start in 0..n {
            let Some(rules) = tables.first_orth.get(&orth[start]) else {
                continue;
            };
            for rule in rules {
                let phrase = &rule.phrase;
                let plen = phrase.len();
                if plen == 0 || start + plen > n {
                    continue;
                }
                if orth[start..start + plen] == phrase[..] {
                    out.push((Arc::clone(rule), start, start + plen));
                }
            }
        }
        out
    }

    /// `_filter_special_spans` (`tokenizer.pyx:287-301`): sort by
    /// (length asc, start desc), keep a span only when neither its start nor
    /// its end-1 was covered by an earlier (longer) span, then sort by start.
    fn find_prefix(&self, string: &str) -> Result<usize, SpacyError> {
        match &self.prefixes {
            None => Ok(0),
            Some(re) => {
                let len = re
                    .find(string)
                    .map_err(|e| SpacyError::Regex(e.to_string()))?
                    .map_or(0, |m| m.end() - m.start());
                Ok(len)
            }
        }
    }

    /// `find_suffix` (`tokenizer.pyx:562-574`): the suffix patterns are
    /// `$`-anchored, so the match length is the peeled suffix length in bytes.
    fn find_suffix(&self, string: &str) -> Result<usize, SpacyError> {
        match &self.suffixes {
            None => Ok(0),
            Some(re) => {
                let len = re
                    .find(string)
                    .map_err(|e| SpacyError::Regex(e.to_string()))?
                    .map_or(0, |m| m.end() - m.start());
                Ok(len)
            }
        }
    }

    /// `find_infix` (`tokenizer.pyx:534-546`): byte ranges of every infix
    /// match, left to right; `None` when no infix regex is configured.
    fn find_infix(&self, string: &str) -> Result<Option<Vec<(usize, usize)>>, SpacyError> {
        let Some(re) = &self.infixes else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for m in re.find_iter(string) {
            let m = m.map_err(|e| SpacyError::Regex(e.to_string()))?;
            out.push((m.start(), m.end()));
        }
        Ok(Some(out))
    }

    /// Whether `s` has a loaded special-case rule (`_specials` lookup).
    /// Reads the frozen snapshot without locking.
    fn has_special(&self, s: &str) -> bool {
        self.tables().specials.contains_key(&hash_utf8(s))
    }

    /// The lexeme for `text` (`vocab.get(mem, ...)`), interning on sight.
    fn lex(&self, text: &str) -> Arc<Lexeme> {
        self.vocab.lexicon().get_or_create(text)
    }

    /// `add_special_case` (`tokenizer.pyx:600-624`): validate the rule, stage
    /// it for the freeze, decide matcher membership, tokenize its phrase.
    /// Only the constructor may call this: after the freeze it returns
    /// `TokenizerFrozen` instead of mutating live tables (the one intentional
    /// behavior change — rule genesis rebuilds the tokenizer).
    fn add_special_case(
        &self,
        key: &str,
        tokens: &[(&str, Option<&str>)],
    ) -> Result<(), SpacyError> {
        if self.frozen.get().is_some() {
            return Err(SpacyError::TokenizerFrozen {
                key: key.to_string(),
            });
        }
        let concat: String = tokens.iter().map(|(orth, _)| *orth).collect();
        if concat != key {
            return Err(SpacyError::SpecialCase {
                key: key.to_string(),
                detail: format!("concatenated ORTH {concat:?} != rule text"),
            });
        }
        let special_tokens: Vec<SpecialToken> = tokens
            .iter()
            .map(|(orth, norm)| SpecialToken {
                orth: self.lex(orth),
                norm: norm.map_or(0, |n| self.vocab.strings().add(n)),
            })
            .collect();
        let has_space = key.contains(' ');
        let has_affix = self.find_prefix(key)? != 0
            || self.find_infix(key)?.is_some_and(|v| !v.is_empty())
            || self.find_suffix(key)? != 0;
        let in_matcher = !self.faster_heuristics || has_space || has_affix;
        let phrase = if in_matcher {
            self.tokenize_affixes(key, false)?
                .tokens()
                .iter()
                .map(|t| t.lexeme.orth)
                .collect()
        } else {
            Vec::new()
        };
        let rule = Arc::new(SpecialRule {
            key: key.to_string(),
            tokens: special_tokens,
            phrase,
        });
        common_core::sync::lock(&self.staged_specials).insert(hash_utf8(key), Arc::clone(&rule));
        common_core::sync::lock(&self.staged_rules).insert(key.to_string(), Arc::clone(&rule));
        if in_matcher {
            common_core::sync::lock(&self.staged_matcher).push(rule);
        }
        for shard in &self.cache {
            common_core::sync::lock(shard).clear();
        }
        Ok(())
    }
}

/// `_filter_special_spans` (`tokenizer.pyx:287-301`): longest-first span
/// resolution, then start-sorted. Spans overlapping an already-kept span's
/// tokens are dropped.
fn filter_special_spans(
    mut matches: Vec<(Arc<SpecialRule>, usize, usize)>,
) -> Vec<(Arc<SpecialRule>, usize, usize)> {
    matches.sort_by(|a, b| {
        let la = a.2 - a.1;
        let lb = b.2 - b.1;
        la.cmp(&lb).then(b.1.cmp(&a.1))
    });
    let mut seen: HashSet<usize> = HashSet::new();
    let mut filtered: Vec<(Arc<SpecialRule>, usize, usize)> = Vec::new();
    for (rule, start, end) in matches.into_iter().rev() {
        if !seen.contains(&start) && !seen.contains(&(end - 1)) {
            filtered.push((rule, start, end));
        }
        seen.extend(start..end);
    }
    filtered.sort_by_key(|(_, start, _)| *start);
    filtered
}

#[cfg(test)]
#[path = "../tests/tokenizer.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/tokenizer_snapshot.rs"]
mod tokenizer_snapshot_tests;
