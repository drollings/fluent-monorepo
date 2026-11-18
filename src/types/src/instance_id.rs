use internment::ArcIntern;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Whether a string is a valid instance/snapshot name (`[A-Za-z0-9._-]+`).
pub fn is_valid_instance_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstanceIdError {
    /// The value has no `:` separator.
    #[error("missing ':' separator")]
    MissingSeparator,
    /// The value has more than one `:` separator.
    #[error("extra ':' separator in instance name")]
    ExtraSeparator,
    /// The model part is empty.
    #[error("model id is empty")]
    EmptyModel,
    /// The name part contains characters outside `[A-Za-z0-9._-]`.
    #[error("invalid instance name '{0}' (allowed: [A-Za-z0-9._-])")]
    InvalidName(String),
    /// The name part is empty.
    #[error("instance name is empty")]
    EmptyName,
}

/// Typed opaque pair `(model_key, instance_name)` with grammar validation.
/// Backed by `ArcIntern<str>` per VISION §Shared Content Nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceId {
    model: ArcIntern<str>,
    name: ArcIntern<str>,
}

impl InstanceId {
    pub fn new(
        model: impl Into<ArcIntern<str>>,
        name: impl Into<ArcIntern<str>>,
    ) -> Result<Self, InstanceIdError> {
        let model: ArcIntern<str> = model.into();
        let name: ArcIntern<str> = name.into();
        if model.is_empty() {
            return Err(InstanceIdError::EmptyModel);
        }
        if name.is_empty() {
            return Err(InstanceIdError::EmptyName);
        }
        // Separator check before character validation (same order as
        // `parse`): `a:b` names report `ExtraSeparator`, not `InvalidName`.
        if name.contains(':') {
            return Err(InstanceIdError::ExtraSeparator);
        }
        if !is_valid_instance_name(&name) {
            return Err(InstanceIdError::InvalidName(name.to_string()));
        }
        if model.contains(':') {
            return Err(InstanceIdError::ExtraSeparator);
        }
        Ok(Self { model, name })
    }

    pub fn parse(s: &str) -> Result<Self, InstanceIdError> {
        let (model, name) = s.split_once(':').ok_or(InstanceIdError::MissingSeparator)?;
        if model.is_empty() {
            return Err(InstanceIdError::EmptyModel);
        }
        if name.is_empty() {
            return Err(InstanceIdError::EmptyName);
        }
        if name.contains(':') {
            return Err(InstanceIdError::ExtraSeparator);
        }
        if !is_valid_instance_name(name) {
            return Err(InstanceIdError::InvalidName(name.to_string()));
        }
        Ok(Self {
            model: ArcIntern::from(model),
            name: ArcIntern::from(name),
        })
    }

    pub fn model(&self) -> &ArcIntern<str> {
        &self.model
    }

    pub fn name(&self) -> &ArcIntern<str> {
        &self.name
    }

    pub fn as_pair(&self) -> (&str, &str) {
        (&self.model, &self.name)
    }

    /// Alias set for one public instance id: the bare model id and `latest`
    /// form on the default instance, the group form, and the exact id.
    pub fn aliases(&self, group: &str, is_default: bool) -> Vec<String> {
        let model_key: &str = self.model.as_ref();
        let instance_id = self.to_string();
        let mut aliases = Vec::new();
        if is_default {
            aliases.push(model_key.to_string());
            aliases.push(format!("{model_key}:latest"));
        }
        aliases.push(format!("{model_key}:{group}"));
        aliases.push(instance_id);
        aliases.sort();
        aliases.dedup();
        aliases
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.model, self.name)
    }
}

impl FromStr for InstanceId {
    type Err = InstanceIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for InstanceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for InstanceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[path = "../tests/instance_id.rs"]
mod tests;
