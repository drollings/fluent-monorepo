use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::constants::MAX_FILE_SIZE;
use crate::error::IoError;

pub fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Nanoseconds since the UNIX epoch, saturating at `u64::MAX` (the horizon
/// is the year 2554, past which "newer" stops being a meaningful answer).
#[must_use]
pub fn system_time_ns(time: SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

/// Wall clock now, in nanoseconds since the UNIX epoch.
#[must_use]
pub fn now_ns() -> u64 {
    system_time_ns(SystemTime::now())
}

/// Modification time of `metadata`, in nanoseconds since the UNIX epoch, or
/// 0 when the platform reports none.
///
/// Nanoseconds, not milliseconds: these stamps are used as a *change signal*,
/// so they must out-resolve the interval between two edits. A millisecond
/// stamp cannot tell a same-size rewrite that lands one millisecond after a
/// recorded one from no rewrite at all, which is a correctness failure for any
/// caller that skips work when the stamp is unchanged.
#[must_use]
pub fn mtime_ns(metadata: &fs::Metadata) -> u64 {
    metadata.modified().map_or(0, system_time_ns)
}

/// Status-change time of `metadata` (inode metadata change, distinct from
/// `mtime`) in nanoseconds since the UNIX epoch, or 0 on platforms that do
/// not report one.
///
/// Kept as a separate signal from [`mtime_ns`]: a same-size, same-mtime
/// content swap performed through a path that preserves mtime still moves
/// ctime, so the pair catches more than mtime alone.
#[must_use]
pub fn ctime_ns(metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let seconds = metadata.ctime();
        let nanos = u64::try_from(metadata.ctime_nsec()).unwrap_or(0);
        // A pre-epoch or absurdly large ctime cannot be represented; the
        // saturating cast keeps the stamp monotonic rather than panicking.
        let seconds = u64::try_from(seconds).unwrap_or(0);
        seconds.saturating_mul(1_000_000_000).saturating_add(nanos)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

/// Whether a nanosecond stamp carries enough resolution to be trusted as a
/// change signal: it must be non-zero *and* have a sub-millisecond
/// remainder.
///
/// A stamp sitting exactly on a millisecond boundary is what a millisecond-
/// truncated read produces, and is also what a filesystem storing coarse
/// timestamps reports — in both cases two rewrites inside one millisecond are
/// indistinguishable, so a caller comparing stamps for change detection should
/// fall back to a content check rather than conclude "unchanged".
#[must_use]
pub fn timestamp_is_resolved(stamp_ns: u64) -> bool {
    stamp_ns != 0 && stamp_ns % 1_000_000 != 0
}

/// Atomically write data to a file by writing to a unique sibling temp file
/// and renaming over the target.
///
/// The temp file lives in the same directory as the target (so the rename
/// never crosses filesystems) and is named
/// `<file>.<pid>.<counter>.<rand>.tmp`, which can neither collide with a
/// sibling target (`a.json` vs. `a.toml`) nor be mistaken for the real file
/// by a reader. The file is fsync'd before the rename and the parent
/// directory is fsync'd on a best-effort basis; the temp file is removed on
/// any failure. Readers always observe the old or the new contents, never a
/// mix.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use common_core::io::write_atomic;
///
/// write_atomic(Path::new("config.json"), b"{\"key\": \"value\"}")?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    use std::io::Write as _;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let stem = path
        .file_name()
        .map_or_else(|| "file".to_string(), |s| s.to_string_lossy().into_owned());
    let slot = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_name = format!(
        "{stem}.{}.{slot}.{}.tmp",
        std::process::id(),
        fastrand::u32(..)
    );
    let tmp = match parent {
        Some(dir) => dir.join(tmp_name),
        None => Path::new(&tmp_name).to_path_buf(),
    };
    let write_result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        // Durability best-effort: a failed directory sync must not fail the
        // write — atomicity (all-or-nothing via rename) already holds.
        if let Some(dir) = parent {
            if let Ok(dir_fd) = fs::File::open(dir) {
                let _ = dir_fd.sync_all();
            }
        }
        fs::rename(&tmp, path)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

/// Read a file to a string, returning `IoError` for I/O failures and files
/// exceeding the 100 MiB size cap.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use common_core::io::read_to_string_err;
///
/// let content = read_to_string_err(Path::new("Cargo.toml"))?;
/// assert!(content.contains("[package]"));
/// # Ok::<(), common_core::error::IoError>(())
/// ```
pub fn read_to_string_err(path: &Path) -> Result<String, IoError> {
    let meta = fs::metadata(path).map_err(IoError)?;
    let size = meta.len() as usize;
    if size > MAX_FILE_SIZE {
        return Err(IoError(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file too large: {size} bytes exceeds maximum {MAX_FILE_SIZE} bytes"),
        )));
    }
    fs::read_to_string(path).map_err(IoError)
}

/// Resolve `path` against the current directory without touching the
/// filesystem. Unlike earlier versions this never creates directories;
/// callers that need the directory to exist call [`ensure_dir`] explicitly.
/// For a pure lexical absolute check without I/O see
/// [`is_absolute_lexical`](crate::path::is_absolute_lexical).
pub fn make_path_absolute(path: &str) -> std::io::Result<String> {
    let pb = Path::new(path);
    let abs = if pb.is_absolute() {
        pb.to_path_buf()
    } else {
        std::env::current_dir()?.join(pb)
    };
    Ok(abs.to_string_lossy().to_string())
}

/// Read a file to a string, returning `None` on any failure (missing file,
/// oversized file, invalid UTF-8). Fail-open convenience over
/// [`read_file_alloc_err`]: use when a missing file means "unchanged" or
/// "absent" rather than an error (file-id hashing, optional config).
pub fn read_file_alloc(path: &str) -> Option<String> {
    read_file_alloc_err(path).ok()
}

/// Read a file to a string, returning `std::io::Error` on failure.
/// Unlike [`read_to_string_err`] this reports the raw OS error (no
/// [`IoError`](crate::error::IoError) wrapping, no size cap): use at
/// trust boundaries where the caller maps the error itself.
pub fn read_file_alloc_err(path: &str) -> Result<String, std::io::Error> {
    fs::read_to_string(path)
}

/// Join `relative` onto the parent directory of `base` without touching the
/// filesystem (pure string join; no normalization, no existence check).
/// For lexical normalization without I/O use
/// [`normalize_lexical`](crate::path::normalize_lexical) instead.
pub fn resolve_path(base: &str, relative: &str) -> String {
    if relative == "." {
        let base_path = Path::new(base);
        return base_path
            .parent()
            .unwrap_or(base_path)
            .to_string_lossy()
            .to_string();
    }
    let base_path = Path::new(base);
    if base_path.is_absolute() {
        let joined = base_path.parent().unwrap_or(base_path).join(relative);
        return joined.to_string_lossy().to_string();
    }
    let rel_path = Path::new(relative);
    if rel_path.is_absolute() {
        return relative.to_string();
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let joined = cwd.join(base).parent().unwrap_or(&cwd).join(relative);
    joined.to_string_lossy().to_string()
}

pub fn strip_path_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    if let Some(stripped) = path.strip_prefix(prefix) {
        stripped.trim_start_matches('/')
    } else {
        path
    }
}

/// Idempotent directory creation with canonical error wrapping.
/// Replaces the 10+ ad-hoc `std::fs::create_dir_all(...)` calls across the workspace.
pub fn ensure_dir(path: impl AsRef<Path>) -> Result<(), IoError> {
    std::fs::create_dir_all(path.as_ref()).map_err(IoError)
}

/// Same as `ensure_dir` but `.expect`s on failure (for test setup and config init).
pub fn ensure_dir_or_panic(path: impl AsRef<Path>) {
    std::fs::create_dir_all(path.as_ref()).expect("ensure_dir");
}

/// Bounded stale-Unix-socket reclaim: probe → unlink → bind retry.
///
/// A `UnixListener::bind` that fails with `AddrInUse` either means a
/// live holder (a `connect` succeeds — stop with the original error)
/// or a stale socket file (no listener — unlink and retry, bounded by
/// `max_reclaims`). The only loop in the tree; both callers compose it
/// instead of hand-rolling their own.
///
/// ```no_run
/// use std::path::Path;
/// use common_core::io::bind_unix_listener_with_stale_reclaim;
///
/// let listener = bind_unix_listener_with_stale_reclaim(
///     Path::new("/run/orator/orator.sock"),
///     3,
///     || Ok(()),
/// )?;
/// # Ok::<(), common_core::io::ReclaimBindError>(())
/// ```
///
/// OS calls run through a small private operations seam
/// (`ReclaimOps`/`reclaim_with_ops`) so the walk is unit-testable
/// without a race; the public function uses the real std operations.
/// Post-unlink validation stays with the caller (it owns the path
/// policy and error wording): return `Err` from
/// `validate_after_unlink` to refuse a reappeared path instead of
/// retrying. Unix-only (`#[cfg(unix)]`); no unsafe code.
#[cfg(unix)]
#[derive(thiserror::Error, Debug)]
pub enum ReclaimBindError {
    /// A live holder answers: the original bind error, unwrapped.
    #[error("{0}")]
    LiveHolder(#[source] std::io::Error),
    /// Bind failed for any other reason — or `AddrInUse` with no
    /// reclaims left. The original error, unwrapped.
    #[error("{0}")]
    Bind(#[source] std::io::Error),
    /// The stale probe passed but the unlink failed.
    #[error("{0}")]
    Remove(#[source] std::io::Error),
    /// The caller's post-unlink validation refused the retry. Carries
    /// the exact error the caller's closure built.
    #[error("{0}")]
    Validate(#[source] std::io::Error),
}

/// Unix-socket bind operations behind the reclaim walk. The public
/// function uses the real std operations (`LiveOps`); focused tests
/// script a mock instead, so live/stale sequences, validation
/// failure, remove failure, and cap exhaustion are deterministic with
/// no filesystem race.
#[cfg(unix)]
trait ReclaimOps {
    /// Held across a successful bind (the real listener, or `()` for a
    /// scripted mock).
    type Listener;
    /// Bind the socket path.
    fn bind(&self, path: &Path) -> std::io::Result<Self::Listener>;
    /// Whether a live holder answers at `path`.
    fn probe_live(&self, path: &Path) -> bool;
    /// Remove a stale file at `path`.
    fn remove(&self, path: &Path) -> std::io::Result<()>;
}

/// The reclaim walk over `ops`: first success wins; `AddrInUse` with
/// reclaims left probes live (stop) vs stale (remove, validate, count
/// one reclaim, retry); anything else — including `AddrInUse` with no
/// reclaims left — returns the original error. Validation failure
/// returns immediately without consuming a reclaim.
#[cfg(unix)]
fn reclaim_with_ops<O: ReclaimOps>(
    ops: &O,
    path: &Path,
    max_reclaims: u32,
    mut validate_after_unlink: impl FnMut() -> std::io::Result<()>,
) -> Result<O::Listener, ReclaimBindError> {
    let mut attempts = 0u32;
    loop {
        match ops.bind(path) {
            Ok(listener) => return Ok(listener),
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && attempts < max_reclaims =>
            {
                if ops.probe_live(path) {
                    return Err(ReclaimBindError::LiveHolder(error));
                }
                ops.remove(path).map_err(ReclaimBindError::Remove)?;
                validate_after_unlink().map_err(ReclaimBindError::Validate)?;
                attempts += 1;
            }
            Err(error) => return Err(ReclaimBindError::Bind(error)),
        }
    }
}

/// Bind a Unix socket with bounded stale-file reclaim (Unix-only).
///
/// `max_reclaims` bounds the probe → unlink → retry loop (both
/// callers pass `3` — a racing starter may win the unlink window, so
/// retry instead of failing the first boot). `validate_after_unlink`
/// runs after every unlink and before the reclaim is counted: return
/// `Err` to refuse a reappeared path instead of retrying. Successful
/// binds return the standard listener unchanged.
#[cfg(unix)]
pub fn bind_unix_listener_with_stale_reclaim(
    path: &Path,
    max_reclaims: u32,
    validate_after_unlink: impl FnMut() -> std::io::Result<()>,
) -> Result<std::os::unix::net::UnixListener, ReclaimBindError> {
    struct LiveOps;
    impl ReclaimOps for LiveOps {
        type Listener = std::os::unix::net::UnixListener;
        fn bind(&self, path: &Path) -> std::io::Result<Self::Listener> {
            std::os::unix::net::UnixListener::bind(path)
        }
        fn probe_live(&self, path: &Path) -> bool {
            std::os::unix::net::UnixStream::connect(path).is_ok()
        }
        fn remove(&self, path: &Path) -> std::io::Result<()> {
            std::fs::remove_file(path)
        }
    }
    reclaim_with_ops(&LiveOps, path, max_reclaims, validate_after_unlink)
}
