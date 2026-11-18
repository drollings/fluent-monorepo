//! The disambiguated interlingua: a portable, content-addressed 64-bit ID
//! system bridging annotated text (spaCy lemmas/deps), RDF knowledge stores
//! (YaGO classes), and deterministic routing actions.
//!
//! Layout: 16-bit namespace + 48-bit local id. Every id is a **pure function
//! of content** — `trunc48(hash(content))` — so the same content yields the
//! same id on every machine, every file order, every restart. There is
//! deliberately **no raw constructor**: the only way in is [`InterlinguaId::new`]
//! (the F1 fix — a `from_raw` path made collision handling inert and could
//! manufacture arbitrary ids).
//!
//! Collision policy (F2): **first-wins with canonical disambiguation**. When
//! two distinct canonicals truncate to the same local id they share the
//! `InterlinguaId` as a stable *bucket*; the registry keeps both canonicals
//! under it and consumers that need injectivity key on
//! `(InterlinguaId, canonical_name)`. This mirrors spaCy's own `StringStore`
//! and keeps ids order-independent (a probe family would be universe-
//! dependent — rejected, §2.3).
//!
//! Collision math (§2.3): birthday probability for *n* entries ≈ `n²/(2·2⁴⁸)`.
//! 130k YaGO classes → ≈ 3.0e-5; 1M lemmas → ≈ 1.8e-3. Each occurrence is
//! surfaced as a `CollisionNote` and disambiguated by the canonical string.

use serde::{Deserialize, Serialize};

use crate::NodeId;

/// The 64-bit cross-system identifier: 16-bit namespace, 48-bit local id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// The 64-bit cross-system identifier: 16-bit namespace, 48-bit local id.
pub struct InterlinguaId(u64);

/// The 16-bit namespace of an [`InterlinguaId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u16)]
/// The 16-bit namespace of an [`InterlinguaId`].
pub enum InterlinguaNamespace {
    Reserved = 0x0000,
    YagoClass = 0x0100,
    YagoEntity = 0x0200,
    SpacyLemma = 0x0300,
    UserDefined = 0x0400,
    RdfProperty = 0x0500,
}

/// The namespace occupies the top 16 bits (48–63); the local id the bottom
/// 48 bits (0–47), per the layout diagram above.
pub const NAMESPACE_BITS: u64 = 16;
pub const LOCAL_BITS: u64 = 48;
pub const LOCAL_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// The single truncation helper (DRY). Every namespace derives its local id
/// through this function.
#[must_use]
pub const fn local_id_of(full_hash: i64) -> i64 {
    (full_hash as u64 & LOCAL_MASK) as i64
}

/// Generic helper: derive an `InterlinguaId` for `ns` from a full-width content hash.
///
/// This is the single truncation seam — per-kind helpers delegate here.
#[must_use]
pub fn id_for_namespace(hash: i64, ns: InterlinguaNamespace) -> InterlinguaId {
    InterlinguaId::new(ns, local_id_of(hash))
}

// ─── Hash primitives (DRY, hoisted from spacy-rs / guidance-rdf) ───────────

/// MurmurHash64A with seed 1 — the spaCy `hash_utf8`.
///
/// Copied from `spacy-rs/src/hash.rs` so the helper can live in `types` without
/// creating a `types → spacy-rs` edge (import boundary). Byte-exact with the
/// authoritative C implementation.
fn murmur64a(data: &[u8], seed: u64) -> u64 {
    const M: u64 = 0xc6a4_a793_5bd1_e995;
    const R: u32 = 47;
    let len = data.len();
    let mut h = seed ^ (len as u64).wrapping_mul(M);
    let mut i = 0;
    while i + 8 <= len {
        let mut k = u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h ^= k;
        h = h.wrapping_mul(M);
        i += 8;
    }
    let mut tail = 0u64;
    let remaining = len - i;
    if remaining >= 7 {
        tail |= u64::from(data[i + 6]) << 48;
    }
    if remaining >= 6 {
        tail |= u64::from(data[i + 5]) << 40;
    }
    if remaining >= 5 {
        tail |= u64::from(data[i + 4]) << 32;
    }
    if remaining >= 4 {
        tail |= u64::from(data[i + 3]) << 24;
    }
    if remaining >= 3 {
        tail |= u64::from(data[i + 2]) << 16;
    }
    if remaining >= 2 {
        tail |= u64::from(data[i + 1]) << 8;
    }
    if remaining >= 1 {
        tail |= u64::from(data[i]);
    }
    if remaining != 0 {
        h ^= tail;
        h = h.wrapping_mul(M);
    }
    h ^= h >> R;
    h = h.wrapping_mul(M);
    h ^= h >> R;
    h
}

