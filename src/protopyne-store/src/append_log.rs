//! Durable CSV record log: the one append-only persistence primitive.
//!
//! [`AppendLog`] is generic over its record: the record trait supplies the
//! domain, the format version, the header, and the row encoding, so the
//! market tape and the venue journal share one durability rule instead of
//! each hand-rolling file handling. The rule is deterministic by
//! construction, never heuristic:
//!
//! - `append_batch` encodes every row in memory first, then performs one
//!   write, one flush, one fsync before reporting success. On a write
//!   failure the file truncates back to the pre-batch offset, so no partial
//!   batch ever lands.
//! - `open` replays forward. A single incomplete trailing record with no
//!   bytes after it is torn — dropped with a logged repair, truncating the
//!   file to the last whole record. A parse failure with more data after it
//!   is corruption and refuses loudly.
//!
//! Every method is synchronous and the handle holds no lock of its own: the
//! owner (the tape, the journal) serializes access under its own mutex, and
//! no method holds anything across an await.

use std::io::{Seek as _, Write as _};
use std::path::{Path, PathBuf};

use crate::error::StoreError;

/// One loggable record: its domain identity plus its row encoding.
///
/// The domain and version ride the log's first line (`<domain>,<version>`),
/// so opening a log written by another domain — or a newer format —
/// refuses instead of misreading rows.
pub trait AppendRecord: Sized {
    /// The log domain this record belongs to (e.g. `"protopyne_tape"`).
    fn domain() -> &'static str;
    /// The row-encoding version. Bumped only with a decoder that still
    /// reads every older version it claims.
    fn version() -> u32;
    /// The column header, written once when the log is created.
    fn header() -> Vec<&'static str>;
    /// Encode one record as its row fields, in header order.
    fn encode(&self) -> Vec<String>;
    /// Decode one row. The error names the offending field, never a
    /// defaulted value.
    fn decode(fields: &[&str]) -> Result<Self, String>;
}

/// A durable, append-only CSV record log at `path`.
///
/// Created by [`open`](Self::open) (which replays and repairs the torn tail
/// when the file already exists), extended by
/// [`append_batch`](Self::append_batch), read by [`read_all`](Self::read_all).
pub struct AppendLog<R> {
    path: PathBuf,
    file: FailFile,
    committed_len: u64,
    records: std::marker::PhantomData<R>,
}

impl<R> std::fmt::Debug for AppendLog<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppendLog")
            .field("path", &self.path)
            .field("committed_len", &self.committed_len)
            .finish_non_exhaustive()
    }
}

/// The append handle with a test-only write-failure injector: `fail_after`
/// caps the bytes the next writes may emit, so the batch-atomicity suite
/// can prove a mid-batch failure truncates cleanly. `None` in production.
struct FailFile {
    file: std::fs::File,
    fail_after: Option<u64>,
    written: u64,
}

