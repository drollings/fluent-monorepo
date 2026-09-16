//! YSM1 v1 — the binary view of the YaGO class hierarchy.
//!
//! The TTL parse (`turtle::subclass_edges` over every triple plus the
//! `HierarchyIndex` closure build) is the boot cost; the file read is not.
//! This module is the binary shortcut around that parse: the writer encodes a
//! TTL through the same edge path once, and the reader decodes classes plus
//! index edges directly. TTL remains the source of truth; the writer is the
//! only trusted producer.
//!
//! Layout (all multi-byte integers little-endian; the 44-byte header frame
//! follows the `knowledge::index_header` magic+version envelope precedent):
//!
//! ```text
//! header (44B): magic u32 = 0x5953_4D31 · format version u32 = 1
//!               · taxonomy_version u32 · content_digest [u8; 32]
//! payload:      class_count u32 · edge_count u32
//!               · class records, sorted by curie:
//!                 curie_len u32 · curie bytes · id u64
//!               · edge pairs over the sorted class table: (u32, u32)
//! ```
//!
//! `content_digest` is blake3 over the payload bytes only
//! (`common_core::hash`, the canonical hash home). `taxonomy_version` binds
//! the binary to the pinned taxonomy identity: the first four digest bytes
//! of the pinned `YAGO_VERSION` string, so a binary encoded from a different
//! taxonomy release is rejected at load like a digest mismatch.

use std::collections::HashMap;

use fluent_types::{yago_class_id_for_iri, InterlinguaId};

use crate::normalize::iri_to_curie;
use crate::yago::YAGO_VERSION;
use crate::OntologyError;

/// Magic word, as a little-endian u32. The byte order follows the
/// pre-existing header check in `yago_view` (kept byte-identical so the
/// header probe and the codec agree on what "magic" means).
pub const YSM1_MAGIC_U32: u32 = 0x5953_4D31;
/// The only format version this reader accepts.
pub const YSM1_FORMAT_VERSION: u32 = 1;
/// Header length in bytes: magic + version + taxonomy version + digest.
pub const YSM1_HEADER_LEN: usize = 44;

/// The pinned taxonomy identity stamped by the writer and required by the
/// reader: the first four blake3 bytes of the pinned `YAGO_VERSION` string.
#[must_use]
pub fn taxonomy_version() -> u32 {
    let digest = common_core::hash::blake3_hash(YAGO_VERSION.as_bytes());
    u32::from_le_bytes(digest[0..4].try_into().unwrap())
}

/// A decoded YSM1 payload: the sorted class table plus hierarchy edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ysm1File {
    /// `(curie, id)` records in curie-sorted order.
    pub classes: Vec<(String, InterlinguaId)>,
    /// `(child, parent)` hierarchy edges.
    pub edges: Vec<(InterlinguaId, InterlinguaId)>,
}

/// Encode a TTL document to YSM1 bytes through the existing
/// `turtle::subclass_edges` path — the same derivation `YagoView` uses, so
/// the binary and the TTL leg agree by construction.
#[must_use]
pub fn encode_ttl(ttl: &str) -> Vec<u8> {
    let (_, edges) = crate::turtle::subclass_edges(ttl);
    let mut ids: HashMap<String, InterlinguaId> = HashMap::new();
    let mut id_edges: Vec<(InterlinguaId, InterlinguaId)> = Vec::new();
    for (subject_iri, object_iri) in &edges {
        let subject_id = yago_class_id_for_iri(subject_iri);
        let object_id = yago_class_id_for_iri(object_iri);
        ids.entry(iri_to_curie(subject_iri)).or_insert(subject_id);
        ids.entry(iri_to_curie(object_iri)).or_insert(object_id);
        id_edges.push((subject_id, object_id));
    }
    let mut classes: Vec<(String, InterlinguaId)> =
        ids.into_iter().collect();
    classes.sort_by(|a, b| a.0.cmp(&b.0));
    encode(&classes, &id_edges)
}

