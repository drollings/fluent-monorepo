//! Canonical file-kind classification: the single home for extension
//! knowledge shared by ingest dispatch, the tree-sitter adapters, the
//! graph gate, and (from M13) tokenizer dispatch.
//!
//! One module owns every map so the two historical divergences stay
//! explicit instead of accidental:
//! - D1 `h`: tree-sitter parses headers as C (`code_format_for_extension`),
//!   ingest tags them C++ (`kind_format_for_extension`). Both paths are
//!   load-bearing — never unify without a migration that re-gates both.
//! - D2 `mts`/`cts`: the adapters know `typescript`; ingest falls back to
//!   text. Same rule.
//! - D3 kind-tag parsing disagrees by direction: scan tags fall back to
//!   `Text` ([`FileKind::parse_scan`]), stored tags keep `Image` and yield
//!   `None` on unknown ([`FileKind::parse_stored`]).
//!
//! Extension inputs are matched case-insensitively (callers lowercase
//! first); the maps below hold lowercase keys.

use serde::{Deserialize, Serialize};

/// Source file kinds (closed set from types.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// Plain text / markdown.
    Text,
    /// Source code.
    Code,
    /// Structured data.
    Data,
    /// Raster image.
    Image,
}

impl FileKind {
    /// Scan-boundary tag (`text` | `code` | `data` | `image`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Code => "code",
            Self::Data => "data",
            Self::Image => "image",
        }
    }

    /// Parse an ingest kind tag (scan direction): unknown tags — including
    /// `"image"` — fall back to `Text`.
    #[must_use]
    pub fn parse_scan(tag: Option<&str>) -> Self {
        match tag {
            Some("code") => Self::Code,
            Some("data") => Self::Data,
            _ => Self::Text,
        }
    }

    /// Parse a stored kind tag (DB direction): unknown tags yield `None`.
    #[must_use]
    pub fn parse_stored(tag: Option<&str>) -> Option<Self> {
        match tag {
            Some("text") => Some(Self::Text),
            Some("code") => Some(Self::Code),
            Some("data") => Some(Self::Data),
            Some("image") => Some(Self::Image),
            _ => None,
        }
    }
}

/// Tree-sitter format for a lowercase extension (adapter authority —
/// D1: `h` parses as C here while ingest tags C++).
#[must_use]
pub fn code_format_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("jsx"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("cpp"),
        _ => None,
    }
}

/// Ingest `(kind, format)` for a lowercase extension (selection
/// authority — D1: `h` tags C++ here; D2: `mts`/`cts` fall to text).
#[must_use]
pub fn kind_format_for_extension(ext: &str) -> Option<(FileKind, &'static str)> {
    if let Some(format) = code_kind_format(ext) {
        return Some((FileKind::Code, format));
    }
    if let Some(format) = data_kind_format(ext) {
        return Some((FileKind::Data, format));
    }
    if let Some(format) = text_kind_format(ext) {
        return Some((FileKind::Text, format));
    }
    if let Some(format) = image_kind_format(ext) {
        return Some((FileKind::Image, format));
    }
    None
}

/// Ingest code formats by extension (selection authority).
#[must_use]
pub fn code_kind_format(ext: &str) -> Option<&'static str> {
    match ext {
        "c" => Some("c"),
        "cc" | "cpp" | "cxx" | "h" | "hpp" => Some("cpp"),
        "go" => Some("go"),
        "java" => Some("java"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("jsx"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "kt" | "kts" => Some("kotlin"),
        "cs" => Some("csharp"),
        "scala" => Some("scala"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "sql" => Some("sql"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "less" => Some("less"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

/// Ingest data formats by extension.
#[must_use]
pub fn data_kind_format(ext: &str) -> Option<&'static str> {
    match ext {
        "csv" => Some("csv"),
        "json" | "jsonc" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

/// Ingest text formats by extension.
#[must_use]
pub fn text_kind_format(ext: &str) -> Option<&'static str> {
    match ext {
        "md" | "mdx" => Some("markdown"),
        "rst" => Some("rst"),
        "txt" => Some("text"),
        "html" | "htm" => Some("html"),
        "xml" => Some("xml"),
        _ => None,
    }
}

/// Ingest image formats by extension.
#[must_use]
pub fn image_kind_format(ext: &str) -> Option<&'static str> {
    match ext {
        "gif" => Some("gif"),
        "jpeg" | "jpg" => Some("jpeg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        _ => None,
    }
}

/// Ingest kind for extensionless well-known file names.
#[must_use]
pub fn named_file_kind(name: &str) -> Option<(FileKind, &'static str)> {
    match name {
        "Dockerfile" => Some((FileKind::Code, "dockerfile")),
        "Makefile" => Some((FileKind::Code, "makefile")),
        _ => None,
    }
}

/// Extensions whose files are never classified (archives, binaries, media).
#[must_use]
pub fn is_binary_extension(ext: &str) -> bool {
    matches!(
        ext,
        "zip" | "tar"
            | "gz"
            | "bz2"
            | "xz"
            | "7z"
            | "rar"
            | "exe"
            | "dll"
            | "dylib"
            | "so"
            | "a"
            | "o"
            | "obj"
            | "wasm"
            | "class"
            | "jar"
            | "pdf"
            | "doc"
            | "docx"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "mp3"
            | "mp4"
            | "mov"
            | "avi"
            | "mkv"
            | "db"
            | "sqlite"
    )
}

/// Extensions classified `Code` by the ingest map.
pub const CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "h", "hpp", "go", "java", "js", "mjs", "cjs", "jsx", "ts",
    "tsx", "py", "rs", "rb", "php", "swift", "kt", "kts", "cs", "scala", "sh", "bash",
    "zsh", "sql", "css", "scss", "less", "vue", "svelte",
];

/// Discovery scope: extensions the walker collects. Mirrors
/// `common-core::walk::SOURCE_EXTENSIONS` (the walker's zero-domain
/// default, which cannot depend on this crate) — a guidance test pins the
/// two lists together so they cannot drift.
pub const SOURCE_EXTENSIONS: &[&str] = &["zig", "zon", "py", "rs", "md"];

#[cfg(test)]
#[path = "../tests/file_kind.rs"]
mod tests;
