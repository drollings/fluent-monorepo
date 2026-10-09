//! Version-constraint resolution for package imports.
//!
//! A seed like `libBars/11` is a versioned import: capability `libBars` at
//! exactly version 11. [`VersionReq`] also spells `>=` floors (`>=1.2`) and
//! inclusive ranges (`10..12`). [`DependencyResolver::resolve_versioned`]
//! pre-selects one provider per constrained import — match-or-refuse — and
//! then delegates to the unchanged `resolve` paths, so unconstrained seeds
//! behave byte-identically with or without this layer.
//!
//! Refusals reuse the existing error shapes: no matching provider is a
//! `MissingDependency` carrying the request plus the versions that do exist,
//! and several providers at one pinned version are an `AmbiguousDependency`
//! with the full candidate list. There is no silent fallback.
//!
//! [`DependencyResolver::resolve_versioned`]: crate::resolver::DependencyResolver::resolve_versioned

use std::collections::HashMap;
use std::fmt;

use common_core::error::ResolverError;

use crate::target::Target;

/// A parsed dotted-numeric version (`11`, `1.2`, `1.2.3`). Trailing zeros are
/// trimmed at parse time, so `1.2 == 1.2.0` under `Eq`/`Ord`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    parts: Vec<u64>,
}

impl Version {
    /// Parse `major[.minor[.patch...]]` (all numeric, at least one part).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty version".to_string());
        }
        let mut parts = Vec::new();
        for part in raw.split('.') {
            part.parse::<u64>().map_err(|_| format!("invalid version {raw:?}"))?;
            parts.push(part.parse::<u64>().expect("just validated"));
        }
        while parts.len() > 1 && parts.last() == Some(&0) {
            parts.pop();
        }
        Ok(Self { parts })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text: Vec<String> = self.parts.iter().map(u64::to_string).collect();
        f.write_str(&text.join("."))
    }
}

/// A version constraint from an import seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionReq {
    /// No constraint (`Any` never appears from seed parsing; it is the
    /// explicit "unconstrained" value for callers composing filters by hand).
    Any,
    /// Exactly this version (`11`, `1.2.3`).
    Exact(Version),
    /// This version or newer (`>=1.2`).
    AtLeast(Version),
    /// Inclusive on both ends (`10..12`).
    Range {
        /// Lowest accepted version.
        min: Version,
        /// Highest accepted version.
        max: Version,
    },
}

impl VersionReq {
    /// Parse an exact version (`11`), a floor (`>=11`), or an inclusive
    /// range (`10..12`). Whitespace around the operator is tolerated.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if let Some(floor) = raw.strip_prefix(">=") {
            return Ok(Self::AtLeast(Version::parse(floor.trim())?));
        }
        if let Some((lo, hi)) = raw.split_once("..") {
            let min = Version::parse(lo.trim())?;
            let max = Version::parse(hi.trim())?;
            if min > max {
                return Err(format!("inverted version range {raw:?}"));
            }
            return Ok(Self::Range { min, max });
        }
        Ok(Self::Exact(Version::parse(raw)?))
    }

    /// Whether `version` satisfies this constraint.
    #[must_use]
    pub fn matches(&self, version: &Version) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(want) => version == want,
            Self::AtLeast(floor) => version >= floor,
            Self::Range { min, max } => version >= min && version <= max,
        }
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Exact(v) => write!(f, "{v}"),
            Self::AtLeast(v) => write!(f, ">={v}"),
            Self::Range { min, max } => write!(f, "{min}..{max}"),
        }
    }
}

/// Split an import seed into its capability name and optional constraint:
/// `libBars/11` becomes `("libBars", Some("11"))`, a bare `build` becomes
/// `("build", None)`. Only the first `/` splits; anything after it is the
/// raw constraint text for [`VersionReq::parse`].
pub fn split_import(seed: &str) -> (&str, Option<&str>) {
    match seed.split_once('/') {
        Some((name, req)) => (name.trim(), Some(req.trim())),
        None => (seed, None),
    }
}

/// Pick the single provider of `capability` satisfying `req`.
///
/// Providers with no entry in `versions` never match a constraint (running
/// unversioned code against a pinned import would be a silent downgrade).
/// Zero matches refuse with a `MissingDependency` naming the request and the
/// versions that do exist; several exact-version holders refuse with an
/// `AmbiguousDependency` carrying every candidate.
#[allow(clippy::implicit_hasher)]
pub fn select_provider<'t>(
    capability: &str,
    req: &VersionReq,
    candidates: Vec<&'t Target>,
    versions: &HashMap<String, Version>,
) -> Result<&'t Target, ResolverError> {
    let mut matching: Vec<&Target> = candidates
        .into_iter()
        .filter(|t| versions.get(&t.name.to_string()).is_some_and(|v| req.matches(v)))
        .collect();
    matching.sort_by(|a, b| a.name.cmp(&b.name));
    match matching.len() {
        0 => Err(ResolverError::MissingDependency(format!(
            "no provider for '{capability}' at version '{req}'{}",
            available_versions(capability, versions),
        ))),
        1 => Ok(matching[0]),
        _ => Err(ResolverError::AmbiguousDependency {
            name: capability.to_string(),
            candidates: matching.iter().map(|t| t.name.to_string()).collect(),
        }),
    }
}

/// `(found versions: a@9, b@10)` suffix for refusal messages, or `""` when
/// no provider is versioned at all.
#[allow(clippy::implicit_hasher)]
fn available_versions(capability: &str, versions: &HashMap<String, Version>) -> String {
    if versions.is_empty() {
        return format!(" (no versioned provider of '{capability}' known)");
    }
    let mut known: Vec<(&String, &Version)> = versions.iter().collect();
    known.sort_by(|a, b| a.0.cmp(b.0));
    let list: Vec<String> = known.iter().map(|(n, v)| format!("{n}@{v}")).collect();
    format!(" (found versions: {})", list.join(", "))
}

#[cfg(test)]
#[path = "../tests/version.rs"]
mod tests;
