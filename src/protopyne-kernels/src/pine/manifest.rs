//! Boot-time Pine indicator manifest: discovery at the data edge, gated on
//! resolution, served as rows the widget renders.
//!
//! The builder walks `<root>/indicators/*.pine` and resolves each script's
//! import closure against `<root>/libraries`. A script is [`ManifestParity::Verified`]
//! when it loads, declares the supported version, and resolves every import;
//! anything else is listed with its refusal reason — advertised, never runnable.
//! `Verified` is structural readiness (version plus resolution), not a fitness
//! claim: numeric parity lives in the hermetic engine suites, which run over
//! checked-in fixtures instead of this volatile directory. The widget
//! consumes these rows; it never reads the directory itself.
//!
//! Pure over caller-supplied files: no I/O here, so the language surface
//! owns no path to disk (the no-cache guard pins this). The thin boot driver
//! that walks the root lives in [`crate::manifest_files`].

use std::collections::HashMap;
use std::path::Path;

use crate::pine::source::{parse_pine_source, SUPPORTED_PINE_VERSION};

/// Parity standing of one manifest row: structural agreement proven, or
/// listed with the reason it must not run. Unverified is availability
/// information (the engine cannot vouch for this script), never a fitness
/// claim.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ManifestParity {
    /// Structure resolves and a harness covers this script.
    Verified,
    /// Listed but refused on instantiate, with the reason.
    Unverified {
        /// Why this script must not run.
        reason: String,
    },
}

/// One discovered indicator: its catalog identity, display title, version,
/// import-closure hash, and parity standing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndicatorManifestEntry {
    /// Catalog id (`pine:<stem>`), namespaced so scripts never collide with
    /// kernel ids.
    pub id: String,
    /// Display title from the `indicator("...")` header, or the stem.
    pub title: String,
    /// Source path relative to the Pine root, for audit.
    pub source_path: String,
    /// Declared `//@version`, or `None` when the header is missing.
    pub version: Option<u64>,
    /// Closure hash over the indicator plus its resolved libraries.
    pub closure_hash: u64,
    /// Parity standing.
    pub parity: ManifestParity,
}

/// One Pine file as data: its root-relative path plus byte-identical text.
/// An empty text reads as an empty source (listed, refused).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    /// Path relative to the Pine root (`indicators/_Foo.pine`).
    pub relpath: String,
    /// Full file text, byte-identical to disk.
    pub text: String,
}

/// The catalog id for an indicator stem.
#[must_use]
pub fn manifest_id(stem: &str) -> String {
    format!("pine:{stem}")
}

/// The library name a `libraries/` file provides: `_libUtil.pine` serves
/// `libUtil` (one leading underscore is the on-disk convention).
fn library_name(relpath: &str) -> Option<String> {
    let file = relpath.rsplit('/').next()?;
    let stem = file.strip_suffix(".pine")?;
    Some(stem.strip_prefix('_').unwrap_or(stem).to_owned())
}

