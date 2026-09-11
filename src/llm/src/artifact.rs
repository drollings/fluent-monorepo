//! Pinned model-artifact plane: cache-first, integrity-checked snapshot
//! resolution with Hugging Face → ModelScope fallback.
//!
//! Resolution never trusts a partial download: every artifact is streamed to
//! a unique `.part-*` file, hashed on the fly, size-capped, and atomically
//! renamed only after its sha256 matches the catalog pin. A fingerprint
//! completion marker (`.guidance-artifacts-<fp>.complete`) short-circuits
//! re-hashing; a directory lock serializes concurrent writers for one
//! snapshot. Network access goes through the injected [`ArtifactFetcher`]
//! seam, so hermetic tests serve stub mirrors.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;

use crate::artifact_lock::{LockError, LockOptions, acquire_artifact_cache_lock};


/// Where an artifact snapshot may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    HuggingFace,
    ModelScope,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HuggingFace => write!(formatter, "huggingface"),
            Self::ModelScope => write!(formatter, "modelscope"),
        }
    }
}

/// Hook receiving the verified-missing artifact plan before downloading.
pub type DownloadPlanHook<'a> = &'a (dyn Fn(&[ModelArtifact]) + Send + Sync);
/// Hook receiving per-artifact byte progress.
pub type ArtifactProgressHook<'a> = &'a (dyn Fn(ModelArtifactDownloadProgress) + Send + Sync);
/// Hook receiving the mirror-fallback warning.
pub type FallbackHook<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// One download source: mirror coordinates plus the local cache layout.
#[derive(Debug, Clone)]
pub struct ModelArtifactSource {
    pub kind: SourceKind,
    pub repo: String,
    pub revision: String,
    pub cache_directory: PathBuf,
    /// Maps a remote artifact path to a path relative to `cache_directory`.
    pub local_paths: HashMap<String, String>,
}

/// One pinned file inside a snapshot.
#[derive(Debug, Clone)]
pub struct ModelArtifact {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Per-artifact download progress event.
#[derive(Debug, Clone)]
pub struct ModelArtifactDownloadProgress {
    pub model: String,
    pub source: SourceKind,
    pub artifact: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// A resolved, integrity-checked local snapshot.
#[derive(Debug, Clone)]
pub struct ResolvedModelArtifacts {
    pub source: ModelArtifactSource,
    pub directory: String,
    /// Absolute local paths keyed by remote artifact path.
    pub paths: HashMap<String, String>,
}

/// Download failure taxonomy. Only `fallback_allowed` kinds try the next
/// mirror; anything else surfaces immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Http,
    Network,
    Timeout,
    Integrity,
    Filesystem,
    Aborted,
    Callback,
    InvalidInput,
}

/// A single-source download failure.
#[derive(Debug, Clone)]
pub struct ArtifactDownloadError {    pub message: String,
    pub kind: FailureKind,
    pub source: Option<SourceKind>,
    pub artifact: Option<String>,
    pub status: Option<u16>,
}

impl ArtifactDownloadError {
    #[must_use]
    pub fn fallback_allowed(&self) -> bool {
        match self.kind {
            FailureKind::Network | FailureKind::Timeout | FailureKind::Integrity => true,
            FailureKind::Http => self.status.is_some_and(|status| {
                status == 403
                    || status == 404
                    || status == 408
                    || status == 429
                    || (500..=599).contains(&status)
            }),
            FailureKind::Filesystem
            | FailureKind::Aborted
            | FailureKind::Callback
            | FailureKind::InvalidInput => false,
        }
    }
}

impl fmt::Display for ArtifactDownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ArtifactDownloadError {}

/// Resolution outcome: either one source's failure, or both sources'
/// failures preserved together.
#[derive(Debug)]
pub enum ArtifactError {
    Download(ArtifactDownloadError),
    Resolution {
        model: String,
        primary: Box<ArtifactDownloadError>,
        fallback: Box<ArtifactDownloadError>,
    },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Download(error) => write!(formatter, "{error}"),
            Self::Resolution {
                model,
                primary,
                fallback,
            } => write!(
                formatter,
                "Unable to download artifacts for {model} from Hugging Face or ModelScope: {primary}; {fallback}"
            ),
        }
    }
}

impl std::error::Error for ArtifactError {}

// ---------------------------------------------------------------------------
// Fetch seam (network boundary; stub mirrors in tests).
// ---------------------------------------------------------------------------

/// One fetched response: status line plus a streaming body.
pub struct ArtifactFetchResponse {
    pub status: u16,
    pub status_text: String,
    pub retry_after: Option<String>,
    pub body: Box<dyn ArtifactBody>,
}

/// Fetch-level failures (mapped to [`FailureKind`] by the downloader).
#[derive(Debug)]
pub enum ArtifactFetchError {
    Network(String),
    Cancelled,
}

/// Streaming response body: each call yields the next chunk.
#[async_trait]
pub trait ArtifactBody: Send {
    async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, ArtifactFetchError>>;
}

/// Injectable HTTP boundary for artifact downloads.
#[async_trait]
pub trait ArtifactFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<ArtifactFetchResponse, ArtifactFetchError>;
}

/// Production fetcher over `reqwest`, adapting `chunk()` to the body seam.
pub struct ReqwestArtifactFetcher {
    client: reqwest::Client,
}

impl ReqwestArtifactFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestArtifactFetcher {
    fn default() -> Self {
        Self::new()
    }
}

struct ReqwestArtifactBody {
    response: reqwest::Response,
}

#[async_trait]
impl ArtifactBody for ReqwestArtifactBody {
    async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, ArtifactFetchError>> {
        match self.response.chunk().await {
            Ok(Some(bytes)) => Some(Ok(bytes.to_vec())),
            Ok(None) => None,
            Err(error) => Some(Err(ArtifactFetchError::Network(error.to_string()))),
        }
    }
}