fn hash_utf8(s: &str) -> u64 {
    murmur64a(s.as_bytes(), 1)
}

fn hash_iri(iri: &str) -> i64 {
    let hash = common_core::hash::blake3_hash(iri.as_bytes());
    i64::from_le_bytes(hash[0..8].try_into().unwrap())
}

// ─── Per-kind helpers (the primary public API) ─────────────────────────────

/// `SpacyLemma` id for a lemma string (MurmurHash64A seed 1, 48-bit truncation).
///
/// `node_id` (the full 64-bit graph key) is **not** derived from this — it is a
/// stored `ConceptMetadata.node_id` cross-reference (F5). This helper is for the
/// 48-bit `InterlinguaId` only.
#[must_use]
/// ─── Per-kind helpers (the primary public API) ─────────────────────────────
/// `SpacyLemma` id for a lemma string (MurmurHash64A seed 1, 48-bit truncation).
///
/// `node_id` (the full 64-bit graph key) is **not** derived from this — it is a
/// stored `ConceptMetadata.node_id` cross-reference (F5). This helper is for the
/// 48-bit `InterlinguaId` only.
pub fn lemma_id_for_str(s: &str) -> InterlinguaId {
    id_for_namespace(hash_utf8(s) as i64, InterlinguaNamespace::SpacyLemma)
}

/// `YagoClass` id for a class IRI (BLAKE3, 48-bit truncation).
#[must_use]
/// `YagoClass` id for a class IRI (BLAKE3, 48-bit truncation).
pub fn yago_class_id_for_iri(iri: &str) -> InterlinguaId {
    id_for_namespace(hash_iri(iri), InterlinguaNamespace::YagoClass)
}

/// `YagoEntity` id for an entity IRI (BLAKE3, 48-bit truncation).
#[must_use]
/// `YagoEntity` id for an entity IRI (BLAKE3, 48-bit truncation).
pub fn yago_entity_id_for_iri(iri: &str) -> InterlinguaId {
    id_for_namespace(hash_iri(iri), InterlinguaNamespace::YagoEntity)
}

/// `RdfProperty` id for a property IRI (BLAKE3, 48-bit truncation).
#[must_use]
/// `RdfProperty` id for a property IRI (BLAKE3, 48-bit truncation).
pub fn property_id_for_iri(iri: &str) -> InterlinguaId {
    id_for_namespace(hash_iri(iri), InterlinguaNamespace::RdfProperty)
}

impl InterlinguaId {
    /// Construct from namespace + a full-width content hash. Truncates to 48
    /// bits. This is the ONLY constructor — there is deliberately no raw
    /// constructor (F1): the local id is always `trunc48(hash(content))`.
    pub const fn new(namespace: InterlinguaNamespace, local: i64) -> Self {
        Self(((namespace as u64) << LOCAL_BITS) | ((local as u64) & LOCAL_MASK))
    }

    pub const fn namespace(self) -> InterlinguaNamespace {
        match (self.0 >> LOCAL_BITS) as u16 {
            0x0100 => InterlinguaNamespace::YagoClass,
            0x0200 => InterlinguaNamespace::YagoEntity,
            0x0300 => InterlinguaNamespace::SpacyLemma,
            0x0400 => InterlinguaNamespace::UserDefined,
            0x0500 => InterlinguaNamespace::RdfProperty,
            _ => InterlinguaNamespace::Reserved,
        }
    }

