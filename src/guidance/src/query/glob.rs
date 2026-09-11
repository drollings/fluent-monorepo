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
pub enum GlobError {
    /// Unknown ripgrep file-type name.
    #[error("unknown ripgrep file type: {0}")]
    UnknownFileType(String),
}

/// Include/exclude glob sets in ripgrep type-map form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileTypePatterns {
    /// Inclusion globs.
    pub include: Vec<String>,
    /// Exclusion globs.
    pub exclude: Vec<String>,
}

/// Normalize a path pattern: backslashes and runs collapse to one `/`,
/// leading `./` segments strip (absolute patterns pass through).
/// Source: `normalizePathPattern`.
#[must_use]
pub fn normalize_path_pattern(pattern: &str) -> String {
    let mut normalized = pattern.trim().replace('\\', "/");
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    if is_absolute_path_pattern(&normalized) {
        return normalized;
    }
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    normalized
}

/// Normalize a candidate path for matching.
#[must_use]
pub fn normalize_path_for_match(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    normalized
}

/// Absolute patterns (`/…` or `X:/…`) match from the root.
#[must_use]
pub fn is_absolute_path_pattern(pattern: &str) -> bool {
    if pattern.starts_with('/') {
        return true;
    }
    let bytes = pattern.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// Whether a pattern carries glob metacharacters.
#[must_use]
pub fn has_path_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

/// ripgrep-style glob match (case-sensitive).
#[must_use]
pub fn ripgrep_glob_matches(pattern: &str, path: &str) -> bool {
    ripgrep_glob_matches_with_case(pattern, path, false)
}

/// ripgrep-style glob match (case-insensitive).
#[must_use]
pub fn ripgrep_glob_matches_case_insensitive(pattern: &str, path: &str) -> bool {
    ripgrep_glob_matches_with_case(pattern, path, true)
}

fn ripgrep_glob_matches_with_case(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    let normalized = normalize_path_pattern(pattern);
    if normalized.is_empty() {
        return false;
    }
    if glob_pattern_matches(
        &normalized,
        &normalize_path_for_match(path),
        case_insensitive,
    ) {
        return true;
    }
    // Trailing `/**` also matches the directory itself.
    if let Some(dir) = normalized.strip_suffix("/**") {
        if glob_to_regex(dir, case_insensitive).is_match(&normalize_path_for_match(path)) {
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
    glob_to_regex(&normalize_path_pattern(pattern), case_insensitive)
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
pub fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    path_pattern_matches_with_case(pattern, path, false)
}

/// Case-insensitive `path_pattern_matches`.
#[must_use]
pub fn path_pattern_matches_case_insensitive(pattern: &str, path: &str) -> bool {
    path_pattern_matches_with_case(pattern, path, true)
}

fn path_pattern_matches_with_case(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    let normalized = normalize_path_pattern(pattern);
    let candidate = normalize_path_for_match(path);
    if normalized.is_empty() {
        return false;
    }
    if has_path_glob(&normalized) {
        return glob_pattern_matches(&normalized, &candidate, case_insensitive);
    }
    if case_insensitive {
        let candidate = candidate.to_lowercase();
        let expected = normalized.to_lowercase();
        let prefix = format!("{expected}/");
        return candidate == expected || candidate.starts_with(&prefix);
    }
    let prefix = format!("{normalized}/");
    candidate == normalized || candidate.starts_with(&prefix)
}

/// Ordered glob walk: later rules override earlier ones; a leading `!`
/// negates. With no positive rule, everything starts included.
/// Source: `matchesOrderedGlobs`.
#[must_use]
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