#[async_trait]
impl ArtifactFetcher for ReqwestArtifactFetcher {
    async fn fetch(&self, url: &str) -> Result<ArtifactFetchResponse, ArtifactFetchError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| ArtifactFetchError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_string();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Ok(ArtifactFetchResponse {
            status,
            status_text,
            retry_after,
            body: Box::new(ReqwestArtifactBody { response }),
        })
    }
}

// ---------------------------------------------------------------------------
// Options.
// ---------------------------------------------------------------------------

/// Network deadlines. The read-idle deadline resets after every chunk;
/// there is deliberately no total deadline.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactTimeouts {
    pub response_header_ms: u64,
    pub read_idle_ms: u64,
}

impl Default for ArtifactTimeouts {
    fn default() -> Self {
        Self {
            response_header_ms: 10_000,
            read_idle_ms: 30_000,
        }
    }
}

pub struct ResolveArtifactsOptions<'a> {
    pub model: &'a str,
    pub sources: Vec<ModelArtifactSource>,
    pub artifacts: Vec<ModelArtifact>,
    pub on_progress: Option<ArtifactProgressHook<'a>>,
    pub on_download_plan: Option<DownloadPlanHook<'a>>,
    pub on_fallback: Option<FallbackHook<'a>>,
    pub fetcher: &'a dyn ArtifactFetcher,
    pub timeouts: ArtifactTimeouts,
    pub lock: LockOptions,
}

struct NormalizedOptions<'a> {
    model: String,
    sources: Vec<ModelArtifactSource>,
    artifacts: Vec<ModelArtifact>,
    on_progress: Option<ArtifactProgressHook<'a>>,
    on_download_plan: Option<DownloadPlanHook<'a>>,
    on_fallback: Option<FallbackHook<'a>>,
    fetcher: &'a dyn ArtifactFetcher,
    response_header_ms: u64,
    read_idle_ms: u64,
    lock: LockOptions,
}

// ---------------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------------

const MANIFEST_VERSION: u32 = 1;

/// Resolve a complete, integrity-checked local artifact snapshot.
///
/// Every source's cache is checked before any network request, so a complete
/// mirror snapshot never triggers a doomed primary download. Sources always
/// run Hugging Face first regardless of input order.
pub async fn resolve_model_artifacts(
    options: &ResolveArtifactsOptions<'_>,
) -> Result<ResolvedModelArtifacts, ArtifactError> {
    let normalized = normalize_options(options)?;
    let manifests: HashMap<SourceKind, SourceManifest> = normalized
        .sources
        .iter()
        .map(|source| -> Result<_, ArtifactError> {
            Ok((
                source.kind,
                create_source_manifest(&normalized.model, source, &normalized.artifacts)?,
            ))
        })
        .collect::<Result<_, _>>()?;

    for source in &normalized.sources {
        let manifest = &manifests[&source.kind];
        if validate_snapshot(&normalized.model, source, &normalized.artifacts, manifest)? {
            write_complete_marker_best_effort(source, &normalized.artifacts, manifest);
            return Ok(resolved_result(source, manifest));
        }
    }

    let mut primary_error: Option<ArtifactDownloadError> = None;
    let mut warned = false;
    for (index, source) in normalized.sources.iter().enumerate() {
        let manifest = &manifests[&source.kind];
        match download_source_snapshot(&normalized, source, manifest).await {
            Ok(()) => return Ok(resolved_result(source, manifest)),
            Err(error) => {
                if index == 0 {
                    primary_error = Some(error.clone());
                }
                let Some(_next) = normalized.sources.get(index + 1) else {
                    return Err(final_error(
                        &normalized.model,
                        primary_error,
                        index,
                        error,
                    ));
                };
                if !error.fallback_allowed() {
                    return Err(final_error(
                        &normalized.model,
                        primary_error,
                        index,
                        error,
                    ));
                }
                if !warned {
                    warned = true;
                    invoke_fallback_callback(
                        normalized.on_fallback,
                        &format!(
                            "Hugging Face download failed for {}; falling back to ModelScope.",
                            normalized.model
                        ),
                        source,
                    )?;
                }
                if index > 0 {
                    let primary = primary_error
                        .take()
                        .expect("primary error recorded at index 0");
                    return Err(ArtifactError::Resolution {
                        model: normalized.model.clone(),
                        primary: Box::new(primary),
                        fallback: Box::new(error),
                    });
                }
            }
        }
    }

    Err(ArtifactDownloadError {
        message: format!("No artifact source is available for {}", normalized.model),
        kind: FailureKind::InvalidInput,
        source: None,
        artifact: None,
        status: None,
    }
    .into())
}

fn final_error(
    model: &str,
    primary_error: Option<ArtifactDownloadError>,
    index: usize,
    error: ArtifactDownloadError,
) -> ArtifactError {
    if index > 0 {
        if let Some(primary) = primary_error {
            return ArtifactError::Resolution {
                model: model.to_string(),
                primary: Box::new(primary),
                fallback: Box::new(error),
            };
        }
    }
    ArtifactError::Download(error)
}

// ---------------------------------------------------------------------------
// URLs.
// ---------------------------------------------------------------------------

/// Canonical download URL for one artifact from one mirror.
#[must_use]
pub fn model_artifact_url(
    source: &ModelArtifactSource,
    artifact_path: &str,
) -> String {
    let base = match source.kind {
        SourceKind::HuggingFace => "https://huggingface.co",
        SourceKind::ModelScope => "https://modelscope.cn/models",
    };
    format!(
        "{}/{}/resolve/{}/{}",
        base,
        encode_path(&source.repo),
        percent_encode_segment(&source.revision),
        encode_path(artifact_path)
    )
}

fn encode_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode_segment(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 15) as usize] as char);
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Validation (before any filesystem or network access).
// ---------------------------------------------------------------------------

