//! Sidecar header: fixed-layout first line naming the source hash and
//! the input components the sidecar receipt consumed.
//!
//! The header is framing, not content: readers that only need the JSON
//! body skip the first line when it carries the magic prefix, and files
//! without a header load exactly as before.

use std::collections::BTreeMap;

/// Magic prefix of every header line, including the trailing space.
pub const HEADER_MAGIC: &str = "GHDR1 ";
/// Schema version of the header layout itself.
pub const HEADER_VERSION: u32 = 1;

/// Per-file derived facts: which source bytes were read and which named
/// input components the receipt consumed. Absent components were not
/// consumed, so their later change never invalidates this sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarHeader {
    /// Source path the sidecar was derived from, as stored.
    pub src: String,
    /// Hex hash of the derived JSON body at write time.
    pub body: String,
    /// Hex hash of the source bytes at write time, when recorded.
    /// Absent on legacy receipts, which rename adoption must skip.
    pub src_hash: Option<String>,
    /// Consumed input components (name to hex hash), sorted.
    pub components: BTreeMap<String, String>,
}

impl SidecarHeader {
    /// Render the fixed-layout first line (no trailing newline).
    #[must_use]
    pub fn render(&self) -> String {
        let comps = self
            .components
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        use std::fmt::Write as _;
        let mut line = format!("{HEADER_MAGIC}src={}\tbody={}", self.src, self.body);
        if let Some(hash) = &self.src_hash {
            let _ = write!(line, "\tsrchash={hash}");
        }
        let _ = write!(line, "\tcomp={comps}");
        line
    }

    /// Parse one header line; `None` when the magic prefix is absent.
    #[must_use]
    pub fn parse_line(line: &str) -> Option<Self> {
        let rest = line.strip_prefix(HEADER_MAGIC)?;
        let mut src = None;
        let mut body = None;
        let mut src_hash = None;
        let mut components = BTreeMap::new();
        for field in rest.split('\t') {
            let (key, value) = field.split_once('=')?;
            match key {
                "src" => src = Some(value.to_string()),
                "body" => body = Some(value.to_string()),
                "srchash" => src_hash = Some(value.to_string()),
                "comp" => {
                    if !value.is_empty() {
                        for entry in value.split(',') {
                            let (k, v) = entry.split_once('=')?;
                            components.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                _ => return None,
            }
        }
        Some(Self {
            src: src?,
            body: body?,
            src_hash,
            components,
        })
    }
}

/// Split file content into its header (when the first line carries one)
/// and the remaining JSON body. Old files return `None` with the full
/// content untouched.
#[must_use]
pub fn strip_header(content: &str) -> (Option<SidecarHeader>, &str) {
    let (first, rest) = content.split_once('\n').unwrap_or((content, ""));
    match SidecarHeader::parse_line(first) {
        Some(header) => (Some(header), rest),
        None => (None, content),
    }
}