/// Encode a sorted class table plus hierarchy edges to YSM1 bytes.
pub fn encode(
    classes: &[(String, InterlinguaId)],
    edges: &[(InterlinguaId, InterlinguaId)],
) -> Vec<u8> {
    let index: HashMap<InterlinguaId, u32> = classes
        .iter()
        .enumerate()
        .map(|(i, (_, id))| (*id, i as u32))
        .collect();
    let mut payload = Vec::new();
    payload.extend_from_slice(&(classes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(edges.len() as u32).to_le_bytes());
    for (curie, id) in classes {
        payload.extend_from_slice(&(curie.len() as u32).to_le_bytes());
        payload.extend_from_slice(curie.as_bytes());
        payload.extend_from_slice(&id.as_u64().to_le_bytes());
    }
    for (child, parent) in edges {
        // Every edge endpoint is a registered class by construction (both
        // endpoints are inserted during encoding). A foreign endpoint encodes
        // as an out-of-range index, which the reader rejects — a caller bug
        // surfaces as a typed decode error, never a misindexed edge.
        let ci = index.get(child).copied().unwrap_or(u32::MAX);
        let pi = index.get(parent).copied().unwrap_or(u32::MAX);
        payload.extend_from_slice(&ci.to_le_bytes());
        payload.extend_from_slice(&pi.to_le_bytes());
    }
    let digest = common_core::hash::blake3_hash(&payload);
    let mut out = Vec::with_capacity(YSM1_HEADER_LEN + payload.len());
    out.extend_from_slice(&YSM1_MAGIC_U32.to_le_bytes());
    out.extend_from_slice(&YSM1_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&taxonomy_version().to_le_bytes());
    out.extend_from_slice(&digest);
    out.extend_from_slice(&payload);
    out
}

/// Decode YSM1 bytes, verifying magic, format version, taxonomy version, and
/// the blake3 content digest. Any mismatch is a typed rejection — the caller
/// falls back to the TTL leg (fail-open to provisional, as before).
pub fn decode(bytes: &[u8]) -> Result<Ysm1File, OntologyError> {
    let reject = |why: &str| OntologyError::YagoView(format!("YSM1 rejected: {why}"));
    if bytes.len() < YSM1_HEADER_LEN {
        return Err(reject("short header"));
    }
    if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != YSM1_MAGIC_U32 {
        return Err(reject("bad magic"));
    }
    if u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != YSM1_FORMAT_VERSION {
        return Err(reject("unknown format version"));
    }
    if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != taxonomy_version() {
        return Err(reject("taxonomy version mismatch"));
    }
    let stored: [u8; 32] = bytes[12..44].try_into().unwrap();
    let payload = &bytes[YSM1_HEADER_LEN..];
    if common_core::hash::blake3_hash(payload) != stored {
        return Err(reject("content digest mismatch"));
    }
    let mut cursor = Reader::new(payload).ok_or_else(|| reject("short payload"))?;
    let class_count = cursor.u32().ok_or_else(|| reject("short class count"))? as usize;
    let edge_count = cursor.u32().ok_or_else(|| reject("short edge count"))? as usize;
    let mut classes = Vec::with_capacity(class_count.min(1 << 20));
    for _ in 0..class_count {
        let len = cursor.u32().ok_or_else(|| reject("short record"))? as usize;
        let curie = cursor
            .bytes(len)
            .ok_or_else(|| reject("short curie"))
            .and_then(|b| String::from_utf8(b.to_vec()).map_err(|_| reject("bad curie")))?;
        let id = cursor.u64().ok_or_else(|| reject("short id"))?;
        classes.push((curie, InterlinguaId::from_u64(id)));
    }
    let mut edges = Vec::with_capacity(edge_count.min(1 << 20));
    for _ in 0..edge_count {
        let ci = cursor.u32().ok_or_else(|| reject("short edge"))?;
        let pi = cursor.u32().ok_or_else(|| reject("short edge"))?;
        let child = classes
            .get(ci as usize)
            .map(|(_, id)| *id)
            .ok_or_else(|| reject("edge index out of range"))?;
        let parent = classes
            .get(pi as usize)
            .map(|(_, id)| *id)
            .ok_or_else(|| reject("edge index out of range"))?;
        edges.push((child, parent));
    }
    Ok(Ysm1File { classes, edges })
}

/// Cursor over a little-endian payload; `None` on overrun (never panics).
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 8 {
            return None;
        }
        Some(Self { buf, pos: 0 })
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        self.take(n)
    }
}

#[cfg(test)]
#[path = "../tests/ysm1.rs"]
mod tests;
