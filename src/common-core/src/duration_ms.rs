//! Serde helper for `std::time::Duration` as whole milliseconds.
//!
//! Wire format stays numeric milliseconds (`2000`); the Rust API is a typed
//! `Duration`. Sub-millisecond precision truncates on write; values that do
//! not fit `u64` milliseconds are rejected on read (they cannot come from a
//! JSON number anyway).

use serde::{Deserialize, Deserializer, Serializer};
use std::time::Duration;

/// Serialize a `Duration` as whole milliseconds.
pub fn serialize<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
    let ms: u64 = duration
        .as_millis()
        .try_into()
        .map_err(serde::ser::Error::custom)?;
    serializer.serialize_u64(ms)
}

/// Deserialize whole milliseconds into a `Duration`.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_millis)
}

/// `Option<Duration>` as nullable milliseconds.
pub mod opt {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(
        duration: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match duration {
            Some(d) => {
                let ms: u64 = u64::try_from(d.as_millis()).map_err(serde::ser::Error::custom)?;
                serializer.serialize_some(&ms)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        Option::<u64>::deserialize(deserializer)
            .map(|opt| opt.map(Duration::from_millis))
    }
}
