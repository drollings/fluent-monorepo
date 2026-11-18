//! Directory-driven Pine closure discovery: per-indicator import closures
//! resolved against the on-disk libraries, with link-or-refuse verdicts.
//!
//! Nothing here names a file: indicators are enumerated from the
//! caller-supplied rows, and each import resolves against whatever library
//! sources are supplied. Loading warns on version skew and never refuses;
//! unknown imports and unparseable text refuse with a reason.

use std::path::PathBuf;

use super::source::{parse_pine_source, LibraryId, PineSource};
use crate::error::KernelError;
use crate::pine::manifest::ManifestFile;

/// Data directory holding the Pine sources and histories, derived from the
/// calling crate's manifest directory (unit tests pass `env!("CARGO_MANIFEST_DIR")`).
/// A pure path join: it assumes no file, only the root.
#[must_use]
pub fn closure_data_root(manifest_dir: &str) -> PathBuf {
    PathBuf::from(manifest_dir).join("../../data/protopyne-rs")
}

/// One indicator closure discovered from the directory: the root plus its
/// transitively resolved libraries, keyed `publisher/name` with the
/// indicator last. Versions ride each file's own header (metadata only,
/// never enforced).
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredClosure {
    /// File stem of the indicator (`_FastMAs`).
    pub stem: String,
    /// Root-relative indicator path (`indicators/_FastMAs.pine`).
    pub relpath: String,
    /// `(key, source)` pairs, indicator root last.
    pub sources: Vec<(String, PineSource)>,
}

/// Discovery outcome per indicator file: linked with its closure, or
/// refused with the reason it cannot run. A refusal names its cause
/// (unparseable text, unsupported version, unknown import) — never a
/// silent absence.
#[derive(Debug, Clone, PartialEq)]
pub enum ClosureOutcome {
    /// The indicator parsed and every import resolved.
    Linked(DiscoveredClosure),
    /// The indicator cannot run; `reason` is the refusal a slot would show.
    Refused {
        /// File stem of the indicator.
        stem: String,
        /// Root-relative indicator path.
        relpath: String,
        /// Why the indicator cannot run.
        reason: String,
    },
}

impl ClosureOutcome {
    /// Stem of the indicator this outcome describes.
    #[must_use]
    pub fn stem(&self) -> &str {
        match self {
            ClosureOutcome::Linked(closure) => &closure.stem,
            ClosureOutcome::Refused { stem, .. } => stem,
        }
    }
}

/// Library lookup key for one file: the bare stem, plus the stem with one
/// leading underscore stripped (the on-disk `_libUtil.pine` convention).
fn library_keys(relpath: &str) -> Vec<String> {
    let stem = relpath
        .rsplit('/')
        .next()
        .unwrap_or(relpath)
        .strip_suffix(".pine")
        .unwrap_or(relpath)
        .to_owned();
    if let Some(stripped) = stem.strip_prefix('_').map(str::to_owned) {
        vec![stem, stripped]
    } else {
        vec![stem]
    }
}

/// Discover every indicator closure over caller-supplied files: for each
/// `indicators/*.pine`, parse the header, resolve its import closure
/// transitively against `libraries/*.pine` (one on-disk source per library
/// name), and report the linked sources or the refusal. Pure over values:
/// no I/O, deterministic in directory order.
#[must_use]
pub fn discover_closures(
    indicators: &[ManifestFile],
    libraries: &[ManifestFile],
) -> Vec<ClosureOutcome> {
    let by_name: std::collections::HashMap<String, &ManifestFile> = libraries
        .iter()
        .flat_map(|file| {
            library_keys(&file.relpath)
                .into_iter()
                .map(move |key| (key, file))
        })
        .collect();
    let mut outcomes = Vec::with_capacity(indicators.len());
    for file in indicators {
        let stem = file
            .relpath
            .rsplit('/')
            .next()
            .unwrap_or(&file.relpath)
            .strip_suffix(".pine")
            .unwrap_or(&file.relpath)
            .to_owned();
        if file.text.trim().is_empty() {
            outcomes.push(ClosureOutcome::Refused {
                stem,
                relpath: file.relpath.clone(),
                reason: "empty source (unreadable or empty file)".to_owned(),
            });
            continue;
        }
        let root = parse_pine_source(&file.relpath, &file.text);
        let mut sources: Vec<(String, PineSource)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue: Vec<super::source::ImportDecl> = root.imports.clone();
        let mut refusal: Option<String> = None;
        while let Some(import) = queue.pop() {
            if !seen.insert(import.library.name.clone()) {
                continue;
            }
            if let Some(library) = by_name.get(&import.library.name) {
                let source = parse_pine_source(&library.relpath, &library.text);
                queue.extend(source.imports.clone());
                sources.push((
                    format!("{}/{}", import.library.publisher, import.library.name),
                    source,
                ));
            } else {
                refusal = Some(format!(
                    "unknown import {:?} (no libraries/_{}.pine on disk)",
                    import.library.to_string(),
                    import.library.name,
                ));
                break;
            }
        }
        if let Some(reason) = refusal {
            outcomes.push(ClosureOutcome::Refused {
                stem,
                relpath: file.relpath.clone(),
                reason,
            });
            continue;
        }
        // A linked closure parses end to end: unparseable text refuses
        // here with the parser's reason, never as a link-time surprise.
        let mut unparseable: Option<String> = None;
        if let Err(err) = super::syntax::parse_program(&root.text) {
            unparseable = Some(format!("{}: {err}", file.relpath));
        }
        if unparseable.is_none() {
            for (key, source) in &sources {
                if let Err(err) = super::syntax::parse_program(&source.text) {
                    unparseable = Some(format!("{key} parses: {err}"));
                    break;
                }
            }
        }
        if let Some(reason) = unparseable {
            outcomes.push(ClosureOutcome::Refused {
                stem,
                relpath: file.relpath.clone(),
                reason,
            });
            continue;
        }
        sources.push((format!("local/{stem}"), root));
        outcomes.push(ClosureOutcome::Linked(DiscoveredClosure {
            stem,
            relpath: file.relpath.clone(),
            sources,
        }));
    }
    outcomes
}

/// Parse every source of a discovered closure into graph inputs, keyed by
/// the same `publisher/name` keys the loader used. Versions ride each
/// file's own declared header (metadata only, never enforced on link).
/// A source that fails to parse refuses with its position.
pub fn closure_inputs(
    closure: &DiscoveredClosure,
) -> Result<Vec<crate::pine::modules::ModuleInput>, KernelError> {
    closure
        .sources
        .iter()
        .map(|(key, source)| {
            let program = super::syntax::parse_program(&source.text).map_err(|err| {
                KernelError::InvalidInput(format!("closure file {key} parses: {err}"))
            })?;
            let (publisher, name) = key.split_once('/').unwrap_or(("local", key.as_str()));
            Ok(crate::pine::modules::ModuleInput::new(
                LibraryId {
                    publisher: publisher.to_owned(),
                    name: name.to_owned(),
                    version: source
                        .version
                        .unwrap_or(super::source::SUPPORTED_PINE_VERSION),
                },
                source.clone(),
                program,
            ))
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/pine_closure.rs"]
mod tests;

/// Task-indicator triage pins (file homes, absent-file disposal, substitute
/// quarantine, skew table, zero-reference scan, control, loader property).
#[cfg(test)]
#[path = "../../tests/pine_triage.rs"]
mod triage_tests;

/// Task-indicator closures (parse + link + render + shape pins, no execution).
#[cfg(test)]
#[path = "../../tests/pine_task_closure.rs"]
mod task_closure_tests;
