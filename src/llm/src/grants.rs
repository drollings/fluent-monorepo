//! Workspace grants for remote embedding.
//!
//! A grant is an HMAC-signed document at `<root>/.zvec-grep/authorization.json`
//! binding one remote target (provider/model/endpoint fingerprint) to one
//! workspace. `has_grant` gates every remote call; without a stored grant
//! the caller needs a one-shot `--allow-remote` or an approved elicitation —
//! otherwise no remote data flows and the error names the grant path. Writes
//! serialize through the artifact-plane directory lock.

use std::path::{Path, PathBuf};

use crate::artifact_lock::{LockOptions, acquire_artifact_cache_lock};


pub const REMOTE_EMBEDDING_CAPABILITY: &str = "remote-embedding";
const GRANT_FILE: &str = "authorization.json";
const DOCUMENT_VERSION: u32 = 1;
const SIGNING_KEY_BYTES: usize = 32;

/// A remote embedding target: one provider/model/endpoint for workspace roots.
#[derive(Debug, Clone)]
pub struct RemoteEmbeddingTarget {
    pub workspace_roots: Vec<PathBuf>,
    pub workspace_fingerprint: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub target_fingerprint: String,
}

impl RemoteEmbeddingTarget {
    #[must_use]
    pub fn new(
        workspace_roots: Vec<PathBuf>,
        provider: &str,
        model: &str,
        endpoint: &str,
    ) -> Self {
        let workspace_fingerprint = common_core::hash::sha256_hex(
            workspace_roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\0")
                .as_bytes(),
        );
        let target_fingerprint = common_core::hash::sha256_hex(
            serde_json::json!({
                "capability": REMOTE_EMBEDDING_CAPABILITY,
                "workspaceFingerprint": workspace_fingerprint,
                "provider": provider,
                "model": model,
                "endpoint": endpoint,
            })
            .to_string()
            .as_bytes(),
        );
        Self {
            workspace_roots,
            workspace_fingerprint,
            provider: provider.to_string(),
            model: model.to_string(),
            endpoint: endpoint.to_string(),
            target_fingerprint,
        }
    }

    #[must_use]
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// Grant store failures. Absent keys/documents are clean negatives (`false`
/// / empty), never errors.
#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("grant store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("grant document is malformed: {0}")]
    MalformedDocument(String),
    #[error("remote embedding target has no workspace roots")]
    NoWorkspaceRoots,
    #[error("remote embedding is not authorized for {reference}: {detail} (grant path: {grant_path})")]
    AuthorizationRequired {
        reference: String,
        detail: String,
        grant_path: PathBuf,
    },
}

/// One persisted workspace grant.
#[derive(Debug, Clone)]
pub struct RemoteEmbeddingWorkspaceGrant {
    pub id: String,
    pub capability: String,
    pub scope: String,
    pub workspace_roots: Vec<PathBuf>,
    pub workspace_fingerprint: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub target_fingerprint: String,
    pub granted_at: u64,
    pub signature: String,
}

/// Signed grant store rooted at a signing-key file.
pub struct RemoteEmbeddingAuthorizationStore {
    signing_key_path: PathBuf,
}

impl RemoteEmbeddingAuthorizationStore {
    #[must_use]
    pub fn with_signing_key_path(path: impl Into<PathBuf>) -> Self {
        Self {
            signing_key_path: path.into(),
        }
    }

    /// Document path for a target: `<first-root>/.zvec-grep/authorization.json`.
    pub fn grant_path(&self, target: &RemoteEmbeddingTarget) -> Result<PathBuf, GrantError> {
        let root = target
            .workspace_roots
            .first()
            .ok_or(GrantError::NoWorkspaceRoots)?;
        Ok(root.join(".zvec-grep").join(GRANT_FILE))
    }

