//! rg-style glob + path + file-type matching, ported from zvec
//! `engine/utils/glob.ts` and `engine/utils/file-selection.ts`.
//!
//! One documented deviation: `resolve_file_type_patterns` uses a curated
//! static table instead of shelling out to `rg --type-list` (hermetic;
//! unknown type names still error, mirroring the zvec throw).

use regex::RegexBuilder;
use std::fmt::Write as _;
use thiserror::Error;

/// Glob / file-type selection failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Glob / file-type selection failure.
pub enum GlobError {
    /// Unknown ripgrep file-type name.
    #[error("unknown ripgrep file type: {0}")]
    UnknownFileType(String),
}

/// Include/exclude glob sets in ripgrep type-map form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Include/exclude glob sets in ripgrep type-map form.
pub struct FileTypePatterns {
    /// Inclusion globs.
    pub include: Vec<String>,
    /// Exclusion globs.
    pub exclude: Vec<String>,
}

/// Normalize a path pattern: backslashes and runs collapse to one `/`,
/// leading `./` segments strip (absolute patterns pass through).
/// Source: `normalizePathPattern`. Composes the canonical
/// `common_core::path::collapse_separators` for the separator step; dot
/// segments intentionally survive (pattern syntax, not a path — pinned by
/// `m1_glob_normalizer_matrix`).
#[must_use]
/// Normalize a path pattern: backslashes and runs collapse to one `/`,
/// leading `./` segments strip (absolute patterns pass through).
/// Source: `normalizePathPattern`. Composes the canonical
/// `common_core::path::collapse_separators` for the separator step; dot
/// segments intentionally survive (pattern syntax, not a path — pinned by
/// `m1_glob_normalizer_matrix`).
pub fn normalize_path_pattern(pattern: &str) -> String {
    let mut normalized = common_core::path::collapse_separators(pattern.trim());
    if is_absolute_path_pattern(&normalized) {
        return normalized;
    }
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    normalized
}

/// Normalize a candidate path for matching (separators only; no dot
/// handling). Composes `common_core::path::collapse_separators`.
#[must_use]
/// Normalize a candidate path for matching (separators only; no dot
/// handling). Composes `common_core::path::collapse_separators`.
pub fn normalize_path_for_match(path: &str) -> String {
    common_core::path::collapse_separators(path)
}

/// Normalize one selection glob to the indexed root-relative normal form:
/// a leading segment naming an indexing root (`src_dirs` entry, e.g.
/// `src`) is stripped once, so workspace-relative `src/<crate>/**` and
/// index-relative `<crate>/**` select the same files. Absolute patterns,
/// `**`-led patterns, and bare roots (empty remainder) pass through
/// unchanged — a bare root matches nothing new rather than everything.
/// A leading `!` negation is preserved across the strip.
/// Normal form is defined by `selection::select_files` roots: matching
/// runs against `file.relative_path`, never the workspace path.
#[must_use]
/// Normalize one selection glob to the indexed root-relative normal form.
pub fn normalize_selection_glob(glob: &str, src_dir_names: &[&str]) -> String {
    let normalized = normalize_path_pattern(glob);
    if is_absolute_path_pattern(&normalized) {
        return normalized;
    }
    let negated = normalized.starts_with('!');
    let body = normalized
        .strip_prefix('!')
        .map_or(normalized.as_str(), str::trim);
    let mut parts = body.split('/');
    let first = parts.next().unwrap_or("");
    if !first.is_empty() && src_dir_names.contains(&first) {
        let rest: String = parts.collect::<Vec<_>>().join("/");
        if !rest.is_empty() {
            return if negated { format!("!{rest}") } else { rest };
        }
    }
    normalized
}

