//! Bounded config newtypes — validated counts and thresholds.
//!
//! Moved verbatim from `fluent-router`'s `config/rounds.rs` (M3.3): type
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
/// validation errors plus that consumer's historical range suffix (kept
/// per-tag so every alias preserves its exact error text).
pub trait ThresholdTag: Send + Sync + 'static {
    /// Consumer name in validation errors (e.g. `"SeverityThreshold"`).
    const NAME: &'static str;
    /// Range suffix in validation errors (e.g. `"out of 0.0..=1.0"`).
    const RANGE_SUFFIX: &'static str;
}

/// One validated 0.0–1.0 threshold, parameterized by consumer tag. Replaces
/// four hand-rolled newtypes; the old names survive as aliases below with
/// identical validation, wire shape, error text, and defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
/// One validated 0.0–1.0 threshold, parameterized by consumer tag. Replaces
/// four hand-rolled newtypes; the old names survive as aliases below with
/// identical validation, wire shape, error text, and defaults.
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
    const RANGE_SUFFIX: &'static str = "out of 0.0..=1.0";
}

/// Tag for [`EscalationConfidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`EscalationConfidence`].
pub struct EscalationTag;
impl ThresholdTag for EscalationTag {
    const NAME: &'static str = "EscalationConfidence";
    const RANGE_SUFFIX: &'static str = "out of 0.0..=1.0";
}

/// Tag for [`TaskRelevanceThreshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`TaskRelevanceThreshold`].
pub struct TaskRelevanceTag;
impl ThresholdTag for TaskRelevanceTag {
    const NAME: &'static str = "TaskRelevanceThreshold";
    const RANGE_SUFFIX: &'static str = "out of range";
}

/// Tag for [`ProducerConfidenceThreshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Tag for [`ProducerConfidenceThreshold`].
pub struct ProducerConfidenceTag;
impl ThresholdTag for ProducerConfidenceTag {
    const NAME: &'static str = "ProducerConfidenceThreshold";
    const RANGE_SUFFIX: &'static str = "out of range";
}

/// Severity gate (0.0–1.0); default 0.7.
pub type SeverityThreshold = UnitThreshold<SeverityTag>;
/// Escalation gate (0.0–1.0); default 0.4.
pub type EscalationConfidence = UnitThreshold<EscalationTag>;
/// Task relevance threshold, axis B (0.0–1.0). Canonical home for the
/// `fluent-db` policy of the same name, which re-exports this alias.
pub type TaskRelevanceThreshold = UnitThreshold<TaskRelevanceTag>;
/// Producer confidence threshold, axis A (0.0–1.0). Canonical home for the
/// `fluent-db` policy of the same name, which re-exports this alias.
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
mod tests {
    use super::*;

    #[test]
    fn bounded_rounds_rejects_zero_and_over_max() {
        assert!(BoundedRounds::new(0).is_err());
        assert!(BoundedRounds::new(BoundedRounds::MAX).is_ok());
        assert!(BoundedRounds::new(BoundedRounds::MAX + 1).is_err());
        assert_eq!(BoundedRounds::default().get(), 2);
    }

    #[test]
    fn bounded_rounds_advance_terminates() {
        let r = BoundedRounds::new(3).expect("valid");
        assert_eq!(r.rounds(), 3);
        let r = r.advance().expect("advance");
        assert_eq!(r.get(), 2);
        let r = r.advance().expect("advance");
        assert!(r.advance().is_none());
    }

    #[test]
    fn bounded_rounds_deserialize_validates() {
        assert!(serde_json::from_str::<BoundedRounds>("3").is_ok());
        assert!(serde_json::from_str::<BoundedRounds>("0").is_err());
        assert!(serde_json::from_str::<BoundedRounds>("9").is_err());
    }

