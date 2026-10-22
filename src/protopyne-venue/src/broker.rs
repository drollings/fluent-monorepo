//! Broker composition seam: one directory for execution, reads, feeds, and
//! credentials.
//!
//! [`VenueRegistry`](crate::registry::VenueRegistry) remains the execution
//! router. [`BrokerRegistry`] is the read/feed/auth directory: a bundle names
//! the optional capabilities a broker actually implements, so an unsupported
//! surface refuses honestly instead of presenting an empty list.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::account::AccountReader;
use crate::auth::{AuthHandle, CredentialSource};
use crate::error::VenueError;
use crate::feed::MarketDataFeed;
use crate::venue::Venue;

/// Opaque broker identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BrokerId(String);

impl BrokerId {
    /// Build an id from a non-empty spelling.
    pub fn new(id: impl Into<String>) -> Result<Self, BrokerError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(BrokerError::Invalid(
                "broker id must not be blank".to_owned(),
            ));
        }
        Ok(Self(id))
    }

    /// The spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BrokerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which broker surfaces exist. Each flag is a correctness statement:
/// `false` means the route must refuse, never answer empty.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BrokerCapabilities {
    /// Market-data streams.
    pub streams: bool,
    /// Account summaries.
    pub accounts: bool,
    /// Order reads.
    pub orders: bool,
    /// Execution/deal reads.
    pub deals: bool,
    /// Credential refresh.
    pub auth_refresh: bool,
    /// Native broker bars.
    pub native_bars: bool,
}

impl BrokerCapabilities {
    /// Every surface implemented.
    #[must_use]
    pub fn all() -> Self {
        Self {
            streams: true,
            accounts: true,
            orders: true,
            deals: true,
            auth_refresh: true,
            native_bars: true,
        }
    }

    /// No optional surface.
    #[must_use]
    pub fn execution_only() -> Self {
        Self::default()
    }
}

/// Broker directory failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrokerError {
    /// The id or bundle shape is invalid.
    #[error("invalid broker: {0}")]
    Invalid(String),
    /// The id is already registered.
    #[error("broker {0:?} is already registered")]
    Duplicate(String),
    /// No bundle holds the id.
    #[error("unknown broker {0:?}")]
    Unknown(String),
    /// The bundle does not implement the requested surface.
    #[error("broker {broker:?} does not support {surface}")]
    Unsupported {
        /// Broker named by the refusal.
        broker: String,
        /// Surface that is absent.
        surface: &'static str,
    },
}

impl BrokerError {
    /// Map a bundle into a venue error without inventing a second vocabulary.
    #[must_use]
    pub fn into_venue(self) -> VenueError {
        match self {
            Self::Unsupported { broker, surface } => {
                VenueError::Refused(format!("broker {broker:?} does not support {surface}"))
            }
            other => VenueError::Invalid(other.to_string()),
        }
    }
}

/// One broker assembled from orthogonal optional handles.
#[derive(Clone)]
pub struct BrokerBundle {
    /// Stable directory id.
    pub id: BrokerId,
    /// Execution handle (always present).
    pub venue: Arc<dyn Venue>,
    /// Read-only account surfaces.
    pub account: Option<Arc<dyn AccountReader>>,
    /// Market-data feed.
    pub feed: Option<Arc<dyn MarketDataFeed>>,
    /// Credential authority publication.
    pub credentials: Option<Arc<dyn CredentialSource>>,
    /// Declared surfaces.
    pub capabilities: BrokerCapabilities,
}

impl std::fmt::Debug for BrokerBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerBundle")
            .field("id", &self.id)
            .field("capabilities", &self.capabilities)
            .field("account", &self.account.is_some())
            .field("feed", &self.feed.is_some())
            .field("credentials", &self.credentials.is_some())
            .finish_non_exhaustive()
    }
}

impl BrokerBundle {
    /// Build a bundle from its parts, deriving capability flags from the
    /// handles it actually carries.
    #[must_use]
    pub fn new(id: BrokerId, venue: Arc<dyn Venue>) -> Self {
        Self {
            id,
            venue,
            account: None,
            feed: None,
            credentials: None,
            capabilities: BrokerCapabilities::execution_only(),
        }
    }