impl FailFile {
    fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(limit) = self.fail_after {
            if self.written.saturating_add(bytes.len() as u64) > limit {
                return Err(std::io::Error::other("injected write failure"));
            }
        }
        self.file.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

impl<R: AppendRecord> AppendLog<R> {
    /// Open (or create) the log at `path`, replaying and repairing it.
    ///
    /// A missing or empty file is a fresh log: the version and header lines
    /// are written and synced. An existing file replays forward; a torn
    /// trailing record truncates with one logged repair, while mid-file
    /// corruption refuses loudly.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(common_core::error::IoError::from)?;
        }
        let missing_or_empty = std::fs::metadata(path).map_or(true, |meta| meta.len() == 0);
        if missing_or_empty {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .map_err(common_core::error::IoError::from)?;
            let bytes = header_bytes::<R>();
            file.write_all(&bytes).map_err(common_core::error::IoError::from)?;
            file.flush().map_err(common_core::error::IoError::from)?;
            file.sync_all().map_err(common_core::error::IoError::from)?;
            let committed_len = bytes.len() as u64;
            return Ok(Self {
                path: path.to_path_buf(),
                file: FailFile {
                    file,
                    fail_after: None,
                    written: committed_len,
                },
                committed_len,
                records: std::marker::PhantomData,
            });
        }
        let raw = std::fs::read(path).map_err(common_core::error::IoError::from)?;
        let (_records, good_len) = parse_forward::<R>(path, &raw)?;
        if good_len < raw.len() as u64 {
            tracing::warn!(
                path = %path.display(),
                dropped_bytes = raw.len() as u64 - good_len,
                "append log torn tail repaired: truncated to the last whole record"
            );
        }
        let file = std::fs::OpenOptions::new().append(true).open(path).map_err(common_core::error::IoError::from)?;
        let mut log = Self {
            path: path.to_path_buf(),
            file: FailFile {
                file,
                fail_after: None,
                written: good_len,
            },
            committed_len: good_len,
            records: std::marker::PhantomData,
        };
        if good_len < raw.len() as u64 {
            log.truncate_to(good_len)?;
        }
        if log.committed_len == 0 {
            // The tear took the header with it: rewrite it so the log
            // reads empty rather than corrupt.
            let bytes = header_bytes::<R>();
            log.file.file.write_all(&bytes).map_err(common_core::error::IoError::from)?;
            log.file.file.sync_all().map_err(common_core::error::IoError::from)?;
            log.file.written = bytes.len() as u64;
            log.committed_len = bytes.len() as u64;
        }
        Ok(log)
    }

    /// The log's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The committed length: the offset a reopen replays through.
    #[must_use]
    pub fn last_good_offset(&self) -> u64 {
        self.committed_len
    }

    /// Append every record atomically: one in-memory encode, one write, one
    /// flush, one fsync, then success. A write failure truncates back to the
    /// pre-batch offset, so the reopen shows the pre-batch state. An empty
    /// batch writes nothing and succeeds.
    pub fn append_batch(&mut self, records: &[R]) -> Result<(), StoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let encoded = {
            let mut writer = csv::WriterBuilder::new().has_headers(false).from_writer(Vec::new());
            for record in records {
                writer.write_record(record.encode()).map_err(|e| {
                    StoreError::InvalidBar(format!("log record does not encode: {e}"))
                })?;
            }
            writer.into_inner().map_err(|e| {
                StoreError::InvalidBar(format!("log record does not encode: {e}"))
            })?
        };
        let before = self.committed_len;
        if let Err(e) = self.file.write_chunk(&encoded).and_then(|()| {
            self.file.file.flush()?;
            self.file.file.sync_all()?;
            Ok(())
        }) {
            // Best-effort rollback: the batch must not leave a prefix
            // behind. A rollback failure is itself loud — the caller learns
            // the log needs attention rather than assuming atomicity.
            truncate_file_to(&mut self.file.file, before)
                .map_err(common_core::error::IoError::from)?;
            self.file.written = before;
            self.committed_len = before;
            return Err(StoreError::Io(common_core::error::IoError::from(e)));
        }
        self.committed_len = before + encoded.len() as u64;
        self.file.written = self.committed_len;
        Ok(())
    }

    /// Replay every committed record, in log order.
    pub fn read_all(&self) -> Result<Vec<R>, StoreError> {
        let raw = std::fs::read(&self.path).map_err(common_core::error::IoError::from)?;
        let (records, good_len) = parse_forward::<R>(&self.path, &raw)?;
        if good_len < raw.len() as u64 {
            return Err(StoreError::InvalidBar(format!(
                "log {} ends in a torn record: reopen to repair, refusing to read past it",
                self.path.display()
            )));
        }
        Ok(records)
    }

    /// Forget everything from `offset` on: truncate the file and move the
    /// committed length back. Truncating into the header (or to zero) drops
    /// the header too; the next [`open`](Self::open) rewrites it, so the
    /// log reads empty rather than corrupt.
    pub fn truncate_to(&mut self, offset: u64) -> Result<(), StoreError> {
        truncate_file_to(&mut self.file.file, offset).map_err(common_core::error::IoError::from)?;
        self.file.written = offset;
        self.committed_len = offset;
        Ok(())
    }

    /// Fsync the log without appending. For callers that wrote through
    /// another handle and need the durability promise stated once.
    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.file.file.flush().map_err(common_core::error::IoError::from)?;
        self.file.file.sync_all().map_err(common_core::error::IoError::from)?;
        Ok(())
    }

    /// Forget the whole log and start over: truncate to zero and rewrite
    /// the header, synced. The compaction operator calls this only after
    /// the seal artifact and its manifest row are durable; a crash before
    /// that point replays the untruncated log instead.
    pub fn truncate_all(&mut self) -> Result<(), StoreError> {
        truncate_file_to(&mut self.file.file, 0).map_err(common_core::error::IoError::from)?;
        let bytes = header_bytes::<R>();
        self.file.file.write_all(&bytes).map_err(common_core::error::IoError::from)?;
        self.file.file.sync_all().map_err(common_core::error::IoError::from)?;
        self.file.written = bytes.len() as u64;
        self.committed_len = bytes.len() as u64;
        Ok(())
    }

    /// Cap the bytes the next writes may emit: the batch-atomicity
    /// injector. Test-only; production writes never fail this way.
    #[cfg(test)]
    pub(crate) fn fail_after_bytes(&mut self, limit: u64) {
        self.file.fail_after = Some(limit);
        self.file.written = self.committed_len;
    }
}