fn normalize_options<'a>(
    options: &ResolveArtifactsOptions<'a>,
) -> Result<NormalizedOptions<'a>, ArtifactError> {
    if options.model.trim().is_empty() {
        return Err(invalid_input("Model reference must not be empty"));
    }
    if options.artifacts.is_empty() {
        return Err(invalid_input(format!(
            "At least one artifact is required for {}",
            options.model
        )));
    }
    let mut kinds = std::collections::HashSet::new();
    for source in &options.sources {
        if !kinds.insert(source.kind) {
            return Err(invalid_input(format!(
                "Duplicate artifact source '{source}'",
                source = source.kind
            )));
        }
        validate_source(source)?;
    }
    if kinds.is_empty() {
        return Err(invalid_input(format!(
            "At least one artifact source is required for {}",
            options.model
        )));
    }
    let mut paths = std::collections::HashSet::new();
    for artifact in &options.artifacts {
        validate_relative_path(&artifact.path, "artifact path")?;
        if !paths.insert(artifact.path.clone()) {
            return Err(invalid_input(format!(
                "Duplicate artifact path '{}'",
                artifact.path
            )));
        }
        if artifact.size > i64::MAX as u64 {
            return Err(invalid_input(format!(
                "Invalid size for artifact '{}'",
                artifact.path
            )));
        }
        if !is_sha256_hex(&artifact.sha256) {
            return Err(invalid_input(format!(
                "Invalid SHA-256 for artifact '{}'",
                artifact.path
            )));
        }
    }
    for source in &options.sources {
        for key in source.local_paths.keys() {
            if !paths.contains(key) {
                return Err(invalid_input(format!(
                    "{} local path mapping references unknown artifact '{key}'",
                    source.kind
                )));
            }
        }
    }
    let timeouts = options.timeouts;
    for (name, value) in [
        ("responseHeaderMs", timeouts.response_header_ms),
        ("readIdleMs", timeouts.read_idle_ms),
        ("lockPollMs", options.lock.poll_ms),
        ("lockStaleMs", options.lock.stale_ms),
        ("lockHeartbeatMs", options.lock.heartbeat_ms),
    ] {
        if value == 0 {
            return Err(invalid_input(format!("{name} must be a positive number")));
        }
    }
    if options.lock.heartbeat_ms >= options.lock.stale_ms {
        return Err(invalid_input(
            "lock heartbeat must be shorter than its stale timeout",
        ));
    }
    let mut sources = options.sources.clone();
    sources.sort_by_key(|source| match source.kind {
        SourceKind::HuggingFace => 0,
        SourceKind::ModelScope => 1,
    });
    Ok(NormalizedOptions {
        model: options.model.to_string(),
        sources,
        artifacts: options
            .artifacts
            .iter()
            .map(|artifact| ModelArtifact {
                sha256: artifact.sha256.to_lowercase(),
                ..artifact.clone()
            })
            .collect(),
        on_progress: options.on_progress,
        on_download_plan: options.on_download_plan,
        on_fallback: options.on_fallback,
        fetcher: options.fetcher,
        response_header_ms: timeouts.response_header_ms,
        read_idle_ms: timeouts.read_idle_ms,
        lock: options.lock,
    })
}

fn validate_source(source: &ModelArtifactSource) -> Result<(), ArtifactError> {
    if source.repo.trim().is_empty() || source.revision.trim().is_empty() {
        return Err(invalid_input(format!(
            "{} repo and revision must not be empty",
            source.kind
        )));
    }
    validate_relative_path(&source.repo, "repository")?;
    if source.revision.contains('\0')
        || source.revision == "."
        || source.revision == ".."
    {
        return Err(invalid_input(format!(
            "Invalid {} revision",
            source.kind
        )));
    }
    if source.cache_directory.to_string_lossy().trim().is_empty()
        || source
            .cache_directory
            .to_string_lossy()
            .contains('\0')
    {
        return Err(invalid_input(format!(
            "{} cache directory must not be empty",
            source.kind
        )));
    }
    let mut local_paths = std::collections::HashSet::new();
    for (artifact, local) in &source.local_paths {
        validate_relative_path(artifact, "artifact mapping key")?;
        validate_relative_path(local, "local artifact path")?;
        if !local_paths.insert(local.clone()) {
            return Err(invalid_input(format!(
                "Multiple {} artifacts map to '{local}'",
                source.kind
            )));
        }
    }
    Ok(())
}