    pub const fn local_id(self) -> i64 {
        (self.0 & LOCAL_MASK) as i64
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Reconstruct an id previously produced by [`Self::new`] (the serde/DB
    /// round-trip) — **not** a raw constructor. It exists so a stored `u64`
    /// (a serialized id, an `interlingua_index`/`interlingua_concepts` column,
    /// a persisted `RoutingSignal`) can be brought back as an `InterlinguaId`
    /// verbatim. Do **not** use it to manufacture an id from an arbitrary
    /// value in new code (F1): ids are content-derived, and the `0` sentinel
    /// (RESERVED namespace) should be modeled as `Option::None` where possible.
    /// There is deliberately no second raw constructor.
    #[must_use]
    pub const fn from_stored_u64(v: u64) -> Self {
        Self(v)
    }

    /// Deprecated alias: use [`Self::from_stored_u64`] instead. The old
    /// name hid the DB-round-trip-only contract.
    #[must_use]
    #[deprecated(note = "use from_stored_u64 instead")]
    pub const fn from_u64(v: u64) -> Self {
        Self::from_stored_u64(v)
    }

    pub fn as_i64(self) -> i64 {
        self.0 as i64
    }

    /// Reconstruct from a non-negative `i64`. Returns `None` for negative
    /// input (all built-in namespaces sit below bit 63, so their `as_i64` is
    /// always non-negative; `UserDefined` callers must respect this).
    pub fn from_i64(v: i64) -> Option<Self> {
        if v >= 0 {
            Some(Self(v as u64))
        } else {
            None
        }
    }

    pub const fn is_yago(self) -> bool {
        matches!(
            self.namespace(),
            InterlinguaNamespace::YagoClass | InterlinguaNamespace::YagoEntity
        )
    }

    pub const fn is_spacy_lemma(self) -> bool {
        (self.0 >> LOCAL_BITS) as u16 == InterlinguaNamespace::SpacyLemma as u16
    }

    /// The 48-bit local id as a non-negative `i64` for SQLite INTEGER storage.
    pub fn sql_local(self) -> i64 {
        self.local_id()
    }

    /// Reconstruct the id from the `(namespace, local_id)` column pair.
    pub fn from_sql(namespace: u16, local_id: i64) -> Self {
        InterlinguaId::new(InterlinguaNamespace::from_u16(namespace), local_id)
    }
}

impl InterlinguaNamespace {
    /// The namespace for a raw `u16` value; unknown values map to `Reserved`.
    pub const fn from_u16(v: u16) -> Self {
        match v {
            0x0100 => Self::YagoClass,
            0x0200 => Self::YagoEntity,
            0x0300 => Self::SpacyLemma,
            0x0400 => Self::UserDefined,
            0x0500 => Self::RdfProperty,
            _ => Self::Reserved,
        }
    }
}

impl std::fmt::Display for InterlinguaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}:{:012x}", self.namespace(), self.local_id())
    }
}

/// The disambiguation record for a concept: identity (`id`), the canonical
/// string (which resolves collisions), the origin, and the cross-reference to
/// the concept's node in Coral's durable graph.
///
/// F5 discipline: `node_id` (the **full 64-bit** `hash_iri`) is *stored,
/// never derived* from `id.local_id()` (the **truncated 48-bit** value) — the
/// 16 truncated-away bits cannot be recovered. The bridge from an
/// `InterlinguaId` to its graph node is a lookup through `ConceptMetadata.node_id`,
/// nothing more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptMetadata {
    pub id: InterlinguaId,
    pub canonical_name: String,
    pub namespace: InterlinguaNamespace,
    pub yago_iri: Option<String>,
    pub yago_class_iri: Option<String>,
    pub label: Option<String>,
    /// Cross-reference to the concept's node in Coral's graph (the SQLite
    /// `context_nodes` row). Set when the concept was ingested via the
    /// content-addressed bridge.
    pub node_id: Option<NodeId>,
    /// The `rdfs:subClassOf` parent's interlingua id (a `YagoClass`), when
    /// known at load. Carried on the metadata so every store materializes the
    /// hierarchy from the single loader output (C5 — one source of edges).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_class_id: Option<InterlinguaId>,
}

#[cfg(test)]
#[path = "../tests/interlingua.rs"]
mod tests;
