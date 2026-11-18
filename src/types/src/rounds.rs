//! Bounded config newtypes — validated counts and thresholds.
//!
//! Moved verbatim from `fluent-router`'s `config/rounds.rs`: type
//! definitions + serde + validation only, no router logic. Pattern follows
//! `instance_id::is_valid_instance_name` (fail-fast constructors returning
//! `Result<_, String>`, validating `Deserialize` impls).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Fixed round count for rigor/any loop (VISION: terminate, don't loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Fixed round count for rigor/any loop (VISION: terminate, don't loop).
pub struct BoundedRounds(u8);
impl BoundedRounds {
    pub const MAX: u8 = 8;
    pub fn new(v: u8) -> Result<Self, String> {
        if v == 0 {
            return Err("max_passes must be >=1".into());
        }
        if v > Self::MAX {
            return Err(format!("max_passes {} exceeds MAX={}", v, Self::MAX));
        }
        Ok(Self(v))
    }
    pub fn rounds(self) -> usize {
        self.0 as usize
    }
    pub fn advance(self) -> Option<Self> {
        if self.0 > 1 {
            Some(Self(self.0 - 1))
        } else {
            None
        }
    }
    pub fn get(self) -> u8 {
        self.0
    }
}
impl TryFrom<u8> for BoundedRounds {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, String> {
        Self::new(v)
    }
}
impl Default for BoundedRounds {
    fn default() -> Self {
        Self(2)
    }
}
impl Serialize for BoundedRounds {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.0)
    }
}
impl<'de> Deserialize<'de> for BoundedRounds {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = u8::deserialize(d)?;
        Self::new(v).map_err(serde::de::Error::custom)
    }
}
/// Tag for one validated unit-interval threshold: the consumer name used in
/// validation errors plus that consumer's historical range suffix. All tags
/// share one suffix so every alias reports the same range text.
pub trait ThresholdTag: Send + Sync + 'static {
    /// Consumer name in validation errors (e.g. `"SeverityThreshold"`).
    const NAME: &'static str;
    /// Range suffix in validation errors: the shared unit-interval text.
    const RANGE_SUFFIX: &'static str = UNIT_RANGE_SUFFIX;
}

/// One shared range suffix for every unit-interval threshold error.
pub const UNIT_RANGE_SUFFIX: &str = "out of 0.0..=1.0";

/// One validated 0.0–1.0 threshold, parameterized by consumer tag. Replaces
/// four hand-rolled newtypes; the old names survive as aliases below with
/// identical validation, wire shape, error text, and defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitThreshold<Tag: ThresholdTag>(f64, std::marker::PhantomData<Tag>);

impl<Tag: ThresholdTag> UnitThreshold<Tag> {
    pub fn new(v: f64) -> Result<Self, String> {
        if (0.0..=1.0).contains(&v) {
            Ok(Self(v, std::marker::PhantomData))
        } else {
            Err(format!("{} {v} {}", Tag::NAME, Tag::RANGE_SUFFIX))
        }
    }
    pub fn get(self) -> f64 {
        self.0
    }
}

impl<Tag: ThresholdTag> Serialize for UnitThreshold<Tag> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.0)
    }
}

impl<'de, Tag: ThresholdTag> Deserialize<'de> for UnitThreshold<Tag> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = f64::deserialize(d)?;
        Self::new(v).map_err(serde::de::Error::custom)
    }
}

/// Tag for [`SeverityThreshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`SeverityThreshold`].
pub struct SeverityTag;
impl ThresholdTag for SeverityTag {
    const NAME: &'static str = "SeverityThreshold";
}

/// Tag for [`EscalationConfidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`EscalationConfidence`].
pub struct EscalationTag;
impl ThresholdTag for EscalationTag {
    const NAME: &'static str = "EscalationConfidence";
}

/// Tag for [`TaskRelevanceThreshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`TaskRelevanceThreshold`].
pub struct TaskRelevanceTag;
impl ThresholdTag for TaskRelevanceTag {
    const NAME: &'static str = "TaskRelevanceThreshold";
}

/// Tag for [`ProducerConfidenceThreshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`ProducerConfidenceThreshold`].
pub struct ProducerConfidenceTag;
impl ThresholdTag for ProducerConfidenceTag {
    const NAME: &'static str = "ProducerConfidenceThreshold";
}

/// Severity gate (0.0–1.0); default 0.7. Task-value gated: measured
/// against labeled outcomes before it steers anything.
pub type SeverityThreshold = UnitThreshold<SeverityTag>;
/// Escalation gate (0.0–1.0); default 0.4. Task-value gated: precision
/// and recall on labeled cases, plus a must-not-fire control group.
pub type EscalationConfidence = UnitThreshold<EscalationTag>;
/// Task relevance threshold, axis B (0.0–1.0). Task-value gated.
/// Canonical home for the `fluent-db` policy of the same name, which
/// re-exports this alias.
pub type TaskRelevanceThreshold = UnitThreshold<TaskRelevanceTag>;
/// Producer confidence threshold, axis A (0.0–1.0). Confidence-gated:
/// self-doubt signal, never a correctness verdict on its own.
/// Canonical home for the `fluent-db` policy of the same name, which
/// re-exports this alias.
pub type ProducerConfidenceThreshold = UnitThreshold<ProducerConfidenceTag>;

impl Default for UnitThreshold<SeverityTag> {
    fn default() -> Self {
        Self(0.7, std::marker::PhantomData)
    }
}
impl Default for UnitThreshold<EscalationTag> {
    fn default() -> Self {
        Self(0.4, std::marker::PhantomData)
    }
}

#[cfg(test)]
#[path = "../tests/rounds.rs"]
mod tests;
