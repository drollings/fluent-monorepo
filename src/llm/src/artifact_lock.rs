//! Directory-based mutual exclusion for model snapshot downloads.
//!
//! One lock is one directory holding a single `.owner-<token>` file with a
//! JSON `{token, pid, hostname, createdAt}` payload. Acquisition publishes an
//! already-initialized staging directory via rename, so concurrent publishers
//! can neither replace each other nor write owners into a successor's lock.
//! Ownership is refreshed by rewriting the owner file (the write bumps its
//! mtime); a displaced owner fences itself on the missing token file.
//! Stale locks are taken over after `stale_ms` without heartbeats, or
//! immediately when the sole owner is a provably exited local PID.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};


/// Tuning for lock acquisition.
#[derive(Debug, Clone, Copy)]
pub struct LockOptions {
    /// Wait between acquisition polls.
    pub poll_ms: u64,
    /// Heartbeat silence after which a lock counts as abandoned.
    pub stale_ms: u64,
    /// Minimum gap between owner heartbeat rewrites.
    pub heartbeat_ms: u64,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self {
            poll_ms: 250,
            stale_ms: 10 * 60_000,
            heartbeat_ms: 30_000,
        }
    }
}

/// Lock failures. Every variant preserves the underlying `io::Error` so
/// callers can distinguish contention handling from real I/O failures.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("artifact cache lock I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("lock is no longer owned by this holder")]
    NotOwned,
}

/// An acquired snapshot lock. Heartbeats are synchronous local file writes;
/// only acquisition waits (async poll loop).
pub struct ArtifactCacheLock {
    owner_path: PathBuf,
    lock_path: PathBuf,
    payload: String,
    last_heartbeat_ms: u64,
    heartbeat_ms: u64,
    released: bool,
}

impl ArtifactCacheLock {
    /// Refresh the heartbeat when due (cheap: skipped inside `heartbeat_ms`).
    pub fn touch(&mut self) -> Result<(), LockError> {
        self.refresh(false)
    }

    /// Refresh the heartbeat unconditionally; fails when displaced.
    pub fn assert_owned(&mut self) -> Result<(), LockError> {
        self.refresh(true)
    }

    /// Release ownership. Idempotent; removes the owner file and the lock
    /// directory when empty. Never removes a successor's directory: `rmdir`
    /// only succeeds on an empty directory, and foreign files are left alone.
    pub fn release(&mut self) -> Result<(), LockError> {
        self.release_ref()
    }

    fn release_ref(&mut self) -> Result<(), LockError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        match std::fs::remove_file(&self.owner_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(LockError::Io(error)),
        }
        match std::fs::remove_dir(&self.lock_path) {
            Ok(()) | Err(_) => Ok(()),
        }
    }

    fn refresh(&mut self, force: bool) -> Result<(), LockError> {
        if self.released {
            return Err(LockError::NotOwned);
        }
        let now = now_ms();
        if !force && now.saturating_sub(self.last_heartbeat_ms) < self.heartbeat_ms {
            return Ok(());
        }
        // Rewriting the owner file refreshes its mtime and re-asserts the
        // token: a replaced directory cannot contain this owner's token, so a
        // missing file here also fences a holder after a stale takeover. The
        // open deliberately omits `create`: recreating a taken-over token
        // file would resurrect a displaced owner inside its successor's lock.
        let write_result = std::fs::OpenOptions::new()
            .write(true)
            .open(&self.owner_path)
            .and_then(|mut file| {
                use std::io::Write as _;
                file.write_all(self.payload.as_bytes())
                    .and_then(|()| file.write_all(b"\n"))
            });
        if let Err(error) = write_result {
            if error.kind() == io::ErrorKind::NotFound {
                return Err(LockError::NotOwned);
            }
            return Err(LockError::Io(error));
        }
        self.last_heartbeat_ms = now;
        Ok(())
    }
}

impl Drop for ArtifactCacheLock {
    fn drop(&mut self) {
        let _ = self.release_ref();
    }
}