/// Absolute patterns (`/…` or `X:/…`) match from the root. Stays local by
/// design: unlike `common_core::path::is_absolute_lexical` it rejects
/// backslash drives (`C:\x`) and non-alpha drives (`1:/x`) — glob-pattern
/// semantics pinned by `m1_glob_absolute_detection_matrix`.
#[must_use]
/// Absolute patterns (`/…` or `X:/…`) match from the root. Stays local by
/// design: unlike `common_core::path::is_absolute_lexical` it rejects
/// backslash drives (`C:\x`) and non-alpha drives (`1:/x`) — glob-pattern
/// semantics pinned by `m1_glob_absolute_detection_matrix`.
pub fn is_absolute_path_pattern(pattern: &str) -> bool {
    if pattern.starts_with('/') {
        return true;
    }
    let bytes = pattern.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// Whether a pattern carries glob metacharacters.
#[must_use]
/// Whether a pattern carries glob metacharacters.
pub fn has_path_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

/// ripgrep-style glob match (case-sensitive).
#[must_use]
/// ripgrep-style glob match (case-sensitive).
pub fn ripgrep_glob_matches(pattern: &str, path: &str) -> bool {
    ripgrep_glob_matches_with_case(pattern, path, false)
}

/// ripgrep-style glob match (case-insensitive).
#[must_use]
/// ripgrep-style glob match (case-insensitive).
pub fn ripgrep_glob_matches_case_insensitive(pattern: &str, path: &str) -> bool {
    ripgrep_glob_matches_with_case(pattern, path, true)
}

fn ripgrep_glob_matches_with_case(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    // Root tolerance runs on both axes: pattern spellings (as written
    // plus `src/`-stripped) against candidate spellings (as stored plus
    // root-stripped), so workspace-relative and index-relative globs
    // select the same rows without the caller pre-stripping.
    for variant in pattern_variants(pattern) {
        for candidate in candidate_spellings(path) {
            if variant_matches_ripgrep(&variant, &candidate, case_insensitive) {
                return true;
            }
        }
    }
    false
}

fn variant_matches_ripgrep(pattern: &str, candidate: &str, case_insensitive: bool) -> bool {
    if glob_pattern_matches(pattern, candidate, case_insensitive) {
        return true;
    }
    if let Some(dir) = pattern.strip_suffix("/**") {
        if glob_to_regex(dir, case_insensitive).is_match(candidate) {
            return true;
        }
    }
    if !pattern.contains('/') {
        let base = candidate.rsplit('/').next().unwrap_or(candidate);
        if glob_pattern_matches(pattern, base, case_insensitive) {
            return true;
        }
    }
    false
}

fn glob_pattern_matches(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    glob_to_regex(pattern, case_insensitive).is_match(path)
}

/// Precompile a glob pattern for repeated matching (same semantics as
/// the per-call path: normalized pattern, case flag fixed at build).
/// Used for static pattern lists matched once per file.
pub(crate) fn compile_path_glob(pattern: &str, case_insensitive: bool) -> regex::Regex {
    #[cfg(test)]
    COMPILE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    glob_to_regex(&normalize_path_pattern(pattern), case_insensitive)
}

/// Test-only count of `compile_path_glob` constructions: the
/// precompile-once contract pins this to the pattern count, not the
/// file count.
#[cfg(test)]
static COMPILE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test-only reset for the compile counter.
#[cfg(test)]
/// Test-only reset for the compile counter.
pub(crate) fn reset_compile_count() {
    COMPILE_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Test-only read for the compile counter.
#[cfg(test)]
#[must_use]
/// Test-only read for the compile counter.
pub(crate) fn compile_count() -> usize {
    COMPILE_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pattern spellings for root tolerance: as written plus `src/`-stripped
/// when the remainder stays scoped. Shared by per-call and precompiled
/// matchers so the two never disagree.
fn pattern_variants(pattern: &str) -> Vec<String> {
    let normalized = normalize_path_pattern(pattern);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut variants = vec![normalized.clone()];
    if let Some(stripped) = normalized.strip_prefix("src/") {
        if !stripped.is_empty() && stripped.contains('/') {
            variants.push(stripped.to_string());
        }
    }
    variants
}

/// Candidate spellings for root tolerance: as stored plus the path with
/// one leading root segment stripped when the remainder stays scoped.
/// Lets index-relative globs (`bin/guidance/**`, after root stripping)
/// select workspace-relative rows (`src/bin/guidance/...`).
pub(crate) fn candidate_spellings(path: &str) -> Vec<String> {
    let candidate = normalize_path_for_match(path);
    let mut out = vec![candidate.clone()];
    if let Some((_, rest)) = candidate.split_once('/') {
        if !rest.is_empty() && rest.contains('/') {
            out.push(rest.to_string());
        }
    }
    out
}

/// One precompiled pattern spelling.
struct CompiledVariant {
    text: String,
    regex: Option<regex::Regex>,
    dir_regex: Option<regex::Regex>,
}

fn build_variant(text: &str, case_insensitive: bool, literal_prefix: bool) -> CompiledVariant {
    // Ripgrep mode compiles every spelling (literals become exact or
    // basename regexes, mirroring the per-call path); path mode keeps
    // literals on the prefix-compare branch and compiles glob
    // spellings only. Builds on `glob_to_regex` directly: variant
    // inputs are already normalized, and the shared test counter keeps
    // tracking the selection path only.
    let needs_regex = !literal_prefix || has_path_glob(text);
    CompiledVariant {
        text: text.to_string(),
        regex: needs_regex.then(|| glob_to_regex(text, case_insensitive)),
        dir_regex: text
            .strip_suffix("/**")
            .map(|dir| glob_to_regex(dir, case_insensitive)),
    }
}

/// One precompiled pattern: all spellings, one case flag, one mode.
/// Matches exactly what the per-call path matches; the regexes just
/// build once instead of once per file.
pub(crate) struct CompiledPattern {
    variants: Vec<CompiledVariant>,
    case_insensitive: bool,
    literal_prefix: bool,
}

impl CompiledPattern {
    /// Precompile a `path_pattern_matches` spelling (literal prefixes).
    pub(crate) fn path(pattern: &str) -> Self {
        Self::build(pattern, false, true)
    }

    /// Precompile a `ripgrep_glob_matches` spelling (regex only).
    pub(crate) fn ripgrep(pattern: &str, case_insensitive: bool) -> Self {
        Self::build(pattern, case_insensitive, false)
    }

    fn build(pattern: &str, case_insensitive: bool, literal_prefix: bool) -> Self {
        Self {
            variants: pattern_variants(pattern)
                .iter()
                .map(|variant| build_variant(variant, case_insensitive, literal_prefix))
                .collect(),
            case_insensitive,
            literal_prefix,
        }
    }

    /// True when any spelling matches any candidate spelling.
    pub(crate) fn matches(&self, path: &str) -> bool {
        for variant in &self.variants {
            for candidate in candidate_spellings(path) {
                if self.variant_matches(variant, &candidate) {
                    return true;
                }
            }
        }
        false
    }

    /// Regexes built for this pattern: bounded by spelling count,
    /// never by file count.
    #[cfg(test)]
    pub(crate) fn regex_count(&self) -> usize {
        self.variants
            .iter()
            .map(|variant| {
                usize::from(variant.regex.is_some()) + usize::from(variant.dir_regex.is_some())
            })
            .sum()
    }

    fn variant_matches(&self, variant: &CompiledVariant, candidate: &str) -> bool {
        if let Some(regex) = &variant.regex {
            if regex.is_match(candidate) {
                return true;
            }
        } else if self.literal_prefix {
            let hit = if self.case_insensitive {
                let candidate = candidate.to_lowercase();
                let expected = variant.text.to_lowercase();
                candidate == expected || candidate.starts_with(&format!("{expected}/"))
            } else {
                candidate == variant.text || candidate.starts_with(&format!("{}/", variant.text))
            };
            if hit {
                return true;
            }
            if !variant.text.contains('/') {
                let base = candidate.rsplit('/').next().unwrap_or(candidate);
                let hit = if self.case_insensitive {
                    base.to_lowercase() == variant.text.to_lowercase()
                } else {
                    base == variant.text
                };
                if hit {
                    return true;
                }
            }
        }
        if let Some(dir) = &variant.dir_regex {
            if dir.is_match(candidate) {
                return true;
            }
        }
        false
    }
}

/// One ordered glob rule with its negation flag.
struct CompiledRule {
    pattern: CompiledPattern,
    negated: bool,
}

/// Ordered glob set compiled once: sensitive rules then insensitive
/// rules, later matches overriding earlier ones. Same walk as
/// `matches_ordered_globs`, shared by every walked file.
pub(crate) struct CompiledOrderedGlobs {
    rules: Vec<CompiledRule>,
    has_positive: bool,
}

impl CompiledOrderedGlobs {
    pub(crate) fn new(globs: &[String], insensitive_globs: &[String]) -> Self {
        let mut rules = Vec::new();
        let mut has_positive = false;
        for (patterns, case_insensitive) in [(globs, false), (insensitive_globs, true)] {
            for raw in patterns {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let negated = trimmed.starts_with('!');
                let body = if negated {
                    trimmed[1..].trim()
                } else {
                    trimmed
                };
                if body.is_empty() {
                    continue;
                }
                has_positive = has_positive || !negated;
                rules.push(CompiledRule {
                    pattern: CompiledPattern::ripgrep(body, case_insensitive),
                    negated,
                });
            }
        }
        Self {
            rules,
            has_positive,
        }
    }

    /// True under the ordered walk: later rules override earlier ones.
    pub(crate) fn matches(&self, path: &str) -> bool {
        let mut included = !self.has_positive;
        for rule in &self.rules {
            if rule.pattern.matches(path) {
                included = !rule.negated;
            }
        }
        included
    }

    /// Regexes built for the whole ordered set.
    #[cfg(test)]
    pub(crate) fn regex_count(&self) -> usize {
        self.rules
            .iter()
            .map(|rule| rule.pattern.regex_count())
            .sum()
    }
}

/// Whole file-selection predicate compiled once per recall: include /
/// exclude path patterns plus the ordered glob walk plus file-type
/// patterns. Mirrors the `resolve_filtered_file_ids` closure exactly.
pub(crate) struct CompiledFileFilter {
    includes: Vec<CompiledPattern>,
    excludes: Vec<CompiledPattern>,
    ordered: CompiledOrderedGlobs,
    type_include: Vec<CompiledPattern>,
    type_exclude: Vec<CompiledPattern>,
    #[cfg(test)]
    regex_count: usize,
}

impl CompiledFileFilter {
    pub(crate) fn new(
        includes: &[String],
        excludes: &[String],
        globs: &[String],
        insensitive_globs: &[String],
        types: &FileTypePatterns,
    ) -> Self {
        let includes: Vec<CompiledPattern> = includes
            .iter()
            .map(|pattern| CompiledPattern::path(pattern))
            .collect();
        let excludes: Vec<CompiledPattern> = excludes
            .iter()
            .map(|pattern| CompiledPattern::path(pattern))
            .collect();
        let ordered = CompiledOrderedGlobs::new(globs, insensitive_globs);
        let type_include: Vec<CompiledPattern> = types
            .include
            .iter()
            .map(|pattern| CompiledPattern::ripgrep(pattern, false))
            .collect();
        let type_exclude: Vec<CompiledPattern> = types
            .exclude
            .iter()
            .map(|pattern| CompiledPattern::ripgrep(pattern, false))
            .collect();
        #[cfg(test)]
        let regex_count = includes
            .iter()
            .map(CompiledPattern::regex_count)
            .sum::<usize>()
            + excludes
                .iter()
                .map(CompiledPattern::regex_count)
                .sum::<usize>()
            + ordered.regex_count()
            + type_include
                .iter()
                .map(CompiledPattern::regex_count)
                .sum::<usize>()
            + type_exclude
                .iter()
                .map(CompiledPattern::regex_count)
                .sum::<usize>();
        Self {
            includes,
            excludes,
            ordered,
            type_include,
            type_exclude,
            #[cfg(test)]
            regex_count,
        }
    }

    /// Regexes built for the whole filter (test-only pin): fixed at
    /// construction, independent of how many files are matched.
    #[cfg(test)]
    pub(crate) fn regex_count(&self) -> usize {
        self.regex_count
    }

    /// True when the file survives every selection predicate.
    pub(crate) fn matches(&self, path: &str) -> bool {
        (self.includes.is_empty() || self.includes.iter().any(|rule| rule.matches(path)))
            && !self.excludes.iter().any(|rule| rule.matches(path))
            && self.ordered.matches(path)
            && (self.type_include.is_empty()
                || self.type_include.iter().any(|rule| rule.matches(path)))
            && !self.type_exclude.iter().any(|rule| rule.matches(path))
    }
}

fn glob_to_regex(pattern: &str, case_insensitive: bool) -> regex::Regex {
    let prefix = if pattern.contains('/') {
        "^"
    } else {
        "^(?:.*/)?"
    };
    let expression = format!("{prefix}{}$", glob_fragment_to_regex(pattern));
    RegexBuilder::new(&expression)
        .case_insensitive(case_insensitive)
        .build()
        .unwrap_or_else(|_| {
            RegexBuilder::new("^$")
                .build()
                .expect("empty regex always builds")
        })
}

fn glob_fragment_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut expression = String::new();
    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        let next = chars.get(index + 1).copied();
        let after_next = chars.get(index + 2).copied();
        if char == '*' && next == Some('*') && after_next == Some('/') {
            expression.push_str("(?:.*/)?");
            index += 3;
        } else if char == '*' && next == Some('*') {
            expression.push_str(".*");
            index += 2;
        } else if char == '*' {
            expression.push_str("[^/]*");
            index += 1;
        } else if char == '?' {
            expression.push_str("[^/]");
            index += 1;
        } else if char == '[' {
            if let Some((class, end)) = read_glob_character_class(&chars, index) {
                expression.push_str(&class);
                index = end + 1;
            } else {
                expression.push_str("\\[");
                index += 1;
            }
        } else if char == '{' {
            if let Some((alternatives, end)) = read_glob_alternation(&chars, index) {
                let or: Vec<String> = alternatives
                    .iter()
                    .map(|a| glob_fragment_to_regex(a))
                    .collect();
                let _ = write!(expression, "(?:{})", or.join("|"));
                index = end + 1;
            } else {
                expression.push_str("\\{");
                index += 1;
            }
        } else {
            expression.push_str(&escape_regex_char(char));
            index += 1;
        }
    }
    expression
}

fn read_glob_character_class(chars: &[char], start: usize) -> Option<(String, usize)> {
    let end = chars.iter().skip(start + 1).position(|&c| c == ']')? + start + 1;
    let content: String = chars[start + 1..end].iter().collect();
    if content.is_empty() || content == "!" || content == "^" {
        return None;
    }
    let negated = content.starts_with('!') || content.starts_with('^');
    let mut body: String = if negated {
        content.chars().skip(1).collect()
    } else {
        content
    };
    body = body.replace('\\', "\\\\").replace('/', "\\/");
    Some((format!("[{}{body}]", if negated { "^" } else { "" }), end))
}

fn read_glob_alternation(chars: &[char], start: usize) -> Option<(Vec<String>, usize)> {
    let mut alternatives = Vec::new();
    let mut depth = 0;
    let mut alternative_start = start + 1;
    let mut index = start + 1;
    while index < chars.len() {
        match chars[index] {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            ',' if depth == 0 => {
                alternatives.push(chars[alternative_start..index].iter().collect());
                alternative_start = index + 1;
            }
            '}' => {
                if alternatives.is_empty() {
                    return None;
                }
                alternatives.push(chars[alternative_start..index].iter().collect());
                return Some((alternatives, index));
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn escape_regex_char(char: char) -> String {
    match char {
        '|' | '\\' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '+' | '*' | '?' | '.' => {
            format!("\\{char}")
        }
        _ => char.to_string(),
    }
}

/// Exact-or-prefix path match, or glob match when the pattern carries
/// metacharacters. Source: `pathPatternMatches`.
#[must_use]
/// Exact-or-prefix path match, or glob match when the pattern carries
/// metacharacters. Source: `pathPatternMatches`.
pub fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    path_pattern_matches_with_case(pattern, path, false)
}

/// Case-insensitive `path_pattern_matches`.
#[must_use]
/// Case-insensitive `path_pattern_matches`.
pub fn path_pattern_matches_case_insensitive(pattern: &str, path: &str) -> bool {
    path_pattern_matches_with_case(pattern, path, true)
}

fn path_pattern_matches_with_case(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    for variant in pattern_variants(pattern) {
        for candidate in candidate_spellings(path) {
            if variant_matches_path(&variant, &candidate, case_insensitive) {
                return true;
            }
        }
    }
    false
}

fn variant_matches_path(pattern: &str, candidate: &str, case_insensitive: bool) -> bool {
    if has_path_glob(pattern) {
        return glob_pattern_matches(pattern, candidate, case_insensitive);
    }
    let hit = if case_insensitive {
        let candidate = candidate.to_lowercase();
        let expected = pattern.to_lowercase();
        let prefix = format!("{expected}/");
        candidate == expected || candidate.starts_with(&prefix)
    } else {
        let prefix = format!("{pattern}/");
        candidate == pattern || candidate.starts_with(&prefix)
    };
    if hit {
        return true;
    }
    if pattern.contains('/') {
        false
    } else {
        let base = candidate.rsplit('/').next().unwrap_or(candidate);
        if case_insensitive {
            base.to_lowercase() == pattern.to_lowercase()
        } else {
            base == pattern
        }
    }
}

/// Ordered glob walk: later rules override earlier ones; a leading `!`
/// negates. With no positive rule, everything starts included.
/// Source: `matchesOrderedGlobs`.
#[must_use]
/// Ordered glob walk: later rules override earlier ones; a leading `!`
/// negates. With no positive rule, everything starts included.
/// Source: `matchesOrderedGlobs`.
pub fn matches_ordered_globs(path: &str, globs: &[String], insensitive_globs: &[String]) -> bool {
    let clean = |patterns: &[String]| -> Vec<String> {
        patterns
            .iter()
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty())
            .collect()
    };
    let sensitive = clean(globs);
    let insensitive = clean(insensitive_globs);
    let has_positive = sensitive
        .iter()
        .chain(&insensitive)
        .any(|pattern| !pattern.starts_with('!'));
    let mut included = !has_positive;
    for pattern in &sensitive {
        if rule_matches(pattern, path, false) {
            included = !pattern.starts_with('!');
        }
    }
    for pattern in &insensitive {
        if rule_matches(pattern, path, true) {
            included = !pattern.starts_with('!');
        }
    }
    included
}

fn rule_matches(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    let body = pattern.strip_prefix('!').map_or(pattern, str::trim);
    if body.is_empty() {
        return false;
    }
    if case_insensitive {
        ripgrep_glob_matches_case_insensitive(body, path)
    } else {
        ripgrep_glob_matches(body, path)
    }
}

/// Resolve file-type names to match globs via the curated static table
/// (hermetic; zvec shells out to `rg --type-list`). `"all"` matches
/// everything; unknown names error like the zvec throw.
pub fn resolve_file_type_patterns(
    included: &[String],
    excluded: &[String],
) -> Result<FileTypePatterns, GlobError> {
    Ok(FileTypePatterns {
        include: resolve_type_names(included)?,
        exclude: resolve_type_names(excluded)?,
    })
}

fn resolve_type_names(names: &[String]) -> Result<Vec<String>, GlobError> {
    let mut patterns = Vec::new();
    for raw in names {
        let name = raw.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        if name == "all" {
            patterns.push("**".to_string());
            continue;
        }
        let canonical = canonical_type_name(&name);
        let Some(globs) = type_globs(canonical) else {
            return Err(GlobError::UnknownFileType(raw.clone()));
        };
        patterns.extend(globs.iter().map(ToString::to_string));
    }
    patterns.sort();
    patterns.dedup();
    Ok(patterns)
}

fn canonical_type_name(name: &str) -> &str {
    if type_globs(name).is_some() {
        return name;
    }
    // zvec `RIPGREP_FILE_TYPE_ALIASES` shape: extension → canonical type.
    match name {
        "bash" | "zsh" => "sh",
        "cjs" | "mjs" | "jsx" => "js",
        "cp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => "cpp",
        "h" => "c",
        "markdown" | "mdx" => "md",
        "pyi" => "py",
        "rb" => "ruby",
        "rs" => "rust",
        "tsx" => "ts",
        "yml" => "yaml",
        _ => name,
    }
}

/// Curated static abbreviation of `rg --type-list` for the roadmap
/// languages plus common neighbors.
fn type_globs(canonical: &str) -> Option<&'static [&'static str]> {
    match canonical {
        "ts" => Some(&["*.ts", "*.tsx"]),
        "js" => Some(&["*.js", "*.jsx", "*.mjs", "*.cjs"]),
        "py" => Some(&["*.py", "*.pyi"]),
        "rust" => Some(&["*.rs"]),
        "c" => Some(&["*.c", "*.h"]),
        "cpp" => Some(&["*.cpp", "*.cc", "*.cxx", "*.hpp", "*.hxx", "*.hh"]),
        "go" => Some(&["*.go"]),
        "java" => Some(&["*.java"]),
        "zig" => Some(&["*.zig", "*.zon"]),
        "md" => Some(&["*.md", "*.markdown", "*.mdx"]),
        "sh" => Some(&["*.sh", "*.bash", "*.zsh"]),
        "ruby" => Some(&["*.rb"]),
        "yaml" => Some(&["*.yaml", "*.yml"]),
        "json" => Some(&["*.json"]),
        _ => None,
    }
}

/// Combined glob + type selection. Source: `matchesFileSelection`.
#[must_use]
/// Combined glob + type selection. Source: `matchesFileSelection`.
pub fn matches_file_selection(
    path: &str,
    globs: &[String],
    insensitive_globs: &[String],
    types: &FileTypePatterns,
) -> bool {
    let included_by_glob = matches_ordered_globs(path, globs, insensitive_globs);
    let included_by_type = types.include.is_empty()
        || types
            .include
            .iter()
            .any(|glob| ripgrep_glob_matches(glob, path));
    let excluded_by_type = types
        .exclude
        .iter()
        .any(|glob| ripgrep_glob_matches(glob, path));
    included_by_glob && included_by_type && !excluded_by_type
}

#[cfg(test)]
#[path = "../../tests/query_glob.rs"]
mod tests;
