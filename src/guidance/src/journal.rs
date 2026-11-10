//! Write-ahead intent log plus the reversible edit journal: every
//! applied patch records provenance, forward and reverse texts, and
//! gate results. A killed run reconciles by hash (applied, requeue, or
//! human edit), replays each content once, and never applies twice.
//!
//! The journal complements the in-memory work queue, which owns
//! scheduling, with the durable audit the queue never keeps.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fix::{apply_verified_patch, ApplyCtx, FixReport};
use crate::gates::GateVerdict;

/// Write-ahead intent: what one apply means to do, recorded before the
/// file moves so a crash can always reconcile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentRecord {
    /// Stable intent id.
    pub id: String,
    /// Target file, as passed in.
    pub file: String,
    /// Live hash the apply read.
    pub pre_hash: String,
    /// Hash the apply means to write.
    pub post_hash: String,
}

/// Restart verdict for one intent against the live hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconcile {
    /// Live bytes equal the post hash: the apply landed.
    Applied,
    /// Live bytes equal the pre hash: nothing landed, requeue.
    Requeue,
    /// Live bytes match neither: a human edited in between, drop the
    /// intent and requeue the item for fresh analysis.
    HumanEdit,
}

/// Reconcile one intent: post means applied, pre means requeue, anything
/// else means a human edit.
#[must_use]
pub fn reconcile_intent(record: &IntentRecord, live_hash: &str) -> Reconcile {
    if live_hash == record.post_hash {
        Reconcile::Applied
    } else if live_hash == record.pre_hash {
        Reconcile::Requeue
    } else {
        Reconcile::HumanEdit
    }
}

/// Persist one intent record (atomic write; retries overwrite by id).
pub fn write_intent(dir: &Path, record: &IntentRecord) -> std::io::Result<()> {
    if common_core::io::ensure_dir(dir).is_err() {
        return Err(std::io::Error::other("wal dir unavailable"));
    }
    let raw = serde_json::to_string_pretty(record).map_err(std::io::Error::other)?;
    common_core::io::write_atomic(&dir.join(format!("{}.json", record.id)), raw.as_bytes())
}

/// Read all intent records, sorted by id for determinism.
#[must_use]
pub fn read_intents(dir: &Path) -> Vec<IntentRecord> {
    let mut out = Vec::new();
    crate::walk::walk_files(dir, &["json"], |path| {
        if let Ok(raw) = common_core::io::read_to_string_err(path) {
            if let Ok(record) = serde_json::from_str::<IntentRecord>(&raw) {
                out.push(record);
            }
        }
    });
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Drop one intent record after its journal lands (or its apply reverts).
pub fn clear_intent(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
}

/// One journaled edit: provenance, both texts, and gate results. Full
/// texts (not diffs) make replay and undo total: replay writes forward
/// texts in order, undo writes the reverse text on a clean match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Content id: hash of file plus both texts, so identical appends
    /// deduplicate and replays converge.
    pub id: String,
    /// Target file, as passed in.
    pub file: String,
    /// Work-item id the edit served.
    pub item: String,
    /// Actor (`recipe` or model id plus prompt version).
    pub actor: String,
    /// Fix class (`rustfmt`, `import`, …).
    pub fix_class: String,
    /// Bytes before the edit.
    pub before_text: String,
    /// Bytes after the edit.
    pub after_text: String,
    /// Hash of the before text.
    pub before_hash: String,
    /// Hash of the after text.
    pub after_hash: String,
    /// Gate verdicts that certified the edit, in order.
    pub gates: Vec<GateVerdict>,
}

impl JournalEntry {
    /// Build an entry, hashing both texts for identity and reconcile.
    #[must_use]
    pub fn new(
        file: &str,
        item: &str,
        actor: &str,
        fix_class: &str,
        before_text: &str,
        after_text: &str,
        gates: Vec<GateVerdict>,
    ) -> Self {
        let before_hash = common_core::hash::sha256_hex(before_text.as_bytes());
        let after_hash = common_core::hash::sha256_hex(after_text.as_bytes());
        let id = common_core::hash::blake3_hex(
            format!("{file}\0{before_hash}\0{after_hash}").as_bytes(),
        );
        Self {
            id,
            file: file.to_string(),
            item: item.to_string(),
            actor: actor.to_string(),
            fix_class: fix_class.to_string(),
            before_text: before_text.to_string(),
            after_text: after_text.to_string(),
            before_hash,
            after_hash,
            gates,
        }
    }

    /// File basename for workspace-relative replay.
    #[must_use]
    pub fn file_name(&self) -> String {
        Path::new(&self.file)
            .file_name()
            .map_or_else(|| self.file.clone(), |name| name.to_string_lossy().into_owned())
    }
}

/// Append one entry, deduplicating by content id. Returns the entry id.
pub fn append_journal(dir: &Path, entry: &JournalEntry) -> String {
    if common_core::io::ensure_dir(dir).is_err() {
        return entry.id.clone();
    }
    let path = dir.join(format!("{}.json", entry.id));
    if path.exists() {
        return entry.id.clone();
    }
    if let Ok(raw) = serde_json::to_string_pretty(entry) {
        let _ = common_core::io::write_atomic(&path, raw.as_bytes());
    }
    entry.id.clone()
}