fn validate_relative_path(path: &str, label: &str) -> Result<(), ArtifactError> {
    let normalized = path.replace('\\', "/");
    let bad = normalized.is_empty()
        || normalized.contains('\0')
        || Path::new(path).is_absolute()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if bad {
        return Err(invalid_input(format!("Invalid {label} '{path}'")));
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn invalid_input(message: impl Into<String>) -> ArtifactError {
    ArtifactError::Download(ArtifactDownloadError {
        message: message.into(),
        kind: FailureKind::InvalidInput,
        source: None,
        artifact: None,
        status: None,
    })
}

// ---------------------------------------------------------------------------
// Manifests, markers, fingerprints.
// ---------------------------------------------------------------------------

struct SourceManifest {
    fingerprint: String,
    marker_path: PathBuf,
    lock_path: PathBuf,
    local_paths: HashMap<String, String>,
}

fn create_source_manifest(
    model: &str,
    source: &ModelArtifactSource,
    artifacts: &[ModelArtifact],
) -> Result<SourceManifest, ArtifactError> {
    let mut local_paths = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for artifact in artifacts {
        let local = source
            .local_paths
            .get(&artifact.path)
            .cloned()
            .unwrap_or_else(|| artifact.path.clone());
        validate_relative_path(&local, "local artifact path")?;
        if !seen.insert(local.clone()) {
            return Err(invalid_input(format!(
                "Multiple {} artifacts map to '{local}'",
                source.kind
            )));
        }
        local_paths.insert(artifact.path.clone(), local);
    }
    let fingerprint = snapshot_fingerprint(model, source, artifacts);
    Ok(SourceManifest {
        fingerprint: fingerprint.clone(),
        marker_path: snapshot_marker_path(source, &fingerprint),
        lock_path: snapshot_lock_path(source, &fingerprint),
        local_paths,
    })
}

/// Fingerprint identifying one model snapshot recipe. Changing any pin
/// (repo, revision, artifact bytes, layout) changes the fingerprint, so
/// stale caches never validate against a new recipe.
#[must_use]
pub fn snapshot_fingerprint(
    model: &str,
    source: &ModelArtifactSource,
    artifacts: &[ModelArtifact],
) -> String {
    let mut local_paths: Vec<(&str, &str)> = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let local = source
            .local_paths
            .get(&artifact.path)
            .map_or(artifact.path.as_str(), String::as_str);
        local_paths.push((artifact.path.as_str(), local));
    }
    let serialized = serde_json::json!({
        "version": MANIFEST_VERSION,
        "model": model,
        "source": {
            "kind": source.kind.to_string(),
            "repo": source.repo,
            "revision": source.revision,
        },
        "artifacts": artifacts.iter().zip(local_paths.iter()).map(|(artifact, (path, local))| {
            serde_json::json!({
                "path": path,
                "localPath": local,
                "size": artifact.size,
                "sha256": artifact.sha256.to_lowercase(),
            })
        }).collect::<Vec<_>>(),
    });
    let digest = common_core::hash::sha256_hex(serialized.to_string().as_bytes());
    digest[..24].to_string()
}

/// Completion-marker path for one snapshot recipe.
#[must_use]
pub fn snapshot_marker_path(source: &ModelArtifactSource, fingerprint: &str) -> PathBuf {
    source
        .cache_directory
        .join(format!(".guidance-artifacts-{fingerprint}.complete"))
}

/// Lock path serializing writers of one snapshot recipe.
#[must_use]
pub fn snapshot_lock_path(source: &ModelArtifactSource, fingerprint: &str) -> PathBuf {
    source
        .cache_directory
        .join(format!(".guidance-artifacts-{fingerprint}.lock"))
}

fn resolved_result(
    source: &ModelArtifactSource,
    manifest: &SourceManifest,
) -> ResolvedModelArtifacts {
    let root = absolute_dir(&source.cache_directory);
    ResolvedModelArtifacts {
        source: source.clone(),
        directory: root.clone(),
        paths: manifest
            .local_paths
            .iter()
            .map(|(artifact, local)| {
                (
                    artifact.clone(),
                    safe_local_path(&source.cache_directory, local)
                        .unwrap_or_else(|_| PathBuf::from(local)),
                )
            })
            .map(|(artifact, path)| (artifact, path.to_string_lossy().into_owned()))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Cache validation.
// ---------------------------------------------------------------------------

fn validate_snapshot(
    model: &str,
    source: &ModelArtifactSource,
    artifacts: &[ModelArtifact],
    manifest: &SourceManifest,
) -> Result<bool, ArtifactError> {
    if has_valid_complete_marker(source, artifacts, manifest)? {
        return Ok(true);
    }
    for artifact in artifacts {
        if !validate_artifact(model, source, artifact, manifest)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn has_valid_complete_marker(
    source: &ModelArtifactSource,
    artifacts: &[ModelArtifact],
    manifest: &SourceManifest,
) -> Result<bool, ArtifactError> {
    let text = match std::fs::read_to_string(&manifest.marker_path) {
        Ok(text) => text,
        Err(error) if is_missing(&error) => return Ok(false),
        Err(error) => {
            return Err(filesystem_error(
                format!("Unable to inspect {} artifact completion marker", source.kind),
                source,
                None,
                error,
            )
            .into());
        }
    };
    let marker: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    if marker.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(MANIFEST_VERSION))
        || marker.get("fingerprint").and_then(serde_json::Value::as_str) != Some(&manifest.fingerprint)
        || !marker.get("files").is_some_and(serde_json::Value::is_object)
    {
        return Ok(false);
    }
    let files = &marker["files"];
    for artifact in artifacts {
        let recorded = &files[&artifact.path];
        if !recorded.is_object() {
            return Ok(false);
        }
        let local = safe_local_path(
            &source.cache_directory,
            &manifest.local_paths[&artifact.path],
        )?;
        let stats = match std::fs::metadata(&local) {
            Ok(stats) => stats,
            Err(error) if is_missing(&error) => return Ok(false),
            Err(error) => {
                return Err(filesystem_error(
                    format!(
                        "Unable to inspect {} artifact completion marker",
                        source.kind
                    ),
                    source,
                    None,
                    error,
                )
                .into());
            }
        };
        if !stats.is_file()
            || stats.len() != artifact.size
            || stats.len() != recorded["size"].as_u64().unwrap_or(u64::MAX)
            || mtime_ms(&stats) != recorded["mtimeMs"].as_u64().unwrap_or(u64::MAX)
            || ctime_ms(&stats) != recorded["ctimeMs"].as_u64().unwrap_or(u64::MAX)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_artifact(
    model: &str,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    manifest: &SourceManifest,
) -> Result<bool, ArtifactError> {
    let local = safe_local_path(
        &source.cache_directory,
        &manifest.local_paths[&artifact.path],
    )?;
    let stats = match std::fs::metadata(&local) {
        Ok(stats) => stats,
        Err(error) if is_missing(&error) => return Ok(false),
        Err(error) => {
            return Err(filesystem_error(
                format!(
                    "Unable to validate cached artifact '{}' for {model}",
                    artifact.path
                ),
                source,
                Some(&artifact.path),
                error,
            )
            .into());
        }
    };
    if !stats.is_file() || stats.len() != artifact.size {
        return Ok(false);
    }
    let digest = hash_file(&local).map_err(|error| {
        filesystem_error(
            format!(
                "Unable to validate cached artifact '{}' for {model}",
                artifact.path
            ),
            source,
            Some(&artifact.path),
            error,
        )
    })?;
    Ok(digest == artifact.sha256.to_lowercase())
}

fn hash_file(path: &Path) -> io::Result<String> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        use io::Read as _;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Download.
// ---------------------------------------------------------------------------

async fn download_source_snapshot(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    manifest: &SourceManifest,
) -> Result<(), ArtifactDownloadError> {
    if let Err(error) = std::fs::create_dir_all(&source.cache_directory) {
        return Err(filesystem_error(
            format!("Unable to prepare {} model cache", source.kind),
            source,
            None,
            error,
        ));
    }
    let mut lock = acquire_artifact_cache_lock(&manifest.lock_path, &options.lock)
        .await
        .map_err(|error| match error {
            LockError::Io(error) => filesystem_error(
                format!("Unable to prepare {} model cache", source.kind),
                source,
                None,
                error,
            ),
            LockError::NotOwned => filesystem_error(
                format!("Unable to prepare {} model cache", source.kind),
                source,
                None,
                io::Error::other("snapshot lock lost"),
            ),
        })?;
    // Another writer may have completed the snapshot while we waited.
    if has_valid_complete_marker(source, &options.artifacts, manifest)
        .map_err(flatten_artifact_error)?
    {
        lock.release().ok();
        return Ok(());
    }
    let mut missing = Vec::new();
    for artifact in &options.artifacts {
        if !validate_artifact(&options.model, source, artifact, manifest)
            .map_err(flatten_artifact_error)?
        {
            missing.push(artifact.clone());
        }
    }
    invoke_download_plan_callback(options, &missing)?;
    let result = download_missing(options, source, manifest, &missing, &mut lock).await;
    lock.release().ok();
    result?;
    write_complete_marker(source, manifest).map_err(flatten_artifact_error)?;
    Ok(())
}

async fn download_missing(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    manifest: &SourceManifest,
    missing: &[ModelArtifact],
    lock: &mut crate::artifact_lock::ArtifactCacheLock,
) -> Result<(), ArtifactDownloadError> {
    for artifact in missing {
        download_artifact(options, source, artifact, manifest, lock).await?;
    }
    lock.assert_owned().map_err(|error| match error {
        LockError::Io(error) => filesystem_error(
            format!("Unable to prepare {} model cache", source.kind),
            source,
            None,
            error,
        ),
        LockError::NotOwned => filesystem_error(
            format!("Unable to prepare {} model cache", source.kind),
            source,
            None,
            io::Error::other("snapshot lock lost"),
        ),
    })
}

fn flatten_artifact_error(error: ArtifactError) -> ArtifactDownloadError {
    match error {
        ArtifactError::Download(download) => download,
        ArtifactError::Resolution { fallback, .. } => *fallback,
    }
}

async fn download_artifact(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    manifest: &SourceManifest,
    lock: &mut crate::artifact_lock::ArtifactCacheLock,
) -> Result<(), ArtifactDownloadError> {
    let destination = safe_local_path(
        &source.cache_directory,
        &manifest.local_paths[&artifact.path],
    )
    .map_err(flatten_artifact_error)?;
    let partial = partial_path(&destination);
    let url = model_artifact_url(source, &artifact.path);
    let result = download_artifact_inner(options, source, artifact, manifest, &destination, &partial, &url, lock).await;
    // Partial files never leak beside the destination.
    let _ = std::fs::remove_file(&partial);
    result
}

#[allow(clippy::too_many_arguments)]
async fn download_artifact_inner(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    manifest: &SourceManifest,
    destination: &Path,
    partial: &Path,
    url: &str,
    lock: &mut crate::artifact_lock::ArtifactCacheLock,
) -> Result<(), ArtifactDownloadError> {
    if let Some(parent) = destination.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return Err(filesystem_error(
                format!("Unable to prepare local artifact '{}'", artifact.path),
                source,
                Some(&artifact.path),
                error,
            ));
        }
    }
    let original = inspect_file_identity(destination).map_err(|error| {
        filesystem_error(
            format!("Unable to prepare local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        )
    })?;
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(partial)
            .map(|_| ())
            .map_err(|error| {
                filesystem_error(
                    format!("Unable to prepare local artifact '{}'", artifact.path),
                    source,
                    Some(&artifact.path),
                    error,
                )
            })?;
    }

    invoke_progress_callback(options, source, artifact, 0)?;
    let mut response = with_header_deadline(options, source, artifact, url).await?;

    if !(200..300).contains(&response.status) {
        return Err(ArtifactDownloadError {
            message: format!(
                "HTTP {} {} while downloading model from {}",
                response.status, response.status_text, source.kind
            ),
            kind: FailureKind::Http,
            source: Some(source.kind),
            artifact: Some(artifact.path.clone()),
            status: Some(response.status),
        });
    }

    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut downloaded: u64 = 0;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(partial)
        .map_err(|error| {
            filesystem_error(
                format!("Unable to write local artifact '{}'", artifact.path),
                source,
                Some(&artifact.path),
                error,
            )
        })?;
    loop {
        let chunk = with_read_idle_deadline(options, source, artifact, response.body.as_mut()).await?;
        let Some(bytes) = chunk else { break };
        {
            use io::Write as _;
            if let Err(error) = file.write_all(&bytes) {
                return Err(filesystem_error(
                    format!("Unable to write local artifact '{}'", artifact.path),
                    source,
                    Some(&artifact.path),
                    error,
                ));
            }
        }
        hasher.update(&bytes);
        downloaded += bytes.len() as u64;
        if downloaded > artifact.size {
            return Err(ArtifactDownloadError {
                message: format!(
                    "Integrity check failed for '{}' from {}: response exceeded the expected {} bytes",
                    artifact.path, source.kind, artifact.size
                ),
                kind: FailureKind::Integrity,
                source: Some(source.kind),
                artifact: Some(artifact.path.clone()),
                status: None,
            });
        }
        lock.touch().map_err(|error| match error {
            LockError::Io(error) => filesystem_error(
                format!("Unable to write local artifact '{}'", artifact.path),
                source,
                Some(&artifact.path),
                error,
            ),
            LockError::NotOwned => filesystem_error(
                format!("Unable to write local artifact '{}'", artifact.path),
                source,
                Some(&artifact.path),
                io::Error::other("snapshot lock lost"),
            ),
        })?;
        invoke_progress_callback(options, source, artifact, downloaded)?;
    }
    if let Err(error) = file.sync_all() {
        return Err(filesystem_error(
            format!("Unable to finish local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        ));
    }
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    if downloaded != artifact.size || actual != artifact.sha256.to_lowercase() {
        return Err(ArtifactDownloadError {
            message: format!(
                "Integrity check failed for '{}' from {}: expected {} bytes/{}, received {downloaded} bytes/{actual}",
                artifact.path,
                source.kind,
                artifact.size,
                artifact.sha256.to_lowercase()
            ),
            kind: FailureKind::Integrity,
            source: Some(source.kind),
            artifact: Some(artifact.path.clone()),
            status: None,
        });
    }

    install_downloaded_artifact(
        &options.model,
        source,
        artifact,
        manifest,
        destination,
        partial,
        original,
        lock,
    )
    .await
}

async fn with_header_deadline(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    url: &str,
) -> Result<ArtifactFetchResponse, ArtifactDownloadError> {
    let fetch = options.fetcher.fetch(url);
    match tokio::time::timeout(
        std::time::Duration::from_millis(options.response_header_ms),
        fetch,
    )
    .await
    {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(ArtifactFetchError::Network(message))) => Err(ArtifactDownloadError {
            message: format!(
                "Unable to request model artifact from {}: {message}",
                source.kind
            ),
            kind: FailureKind::Network,
            source: Some(source.kind),
            artifact: Some(artifact.path.clone()),
            status: None,
        }),
        Ok(Err(ArtifactFetchError::Cancelled)) => Err(ArtifactDownloadError {
            message: format!(
                "Unable to request model artifact from {}: request was aborted",
                source.kind
            ),
            kind: FailureKind::Aborted,
            source: Some(source.kind),
            artifact: Some(artifact.path.clone()),
            status: None,
        }),
        Err(_) => Err(ArtifactDownloadError {
            message: format!(
                "Unable to request model artifact from {}: Timed out waiting for response headers",
                source.kind
            ),
            kind: FailureKind::Timeout,
            source: Some(source.kind),
            artifact: Some(artifact.path.clone()),
            status: None,
        }),
    }
}

async fn with_read_idle_deadline(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    body: &mut dyn ArtifactBody,
) -> Result<Option<Vec<u8>>, ArtifactDownloadError> {
    loop {
        match tokio::time::timeout(
            std::time::Duration::from_millis(options.read_idle_ms),
            body.next_chunk(),
        )
        .await
        {
            // Empty chunks carry no data; keep waiting inside the idle budget.
            Ok(Some(Ok(bytes))) if bytes.is_empty() => {}
            Ok(Some(Ok(bytes))) => return Ok(Some(bytes)),
            Ok(Some(Err(ArtifactFetchError::Network(message)))) => {
                return Err(ArtifactDownloadError {
                    message: format!(
                        "Model download stream failed from {}: {message}",
                        source.kind
                    ),
                    kind: FailureKind::Network,
                    source: Some(source.kind),
                    artifact: Some(artifact.path.clone()),
                    status: None,
                });
            }
            Ok(Some(Err(ArtifactFetchError::Cancelled))) => {
                return Err(ArtifactDownloadError {
                    message: format!(
                        "Model download stream failed from {}: request was aborted",
                        source.kind
                    ),
                    kind: FailureKind::Aborted,
                    source: Some(source.kind),
                    artifact: Some(artifact.path.clone()),
                    status: None,
                });
            }
            Ok(None) => return Ok(None),
            Err(_) => {
                return Err(ArtifactDownloadError {
                    message: format!(
                        "Model download stream failed from {}: Timed out waiting for network read",
                        source.kind
                    ),
                    kind: FailureKind::Timeout,
                    source: Some(source.kind),
                    artifact: Some(artifact.path.clone()),
                    status: None,
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
// The only await inside is `cfg(windows)`-gated (`replace_on_windows`); on
// other platforms clippy correctly sees no await, hence the allow.
#[allow(clippy::unused_async)]
async fn install_downloaded_artifact(
    model: &str,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    manifest: &SourceManifest,
    destination: &Path,
    partial: &Path,
    original: Option<FileIdentity>,
    lock: &mut crate::artifact_lock::ArtifactCacheLock,
) -> Result<(), ArtifactDownloadError> {
    assert_lock_owned(source, artifact, lock)?;
    let current = inspect_file_identity(destination).map_err(|error| {
        filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        )
    })?;
    if !same_file_identity(original.as_ref(), current.as_ref()) {
        if validate_artifact(model, source, artifact, manifest)
            .map_err(flatten_artifact_error)?
        {
            return Ok(());
        }
        return Err(filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            io::Error::other("Artifact destination changed concurrently while downloading"),
        ));
    }
    if current.as_ref().is_some_and(|identity| !identity.replaceable) {
        return Err(filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            io::Error::other(format!("Refusing to replace non-file destination '{}'", destination.display())),
        ));
    }
    match std::fs::rename(partial, destination) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(error) if is_windows_replace_error(&error) => {
            replace_on_windows(destination, partial, &current, lock, model, source, artifact).await
        }
        Err(error) => Err(filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        )),
    }
}

fn assert_lock_owned(
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    lock: &mut crate::artifact_lock::ArtifactCacheLock,
) -> Result<(), ArtifactDownloadError> {
    lock.assert_owned().map_err(|error| match error {
        LockError::Io(error) => filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        ),
        LockError::NotOwned => filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            io::Error::other("snapshot lock lost"),
        ),
    })
}

#[cfg(windows)]
fn is_windows_replace_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::AlreadyExists
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
async fn replace_on_windows(
    destination: &Path,
    partial: &Path,
    expected: &Option<FileIdentity>,
    lock: &mut crate::artifact_lock::ArtifactCacheLock,
    model: &str,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
) -> Result<(), ArtifactDownloadError> {
    assert_lock_owned(source, artifact, lock)?;
    let current = inspect_file_identity(destination).map_err(|error| {
        filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        )
    })?;
    if !same_file_identity(expected.as_ref(), current.as_ref()) {
        return Err(filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            io::Error::other("Artifact destination changed concurrently while replacing"),
        ));
    }
    let displaced = destination.with_file_name(format!(
        "{}..replaced-{}-{}",
        destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        now_ms()
    ));
    if let Err(error) = std::fs::rename(destination, &displaced) {
        return Err(filesystem_error(
            format!("Unable to install local artifact '{}'", artifact.path),
            source,
            Some(&artifact.path),
            error,
        ));
    }
    assert_lock_owned(source, artifact, lock)?;
    let installed = std::fs::rename(partial, destination);
    match installed {
        Ok(()) => {
            let _ = std::fs::remove_file(&displaced);
            Ok(())
        }
        Err(error) => {
            let recreated = inspect_file_identity(destination)
                .map(|identity| identity.is_some())
                .unwrap_or(true);
            if !recreated {
                let _ = std::fs::rename(&displaced, destination);
            }
            Err(filesystem_error(
                format!("Unable to install local artifact '{}'", artifact.path),
                source,
                Some(&artifact.path),
                error,
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Completion markers.
// ---------------------------------------------------------------------------

fn write_complete_marker(
    source: &ModelArtifactSource,
    manifest: &SourceManifest,
) -> Result<(), ArtifactError> {
    let mut files = serde_json::Map::new();
    for (artifact_path, local_path) in &manifest.local_paths {
        let local = safe_local_path(&source.cache_directory, local_path)?;
        let stats = std::fs::metadata(&local).map_err(|error| {
            filesystem_error(
                format!("Unable to record completed {} model snapshot", source.kind),
                source,
                None,
                error,
            )
        })?;
        if !stats.is_file() {
            return Err(ArtifactError::from(filesystem_error(
                format!("Unable to record completed {} model snapshot", source.kind),
                source,
                None,
                io::Error::other(format!("Artifact '{artifact_path}' is not a file"),
                ),
            )));
        }
        files.insert(
            artifact_path.clone(),
            serde_json::json!({
                "size": stats.len(),
                "mtimeMs": mtime_ms(&stats),
                "ctimeMs": ctime_ms(&stats),
            }),
        );
    }
    let marker = serde_json::json!({
        "version": MANIFEST_VERSION,
        "fingerprint": manifest.fingerprint,
        "files": files,
    });
    let text = serde_json::to_string(&marker).map_err(|error| {
        filesystem_error(
            format!("Unable to record completed {} model snapshot", source.kind),
            source,
            None,
            error,
        )
    })?;
    common_core::io::write_atomic(&manifest.marker_path, text.as_bytes()).map_err(|error| {
        ArtifactError::from(filesystem_error(
            format!("Unable to record completed {} model snapshot", source.kind),
            source,
            None,
            error,
        ))
    })
}

fn write_complete_marker_best_effort(
    source: &ModelArtifactSource,
    artifacts: &[ModelArtifact],
    manifest: &SourceManifest,
) {
    // The marker is only a hashing optimization: a valid read-only cache
    // stays usable even when metadata cannot be written beside it.
    let complete = has_valid_complete_marker(source, artifacts, manifest);
    if !complete.unwrap_or(false) {
        let _ = write_complete_marker(source, manifest);
    }
}

// ---------------------------------------------------------------------------
// File identity + local paths.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    mtime_ms: u64,
    ctime_ms: u64,
    replaceable: bool,
}

fn inspect_file_identity(path: &Path) -> Result<Option<FileIdentity>, io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(stats) => Ok(Some(file_identity(&stats))),
        Err(error) if is_missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn file_identity(stats: &std::fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: file_device(stats),
        inode: file_inode(stats),
        mode: file_mode(stats),
        size: stats.len(),
        mtime_ms: mtime_ms(stats),
        ctime_ms: ctime_ms(stats),
        replaceable: stats.is_file() || stats.is_symlink(),
    }
}

fn same_file_identity(left: Option<&FileIdentity>, right: Option<&FileIdentity>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.device == right.device
                && left.inode == right.inode
                && left.mode == right.mode
                && left.size == right.size
                && left.mtime_ms == right.mtime_ms
                && left.ctime_ms == right.ctime_ms
                && left.replaceable == right.replaceable
        }
        (Some(_), None) | (None, Some(_)) => false,
    }
}

/// Lexically resolve `local` under `root`, rejecting cache escapes without
/// touching the filesystem (mirrors the upstream containment check).
fn safe_local_path(root: &Path, local: &str) -> Result<PathBuf, ArtifactError> {
    let mut normalized = PathBuf::new();
    for component in Path::new(&local.replace('\\', "/")).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(invalid_input(format!(
                        "Local artifact path escapes cache: '{local}'"
                    )));
                }
            }
            Component::Normal(segment) => normalized.push(segment),
            Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_input(format!(
                    "Local artifact path escapes cache: '{local}'"
                )));
            }
        }
    }
    Ok(root.join(normalized))
}