    /// Whether a valid signed grant covers this exact target.
    pub fn has_grant(&self, target: &RemoteEmbeddingTarget) -> Result<bool, GrantError> {
        let Some(key) = self.read_signing_key()? else {
            return Ok(false);
        };
        let path = self.grant_path(target)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(GrantError::Io(error)),
        };
        let document: serde_json::Value = match serde_json::from_str(&text) {
            Ok(document) => document,
            Err(_) => return Ok(false),
        };
        let grants = document
            .get("grants")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(grants.iter().any(|grant| {
            grant
                .get("targetFingerprint")
                .and_then(serde_json::Value::as_str)
                == Some(target.target_fingerprint.as_str())
                && verify_grant(grant, &key)
        }))
    }

    /// Persist a grant for this target (replacing any same-fingerprint
    /// grant), across every workspace root.
    pub async fn grant(
        &self,
        target: &RemoteEmbeddingTarget,
    ) -> Result<RemoteEmbeddingWorkspaceGrant, GrantError> {
        if target.workspace_roots.is_empty() {
            return Err(GrantError::NoWorkspaceRoots);
        }
        let key = self.get_or_create_signing_key()?;
        let unsigned = serde_json::json!({
            "version": DOCUMENT_VERSION,
            "id": new_id(),
            "capability": REMOTE_EMBEDDING_CAPABILITY,
            "scope": "workspace",
            "workspaceRoots": target.workspace_roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
            "workspaceFingerprint": target.workspace_fingerprint,
            "provider": target.provider,
            "model": target.model,
            "endpoint": target.endpoint,
            "targetFingerprint": target.target_fingerprint,
            "grantedAt": now_ms(),
        });
        let signature = hex_encode(&hmac_sha256(&key, unsigned.to_string().as_bytes()));
        let grant = RemoteEmbeddingWorkspaceGrant {
            id: unsigned["id"].as_str().unwrap_or("").to_string(),
            capability: REMOTE_EMBEDDING_CAPABILITY.to_string(),
            scope: "workspace".to_string(),
            workspace_roots: target.workspace_roots.clone(),
            workspace_fingerprint: target.workspace_fingerprint.clone(),
            provider: target.provider.clone(),
            model: target.model.clone(),
            endpoint: target.endpoint.clone(),
            target_fingerprint: target.target_fingerprint.clone(),
            granted_at: unsigned["grantedAt"].as_u64().unwrap_or(0),
            signature,
        };
        for root in &target.workspace_roots {
            let path = root.join(".zvec-grep").join(GRANT_FILE);
            self.with_document_write(&path, |document: &mut Document| {
                document.grants.retain(|candidate| {
                    candidate.target_fingerprint != grant.target_fingerprint
                });
                document.grants.push(grant.clone());
                Ok(())
            })
            .await?;
        }
        Ok(grant)
    }

    /// Remove this target's grant from every root. Returns whether any grant
    /// was removed.
    pub async fn revoke(&self, target: &RemoteEmbeddingTarget) -> Result<bool, GrantError> {
        let mut revoked = false;
        for root in &target.workspace_roots {
            let path = root.join(".zvec-grep").join(GRANT_FILE);
            self.with_document_write(&path, |document: &mut Document| {
                let before = document.grants.len();
                document
                    .grants
                    .retain(|candidate| candidate.target_fingerprint != target.target_fingerprint);
                revoked |= document.grants.len() != before;
                Ok(())
            })
            .await?;
        }
        Ok(revoked)
    }

    /// Remove every grant under one root. Returns the removal count.
    pub async fn revoke_all(&self, root: &Path) -> Result<usize, GrantError> {
        let path = root.join(".zvec-grep").join(GRANT_FILE);
        let mut removed = 0;
        self.with_document_write(&path, |document: &mut Document| {
            removed = document.grants.len();
            document.grants.clear();
            Ok(())
        })
        .await?;
        Ok(removed)
    }

    async fn with_document_write(
        &self,
        path: &Path,
        update: impl FnOnce(&mut Document) -> Result<(), GrantError>,
    ) -> Result<(), GrantError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = path.with_file_name(format!(
            "{}.lock",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        let mut lock = acquire_artifact_cache_lock(
            &lock_path,
            &LockOptions {
                poll_ms: 10,
                stale_ms: 60_000,
                heartbeat_ms: 5_000,
            },
        )
        .await
        .map_err(|error| GrantError::Io(std::io::Error::other(error.to_string())))?;
        let result = Self::read_modify_write(path, update);
        lock.release().ok();
        result
    }

    fn read_modify_write(
        path: &Path,
        update: impl FnOnce(&mut Document) -> Result<(), GrantError>,
    ) -> Result<(), GrantError> {
        let mut document = match std::fs::read_to_string(path) {
            Ok(text) => Document::parse(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Document::empty(),
            Err(error) => return Err(GrantError::Io(error)),
        };
        update(&mut document)?;
        let text = document.render();
        write_owner_only(path, text.as_bytes())?;
        Ok(())
    }

    fn read_signing_key(&self) -> Result<Option<Vec<u8>>, GrantError> {
        match std::fs::read(&self.signing_key_path) {
            Ok(key) => Ok(Some(key)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(GrantError::Io(error)),
        }
    }

    fn get_or_create_signing_key(&self) -> Result<Vec<u8>, GrantError> {
        if let Some(key) = self.read_signing_key()? {
            return Ok(key);
        }
        let mut key = vec![0u8; SIGNING_KEY_BYTES];
        getrandom::getrandom(&mut key)
            .map_err(|error| GrantError::Io(std::io::Error::other(error.to_string())))?;
        if let Some(parent) = self.signing_key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_owner_only(&self.signing_key_path, &key)?;
        Ok(key)
    }
}

struct Document {
    grants: Vec<RemoteEmbeddingWorkspaceGrant>,
}

impl Document {
    fn empty() -> Self {
        Self { grants: Vec::new() }
    }

    fn parse(text: &str) -> Result<Self, GrantError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| GrantError::MalformedDocument(error.to_string()))?;
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(DOCUMENT_VERSION)) {
            return Err(GrantError::MalformedDocument("unsupported version".to_string()));
        }
        let mut grants = Vec::new();
        for grant in value
            .get("grants")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            grants.push(Document::parse_grant(&grant)?);
        }
        Ok(Self { grants })
    }

    fn parse_grant(grant: &serde_json::Value) -> Result<RemoteEmbeddingWorkspaceGrant, GrantError> {
        let field = |name: &str| {
            grant
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| GrantError::MalformedDocument(format!("grant is missing {name}")))
        };
        Ok(RemoteEmbeddingWorkspaceGrant {
            id: field("id")?,
            capability: field("capability")?,
            scope: field("scope")?,
            workspace_roots: grant
                .get("workspaceRoots")
                .and_then(serde_json::Value::as_array)
                .map(|roots| {
                    roots
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            workspace_fingerprint: field("workspaceFingerprint")?,
            provider: field("provider")?,
            model: field("model")?,
            endpoint: field("endpoint")?,
            target_fingerprint: field("targetFingerprint")?,
            granted_at: grant
                .get("grantedAt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            signature: field("signature")?,
        })
    }

    fn render(&self) -> String {
        let grants: Vec<serde_json::Value> = self
            .grants
            .iter()
            .map(|grant| {
                serde_json::json!({
                    "version": DOCUMENT_VERSION,
                    "id": grant.id,
                    "capability": grant.capability,
                    "scope": grant.scope,
                    "workspaceRoots": grant.workspace_roots,
                    "workspaceFingerprint": grant.workspace_fingerprint,
                    "provider": grant.provider,
                    "model": grant.model,
                    "endpoint": grant.endpoint,
                    "targetFingerprint": grant.target_fingerprint,
                    "grantedAt": grant.granted_at,
                    "signature": grant.signature,
                })
            })
            .collect();
        serde_json::json!({ "version": DOCUMENT_VERSION, "grants": grants }).to_string()
    }
}

fn unsigned_payload(grant: &serde_json::Value) -> Option<String> {
    // The exact field set covered at sign time; the signature lives outside it.
    Some(
        serde_json::json!({
            "version": grant.get("version")?,
            "id": grant.get("id")?,
            "capability": grant.get("capability")?,
            "scope": grant.get("scope")?,
            "workspaceRoots": grant.get("workspaceRoots")?,
            "workspaceFingerprint": grant.get("workspaceFingerprint")?,
            "provider": grant.get("provider")?,
            "model": grant.get("model")?,
            "endpoint": grant.get("endpoint")?,
            "targetFingerprint": grant.get("targetFingerprint")?,
            "grantedAt": grant.get("grantedAt")?,
        })
        .to_string(),
    )
}

fn verify_grant(grant: &serde_json::Value, key: &[u8]) -> bool {
    let (Some(payload), Some(signature)) = (
        unsigned_payload(grant),
        grant.get("signature").and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    let expected = hex_encode(&hmac_sha256(key, payload.as_bytes()));
    constant_time_equal(expected.as_bytes(), signature.as_bytes())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    const BLOCK: usize = 64;
    let mut normalized = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = sha2::Sha256::digest(key);
        normalized[..32].copy_from_slice(&digest);
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| -> Vec<u8> { normalized.iter().map(|key| key ^ byte).collect() };
    let inner = pad(0x36);
    let outer = pad(0x5c);
    let mut inner_hasher = sha2::Sha256::new();
    inner_hasher.update(&inner);
    inner_hasher.update(message);
    let inner_digest = inner_hasher.finalize();
    let mut outer_hasher = sha2::Sha256::new();
    outer_hasher.update(&outer);
    outer_hasher.update(inner_digest);
    outer_hasher.finalize().into()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 15) as usize] as char);
    }
    encoded
}

fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        let fallback = now_ms();
        bytes[..8].copy_from_slice(&fallback.to_le_bytes());
        bytes[8..].copy_from_slice(&std::process::id().to_le_bytes());
    }
    hex_encode(&bytes)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn write_owner_only(path: &Path, data: &[u8]) -> Result<(), GrantError> {
    common_core::io::write_atomic(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Authorization gate + elicitation seam.
// ---------------------------------------------------------------------------

/// How remote use was authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAuthorization {
    /// A stored workspace grant covers the target.
    Stored,
    /// One-shot `--allow-remote` approval: valid for this process only,
    /// never persisted.
    OneShot,
}

/// A user-facing authorization question for hosts that support elicitation.
#[derive(Debug, Clone)]
pub struct ElicitationRequest<'a> {
    pub target: &'a RemoteEmbeddingTarget,
    pub grant_path: PathBuf,
    pub capability: &'static str,
}

/// The host's answer. Anything but `Allow` denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationDecision {
    Allow,
    Deny,
}

/// Host elicitation seam: MCP hosts answer through `elicitation/create`;
/// other hosts bridge their own user-question tool. Test stubs decide inline.
pub trait Elicitor: Send + Sync {
    fn elicit(&self, request: &ElicitationRequest) -> ElicitationDecision;
}

