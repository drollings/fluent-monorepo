//! Single-writer feed and instance/backup gates.
//!
//! The writer is the only task that inserts: producers hand it a
//! pending write and await the assigned cursor over credit-gated
//! channels, so sequence numbers stay monotonic under concurrency.
//! The runtime directory is created private (0700); the socket bind
//! proves single instance (a live holder refuses, a stale file is
//! unlinked and retried once). The database refuses a newer schema,
//! backs itself up before migrating, and a failed migration leaves
//! the file untouched (the migration runs in one transaction).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fluent_concurrency::feed_worker::{CreditedFeedWorker, FeedConfig};
use fluent_db::error::DbError;
use fluent_db::migrate::schema_version;
use fluent_db::store::SqliteStore;
use fluent_wvr::Runtime;
use thiserror::Error;

/// One pending durable write. The writer assigns the cursor and wakes
/// the producer with it.
pub struct WriteReq {
    /// Owning session id.
    pub session: String,
    /// Reply-generation epoch.
    pub epoch: u64,
    /// Event kind label.
    pub kind: String,
    /// JSON payload.
    pub payload: String,
    /// Wall-clock ms.
    pub wall_ms: u64,
    /// Cursor back to the producer.
    pub ack: tokio::sync::oneshot::Sender<Result<i64, String>>,
}

/// Feed sizing for the single writer: a small queue with credit in
/// tens of events (not the router defaults), serial handler.
#[must_use]
pub fn writer_feed_config() -> FeedConfig {
    FeedConfig {
        queue_capacity: 256,
        credit_limit: 64,
        credit_more_after: 8,
        max_concurrent: 1,
        batch_size: 8,
        poll_interval_ms: 100,
    }
}

/// Spawn the single writer over `log`. Producers use
/// `enqueue_with_credit`; the handler inserts and acks in order.
pub fn spawn_writer(
    runtime: Arc<dyn Runtime>,
    log: Arc<crate::events::EventLog>,
) -> Arc<CreditedFeedWorker<WriteReq>> {
    Arc::new(CreditedFeedWorker::new(
        runtime,
        writer_feed_config(),
        move |req: WriteReq| {
            let log = Arc::clone(&log);
            async move {
                let result = log
                    .append(&req.session, req.epoch, &req.kind, &req.payload, req.wall_ms)
                    .map(|event| event.seq)
                    .map_err(|error| error.to_string());
                let _ = req.ack.send(result);
            }
        },
    ))
}

/// Instance/store failures. Every variant names what to do next.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Another live daemon holds the runtime socket.
    #[error("another instance is running at {0}")]
    AlreadyRunning(String),
    /// The database schema is newer than this build.
    #[error("database schema v{found} is newer than supported v{supported}")]
    NewerSchema {
        /// Version in the file.
        found: u32,
        /// Version this build serves.
        supported: u32,
    },
    /// Filesystem or SQLite failure.
    #[error("store open failed: {0}")]
    Store(String),
}

///Marker for DbError conversion.
impl From<DbError> for OpenError {
    fn from(error: DbError) -> Self {
        Self::Store(error.to_string())
    }
}

/// Single-instance guard: the bound socket while held.
#[derive(Debug)]
pub struct InstanceLock {
    _socket: std::os::unix::net::UnixListener,
    /// Socket path, for diagnostics.
    pub path: PathBuf,
}

