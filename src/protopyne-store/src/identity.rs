//! Verified content identity: a fast digest plus the bytes that prove it.
//!
//! A bare `u64` cannot be *verified*: at 64 bits a collision is unlikely but
//! not impossible, and a manifest that aliases two different datasets on a
//! collision has silently mislabelled a run. Here the digest is a fast
//! compare and the canonical bytes are the authority, so a collision costs
//! one comparison instead of a wrong answer.
//!
//! The digest is the shared FNV-1a-64 over the canonical bytes — the same
//! hash the parameter fingerprint uses — so digests agree wherever the
//! bytes agree. The bytes themselves are framed with the parameter
//! encoding's length-prefixed idiom by the crate that owns the framed
//! values; this module never invents a second framing, it only verifies.

use std::sync::Arc;

/// A 64-bit content hash of a canonical byte encoding, **plus** the bytes.
///
/// Construct through [`of`](Self::of): the hash always matches the bytes it
/// was built with, so equality (hash and bytes) is consistent with hashing
/// (the digest), and a map keyed on identities never aliases distinct
/// datasets even when their digests collide. Field construction outside
/// `of` is possible but reserved for the forced-collision control group:
/// production code names its bytes and lets `of` hash them.
#[derive(Clone)]
pub struct VerifiedId {
    /// The FNV-1a-64 digest of [`canonical`](Self::canonical).
    pub hash: u64,
    /// The authority: two identities match only when these match.
    pub canonical: Arc<[u8]>,
}

impl VerifiedId {
    /// Identify `canonical`: hash it once, keep the bytes for verification.
    #[must_use]
    pub fn of(canonical: &[u8]) -> Self {
        Self {
            hash: common_core::hash::fnv1a64(canonical),
            canonical: Arc::from(canonical),
        }
    }

    /// The fast digest. Display and map-hint only: verification always
    /// consults [`matches`](Self::matches), never this alone.
    #[must_use]
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Whether `other` identifies the same bytes: the digests must agree
    /// (fast reject) and the canonical bytes must agree (the authority). A
    /// digest collision across different bytes still reports inequality.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.hash == other.hash && self.canonical == other.canonical
    }

    /// The digest as eight hex characters for logs and report lines.
    /// Display only: truncating the digest never identifies anything.
    #[must_use]
    pub fn short_hex(&self) -> String {
        format!("{:016x}", self.hash).chars().take(8).collect()
    }

    /// The authority bytes this identity verifies against.
    #[must_use]
    pub fn canonical(&self) -> &[u8] {
        &self.canonical
    }
}

impl std::fmt::Debug for VerifiedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedId")
            .field("hash", &format!("{:016x}", self.hash))
            .field("bytes", &self.canonical.len())
            .finish()
    }
}

/// Structural equality: digest and bytes. Consistent with [`Hash`](std::hash::Hash),
/// which feeds the digest — equal identities always hash equal, so a
/// `HashMap` keyed on identities resolves digest collisions through this
/// comparison instead of aliasing.
impl PartialEq for VerifiedId {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.canonical == other.canonical
    }
}

impl Eq for VerifiedId {}

impl std::hash::Hash for VerifiedId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

#[cfg(test)]
#[path = "../tests/verified_id_collision.rs"]
mod tests_verified_id_collision;