    /// Attach an account reader and mark its surfaces.
    #[must_use]
    pub fn with_account(mut self, account: Arc<dyn AccountReader>) -> Self {
        let capabilities = account.capabilities();
        self.capabilities.accounts |= capabilities.accounts;
        self.capabilities.orders |= capabilities.orders;
        self.capabilities.deals |= capabilities.deals;
        self.account = Some(account);
        self
    }

    /// Attach a market-data feed.
    #[must_use]
    pub fn with_feed(mut self, feed: Arc<dyn MarketDataFeed>) -> Self {
        self.capabilities.streams = true;
        self.capabilities.native_bars |= feed.capabilities().native_bars;
        self.feed = Some(feed);
        self
    }

    /// Attach a credential publication.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialSource>) -> Self {
        self.capabilities.auth_refresh = true;
        self.credentials = Some(credentials);
        self
    }

    /// Account reader or a named refusal.
    pub fn require_account(&self) -> Result<&Arc<dyn AccountReader>, BrokerError> {
        self.account
            .as_ref()
            .ok_or_else(|| BrokerError::Unsupported {
                broker: self.id.to_string(),
                surface: "account reads",
            })
    }

    /// Feed or a named refusal.
    pub fn require_feed(&self) -> Result<&Arc<dyn MarketDataFeed>, BrokerError> {
        self.feed.as_ref().ok_or_else(|| BrokerError::Unsupported {
            broker: self.id.to_string(),
            surface: "market-data feed",
        })
    }

    /// Credential handle or a named refusal.
    pub fn require_credentials(&self) -> Result<AuthHandle, BrokerError> {
        self.credentials
            .as_ref()
            .map(|source| source.auth_handle())
            .ok_or_else(|| BrokerError::Unsupported {
                broker: self.id.to_string(),
                surface: "credential refresh",
            })
    }
}

/// Read/feed/auth directory. Sorted `BTreeMap` under `RwLock` (M4: stays;
/// `KeyedRegistry` is single-threaded, `ConcurrentRegistry` is hashed `Arc`
/// values — neither fits shared `&self` + sorted `Result` reads).
#[derive(Default)]
pub struct BrokerRegistry {
    entries: RwLock<BTreeMap<BrokerId, BrokerBundle>>,
}

impl BrokerRegistry {
    /// Empty directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one bundle; duplicate ids refuse.
    pub fn register(&self, bundle: BrokerBundle) -> Result<(), BrokerError> {
        let mut entries = common_core::sync::lock_write(&self.entries);
        if entries.contains_key(&bundle.id) {
            return Err(BrokerError::Duplicate(bundle.id.to_string()));
        }
        entries.insert(bundle.id.clone(), bundle);
        Ok(())
    }

    /// Replace or insert one bundle.
    pub fn upsert(&self, bundle: BrokerBundle) {
        common_core::sync::lock_write(&self.entries).insert(bundle.id.clone(), bundle);
    }

    /// One bundle by id.
    pub fn get(&self, id: &BrokerId) -> Result<BrokerBundle, BrokerError> {
        common_core::sync::lock_read(&self.entries)
            .get(id)
            .cloned()
            .ok_or_else(|| BrokerError::Unknown(id.to_string()))
    }

    /// One bundle by spelling.
    pub fn get_str(&self, id: &str) -> Result<BrokerBundle, BrokerError> {
        let id = BrokerId::new(id)?;
        self.get(&id)
    }

    /// Every bundle, in id order.
    #[must_use]
    pub fn bundles(&self) -> Vec<BrokerBundle> {
        common_core::sync::lock_read(&self.entries)
            .values()
            .cloned()
            .collect()
    }

    /// Every bundle id, in order.
    #[must_use]
    pub fn ids(&self) -> Vec<BrokerId> {
        common_core::sync::lock_read(&self.entries)
            .keys()
            .cloned()
            .collect()
    }

    /// Number of registered bundles.
    #[must_use]
    pub fn len(&self) -> usize {
        common_core::sync::lock_read(&self.entries).len()
    }

    /// Whether the directory is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        common_core::sync::lock_read(&self.entries).is_empty()
    }
}

#[cfg(test)]
#[path = "../tests/broker.rs"]
mod tests;