/// The indicator stem a file provides: the bare file stem, underscore kept
/// (`_FastMAs.pine` serves `_FastMAs` — stems are harness identities).
fn indicator_stem(relpath: &str) -> String {
    Path::new(relpath)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Display title from the `indicator("...")` (or legacy `study("...")`)
/// header's first string literal; the stem when no header parses. Pure
/// over values.
fn manifest_title(stem: &str, text: &str) -> String {
    let start = text
        .find("indicator(")
        .into_iter()
        .chain(text.find("study("))
        .min();
    let Some(start) = start else {
        return stem.to_owned();
    };
    let mut chars = text[start..].chars();
    loop {
        match chars.next() {
            None => return stem.to_owned(),
            Some(quote @ ('"' | '\'')) => {
                let rest: String = chars.clone().collect();
                let mut title = String::new();
                let mut escaped = false;
                for char in rest.chars() {
                    if escaped {
                        title.push(char);
                        escaped = false;
                    } else if char == '\\' {
                        escaped = true;
                    } else if char == quote {
                        return if title.trim().is_empty() {
                            stem.to_owned()
                        } else {
                            title
                        };
                    } else {
                        title.push(char);
                    }
                }
                return stem.to_owned();
            }
            Some(_) => {}
        }
    }
}

/// Refusal constructor: one shape for every unverified listing.
fn unverified(
    stem: &str,
    source_path: &str,
    version: Option<u64>,
    reason: impl Into<String>,
) -> IndicatorManifestEntry {
    IndicatorManifestEntry {
        id: manifest_id(stem),
        title: stem.to_owned(),
        source_path: source_path.to_owned(),
        version,
        closure_hash: 0,
        parity: ManifestParity::Unverified {
            reason: reason.into(),
        },
    }
}

/// Build manifest rows over caller-supplied files, sorted by id. Pure: no
/// I/O, a rerun over the same files is identical.
#[must_use]
pub fn build_manifest(
    indicators: &[ManifestFile],
    libraries: &[ManifestFile],
) -> Vec<IndicatorManifestEntry> {
    let provided: HashMap<String, &ManifestFile> = libraries
        .iter()
        .filter_map(|file| library_name(&file.relpath).map(|name| (name, file)))
        .collect();
    let mut manifest: Vec<IndicatorManifestEntry> = indicators
        .iter()
        .map(|file| entry_for(file, &provided))
        .collect();
    manifest.sort_by(|left, right| left.id.cmp(&right.id));
    manifest
}

/// One manifest row: title, version gate, import resolution, closure hash,
/// and harness coverage — each refusal naming its cause.
fn entry_for(
    file: &ManifestFile,
    libraries: &HashMap<String, &ManifestFile>,
) -> IndicatorManifestEntry {
    let stem = indicator_stem(&file.relpath);
    if file.text.trim().is_empty() {
        return unverified(
            &stem,
            &file.relpath,
            None,
            "empty source (unreadable or empty file)",
        );
    }
    let source = parse_pine_source(&file.relpath, &file.text);
    let title = manifest_title(&stem, &file.text);
    if source.version != Some(SUPPORTED_PINE_VERSION) {
        return IndicatorManifestEntry {
            title,
            ..unverified(
                &stem,
                &file.relpath,
                source.version,
                match source.version {
                    Some(declared) => format!(
                        "unsupported pine version {declared}: this platform executes version {SUPPORTED_PINE_VERSION}"
                    ),
                    None => format!(
                        "missing //@version header: this platform executes version {SUPPORTED_PINE_VERSION}"
                    ),
                },
            )
        };
    }
    let mut closure: Vec<(&str, &[u8])> = vec![(file.relpath.as_str(), file.text.as_bytes())];
    for import in &source.imports {
        match libraries.get(&import.library.name) {
            Some(library) => closure.push((library.relpath.as_str(), library.text.as_bytes())),
            None => {
                return IndicatorManifestEntry {
                    title,
                    ..unverified(
                        &stem,
                        &file.relpath,
                        source.version,
                        format!(
                            "unknown import {:?} (no libraries/_{}.pine on disk)",
                            import.library.to_string(),
                            import.library.name,
                        ),
                    )
                };
            }
        }
    }
    closure.sort_by(|left, right| left.0.cmp(right.0));
    let closure_hash = common_core::hash::fingerprint_framed(&closure);
    // Verified is structural readiness only: the version is supported,
    // every import resolves, and the whole closure parses. Which scripts
    // the hermetic parity suites prove is tracked by those suites over
    // checked-in fixtures, never by name here — this directory is volatile
    // input, so no stem list may live in this builder.
    let mut closure_texts: Vec<(&str, &str)> = vec![(file.relpath.as_str(), file.text.as_str())];
    for import in &source.imports {
        // Resolution succeeded above, so the library is present.
        if let Some(library) = libraries.get(&import.library.name) {
            closure_texts.push((library.relpath.as_str(), library.text.as_str()));
        }
    }
    for (relpath, text) in closure_texts {
        if let Err(err) = crate::pine::syntax::parse_program(text) {
            return IndicatorManifestEntry {
                title,
                ..unverified(
                    &stem,
                    &file.relpath,
                    source.version,
                    format!("unparseable pine source {relpath}: {err}"),
                )
            };
        }
    }
    IndicatorManifestEntry {
        id: manifest_id(&stem),
        title,
        source_path: file.relpath.clone(),
        version: source.version,
        closure_hash,
        parity: ManifestParity::Verified,
    }
}
