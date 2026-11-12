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
/// Per-file derived facts: which source bytes were read and which named
/// input components the receipt consumed. Absent components were not
/// consumed, so their later change never invalidates this sidecar.
pub struct SidecarHeader {
    /// Source path the sidecar was derived from, as stored.
    pub src: String,
    /// Hex hash of the derived JSON body at write time.
    pub body: String,
    /// Hex hash of the source bytes at write time, when recorded.
    /// Absent on legacy receipts, which rename adoption must skip.
    pub src_hash: Option<String>,
    /// Stuck record: the loop escalated this file and holds it for a
    /// human. Stuck receipts never count as fresh, even when the bytes
    /// are unchanged — reprocessing retries the fix each run. Absent
    /// (legacy) means not stuck; a later clean save clears the flag.
    pub stuck: bool,
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
        // Stuck is opt-in on the wire: clean receipts render exactly
        // the legacy shape, so generation markers never rotate on it.
        if self.stuck {
            line.push_str("\tstuck=1");
        }
        let _ = write!(line, "\tcomp={comps}");
        line
    }

    /// Parse one header line; `None` when the magic prefix is absent.
    /// Unknown fields refuse the line: a newer writer's receipt must
    /// never read as current under an older reader.
    #[must_use]
    pub fn parse_line(line: &str) -> Option<Self> {
        let rest = line.strip_prefix(HEADER_MAGIC)?;
        let mut src = None;
        let mut body = None;
        let mut src_hash = None;
        let mut stuck = false;
        let mut components = BTreeMap::new();
        for field in rest.split('\t') {
            let (key, value) = field.split_once('=')?;
            match key {
                "src" => src = Some(value.to_string()),
                "body" => body = Some(value.to_string()),
                "srchash" => src_hash = Some(value.to_string()),
                "stuck" => stuck = value == "1",
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
            stuck,
            components,
        })
    }
}

/// Split file content into its header (when the first line carries one)
/// and the remaining JSON body. Old files return `None` with the full
/// content untouched.
#[must_use]
/// Split file content into its header (when the first line carries one)
/// and the remaining JSON body. Old files return `None` with the full
/// content untouched.
pub fn strip_header(content: &str) -> (Option<SidecarHeader>, &str) {
    let (first, rest) = content.split_once('\n').unwrap_or((content, ""));
    match SidecarHeader::parse_line(first) {
        Some(header) => (Some(header), rest),
        None => (None, content),
    }
}

#[cfg(test)]
mod sidecar_codec_tests {
    use super::SidecarHeader;
    use std::collections::BTreeMap;

    fn header() -> SidecarHeader {
        SidecarHeader {
            src: "a.rs".to_string(),
            body: "b".to_string(),
            src_hash: Some("s".to_string()),
            stuck: false,
            components: BTreeMap::from([("toolchain".to_string(), "t".to_string())]),
        }
    }

    #[test]
    fn clean_receipts_keep_legacy_shape() {
        let line = header().render();
        assert!(!line.contains("stuck"), "{line}");
        let back = SidecarHeader::parse_line(&line).expect("parses");
        assert!(!back.stuck);
        assert_eq!(back.components.len(), 1);
    }

    #[test]
    fn stuck_round_trips_and_reads_stale() {
        let mut stuck = header();
        stuck.stuck = true;
        let line = stuck.render();
        assert!(line.contains("\tstuck=1"), "{line}");
        let back = SidecarHeader::parse_line(&line).expect("parses");
        assert!(back.stuck);
        assert_eq!(back.src_hash.as_deref(), Some("s"));
    }
}