//! Bounded cache over derived view state: one [`ViewsBundle`](super::ViewsBundle)
//! per distinct gateway state, keyed by the content hash of the DTOs it was
//! built from.
//!
//! Same discipline as the pane cache: write-through (`insert`, never
//! `get_or_load`), capacity-bounded LRU, miss-safe — eviction only causes a
//! rebuild, never a stale bundle, because the key IS the content hash.

use common_core::cache::LoadCache;
use common_core::hash::fnv1a64;
use fluent_egui::EguiError;
use protopyne_view::{GatewayError, TerminalGateway};

use super::ViewsBundle;

/// Default capacity: a handful of recent gateway states for scrub-back
/// renders; each bundle is kilobytes, and eviction only rebuilds.
pub const DEFAULT_VIEW_CACHE_CAPACITY: usize = 16;

/// Content key for one gateway state: FNV-1a over the canonical JSON of the
/// four reads the bundle derives from. Struct field order makes the bytes
/// deterministic (no maps inside these DTOs), so equal states key equal.
#[must_use]
pub fn state_key(payload: &[u8]) -> u64 {
    fnv1a64(payload)
}

/// Write-through load: the render path only inserts built bundles, so a
/// mistaken `get_or_load` fails closed instead of synthesizing a view.
/// Takes the key by reference because `LoadCache::new` dictates the shape.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn never_load(_key: &u64) -> Result<ViewsBundle, String> {
    Err("view cache is write-through; load is never called".to_owned())
}

/// Bounded, miss-safe cache over [`ViewsBundle`](super::ViewsBundle).
pub struct ViewCache {
    entries: LoadCache<u64, ViewsBundle, String>,
}

impl ViewCache {
    /// Create an empty store with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_VIEW_CACHE_CAPACITY)
            .expect("default view cache capacity is a non-zero constant")
    }

    /// Create an empty store holding up to `capacity` bundles; refuses zero.
    pub fn with_capacity(capacity: usize) -> Result<Self, EguiError> {
        let entries = LoadCache::new(capacity, never_load).map_err(|reason| {
            EguiError::InvalidParam { what: "capacity".to_owned(), reason }
        })?;
        Ok(Self { entries })
    }

    /// Pull every gateway read and return the shaped bundle: a cache hit
    /// when the gateway state already built one, a build-and-store on a
    /// miss. A journal the provenance graph cannot order refuses with the
    /// misbehavior named.
    pub fn load(&self, gateway: &dyn TerminalGateway) -> Result<ViewsBundle, GatewayError> {
        let health = gateway.health()?;
        let proposals = gateway.proposals()?;
        let portfolio = gateway.portfolio()?;
        let journal = gateway.journal()?;
        let payload = serde_json::to_vec(&(&health, &proposals, &portfolio, &journal))
            .map_err(|refused| {
                GatewayError::Malformed(format!("view state has no canonical bytes: {refused}"))
            })?;
        let key = state_key(&payload);
        if let Some(bundle) = self.entries.get(&key) {
            return Ok(bundle);
        }
        let bundle = ViewsBundle::of(&health, &portfolio, proposals, journal)?;
        self.entries.insert(key, bundle.clone());
        Ok(bundle)
    }

    /// How many distinct gateway states are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no view state is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ViewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ViewCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ViewCache")
            .field("len", &self.entries.len())
            .field("capacity", &self.entries.capacity())
            .finish()
    }
}