/// Acquire the snapshot lock at `lock_path`, polling until it is free or
/// abandoned. Never returns while another live owner holds it.
pub async fn acquire_artifact_cache_lock(
    lock_path: &Path,
    options: &LockOptions,
) -> Result<ArtifactCacheLock, LockError> {
    loop {
        if let Some(lock) = try_acquire(lock_path, options)? {
            return Ok(lock);
        }
        if remove_abandoned_lock(lock_path, options)? {
            continue;
        }
        tokio::time::sleep(std::time::Duration::from_millis(options.poll_ms)).await;
    }
}

fn try_acquire(
    lock_path: &Path,
    options: &LockOptions,
) -> Result<Option<ArtifactCacheLock>, LockError> {
    if lock_path.symlink_metadata().is_ok() {
        return Ok(None);
    }
    let token = unique_token();
    let owner_name = format!(".owner-{token}");
    let staging = staged_path(lock_path, &token);
    std::fs::create_dir_all(&staging)?;
    let owner_path = lock_path.join(&owner_name);
    let payload = owner_payload(&token);
    let staged_owner = staging.join(&owner_name);
    let published = (|| {
        // Exclusive create: a racing publisher that won the rename leaves our
        // staging behind, which we always clean up below.
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_owner)?;
        file.write_all(payload.as_bytes())?;
        file.write_all(b"\n")?;
        match std::fs::rename(&staging, lock_path) {
            Ok(()) => Ok(true),
            Err(error) if is_directory_conflict(&error, lock_path) => Ok(false),
            Err(error) => Err(error),
        }
    })();
    // Always remove our staging directory; the published lock lives at
    // `lock_path` and is never touched here.
    let _ = std::fs::remove_dir_all(&staging);
    if !published.map_err(LockError::Io)? {
        return Ok(None);
    }

    let mut lock = ArtifactCacheLock {
        owner_path,
        lock_path: lock_path.to_path_buf(),
        payload,
        last_heartbeat_ms: now_ms(),
        heartbeat_ms: options.heartbeat_ms,
        released: false,
    };
    // Initialization may have been paused longer than the lease: refresh on
    // publication and verify this owner has not already been displaced.
    match lock.assert_owned() {
        Ok(()) => Ok(Some(lock)),
        Err(LockError::NotOwned) => Ok(None),
        Err(error) => {
            let _ = lock.release_ref();
            Err(error)
        }
    }
}

fn staged_path(lock_path: &Path, token: &str) -> PathBuf {
    let file_name = lock_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    lock_path.with_file_name(format!(
        "{file_name}.pending-{}-{token}",
        std::process::id()
    ))
}

fn unique_token() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher as _};
    let mut hasher = DefaultHasher::new();
    (std::process::id(), now_ms(), std::thread::current().id()).hash(&mut hasher);
    format!("{:016x}{:016x}", hasher.finish(), now_ms())
}

fn owner_payload(token: &str) -> String {
    format!(
        "{{\"token\":{token:?},\"pid\":{},\"hostname\":{:?},\"createdAt\":{}}}",
        std::process::id(),
        local_hostname(),
        now_ms()
    )
}

