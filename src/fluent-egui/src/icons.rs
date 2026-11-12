//! Shell-owned icon cache: fingerprinted raster cache over an injected
//! renderer.
//!
//! The cache keys every entry by `(name, px, theme_fingerprint)`, so a
//! theme switch can never serve the previous theme's tinted bytes: the new
//! fingerprint simply misses. The shell rebuilds the cache (with a renderer
//! closing over the new scheme) on theme switches; the fingerprint keying is
//! the backstop, not the plan. Vector-drawn built-ins (eye, bolt, search)
//! keep the headless-geometry path in [`paint`](crate::paint) and never
//! enter this cache.
//!
//! Rendering is injected: the shell supplies the `resvg`/`tiny-skia`
//! lowering, tests supply a fake. Entries evict insertion-ordered past the
//! cap, so the cache stays bounded under any burst. Locks never cross an
//! await — every lookup is a short critical section and rendering happens
//! outside the map.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use dashmap::DashMap;

use crate::error::EguiError;

/// Cache key: icon name, render size in whole pixels (the caller rounds),
/// and the theme fingerprint the bytes were rasterized under.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Cache key: icon name, render size in whole pixels (the caller rounds),
/// and the theme fingerprint the bytes were rasterized under.
pub struct IconKey {
    /// Icon name the renderer resolves.
    pub name: String,
    /// Render size in whole pixels.
    pub px: u32,
    /// Theme fingerprint at raster time.
    pub fingerprint: u64,
}

/// Icon renderer: `(name, px)` to bytes. The shell closes over its scheme;
/// tests close over fixed bytes.
pub type IconRenderer = Arc<dyn Fn(&str, u32) -> Result<Vec<u8>, EguiError> + Send + Sync>;

/// Shell-owned cache of rendered icon bytes. Clone shares the entries.
pub struct IconCache {
    cap: usize,
    seq: AtomicU64,
    entries: DashMap<IconKey, (u64, Vec<u8>)>,
    render: RwLock<IconRenderer>,
}

impl IconCache {
    /// Cache at most `cap` entries; refuses zero. The renderer maps
    /// `(name, px)` to bytes and may close over the shell's current scheme.
    pub fn new(
        cap: usize,
        render: impl Fn(&str, u32) -> Result<Vec<u8>, EguiError> + Send + Sync + 'static,
    ) -> Result<Self, EguiError> {
        if cap == 0 {
            return Err(EguiError::InvalidParam {
                what: "cap".to_owned(),
                reason: "icon cache must hold at least one entry".to_owned(),
            });
        }
        Ok(Self {
            cap,
            seq: AtomicU64::new(0),
            entries: DashMap::new(),
            render: RwLock::new(Arc::new(render)),
        })
    }

    /// Rendered bytes for `(name, px, fingerprint)`: a hit clones out of
    /// the map, a miss renders outside the map and inserts. Render failures
    /// surface and never populate the cache.
    pub fn get(&self, name: &str, px: u32, fingerprint: u64) -> Result<Vec<u8>, EguiError> {
        let key = IconKey {
            name: name.to_owned(),
            px,
            fingerprint,
        };
        if let Some(entry) = self.entries.get(&key) {
            return Ok(entry.value().1.clone());
        }
        let render = self
            .render
            .read()
            .map_err(|_| EguiError::Dependency {
                reason: "icon renderer lock poisoned".to_owned(),
            })?
            .clone();
        let bytes = render(name, px)?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.entries.insert(key, (seq, bytes.clone()));
        self.evict_over_cap();
        Ok(bytes)
    }

    /// Drop the oldest insertions until the bound holds.
    fn evict_over_cap(&self) {
        while self.entries.len() > self.cap {
            let oldest = self
                .entries
                .iter()
                .map(|entry| (entry.key().clone(), entry.value().0))
                .min_by_key(|(_, seq)| *seq);
            match oldest {
                Some((key, _)) => {
                    self.entries.remove(&key);
                }
                None => break,
            }
        }
    }

    /// Cached entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no icon is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[path = "../tests/icons.rs"]
mod tests;