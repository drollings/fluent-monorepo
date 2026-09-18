//! Bounded config newtypes — validated counts and thresholds.
//!
//! Moved verbatim from `fluent-router`'s `config/rounds.rs` (M3.3): type
//! definitions + serde + validation only, no router logic. Pattern follows
//! `instance_id::is_valid_instance_name` (fail-fast constructors returning
//! `Result<_, String>`, validating `Deserialize` impls).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Fixed round count for rigor/any loop (VISION: terminate, don't loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeverityThreshold(f64);
impl SeverityThreshold {
    pub fn new(v: f64) -> Result<Self, String> {
        if !(0.0..=1.0).contains(&v) {
            return Err(format!("SeverityThreshold {v} out of 0.0..=1.0"));
        }
        Ok(Self(v))
    }
    pub fn get(self) -> f64 {
        self.0
    }
}
impl Default for SeverityThreshold {
    fn default() -> Self {
        Self(0.7)
    }
}
impl Serialize for SeverityThreshold {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.0)
    }
}
impl<'de> Deserialize<'de> for SeverityThreshold {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = f64::deserialize(d)?;
        Self::new(v).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EscalationConfidence(f64);
impl EscalationConfidence {
    pub fn new(v: f64) -> Result<Self, String> {
        if !(0.0..=1.0).contains(&v) {
            return Err(format!("EscalationConfidence {v} out of 0.0..=1.0"));
        }
        Ok(Self(v))
    }
    pub fn get(self) -> f64 {
        self.0
    }
}
impl Default for EscalationConfidence {
    fn default() -> Self {
        Self(0.4)
    }
}
impl Serialize for EscalationConfidence {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.0)
    }
}
impl<'de> Deserialize<'de> for EscalationConfidence {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = f64::deserialize(d)?;
        Self::new(v).map_err(serde::de::Error::custom)
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
}
