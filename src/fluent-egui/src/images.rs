//! Gated image ingestion: pool-bound fetch, validated bytes, caller-held
//! textures.
//!
//! An [`ImageLoader`] fetches URL bytes through a bounded pool behind the
//! injected transport, retries transients through [`TransportRetry`](crate::feed::TransportRetry),
//! and validates every payload once: empty, oversize, and magic-less bodies
//! fail as [`ImageError::Permanent`] before anything caches. Validated bytes
//! cache per URL, so a second load decodes once — concurrent loads for one
//! URL coalesce on an in-flight marker instead of stampeding the pool.
//! Failures never cache, so a transient may recover on the next load.
//!
//! Production transport is [`net_fetch`]: scheme-checked, capability-gated
//! through the shell's [`NetCapability`](fluent_concurrency::io::net::NetCapability)
//! set, and status-routed (4xx permanent, everything else transient). Tests
//! inject fakes instead — no hermetic suite dials the network.
//!
//! Emitter guidance: paint [`placeholder_rect`] until delivery, hold the
//! uploaded [`egui::TextureHandle`] in the caller (the loader caches bytes,
//! never textures), and route [`ImageError`] through
//! [`paint_error_chip`](crate::paint::paint_error_chip) — a failure is an
//! error chip, never a blank tile.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use common_core::sync::lock;
use fluent_concurrency::io::net::NetCapability;
use fluent_concurrency::pool::{ResultPool, ResultPoolError};
use fluent_concurrency::scope::CURRENT_CAPS;
use fluent_concurrency::tokio_runtime;
use fluent_wvr::capability::CapabilitySet;
use tokio::sync::Notify;

use crate::error::EguiError;
use crate::feed::TransportRetry;
use crate::wheel::{Pos2, Rect};

/// Image fetch failure: transients retry, permanents surface immediately.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    /// May succeed on retry (5xx, timeouts, pool backpressure).
    #[error("transient image fetch failure: {0}")]
    Transient(String),
    /// Retrying cannot help (4xx, bad URLs, oversize or magic-less bodies).
    #[error("permanent image fetch failure: {0}")]
    Permanent(String),
}

impl ImageError {
    /// Whether the failure is worth retrying.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

/// Transport: `(url)` to raw body bytes. The shell closes over its
/// capability-gated client ([`net_fetch`]); tests close over fixed bytes.
pub type ImageFetch = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ImageError>> + Send>>
        + Send
        + Sync,
>;

/// Validate one payload: non-empty, within `max_bytes`, and carrying a
/// known image magic (PNG, JPEG, GIF, WebP, BMP). The shell's texture upload
/// does the full decode; this sniffing keeps obvious non-images out of the
/// cache. Every refusal names its cause.
pub fn validate_image_bytes(bytes: &[u8], max_bytes: usize) -> Result<(), ImageError> {
    if bytes.is_empty() {
        return Err(ImageError::Permanent("empty payload".to_owned()));
    }
    if bytes.len() > max_bytes {
        return Err(ImageError::Permanent(format!(
            "oversize payload: {} > {max_bytes}",
            bytes.len()
        )));
    }
    if !has_image_magic(bytes) {
        return Err(ImageError::Permanent("unknown image format".to_owned()));
    }
    Ok(())
}

/// Known image magics (see [`validate_image_bytes`]).
fn has_image_magic(bytes: &[u8]) -> bool {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
    const GIF87: &[u8] = b"GIF87a";
    const GIF89: &[u8] = b"GIF89a";
    const BMP: &[u8] = b"BM";
    bytes.starts_with(PNG)
        || bytes.starts_with(JPEG)
        || bytes.starts_with(GIF87)
        || bytes.starts_with(GIF89)
        || bytes.starts_with(BMP)
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

/// Fetch URL bytes through the shell's capability set: scheme-checked before
/// any gated I/O, then status-routed (4xx permanent, anything else
/// transient). Pool workers run ungated, so the caps travel explicitly.
pub async fn net_fetch(
    caps: CapabilitySet,
    net: &NetCapability,
    url: String,
) -> Result<Vec<u8>, ImageError> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ImageError::Permanent(format!(
            "unsupported URL scheme: {url}"
        )));
    }
    CURRENT_CAPS
        .scope(caps, async move {
            if let Err(error) = fluent_concurrency::io::check_capability(net) {
                return Err(ImageError::Permanent(format!(
                    "net capability missing: {error}"
                )));
            }
            let response = net.client().get(&url).send().await.map_err(|error| {
                if error.is_builder() {
                    ImageError::Permanent(format!("bad request: {error}"))
                } else {
                    ImageError::Transient(format!("transport failure: {error}"))
                }
            })?;
            let status = response.status();
            if status.is_client_error() {
                return Err(ImageError::Permanent(format!("http {status}")));
            }
            if !status.is_success() {
                return Err(ImageError::Transient(format!("http {status}")));
            }
            response
                .bytes()
                .await
                .map(|body| body.to_vec())
                .map_err(|error| ImageError::Transient(format!("body failure: {error}")))
        })
        .await
}