impl InstanceLock {
    /// Private runtime dir plus socket bind. A live holder refuses;
    /// a stale socket file is unlinked and the bind retried in a
    /// bounded loop (a racing starter may win the unlink window).
    /// Hardening failures refuse instead of failing open; symlinks
    /// refuse; unlink errors propagate. Every component is lstat-ed
    /// (symlink parents refuse), and the path is re-lstat-ed after
    /// `remove_file` so a swapped-in link cannot ride the retry.
    pub fn acquire(runtime_dir: &Path, socket_name: &str) -> Result<Self, OpenError> {
        // Lstat before mkdir: a symlink parent refuses before creating
        // anything outside the jail.
        if orator_core::paths::has_symlink_parent(runtime_dir)
            || orator_core::paths::has_symlink_parent(&runtime_dir.join(socket_name))
        {
            return Err(OpenError::Store(format!(
                "runtime dir {} is a symlink or under one; refusing (remove it)",
                runtime_dir.display()
            )));
        }
        std::fs::create_dir_all(runtime_dir)
            .map_err(|error| OpenError::Store(format!("mkdir {}: {error}", runtime_dir.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(runtime_dir, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    OpenError::Store(format!(
                        "chmod 700 {}: {error}",
                        runtime_dir.display()
                    ))
                },
            )?;
        }
        let path = runtime_dir.join(socket_name);
        if orator_core::paths::has_symlink_parent(&path) {
            return Err(OpenError::Store(format!(
                "socket path {} is a symlink or under one; refusing (remove it)",
                path.display()
            )));
        }
        let mut attempts = 0;
        loop {
            match std::os::unix::net::UnixListener::bind(&path) {
                Ok(socket) => {
                    return Ok(Self {
                        _socket: socket,
                        path,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempts < 3 => {
                    // Stale file or live holder? A connect that succeeds
                    // proves live; a refusal proves stale.
                    let live = std::os::unix::net::UnixStream::connect(&path).is_ok();
                    if live {
                        return Err(OpenError::AlreadyRunning(path.display().to_string()));
                    }
                    std::fs::remove_file(&path).map_err(|remove| {
                        OpenError::Store(format!(
                            "stale socket {}: remove failed: {remove}",
                            path.display()
                        ))
                    })?;
                    // Re-lstat after unlink: a swapped-in link refuses
                    // instead of riding the retry.
                    if orator_core::paths::has_symlink_parent(&path)
                        || std::fs::symlink_metadata(&path).is_ok()
                    {
                        return Err(OpenError::Store(format!(
                            "socket path {} reappeared as a link after unlink; refusing",
                            path.display()
                        )));
                    }
                    attempts += 1;
                }
                Err(error) => {
                    return Err(OpenError::Store(format!(
                        "bind {}: {error}",
                        path.display()
                    )))
                }
            }
        }
    }
}

/// Copy `db_path` to a timestamped sibling. Best-effort: a failed
/// backup refuses to migrate rather than risking the file. The process
/// id plus a per-process sequence ride along so two migrates in the
/// same second never share a name.
pub fn backup_db(db_path: &Path) -> Result<PathBuf, OpenError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let backup = db_path.with_extension(format!(
        "bak-{stamp}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::copy(db_path, &backup)
        .map_err(|error| OpenError::Store(format!("backup {}: {error}", db_path.display())))?;
    Ok(backup)
}

/// Open (or create) the store at `db_path` with the backup gate:
/// refuse newer schemas, back up before migrating, migrate in one
/// transaction. `ORATOR_NO_BACKUP=1` opts out of the copy (tests).
pub fn open_store(db_path: &Path) -> Result<SqliteStore, OpenError> {
    let fresh = !db_path.exists();
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| {
                OpenError::Store(format!("mkdir {}: {error}", parent.display()))
            })?;
        }
    }
    let store = SqliteStore::open(db_path)?;
    let found = store.with_conn(schema_version)?;
    if found > crate::schema::CURRENT_VERSION {
        return Err(OpenError::NewerSchema {
            found,
            supported: crate::schema::CURRENT_VERSION,
        });
    }
    if fresh || found < crate::schema::CURRENT_VERSION {
        if !fresh && std::env::var("ORATOR_NO_BACKUP").unwrap_or_default() != "1" {
            backup_db(db_path)?;
        }
        store
            .with_conn(|conn| {
                fluent_db::migrate::migrate(conn, &crate::schema::migrations())?;
                Ok(())
            })
            .map_err(OpenError::from)?;
    }
    Ok(store)
}

#[cfg(test)]
#[path = "../tests/writer.rs"]
mod tests;
