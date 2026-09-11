//! Structure enrichment (P5): lexical hits gain symbol context from the P2
//! tree-sitter adapters — never a parallel parser.
//!
//! Best-effort and capability-gated like all serving I/O: unparsable files,
//! unsupported languages, and unreadable files keep raw hits, with a
//! per-file diagnostic. At most [`RG_STRUCTURE_ENRICH_FILE_LIMIT`] files
//! are parsed per call (budget [`STRUCTURE_ENRICH_FILE_LIMIT`]).

use std::collections::HashMap;
use std::path::Path;

use fluent_types::{GuidanceDoc, Member};

use crate::ast_parser::AstParser;
use crate::query::rg_backend::RgHit;

/// Maximum files parsed per enrichment call (preserved budget).
pub const STRUCTURE_ENRICH_FILE_LIMIT: usize = 100;

/// An L0 hit with its enclosing symbol, when the file parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichedRgHit {
    /// The raw lexical hit.
    pub hit: RgHit,
    /// Innermost enclosing symbol name (`None` when the file did not parse
    /// or no member starts at or above the hit line).
    pub symbol: Option<String>,
}

/// Enrich hits with enclosing symbols, returning per-file diagnostics for
/// every file that kept raw hits. Fails closed without `FsCapability`.
pub fn enrich_hits(root: &Path, hits: &[RgHit]) -> (Vec<EnrichedRgHit>, Vec<String>) {
    if fluent_wvr::capability::check_capability(&fluent_wvr::capability::FsCapability::new())
        .is_err()
    {
        return (
            hits.iter()
                .cloned()
                .map(|hit| EnrichedRgHit { hit, symbol: None })
                .collect(),
            vec!["enrichment skipped: no FsCapability in scope".to_string()],
        );
    }
    // One parse per file, in first-hit order, under the file budget.
    let mut order: Vec<&str> = Vec::new();
    let mut by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, hit) in hits.iter().enumerate() {
        if !by_file.contains_key(hit.path.as_str()) {
            order.push(hit.path.as_str());
        }
        by_file.entry(hit.path.as_str()).or_default().push(index);
    }
    let mut members_by_file: HashMap<&str, Option<Vec<Member>>> = HashMap::new();
    let mut diagnostics = Vec::new();
    let mut parser = AstParser::new();
    for path in order.into_iter().take(STRUCTURE_ENRICH_FILE_LIMIT) {
        match parse_members(root, path, &mut parser) {
            Ok(doc) => {
                members_by_file.insert(path, Some(doc.members));
            }
            Err(reason) => {
                diagnostics.push(format!("{path}: {reason}"));
                members_by_file.insert(path, None);
            }
        }
    }
    let enriched = hits
        .iter()
        .map(|hit| {
            let symbol = members_by_file
                .get(hit.path.as_str())
                .and_then(|members| members.as_ref())
                .and_then(|members| innermost_below(members, hit.line))
                .map(|member| member.name.to_string());
            EnrichedRgHit {
                hit: hit.clone(),
                symbol,
            }
        })
        .collect();
    (enriched, diagnostics)
}

/// Deepest member starting at or above `target` (1-based, matching
/// `RgHit.line` and `Member.line`), descending into nested members.
fn innermost_below<'a>(members: &'a [Member], target: u32) -> Option<&'a Member> {
    let mut best: Option<&'a Member> = None;
    for member in members {
        if member.line.is_some_and(|start| start <= target) {
            best = Some(member);
        }
    }
    match best {
        None => None,
        Some(member) => innermost_below(&member.members, target).or(Some(member)),
    }
}

fn parse_members(
    root: &Path,
    relative: &str,
    parser: &mut AstParser,
) -> Result<GuidanceDoc, String> {
    let absolute = root.join(relative);
    let source =
        std::fs::read_to_string(&absolute).map_err(|error| format!("unreadable ({error})"))?;
    if source.len() > 1024 * 1024 {
        return Err("skipped: over 1MiB code cap".to_string());
    }
    parser
        .parse_file(absolute.as_path(), &source)
        .map_err(|error| format!("unparsable ({error:?})"))
}

#[cfg(test)]
#[path = "../../tests/query_structure_enrich.rs"]
mod tests;