/// Gate every remote embedding call through this: stored grant, one-shot
/// flag, or elicited approval — otherwise [`GrantError::AuthorizationRequired`]
/// naming the grant path, before any remote data flows.
pub async fn ensure_remote_embedding_authorized(
    store: &RemoteEmbeddingAuthorizationStore,
    target: &RemoteEmbeddingTarget,
    allow_remote_one_shot: bool,
    elicitor: Option<&dyn Elicitor>,
) -> Result<RemoteAuthorization, GrantError> {
    if store.has_grant(target)? {
        return Ok(RemoteAuthorization::Stored);
    }
    if allow_remote_one_shot {
        return Ok(RemoteAuthorization::OneShot);
    }
    if let Some(elicitor) = elicitor {
        let request = ElicitationRequest {
            target,
            grant_path: store.grant_path(target)?,
            capability: REMOTE_EMBEDDING_CAPABILITY,
        };
        if elicitor.elicit(&request) == ElicitationDecision::Allow {
            store.grant(target).await?;
            return Ok(RemoteAuthorization::Stored);
        }
    }
    Err(GrantError::AuthorizationRequired {
        reference: target.reference(),
        detail: "allow Remote Embedding for this workspace, use local search only, or cancel".to_string(),
        grant_path: store.grant_path(target)?,
    })
}