fn absolute_dir(path: &Path) -> String {
    if path.is_absolute() {
        return path.to_string_lossy().into_owned();
    }
    match std::env::current_dir() {
        Ok(current) => current.join(path).to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn partial_path(destination: &Path) -> PathBuf {
    destination.with_file_name(format!(
        "{}..part-{}-{}",
        destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        now_ms()
    ))
}

fn mtime_ms(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn ctime_ms(metadata: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let millis = metadata.ctime() * 1000 + metadata.ctime_nsec() / 1_000_000;
        millis.max(0) as u64
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

#[cfg(unix)]
fn file_device(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.dev()
}

#[cfg(unix)]
fn file_inode(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.ino()
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.mode()
}

#[cfg(not(unix))]
fn file_device(_metadata: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(not(unix))]
fn file_inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> u32 {
    0
}

fn is_missing(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

// ---------------------------------------------------------------------------
// Callbacks (panics become local `callback` failures, never fallback).
// ---------------------------------------------------------------------------

fn invoke_download_plan_callback(
    options: &NormalizedOptions<'_>,
    missing: &[ModelArtifact],
) -> Result<(), ArtifactDownloadError> {
    let Some(callback) = options.on_download_plan else {
        return Ok(());
    };
    match catch_unwind(AssertUnwindSafe(|| callback(missing))) {
        Ok(()) => Ok(()),
        Err(_) => Err(ArtifactDownloadError {
            message: format!(
                "Model download plan callback failed for {}",
                options.model
            ),
            kind: FailureKind::Callback,
            source: None,
            artifact: None,
            status: None,
        }),
    }
}

fn invoke_progress_callback(
    options: &NormalizedOptions<'_>,
    source: &ModelArtifactSource,
    artifact: &ModelArtifact,
    downloaded_bytes: u64,
) -> Result<(), ArtifactDownloadError> {
    let Some(callback) = options.on_progress else {
        return Ok(());
    };
    let event = ModelArtifactDownloadProgress {
        model: options.model.clone(),
        source: source.kind,
        artifact: artifact.path.clone(),
        downloaded_bytes,
        total_bytes: artifact.size,
    };
    match catch_unwind(AssertUnwindSafe(|| callback(event))) {
        Ok(()) => Ok(()),
        Err(_) => Err(ArtifactDownloadError {
            message: format!(
                "Artifact progress callback failed for {}",
                options.model
            ),
            kind: FailureKind::Callback,
            source: Some(source.kind),
            artifact: Some(artifact.path.clone()),
            status: None,
        }),
    }
}

fn invoke_fallback_callback(
    callback: Option<FallbackHook<'_>>,
    message: &str,
    source: &ModelArtifactSource,
) -> Result<(), ArtifactDownloadError> {
    let Some(callback) = callback else {
        return Ok(());
    };
    match catch_unwind(AssertUnwindSafe(|| callback(message))) {
        Ok(()) => Ok(()),
        Err(_) => Err(ArtifactDownloadError {
            message: "Artifact fallback warning callback failed".to_string(),
            kind: FailureKind::Callback,
            source: Some(source.kind),
            artifact: None,
            status: None,
        }),
    }
}

fn filesystem_error(
    message: impl Into<String>,
    source: &ModelArtifactSource,
    artifact: Option<&str>,
    cause: impl fmt::Debug,
) -> ArtifactDownloadError {
    ArtifactDownloadError {
        message: format!("{}: {cause:?}", message.into()),
        kind: FailureKind::Filesystem,
        source: Some(source.kind),
        artifact: artifact.map(str::to_string),
        status: None,
    }
}

impl From<ArtifactDownloadError> for ArtifactError {
    fn from(error: ArtifactDownloadError) -> Self {
        Self::Download(error)
    }
}

// ---------------------------------------------------------------------------
// Aggregated progress reporter (one `downloading` event per report).
// ---------------------------------------------------------------------------

/// Model-level progress events surfaced to embedding callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingProgress {
    Preparing {
        model: String,
    },
    Downloading {
        model: String,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Ready {
        model: String,
    },
    Warning {
        model: String,
        message: String,
    },
}

/// Aggregates per-artifact byte reports into model-level progress events.
/// Shared-reference friendly: plan and progress updates take `&self` so
/// `Fn` download hooks can report without exclusive access.
pub struct DownloadProgressReporter<'a> {
    model: String,
    on_progress: Box<dyn Fn(EmbeddingProgress) + Send + Sync + 'a>,
    artifacts: std::sync::Mutex<HashMap<String, (u64, u64)>>,
}

impl<'a> DownloadProgressReporter<'a> {
    pub fn new(
        model: &str,
        on_progress: impl Fn(EmbeddingProgress) + Send + Sync + 'a,
    ) -> Self {
        Self {
            model: model.to_string(),
            on_progress: Box::new(on_progress),
            artifacts: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn start(&self) {
        (self.on_progress)(EmbeddingProgress::Preparing {
            model: self.model.clone(),
        });
    }

    pub fn set_download_plan(&self, artifacts: &[ModelArtifact]) {
        let Ok(mut plan) = self.artifacts.lock() else {
            return;
        };
        plan.clear();
        for artifact in artifacts {
            plan.insert(artifact.path.clone(), (0, artifact.size));
        }
    }

    pub fn report(&self, artifact: &str, downloaded_bytes: u64) {
        let (downloaded, total) = {
            let Ok(mut plan) = self.artifacts.lock() else {
                return;
            };
            let Some(entry) = plan.get_mut(artifact) else {
                return;
            };
            entry.0 = downloaded_bytes;
            (
                plan.values().map(|(done, _)| done).sum(),
                plan.values().map(|(_, total)| total).sum(),
            )
        };
        (self.on_progress)(EmbeddingProgress::Downloading {
            model: self.model.clone(),
            downloaded_bytes: downloaded,
            total_bytes: total,
        });
    }

    /// Returns `false` when nobody listens (callers fall back to stderr).
    pub fn warning(&self, message: &str) -> bool {
        (self.on_progress)(EmbeddingProgress::Warning {
            model: self.model.clone(),
            message: message.to_string(),
        });
        true
    }

    pub fn finish(&self) {
        (self.on_progress)(EmbeddingProgress::Ready {
            model: self.model.clone(),
        });
    }
}
