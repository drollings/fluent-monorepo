//! Pine source identities: file loading with warn-only version flags.
//!
//! [`load_pine_source`] reads one `.pine` file as data and records its
//! identity: the `//@version=N` declaration, the `library('name')` or
//! `indicator(` header, and every `import publisher/name/version as ALIAS`
//! line. The loaded text is kept byte-identical; nothing is executed here.
//!
//! Versioning is deliberately warn-only: a declared version other than
//! [`SUPPORTED_PINE_VERSION`], or a missing version header, collects a
//! [`VersionWarn`] and still loads. Only an unreadable or empty source
//! refuses, through [`KernelError`] with a reason. Grammar ownership belongs
//! to the later lexer/parser stages; this scanner matches line starts only
//! and leaves every unrecognized line for them, never refusing it.

use std::fmt;
use std::path::Path;

use fluent_wvr::prelude::*;

use crate::error::KernelError;

/// Pine surface this platform executes. Any other declared version — or a
/// missing header — is a collected warning, never a refusal.
pub const SUPPORTED_PINE_VERSION: u64 = 6;

/// Identity of one imported Pine library: `publisher/name/version`, as in
/// `import showmethegrail/libUtil/12 as UTIL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryId {
    /// Publisher namespace (`showmethegrail`).
    pub publisher: String,
    /// Library name (`libUtil`).
    pub name: String,
    /// Pinned library version (`12`); retained as metadata, never enforced.
    pub version: u64,
}

impl fmt::Display for LibraryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.publisher, self.name, self.version)
    }
}

/// One `import` line: which library, under which local alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    /// Imported library identity.
    pub library: LibraryId,
    /// Local namespace alias (`UTIL`).
    pub alias: String,
}

/// Which header a source file declares. Strategy scripts are not a target
/// of the current work and scan as [`SourceKind::Unknown`]; the later
/// parser owns precise header grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// `library('name')` declaration with the declared name.
    Library {
        /// Name from the `library('name')` header.
        name: String,
    },
    /// `indicator(` declaration.
    Indicator,
    /// Neither header found (missing header, or a non-target script kind).
    Unknown,
}

/// A version finding that warns instead of refusing: the declared version
/// (or its absence) travels with the loaded source for later stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionWarn {
    /// Declared `//@version`, or `None` when the header is missing.
    pub declared: Option<u64>,
    /// Human-readable finding, naming the declared version and the path.
    pub message: String,
}

/// One loaded `.pine` file: byte-identical text plus its scanned identity.
/// Version findings ride along as [`warnings`](Self::warnings); only an
/// empty text fails [`validation`](fluent_wvr::validate::Validate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PineSource {
    /// Source path as loaded, for audit and error context.
    pub path: String,
    /// Full file text, byte-identical to disk.
    pub text: String,
    /// Declared `//@version`, or `None` when the header is missing.
    pub version: Option<u64>,
    /// Declared header kind.
    pub kind: SourceKind,
    /// Import declarations in file order.
    pub imports: Vec<ImportDecl>,
    /// Version findings (warn-only; loading always succeeds past I/O).
    pub warnings: Vec<VersionWarn>,
}

/// Scan `text` into a [`PineSource`] attributed to `path`. Pure over values:
/// no I/O, no refusal — unrecognized lines belong to the later parser.
#[must_use]
pub fn parse_pine_source(path: &str, text: &str) -> PineSource {
    let mut version: Option<u64> = None;
    let mut saw_version_line = false;
    let mut version_parseable = true;
    let mut kind = SourceKind::Unknown;
    let mut kind_set = false;
    let mut imports = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if !saw_version_line {
            if let Some(rest) = trimmed.strip_prefix("//@version") {
                saw_version_line = true;
                let declared = rest.trim();
                let declared = declared.strip_prefix('=').unwrap_or(declared).trim();
                match declared.parse() {
                    Ok(v) => version = Some(v),
                    Err(_) => version_parseable = false,
                }
                continue;
            }
        }
        if trimmed.starts_with("library") {
            if let Some(name) = parse_library_header(trimmed) {
                if !kind_set {
                    kind = SourceKind::Library { name };
                    kind_set = true;
                }
            }
            continue;
        }
        if trimmed.starts_with("indicator(") || trimmed.starts_with("indicator (") {
            if !kind_set {
                kind = SourceKind::Indicator;
                kind_set = true;
            }
            continue;
        }
        // Legacy header: pre-`indicator` scripts declare `study(...)` with
        // the same header shape and serve as indicators.
        if trimmed.starts_with("study(") || trimmed.starts_with("study (") {
            if !kind_set {
                kind = SourceKind::Indicator;
                kind_set = true;
            }
            continue;
        }
        if let Some(decl) = parse_import_line(trimmed) {
            imports.push(decl);
        }
    }

    let mut warnings = Vec::new();
    if version != Some(SUPPORTED_PINE_VERSION) {
        warnings.push(VersionWarn {
            declared: version,
            message: match version {
                Some(v) => format!(
                    "{path}: declared Pine version {v} differs from supported \
                     version {SUPPORTED_PINE_VERSION}; loaded anyway"
                ),
                None if saw_version_line && !version_parseable => format!(
                    "{path}: unparseable //@version header; assuming version \
                     {SUPPORTED_PINE_VERSION} surface, loaded anyway"
                ),
                None => format!(
                    "{path}: missing //@version header; assuming version \
                     {SUPPORTED_PINE_VERSION} surface, loaded anyway"
                ),
            },
        });
    }

    PineSource {
        path: path.to_owned(),
        text: text.to_owned(),
        version,
        kind,
        imports,
        warnings,
    }
}