/// Best-effort local hostname for dead-owner scoping. Unknown hosts stay
/// conservative: the TTL path applies instead of the fast takeover.
#[must_use]
pub fn local_hostname() -> String {
    if let Ok(name) = std::env::var("HOSTNAME") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    if let Ok(contents) = std::fs::read_to_string("/etc/hostname") {
        let name = contents.trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    "localhost".to_string()
}

fn is_directory_conflict(error: &io::Error, path: &Path) -> bool {
    use io::ErrorKind as Kind;
    match error.kind() {
        Kind::AlreadyExists | Kind::DirectoryNotEmpty => true,
        Kind::PermissionDenied => path.is_dir(),
        _ => false,
    }
}

fn remove_abandoned_lock(
    lock_path: &Path,
    options: &LockOptions,
) -> Result<bool, LockError> {
    let Some(observed) = inspect_lock(lock_path)? else {
        return Ok(true);
    };
    if !is_abandoned(&observed, options) {
        return Ok(false);
    }
    let stale_path = lock_path.with_file_name(format!(
        "{}..stale-{}-{}",
        lock_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        now_ms()
    ));
    if std::fs::rename(lock_path, &stale_path).is_err() {
        return Ok(false);
    }
    let moved = inspect_lock(&stale_path)?;
    let still_abandoned = moved.as_ref().is_some_and(|observation| {
        observation.device == observed.device
            && observation.inode == observed.inode
            && is_abandoned(observation, options)
    });
    if !still_abandoned {
        // Another owner acquired or refreshed while we inspected: restore
        // that directory, never delete its owner file.
        match std::fs::rename(&stale_path, lock_path) {
            Ok(()) => {}
            Err(error) if is_directory_conflict(&error, lock_path) => {}
            Err(error) => return Err(LockError::Io(error)),
        }
        return Ok(false);
    }
    std::fs::remove_dir_all(&stale_path).map_err(LockError::Io)?;
    Ok(true)
}

struct LockObservation {
    device: u64,
    inode: u64,
    newest_heartbeat_ms: u64,
    dead_owner: bool,
}

fn inspect_lock(lock_path: &Path) -> Result<Option<LockObservation>, LockError> {
    let metadata = match std::fs::metadata(lock_path) {
        Ok(metadata) => metadata,
        Err(error) if is_missing(&error) => return Ok(None),
        Err(error) => return Err(LockError::Io(error)),
    };
    if !metadata.is_dir() {
        // A non-directory at the lock path is not ours to manage.
        return Ok(Some(LockObservation {
            device: file_device(&metadata),
            inode: file_inode(&metadata),
            newest_heartbeat_ms: u64::MAX,
            dead_owner: false,
        }));
    }
    let mut newest = mtime_ms(&metadata);
    let mut owners = Vec::new();
    let entries = match std::fs::read_dir(lock_path) {
        Ok(entries) => entries,
        Err(error) if is_missing(&error) => return Ok(None),
        Err(error) => return Err(LockError::Io(error)),
    };
    for entry in entries {
        let entry = entry.map_err(LockError::Io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().map_err(LockError::Io)?.is_file() || !name.starts_with(".owner-") {
            continue;
        }
        owners.push(name.clone());
        match std::fs::metadata(entry.path()) {
            Ok(stats) => newest = newest.max(mtime_ms(&stats)),
            Err(error) if is_missing(&error) => {}
            Err(error) => return Err(LockError::Io(error)),
        }
    }
    let dead_owner = owners.len() == 1 && is_known_dead_owner(lock_path, &owners[0])?;
    Ok(Some(LockObservation {
        device: file_device(&metadata),
        inode: file_inode(&metadata),
        newest_heartbeat_ms: newest,
        dead_owner,
    }))
}

fn is_abandoned(observation: &LockObservation, options: &LockOptions) -> bool {
    observation.dead_owner
        || now_ms().saturating_sub(observation.newest_heartbeat_ms) >= options.stale_ms
}

/// A sole owner counts as dead only when it names this host, its token file
/// matches, its PID parses, and the local PID table proves it exited. Every
/// inconclusive probe keeps the ordinary heartbeat expiry period.
fn is_known_dead_owner(lock_path: &Path, owner_name: &str) -> Result<bool, LockError> {
    let text = match std::fs::read_to_string(lock_path.join(owner_name)) {
        Ok(text) => text,
        Err(error) if is_missing(&error) => return Ok(false),
        Err(error) => return Err(LockError::Io(error)),
    };
    let owner: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let token = owner.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
    if token.is_empty() || format!(".owner-{token}") != owner_name {
        return Ok(false);
    }
    let hostname = owner.get("hostname").and_then(serde_json::Value::as_str).unwrap_or("");
    if hostname != local_hostname() {
        return Ok(false);
    }
    let pid = owner.get("pid").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if pid == 0 || pid > u64::from(u32::MAX) {
        return Ok(false);
    }
    Ok(pid_is_dead(pid as u32))
}

/// Prove a local PID exited. Only a missing `/proc/<pid>` entry counts;
/// every other outcome is inconclusive (never fast-takeover).
fn pid_is_dead(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        matches!(
            std::fs::metadata(format!("/proc/{pid}")),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false;
    }
}

fn is_missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn mtime_ms(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis() as u64)
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

#[cfg(not(unix))]
fn file_device(_metadata: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(not(unix))]
fn file_inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}