/// Read all journal entries, sorted by id for deterministic replay.
#[must_use]
pub fn read_journal(dir: &Path) -> Vec<JournalEntry> {
    let mut out = Vec::new();
    crate::walk::walk_files(dir, &["json"], |path| {
        if let Ok(raw) = common_core::io::read_to_string_err(path) {
            if let Ok(entry) = serde_json::from_str::<JournalEntry>(&raw) {
                out.push(entry);
            }
        }
    });
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// One review group: entries for a file and fix class.
#[derive(Debug, Clone)]
pub struct ReviewGroup {
    /// Target file.
    pub file: String,
    /// Fix class.
    pub fix_class: String,
    /// Entries in the group, in id order.
    pub entries: Vec<JournalEntry>,
}

/// Group journal entries by file and fix class, both sorted.
#[must_use]
pub fn review_journal(dir: &Path) -> Vec<ReviewGroup> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(String, String), Vec<JournalEntry>> = BTreeMap::new();
    for entry in read_journal(dir) {
        groups
            .entry((entry.file.clone(), entry.fix_class.clone()))
            .or_default()
            .push(entry);
    }
    groups
        .into_iter()
        .map(|((file, fix_class), entries)| ReviewGroup {
            file,
            fix_class,
            entries,
        })
        .collect()
}

/// Undo refusal: the live text no longer matches the entry, with why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct UndoConflict {
    /// Entry id that refused.
    pub id: String,
    /// Human-readable reason for the review surface.
    pub reason: String,
}

/// Undo one entry as a three-way operation: the reverse text applies
/// only when the live text still matches the forward text. Anything
/// else refuses with a conflict report instead of guessing.
pub fn undo_to_text(dir: &Path, id: &str, live_text: &str) -> Result<String, UndoConflict> {
    let refused = |reason: &str| UndoConflict {
        id: id.to_string(),
        reason: reason.to_string(),
    };
    let raw = common_core::io::read_to_string_err(&dir.join(format!("{id}.json")))
        .map_err(|_| refused("journal entry missing"))?;
    let entry: JournalEntry =
        serde_json::from_str(&raw).map_err(|_| refused("journal entry corrupt"))?;
    if live_text == entry.after_text {
        Ok(entry.before_text)
    } else {
        Err(refused("live text overlaps a later edit; undo refused"))
    }
}

/// Apply one patch with write-ahead intent and journaling: hash the
/// live file, record intent, certify through the gates, journal kept
/// edits, and always clear the intent. Reverted patches journal
/// nothing; the file stays byte-identical either way.
pub fn apply_journaled(
    path: &Path,
    new_text: &str,
    ctx: &ApplyCtx,
    journal_dir: &Path,
    wal_dir: &Path,
    item: &str,
    actor: &str,
) -> FixReport {
    let live = std::fs::read_to_string(path).unwrap_or_default();
    let pre_hash = common_core::hash::sha256_hex(live.as_bytes());
    let post_hash = common_core::hash::sha256_hex(new_text.as_bytes());
    let intent = IntentRecord {
        id: common_core::hash::blake3_hex(format!("{pre_hash}\0{post_hash}").as_bytes()),
        file: path.to_string_lossy().into_owned(),
        pre_hash: pre_hash.clone(),
        post_hash,
    };
    let _ = write_intent(wal_dir, &intent);
    let journaled = ApplyCtx {
        pre_hash,
        language: ctx.language,
        protected_globs: ctx.protected_globs,
        allow_pub_change: ctx.allow_pub_change,
        allow_cfg_change: ctx.allow_cfg_change,
        fix_class: ctx.fix_class,
        max_bytes: ctx.max_bytes,
        max_changed_lines: ctx.max_changed_lines,
        rustfmt_config: ctx.rustfmt_config.clone(),
        build: ctx.build.clone(),
        tests: ctx.tests.clone(),
        before_diags: ctx.before_diags,
        after_diags: ctx.after_diags,
    };
    let report = apply_verified_patch(path, new_text, &journaled);
    if report.is_kept() {
        let entry = JournalEntry::new(
            &intent.file,
            item,
            actor,
            ctx.fix_class,
            &live,
            new_text,
            report.gates.clone(),
        );
        append_journal(journal_dir, &entry);
    }
    clear_intent(wal_dir, &intent.id);
    report
}

/// Reconcile a whole wal directory after a restart: applied intents
/// stay done, the rest requeue by hash. Returns `(applied, requeue)`.
#[must_use]
pub fn reconcile_wal<S: std::hash::BuildHasher>(
    wal_dir: &Path,
    live_hashes: &std::collections::HashMap<String, String, S>,
) -> (Vec<IntentRecord>, Vec<IntentRecord>) {
    let mut applied = Vec::new();
    let mut requeue = Vec::new();
    for record in read_intents(wal_dir) {
        let live = live_hashes.get(&record.file).map_or("", String::as_str);
        match reconcile_intent(&record, live) {
            Reconcile::Applied => applied.push(record),
            Reconcile::Requeue | Reconcile::HumanEdit => requeue.push(record),
        }
    }
    (applied, requeue)
}

#[cfg(test)]
#[path = "../tests/m7_journal.rs"]
mod m7_journal_tests;