    #[test]
    fn thresholds_clamp_to_unit_interval() {
        assert!(SeverityThreshold::new(0.7).is_ok());
        assert!(SeverityThreshold::new(-0.1).is_err());
        assert!(SeverityThreshold::new(1.1).is_err());
        assert!(EscalationConfidence::new(0.4).is_ok());
        assert!(EscalationConfidence::new(2.0).is_err());
        assert!((SeverityThreshold::default().get() - 0.7).abs() < f64::EPSILON);
        assert!((EscalationConfidence::default().get() - 0.4).abs() < f64::EPSILON);
        assert!(serde_json::from_str::<SeverityThreshold>("1.5").is_err());
    }

    #[test]
    fn unit_threshold_aliases_share_range_validation() {
        // Inclusive bounds accept, out-of-range (incl. NaN) rejects — all four.
        for v in [0.0, 1.0] {
            assert!(SeverityThreshold::new(v).is_ok());
            assert!(EscalationConfidence::new(v).is_ok());
            assert!(TaskRelevanceThreshold::new(v).is_ok());
            assert!(ProducerConfidenceThreshold::new(v).is_ok());
        }
        for v in [-0.1, 1.1, f64::NAN] {
            assert!(SeverityThreshold::new(v).is_err());
            assert!(EscalationConfidence::new(v).is_err());
            assert!(TaskRelevanceThreshold::new(v).is_err());
            assert!(ProducerConfidenceThreshold::new(v).is_err());
        }
        assert!((TaskRelevanceThreshold::new(0.15).unwrap().get() - 0.15).abs() < f64::EPSILON);
    }

    #[test]
    fn unit_threshold_error_text_preserved_per_tag() {
        assert_eq!(
            SeverityThreshold::new(2.0).unwrap_err(),
            "SeverityThreshold 2 out of 0.0..=1.0"
        );
        assert_eq!(
            EscalationConfidence::new(2.0).unwrap_err(),
            "EscalationConfidence 2 out of 0.0..=1.0"
        );
        assert_eq!(
            TaskRelevanceThreshold::new(2.0).unwrap_err(),
            "TaskRelevanceThreshold 2 out of range"
        );
        assert_eq!(
            ProducerConfidenceThreshold::new(2.0).unwrap_err(),
            "ProducerConfidenceThreshold 2 out of range"
        );
    }

    #[test]
    fn unit_threshold_serde_parity_all_aliases() {
        for payload in ["0.5", "0", "1"] {
            assert!(serde_json::from_str::<SeverityThreshold>(payload).is_ok());
            assert!(serde_json::from_str::<EscalationConfidence>(payload).is_ok());
            assert!(serde_json::from_str::<TaskRelevanceThreshold>(payload).is_ok());
            assert!(serde_json::from_str::<ProducerConfidenceThreshold>(payload).is_ok());
        }
        for payload in ["1.5", "-0.1"] {
            assert!(serde_json::from_str::<SeverityThreshold>(payload).is_err());
            assert!(serde_json::from_str::<EscalationConfidence>(payload).is_err());
            assert!(serde_json::from_str::<TaskRelevanceThreshold>(payload).is_err());
            assert!(serde_json::from_str::<ProducerConfidenceThreshold>(payload).is_err());
        }
        // Wire shape stays a bare number.
        assert_eq!(
            serde_json::to_string(&SeverityThreshold::new(0.5).unwrap()).unwrap(),
            "0.5"
        );
        // Defaults unchanged.
        assert!((SeverityThreshold::default().get() - 0.7).abs() < f64::EPSILON);
        assert!((EscalationConfidence::default().get() - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn unit_threshold_tags_stay_distinct_types() {
        use std::any::TypeId;
        assert_ne!(
            TypeId::of::<SeverityThreshold>(),
            TypeId::of::<EscalationConfidence>()
        );
        assert_ne!(
            TypeId::of::<TaskRelevanceThreshold>(),
            TypeId::of::<ProducerConfidenceThreshold>()
        );
        assert_ne!(
            TypeId::of::<SeverityThreshold>(),
            TypeId::of::<TaskRelevanceThreshold>()
        );
    }
}