/// Read one `.pine` file into a [`PineSource`]. Unreadable paths and empty
/// files refuse with a reason; version findings warn via
/// [`parse_pine_source`], never here.
pub fn load_pine_source(path: &Path) -> Result<PineSource, KernelError> {
    let display = path.to_string_lossy().into_owned();
    let text = common_core::io::read_to_string_err(path)
        .map_err(|e| KernelError::UnreadableSource(format!("cannot read {display}: {e}")))?;
    if text.trim().is_empty() {
        return Err(KernelError::InvalidInput(format!(
            "pine source {display} is empty"
        )));
    }
    Ok(parse_pine_source(&display, &text))
}

/// Parse a `library('name')` header line (single or double quotes, optional
/// space before the paren). Returns `None` for non-header lines.
fn parse_library_header(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("library")?.trim_start();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    let name = inner.trim().trim_matches(|c| c == '\'' || c == '"').trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Parse an `import publisher/name/version [as ALIAS]` line. A missing alias
/// defaults to the library name. Returns `None` for non-import lines or
/// lines whose path is not `publisher/name/version` with a numeric version;
/// malformed import grammar belongs to the later parser, not this scanner.
fn parse_import_line(trimmed: &str) -> Option<ImportDecl> {
    let rest = trimmed.strip_prefix("import ")?;
    if rest.trim_start().starts_with('(') {
        return None;
    }
    let (path_part, alias_part) = match rest.split_once(" as ") {
        Some((left, right)) => (left.trim(), Some(right.trim())),
        None => (rest.trim(), None),
    };
    let mut segments = path_part.split('/');
    let (publisher, name, version) = match (segments.next(), segments.next(), segments.next()) {
        (Some(publisher), Some(name), Some(version)) if segments.next().is_none() => {
            (publisher, name, version)
        }
        _ => return None,
    };
    if publisher.is_empty() || name.is_empty() {
        return None;
    }
    let version: u64 = version.parse().ok()?;
    let alias = alias_part
        .filter(|alias| !alias.is_empty())
        .unwrap_or(name)
        .to_owned();
    Some(ImportDecl {
        library: LibraryId {
            publisher: publisher.to_owned(),
            name: name.to_owned(),
            version,
        },
        alias,
    })
}

impl fluent_wvr::validate::Validate for PineSource {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.text.trim().is_empty() {
            return Err(KernelError::InvalidInput(format!(
                "pine source {} is empty",
                self.path
            )));
        }
        Ok(())
    }
}

// A loaded source is immutable data: the path reads back for supervision
// inspection, and reconfiguration means loading a different file, so
// `set_field` has no valid target and refuses rather than misconfiguring.
impl FieldAccess for PineSource {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "path" => Ok(self.path.clone()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["path"]
    }
}

impl Describable for PineSource {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Loaded .pine source path (read-only; load another file to reconfigure)"
                }
            },
            "required": ["path"]
        })
    }
}

impl WorkUnit for PineSource {
    fn name(&self) -> &str {
        "pine_source"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        fluent_wvr::validate::check_for_execute(self, "pine source invalid")?;
        Ok(WorkOutput::ok(format!(
            "pine source ready ({} imports, {} warnings): {}",
            self.imports.len(),
            self.warnings.len(),
            self.path
        )))
    }
}

impl_component!(PineSource);

#[cfg(test)]
#[path = "../../tests/pine_source.rs"]
mod tests;