/// Truncate the file to `offset` and park the cursor there: in append
/// mode the cursor otherwise survives the truncation and the next write
/// lands past the end, leaving a zero-filled gap the replay would read as
/// corruption.
fn truncate_file_to(file: &mut std::fs::File, offset: u64) -> std::io::Result<()> {
    file.set_len(offset)?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    file.sync_all()
}

/// The version + header lines every log opens with. Flexible by
/// construction: the version line is two fields, the header is the
/// record's width, and data rows are never written through this writer.
fn header_bytes<R: AppendRecord>() -> Vec<u8> {
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_writer(Vec::new());
    writer
        .write_record([R::domain(), &R::version().to_string()])
        .expect("version line encodes");
    writer.write_record(R::header()).expect("header encodes");
    writer.into_inner().expect("header fits in memory")
}

/// Replay `raw` forward: the committed records plus the length they commit
/// through. A trailing chunk without its terminator is torn (excluded, never
/// an error); any other parse failure names its line and refuses.
fn parse_forward<R: AppendRecord>(path: &Path, raw: &[u8]) -> Result<(Vec<R>, u64), StoreError> {
    let corrupt = |reason: String| {
        StoreError::InvalidBar(format!("log {} is corrupt: {reason}", path.display()))
    };
    // Line-split without UTF-8 assumptions on the torn tail: the tail is
    // measured in bytes, decoded only when whole.
    let mut chunks: Vec<&[u8]> = raw.split(|b| *b == b'\n').collect();
    let torn = if raw.is_empty() || raw.ends_with(b"\n") {
        false
    } else {
        chunks.pop();
        true
    };
    // A trailing empty chunk is the terminator's shadow, not a record.
    if chunks.last().is_some_and(|last| last.is_empty()) {
        chunks.pop();
    }
    if chunks.is_empty() {
        // Nothing committed: an empty file, or a tear inside the version
        // line. The caller truncates (a no-op) and rewrites the header.
        return Ok((Vec::new(), 0));
    }
    // The version line commits first and alone: a mismatch here is
    // corruption, never a tear — the writer fsyncs it whole.
    let version = csv_record(chunks[0])
        .ok_or_else(|| corrupt("line 1 holds no record".to_owned()))?;
    let domain = version.first().map_or("", String::as_str);
    let file_version = version.get(1).map_or("", String::as_str);
    if domain != R::domain() || file_version != R::version().to_string() {
        return Err(corrupt(format!(
            "line 1 names domain {domain:?} version {file_version:?}, expected {:?} version {}",
            R::domain(),
            R::version()
        )));
    }
    if chunks.len() < 2 {
        // The header never committed: truncate to nothing and rewrite it.
        return Ok((Vec::new(), 0));
    }
    let header_owned = csv_record(chunks[1])
        .ok_or_else(|| corrupt("line 2 holds no record".to_owned()))?;
    let header: Vec<&str> = header_owned.iter().map(String::as_str).collect();
    if header != R::header() {
        return Err(corrupt(format!(
            "line 2 header {header:?} does not match {:?}",
            R::header()
        )));
    }
    let mut records = Vec::new();
    let mut offset: u64 = chunks[0].len() as u64 + 1 + chunks[1].len() as u64 + 1;
    for (index, chunk) in chunks.iter().enumerate().skip(2) {
        let line_no = index + 1;
        let cells_owned = csv_record(chunk)
            .ok_or_else(|| corrupt(format!("line {line_no} holds no record")))?;
        let cells: Vec<&str> = cells_owned.iter().map(String::as_str).collect();
        records.push(R::decode(&cells).map_err(|e| corrupt(format!("line {line_no}: {e}")))?);
        offset += chunk.len() as u64 + 1;
    }
    // `offset` now marks the end of the last whole record: the committed
    // length when something is torn, the whole file otherwise.
    let good_len = if torn { offset } else { raw.len() as u64 };
    Ok((records, good_len))
}

/// One CSV line as its fields. `None` when the line holds no record.
fn csv_record(line: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(line).ok()?;
    let record = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(text.as_bytes())
        .into_records()
        .next()?;
    record.ok().map(|fields| fields.iter().map(str::to_owned).collect())
}

#[cfg(test)]
#[path = "../tests/common/append_log_fixtures.rs"]
pub(crate) mod test_fixtures;

#[cfg(test)]
#[path = "../tests/append_log.rs"]
mod tests;