/// Placeholder rect for an image of `width_px` by `height_px` at `origin`:
/// the shell paints this until delivery. Degenerate sizes collapse to the
/// origin instead of inverting the rect.
#[must_use]
pub fn placeholder_rect(origin: (f32, f32), width_px: f32, height_px: f32) -> Rect {
    let width = if width_px.is_finite() && width_px > 0.0 {
        width_px
    } else {
        0.0
    };
    let height = if height_px.is_finite() && height_px > 0.0 {
        height_px
    } else {
        0.0
    };
    Rect::new(
        Pos2::new(origin.0, origin.1),
        Pos2::new(origin.0 + width, origin.1 + height),
    )
}

/// URL-keyed image ingress: bounded pool, validated-bytes cache, in-flight
/// coalescing. Clone shares the pool and the cache.
pub struct ImageLoader {
    pool: Arc<ResultPool<String, Vec<u8>, ImageError>>,
    cache: Mutex<HashMap<String, Vec<u8>>>,
    inflight: Mutex<HashMap<String, Arc<Notify>>>,
    max_bytes: usize,
    retry: TransportRetry,
}

impl ImageLoader {
    /// Assemble the loader with a dedicated fetch pool. Refuses a zero
    /// worker cap, a zero queue, or a zero payload cap.
    pub fn new(
        cap: usize,
        queue_capacity: usize,
        fetch: ImageFetch,
        retry: TransportRetry,
        max_bytes: usize,
    ) -> Result<Self, EguiError> {
        if cap == 0 {
            return Err(EguiError::InvalidParam {
                what: "cap".to_owned(),
                reason: "image pool needs at least one worker".to_owned(),
            });
        }
        if queue_capacity == 0 {
            return Err(EguiError::InvalidParam {
                what: "queue_capacity".to_owned(),
                reason: "image queue must hold at least one job".to_owned(),
            });
        }
        if max_bytes == 0 {
            return Err(EguiError::InvalidParam {
                what: "max_bytes".to_owned(),
                reason: "image payload cap must be positive".to_owned(),
            });
        }
        let pool = Arc::new(ResultPool::new(
            tokio_runtime(),
            cap,
            queue_capacity,
            move |url: String| {
                let fetch = Arc::clone(&fetch);
                async move { fetch(url).await }
            },
        ));
        Ok(Self {
            pool,
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            max_bytes,
            retry,
        })
    }

    /// Validated bytes for `url`: cache hit, coalesced in-flight fetch, or
    /// a fresh pool job. Failures never populate the cache.
    pub async fn load(&self, url: &str) -> Result<Vec<u8>, ImageError> {
        loop {
            if let Some(hit) = lock(&self.cache).get(url).cloned() {
                return Ok(hit);
            }
            let waiter = {
                let mut inflight = lock(&self.inflight);
                if let Some(notify) = inflight.get(url) {
                    Some(Arc::clone(notify))
                } else {
                    inflight.insert(url.to_owned(), Arc::new(Notify::new()));
                    None
                }
            };
            if let Some(notify) = waiter {
                notify.notified().await;
            } else {
                let result = self.fetch_once(url).await;
                match result {
                    Ok(bytes) => {
                        lock(&self.cache).insert(url.to_owned(), bytes.clone());
                        if let Some(notify) = lock(&self.inflight).remove(url) {
                            notify.notify_waiters();
                        }
                        return Ok(bytes);
                    }
                    Err(error) => {
                        if let Some(notify) = lock(&self.inflight).remove(url) {
                            notify.notify_waiters();
                        }
                        return Err(error);
                    }
                }
            }
        }
    }

    /// One pool submission with retry and validation, outside the cache.
    async fn fetch_once(&self, url: &str) -> Result<Vec<u8>, ImageError> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ImageError::Permanent(format!(
                "unsupported URL scheme: {url}"
            )));
        }
        let url_owned = url.to_owned();
        let bytes = self
            .retry
            .run(
                |error: &ResultPoolError<ImageError>| match error {
                    ResultPoolError::Inner(inner) => inner.is_retryable(),
                    ResultPoolError::Pool(_) => true,
                    ResultPoolError::Canceled => false,
                },
                || {
                    let url = url_owned.clone();
                    async move { self.pool.submit(url).await }
                },
            )
            .await
            .map_err(|error| match error {
                ResultPoolError::Inner(inner) => inner,
                ResultPoolError::Pool(pooled) => {
                    ImageError::Transient(format!("image pool: {pooled}"))
                }
                ResultPoolError::Canceled => ImageError::Permanent("image worker gone".to_owned()),
            })?;
        validate_image_bytes(&bytes, self.max_bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
#[path = "../tests/images.rs"]
mod tests;
