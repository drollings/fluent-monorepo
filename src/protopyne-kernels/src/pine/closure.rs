//! Real-file closure fixture: the indicator and its seven libraries.
//!
//! The indicator under `data/protopyne-rs/indicators` pulls seven libraries;
//! the five execution dependencies plus the drawing and candle helpers load
//! and link as Pine source without executing anything here. Each entry
//! records the publisher/name identity, the pinned version the file declares
//! (metadata only — loading warns on skew, never refuses), the alias the
//! importer binds, the repo-relative path, and the exact import lines the
//! file carries so fixtures fail loudly when the real files drift.

use std::path::{Path, PathBuf};

use super::modules::ModuleId;
use super::source::{LibraryId, PineSource, load_pine_source};
use crate::error::KernelError;

/// One real Pine file in the closure: identity plus its own import lines as
/// `(library name, alias)` pairs in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosureEntry {
    /// Publisher namespace (`showmethegrail`).
    pub publisher: &'static str,
    /// Library name (`libUtil`).
    pub name: &'static str,
    /// Pinned version from the import line (never enforced on load).
    pub version: u64,
    /// Alias the indicator binds for this library.
    pub alias: &'static str,
    /// Path relative to the data root (the `data/protopyne-rs` directory).
    pub relpath: &'static str,
    /// `(library name, alias)` per `import` line in file order.
    pub imports: &'static [(&'static str, &'static str)],
}

impl ClosureEntry {
    /// Graph identity: publisher and name only (versions never participate).
    #[must_use]
    pub fn module_id(&self) -> ModuleId {
        ModuleId::new(self.publisher, self.name)
    }

    /// Claimed identity with the pinned version attached as metadata.
    #[must_use]
    pub fn library_id(&self) -> LibraryId {
        LibraryId {
            publisher: self.publisher.to_owned(),
            name: self.name.to_owned(),
            version: self.version,
        }
    }
}

/// The seven libraries the indicator imports: five execution dependencies
/// plus the drawing and candle helpers (loaded and linked, captured
/// headlessly rather than rendered).
pub const REAL_LIBRARIES: &[ClosureEntry] = &[
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libUtil",
        version: 12,
        alias: "UTIL",
        relpath: "libraries/_libUtil.pine",
        imports: &[],
    },
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libBars",
        version: 12,
        alias: "BARS",
        relpath: "libraries/_libBars.pine",
        imports: &[("libUtil", "UTIL")],
    },
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libBuffers",
        version: 6,
        alias: "libBuffers",
        relpath: "libraries/_libBuffers.pine",
        imports: &[],
    },
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libDrawing",
        version: 61,
        alias: "libDrawing",
        relpath: "libraries/_libDrawing.pine",
        imports: &[("libUtil", "UTIL")],
    },
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libCandles",
        version: 21,
        alias: "libCandles",
        relpath: "libraries/_libCandles.pine",
        imports: &[
            ("libUtil", "UTIL"),
            ("libDrawing", "libDrawing"),
            ("libApex", "libApex"),
        ],
    },
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libMAs",
        version: 95,
        alias: "libMAs",
        relpath: "libraries/_libMAs.pine",
        imports: &[("libUtil", "UTIL")],
    },
    ClosureEntry {
        publisher: "showmethegrail",
        name: "libApex",
        version: 88,
        alias: "libApex",
        relpath: "libraries/_libApex.pine",
        imports: &[
            ("libUtil", "UTIL"),
            ("libBuffers", "libBuffers"),
            ("libBars", "BARS"),
            ("libDrawing", "libDrawing"),
        ],
    },
];

/// Data-root-relative path of the indicator that imports the seven libraries.
pub const INDICATOR_PATH: &str = "indicators/_IntegrationLite.pine";

/// Publisher/name the indicator root travels under in the import graph (the
/// indicator declares no library identity of its own, so it uses its stem).
#[must_use]
pub fn indicator_library_id() -> LibraryId {
    LibraryId {
        publisher: "local".to_owned(),
        name: "IntegrationLite".to_owned(),
        version: super::source::SUPPORTED_PINE_VERSION,
    }
}

/// Data directory holding the Pine sources and histories, derived from the
/// calling crate's manifest directory (unit tests pass `env!("CARGO_MANIFEST_DIR")`).
#[must_use]
pub fn closure_data_root(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir).join("../../data/protopyne-rs")
}

/// Load the seven library sources plus the indicator without executing them.
/// Version skew warns on the loaded source and still resolves; only
/// unreadable or empty files refuse.
pub fn load_closure_sources(
    data_root: &Path,
) -> Result<Vec<(String, PineSource)>, KernelError> {
    let mut sources = Vec::with_capacity(REAL_LIBRARIES.len() + 1);
    for entry in REAL_LIBRARIES {
        let source = load_pine_source(&data_root.join(entry.relpath))?;
        sources.push((format!("{}/{}", entry.publisher, entry.name), source));
    }
    let indicator = load_pine_source(&data_root.join(INDICATOR_PATH))?;
    sources.push(("local/IntegrationLite".to_owned(), indicator));
    Ok(sources)
}

#[cfg(test)]
#[path = "../../tests/pine_closure.rs"]
mod tests;
