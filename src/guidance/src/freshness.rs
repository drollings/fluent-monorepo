//! P3 freshness contract (port of zvec-grep's `fresh` / `possibly_stale`
//! plus the `background|wait|off` serving matrix).
//!
//! Revision comparison decides freshness; the mode decides whether a
//! stale read triggers a background reconcile, waits for it, or serves
//! stale directly. Server default is `Background`, direct CLI reads are
//! `Off` (P5 wiring).
//!
//! Single decision home (M12): this module owns every freshness decision —
//! the revision matrix ([`FreshnessGate`]), the mtime skew table
//! ([`mtime_skew`]), and the content tiebreak ([`hash_decides_fresh`]).
//! The `sync::staleness` sidecar helpers are I/O shells over this core
//! (mtime fetching); they add no decision logic of their own.

use std::time::{Duration, SystemTime};

/// Serving mode for stale reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Serving mode for stale reads.
pub enum FreshnessMode {
    /// Enqueue a reconcile, serve the stale index now.
    #[default]
    Background,
    /// Enqueue a reconcile, block until fresh, then serve.
    Wait,
    /// Serve whatever is indexed; never trigger work.
    Off,
    /// Refuse anything but fresh via `try_serve`; `serve` keeps its
    /// labelled shape so existing callers stay byte-identical.
    Strict,
}

/// Typed strict refusal: the read was valid but not fresh enough.
/// Carries why, never the content that was withheld.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Typed strict refusal: the read was valid but not fresh enough.
/// Carries why, never the content that was withheld.
pub struct StrictRefusal {
    /// Machine-readable reason (`pending`, `stale`, `dependent-stale`).
    pub reason: String,
}

impl StrictRefusal {
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Freshness of the index behind a read.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Freshness of the index behind a read.
pub enum Freshness {
    /// Indexed revision covers all watched changes.
    Fresh {
        /// Serving revision.
        revision: u64,
    },
    /// Changes arrived after the last indexed revision.
    PossiblyStale {
        /// Last indexed revision.
        indexed_revision: u64,
        /// Newest dirty revision.
        dirty_revision: u64,
    },
    /// An open revision exists the pipeline has not landed yet.
    /// Producer state: the pipeline doubts its own completeness.
    Pending {
        /// Open revision being reconciled.
        revision: u64,
    },
    /// Own bytes look clean but a dependency signature changed.
    /// Task value: serving from here would be wrong even though this
    /// file looks fresh, so it must re-derive.
    DependentStale {
        /// Last indexed revision.
        indexed_revision: u64,
        /// Newest dirty revision (often equal: the dirt is elsewhere).
        dirty_revision: u64,
    },
}

impl Freshness {
    /// Fresh and servable under every mode.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        matches!(self, Freshness::Fresh { .. })
    }

    /// Open revision outstanding; a reader should wait or serve the
    /// last verified answer with a pending label.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self, Freshness::Pending { .. })
    }

    /// Clean locally, dirty transitively; re-derive, do not serve.
    #[must_use]
    pub fn is_dependent_stale(&self) -> bool {
        matches!(self, Freshness::DependentStale { .. })
    }

    /// Needs work under any mode: stale, pending, or dependent-stale.
    #[must_use]
    pub fn needs_work(&self) -> bool {
        !self.is_fresh()
    }
}

/// The serving matrix: pure decision + caller-supplied effects (trigger,
/// wait, fetch), so the contract unit-tests without I/O.
#[derive(Debug, Clone, Copy)]
/// The serving matrix: pure decision + caller-supplied effects (trigger,
/// wait, fetch), so the contract unit-tests without I/O.
pub struct FreshnessGate {
    mode: FreshnessMode,
}

impl FreshnessGate {
    /// Build a gate for one serving mode.
    #[must_use]
    pub fn new(mode: FreshnessMode) -> Self {
        Self { mode }
    }

    /// Classify without side effects.
    #[must_use]
    pub fn check(&self, indexed_revision: u64, dirty_revision: u64) -> Freshness {
        if dirty_revision <= indexed_revision {
            Freshness::Fresh {
                revision: indexed_revision,
            }
        } else {
            Freshness::PossiblyStale {
                indexed_revision,
                dirty_revision,
            }
        }
    }

    /// Classify with an in-flight revision and a dependent-change flag.
    /// In-flight wins: an open revision means pending even when the
    /// local revisions look fresh. Otherwise a clean local pair with a
    /// changed dependency means dependent-stale. Falls back to `check`.
    #[must_use]
    pub fn check_extended(
        &self,
        indexed_revision: u64,
        dirty_revision: u64,
        inflight_revision: Option<u64>,
        dependent_changed: bool,
    ) -> Freshness {
        if let Some(open) = inflight_revision {
            if open > indexed_revision {
                return Freshness::Pending { revision: open };
            }
        }
        if dependent_changed && dirty_revision <= indexed_revision {
            return Freshness::DependentStale {
                indexed_revision,
                dirty_revision,
            };
        }
        self.check(indexed_revision, dirty_revision)
    }

    /// Serve one read through the matrix: `trigger` enqueues a reconcile,
    /// `wait_for_fresh` blocks until the reconcile lands and reports the
    /// fresh revision, `fetch` serves under the resulting freshness.
    /// `Strict` keeps the labelled shape here; strict refusal lives in
    /// `try_serve` so existing callers stay byte-identical.
    pub fn serve<T>(
        &self,
        indexed_revision: u64,
        dirty_revision: u64,
        trigger: impl FnOnce(),
        wait_for_fresh: impl FnOnce() -> u64,
        fetch: impl FnOnce(Freshness) -> T,
    ) -> T {
        match self.check(indexed_revision, dirty_revision) {
            Freshness::Fresh { .. } => fetch(Freshness::Fresh {
                revision: indexed_revision,
            }),
            stale @ Freshness::PossiblyStale { .. } => match self.mode {
                FreshnessMode::Off | FreshnessMode::Strict => fetch(stale),
                FreshnessMode::Background => {
                    trigger();
                    fetch(stale)
                }
                FreshnessMode::Wait => {
                    trigger();
                    let revision = wait_for_fresh();
                    fetch(Freshness::Fresh { revision })
                }
            },
            pending @ Freshness::Pending { .. } => match self.mode {
                FreshnessMode::Off | FreshnessMode::Strict => fetch(pending),
                FreshnessMode::Background => {
                    trigger();
                    fetch(pending)
                }
                FreshnessMode::Wait => {
                    trigger();
                    let revision = wait_for_fresh();
                    fetch(Freshness::Fresh { revision })
                }
            },
            dependent @ Freshness::DependentStale { .. } => match self.mode {
                FreshnessMode::Off | FreshnessMode::Strict => fetch(dependent),
                FreshnessMode::Background => {
                    trigger();
                    fetch(dependent)
                }
                FreshnessMode::Wait => {
                    trigger();
                    let revision = wait_for_fresh();
                    fetch(Freshness::Fresh { revision })
                }
            },
        }
    }

    /// Strict read: fresh serves, anything else refuses with a reason.
    /// Task-value gate: refusal means the answer would be wrong if
    /// served, not that the producer is unsure.
    pub fn try_serve<T>(
        &self,
        freshness: Freshness,
        trigger: impl FnOnce(),
        wait_for_fresh: impl FnOnce() -> u64,
        fetch: impl FnOnce(Freshness) -> T,
    ) -> Result<T, StrictRefusal> {
        match freshness {
            Freshness::Fresh { .. } => Ok(fetch(freshness)),
            Freshness::Pending { .. } => {
                if self.mode == FreshnessMode::Wait {
                    trigger();
                    let revision = wait_for_fresh();
                    Ok(fetch(Freshness::Fresh { revision }))
                } else if self.mode == FreshnessMode::Strict {
                    Err(StrictRefusal {
                        reason: "pending".to_string(),
                    })
                } else {
                    trigger();
                    Ok(fetch(freshness))
                }
            }
            Freshness::PossiblyStale { .. } => {
                if self.mode == FreshnessMode::Strict {
                    Err(StrictRefusal {
                        reason: "stale".to_string(),
                    })
                } else {
                    Ok(self.serve_from(freshness, trigger, wait_for_fresh, fetch))
                }
            }
            Freshness::DependentStale { .. } => {
                if self.mode == FreshnessMode::Strict {
                    Err(StrictRefusal {
                        reason: "dependent-stale".to_string(),
                    })
                } else {
                    trigger();
                    Ok(fetch(freshness))
                }
            }
        }
    }

    fn serve_from<T>(
        self,
        freshness: Freshness,
        trigger: impl FnOnce(),
        wait_for_fresh: impl FnOnce() -> u64,
        fetch: impl FnOnce(Freshness) -> T,
    ) -> T {
        match self.mode {
            FreshnessMode::Wait => {
                trigger();
                let revision = wait_for_fresh();
                fetch(Freshness::Fresh { revision })
            }
            FreshnessMode::Background => {
                trigger();
                fetch(freshness)
            }
            FreshnessMode::Off | FreshnessMode::Strict => fetch(freshness),
        }
    }
}

/// Whether a file whose own bytes look clean must still re-derive
/// because a dependency signature changed. Pure membership input from
/// the invalidation closure; task value, never producer confidence.
#[must_use]
/// Whether a file whose own bytes look clean must still re-derive
/// because a dependency signature changed. Pure membership input from
/// the invalidation closure; task value, never producer confidence.
pub fn is_dependent_stale(dependent_changed: bool, own_fresh: bool) -> bool {
    dependent_changed && own_fresh
}

/// Mtime skew classification: the single decision table for the 1-second
/// timestamp-resolution rule (M12). Pure over `SystemTime` pairs — the
/// `sync::staleness` shells fetch the mtimes, this table classifies them,
/// so the stale/ambiguous partition cannot drift between two mirrored
/// predicates.
///
/// The window measures *producer self-doubt* (filesystem timestamp
/// granularity), never task value. Inside it the clock is evidence-free,
/// so the content hash ([`hash_decides_fresh`]) decides instead. The
/// threshold gates which evidence to consult, never the conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Mtime skew classification: the single decision table for the 1-second
/// timestamp-resolution rule (M12). Pure over `SystemTime` pairs — the
/// `sync::staleness` shells fetch the mtimes, this table classifies them,
/// so the stale/ambiguous partition cannot drift between two mirrored
/// predicates.
///
/// The window measures *producer self-doubt* (filesystem timestamp
/// granularity), never task value. Inside it the clock is evidence-free,
/// so the content hash ([`hash_decides_fresh`]) decides instead. The
/// threshold gates which evidence to consult, never the conclusion.
pub enum MtimeSkew {
    /// Source strictly older than the sidecar: no work (both legacy
    /// predicates read false).
    SourceOlder,
    /// Source at-or-newer but within the resolution window: the clock is
    /// evidence-free, consult the content hash.
    Ambiguous,
    /// Source newer beyond the window: stale, regenerate.
    SourceNewer,
}

/// Classify one `(sidecar, source)` mtime pair. Equal mtimes land
/// [`MtimeSkew::Ambiguous`] (zero skew is evidence-free, matching the
/// legacy window predicate exactly).
#[must_use]
/// Classify one `(sidecar, source)` mtime pair. Equal mtimes land
/// [`MtimeSkew::Ambiguous`] (zero skew is evidence-free, matching the
/// legacy window predicate exactly).
pub fn mtime_skew(json_mtime: SystemTime, source_mtime: SystemTime) -> MtimeSkew {
    mtime_skew_with_granularity(json_mtime, source_mtime, std::time::Duration::from_secs(1))
}

/// Classify one `(sidecar, source)` mtime pair under an explicit
/// granularity. Equal mtimes and skews within the window land
/// [`MtimeSkew::Ambiguous`]: the clock is evidence-free there, so the
/// content hash decides instead. `mtime_skew` is this with the 1-second
/// default window, byte-identical.
#[must_use]
/// Classify one `(sidecar, source)` mtime pair under an explicit
/// granularity. Equal mtimes and skews within the window land
/// [`MtimeSkew::Ambiguous`]: the clock is evidence-free there, so the
/// content hash decides instead. `mtime_skew` is this with the 1-second
/// default window, byte-identical.
pub fn mtime_skew_with_granularity(
    json_mtime: SystemTime,
    source_mtime: SystemTime,
    granularity: std::time::Duration,
) -> MtimeSkew {
    match source_mtime.duration_since(json_mtime) {
        Err(_) => MtimeSkew::SourceOlder,
        Ok(skew) if skew > granularity => MtimeSkew::SourceNewer,
        Ok(_) => MtimeSkew::Ambiguous,
    }
}

/// Probe the filesystem timestamp granularity behind `dir`: round-trip a
/// timestamp with distinctive sub-second digits and read what survives.
/// Missing prefixes resolve to the nearest existing ancestor (same
/// filesystem in the normal case); unreadable layouts fall back to 1s,
/// the safe direction — a wider window only spends hashes, never serves
/// stale as fresh.
#[must_use]
/// Probe the filesystem timestamp granularity behind `dir`: round-trip a
/// timestamp with distinctive sub-second digits and read what survives.
/// Missing prefixes resolve to the nearest existing ancestor (same
/// filesystem in the normal case); unreadable layouts fall back to 1s,
/// the safe direction — a wider window only spends hashes, never serves
/// stale as fresh.
pub fn probe_timestamp_granularity(dir: &std::path::Path) -> std::time::Duration {
    use std::time::Duration;
    let mut anchor = dir.to_path_buf();
    while !anchor.exists() {
        match anchor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => anchor = parent.to_path_buf(),
            _ => return Duration::from_secs(1),
        }
    }
    let probe = anchor.join(".guidance-granularity-probe");
    // Odd-second timestamp with distinctive nanos: each granularity
    // below truncates it to a different observable value.
    let now = SystemTime::now();
    let wanted = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(now, |elapsed| {
            let odd = if elapsed.as_secs() % 2 == 0 {
                elapsed.as_secs() + 1
            } else {
                elapsed.as_secs()
            };
            SystemTime::UNIX_EPOCH + std::time::Duration::new(odd, 123_456_789)
        });
    let back = (|| {
        std::fs::write(&probe, b"probe").ok()?;
        std::fs::File::options()
            .write(true)
            .open(&probe)
            .ok()?
            .set_modified(wanted)
            .ok()?;
        std::fs::metadata(&probe).ok()?.modified().ok()
    })();
    let _ = std::fs::remove_file(&probe);
    let Some(seen) = back else {
        return Duration::from_secs(1);
    };
    if seen == wanted {
        return Duration::from_nanos(1);
    }
    if system_time_secs(seen) == system_time_secs(wanted) {
        return Duration::from_secs(1);
    }
    Duration::from_secs(2)
}

/// Truncate a `SystemTime` down to whole seconds.
fn system_time_secs(time: SystemTime) -> std::time::Duration {
    Duration::from_secs(
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

/// Regen decision over clock plus content evidence: outside the window
/// the mtimes rule with zero hash cost; inside it the content hash
/// decides, so identical bytes never regen and differing bytes always do.
#[must_use]
/// Regen decision over clock plus content evidence: outside the window
/// the mtimes rule with zero hash cost; inside it the content hash
/// decides, so identical bytes never regen and differing bytes always do.
pub fn regen_decision(
    sidecar_mtime: SystemTime,
    source_mtime: SystemTime,
    granularity: std::time::Duration,
    hashes_equal: bool,
) -> bool {
    regen_decision_with_dependents(
        sidecar_mtime,
        source_mtime,
        granularity,
        hashes_equal,
        false,
    )
}

/// Regen decision with a dependent-change input: a clean local pair
/// still regenerates when a dependency signature changed elsewhere.
/// Task value (the receipt would be wrong), never producer doubt.
#[must_use]
/// Regen decision with a dependent-change input: a clean local pair
/// still regenerates when a dependency signature changed elsewhere.
/// Task value (the receipt would be wrong), never producer doubt.
pub fn regen_decision_with_dependents(
    sidecar_mtime: SystemTime,
    source_mtime: SystemTime,
    granularity: std::time::Duration,
    hashes_equal: bool,
    dependent_changed: bool,
) -> bool {
    if dependent_changed && hashes_equal {
        return true;
    }
    match mtime_skew_with_granularity(sidecar_mtime, source_mtime, granularity) {
        MtimeSkew::SourceNewer => true,
        MtimeSkew::SourceOlder => false,
        MtimeSkew::Ambiguous => !hashes_equal,
    }
}

/// Complete one sidecar: stamp it strictly newer than its source at
/// `max(now, source + granularity)`, always, even when nothing changed.
/// The stamp is the idempotence hatch — a revalidated file costs one
/// hash plus one timestamp, never lint or compile work downstream.
pub fn completion_touch(
    sidecar: &std::path::Path,
    source: &std::path::Path,
    granularity: std::time::Duration,
) -> std::io::Result<()> {
    let mut target = SystemTime::now();
    if let Some(source_mtime) = common_core::io::mtime(source) {
        target = target.max(source_mtime + granularity);
    }
    std::fs::File::options()
        .write(true)
        .open(sidecar)?
        .set_modified(target)?;
    Ok(())
}

/// Whether a sidecar carries a stuck record: the loop escalated the
/// file and holds it for a human. Header-only read.
#[must_use]
/// Whether a sidecar carries a stuck record: the loop escalated the
/// file and holds it for a human. Header-only read.
pub fn sidecar_stuck(sidecar: &std::path::Path) -> bool {
    use std::io::BufRead as _;
    let Ok(file) = std::fs::File::open(sidecar) else {
        return false;
    };
    let mut lines = std::io::BufReader::new(file).lines();
    let Some(Ok(line)) = lines.next() else {
        return false;
    };
    fluent_types::sidecar::SidecarHeader::parse_line(&line).is_some_and(|header| header.stuck)
}

/// Mark a sidecar stuck without touching its body: the record explains
/// why the file stays incomplete while the timestamp (set separately
/// by `backdate_sidecar`) keeps the mtime verdict truthful. A later
/// clean save rewrites the header and clears the flag. `false` when
/// no parseable header exists to mark.
pub fn mark_stuck_sidecar(sidecar: &std::path::Path) -> bool {
    let Ok(raw) = common_core::io::read_to_string_err(sidecar) else {
        return false;
    };
    let Some((header, _)) = raw.split_once('\n') else {
        return false;
    };
    let Some(mut parsed) = fluent_types::sidecar::SidecarHeader::parse_line(header) else {
        return false;
    };
    if parsed.stuck {
        return true;
    }
    parsed.stuck = true;
    let marked = format!(
        "{}\n{}",
        parsed.render(),
        raw.split_once('\n').map_or("", |(_, rest)| rest)
    );
    common_core::io::write_atomic(sidecar, marked.as_bytes()).is_ok()
}

/// Backdate one sidecar below its source: the stuck/invalidate signal.
/// Invalidation moves the sidecar timestamp, never the source. `false`
/// when the source is missing or the stamp cannot move.
#[must_use]
/// Backdate one sidecar below its source: the stuck/invalidate signal.
/// Invalidation moves the sidecar timestamp, never the source. `false`
/// when the source is missing or the stamp cannot move.
pub fn backdate_sidecar(sidecar: &std::path::Path, source: &std::path::Path) -> bool {
    let Some(source_mtime) = common_core::io::mtime(source) else {
        return false;
    };
    let target = source_mtime
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    std::fs::File::options()
        .write(true)
        .open(sidecar)
        .and_then(|file| file.set_modified(target))
        .is_ok()
}

/// Adopt an orphaned sidecar after a rename: when the expected sidecar
/// for `source_path` is missing and exactly one orphan carries the same
/// source hash, move it instead of reprocessing. Returns the moved-to
/// path, or `None` when there is nothing (or nothing unambiguous) to do.
#[must_use]
/// Adopt an orphaned sidecar after a rename: when the expected sidecar
/// for `source_path` is missing and exactly one orphan carries the same
/// source hash, move it instead of reprocessing. Returns the moved-to
/// path, or `None` when there is nothing (or nothing unambiguous) to do.
pub fn adopt_renamed_sidecar(
    sidecar_dir: &std::path::Path,
    source_dir: &std::path::Path,
    source_path: &std::path::Path,
    source_hash: &str,
) -> Option<std::path::PathBuf> {
    let rel = source_path.strip_prefix(source_dir).unwrap_or(source_path);
    let expected = sidecar_dir.join(format!("{}.json", rel.display()));
    if expected.exists() {
        return None;
    }
    let mut sidecars = Vec::new();
    crate::walk::walk_files(sidecar_dir, &["json"], |path| {
        sidecars.push(path.to_path_buf());
    });
    sidecars.sort();
    let mut orphans = Vec::new();
    for candidate in &sidecars {
        let rel = candidate
            .strip_prefix(sidecar_dir)
            .unwrap_or(candidate)
            .to_string_lossy();
        let rel = rel.strip_suffix(".json").unwrap_or(&rel);
        if source_dir.join(rel).exists() {
            continue;
        }
        let first = (|| {
            use std::io::BufRead as _;
            let file = std::fs::File::open(candidate).ok()?;
            std::io::BufReader::new(file).lines().next()?.ok()
        })();
        let Some(line) = first else {
            continue;
        };
        let matches = fluent_types::sidecar::SidecarHeader::parse_line(&line)
            .and_then(|header| header.src_hash)
            .is_some_and(|hash| hash == source_hash);
        if matches {
            orphans.push(candidate.clone());
        }
    }
    if orphans.len() != 1 {
        return None;
    }
    if let Some(parent) = expected.parent() {
        if common_core::io::ensure_dir(parent).is_err() {
            return None;
        }
    }
    std::fs::rename(&orphans[0], &expected).ok()?;
    Some(expected)
}

/// Audit verdict for one sidecar against live bytes and mtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Audit verdict for one sidecar against live bytes and mtimes.
pub enum VerifyVerdict {
    /// Hashes and clock agree the sidecar is complete.
    Fresh,
    /// Sidecar predates its inputs; revalidate through the loop.
    Stale,
    /// Source bytes moved under a preserved-or-older mtime (restore,
    /// `cp -p`, backup): the clock reads fresh, the hashes refuse it.
    SourceChanged,
    /// Source bytes moved while the clock claims complete-or-newer
    /// (forged mtime): costs work, never a wrong citation.
    SkewSuspect,
    /// Sidecar body no longer matches its own header receipt.
    Tampered,
    /// No header or no source to audit against.
    Unknown,
}

/// Audit hashes against mtimes for skew, restores, and forgeries. The
/// clock is evidence; the hashes decide.
#[must_use]
/// Audit hashes against mtimes for skew, restores, and forgeries. The
/// clock is evidence; the hashes decide.
pub fn verify_sidecar(
    sidecar: &std::path::Path,
    source: &std::path::Path,
    granularity: std::time::Duration,
) -> VerifyVerdict {
    let Ok(content) = common_core::io::read_to_string_err(sidecar) else {
        return VerifyVerdict::Unknown;
    };
    let (header, body) = fluent_types::sidecar::strip_header(&content);
    let Some(header) = header else {
        return VerifyVerdict::Unknown;
    };
    if common_core::hash::sha256_hex(body.as_bytes()) != header.body {
        return VerifyVerdict::Tampered;
    }
    let (Some(sidecar_mtime), Some(source_mtime)) = (
        common_core::io::mtime(sidecar),
        common_core::io::mtime(source),
    ) else {
        return VerifyVerdict::Unknown;
    };
    let clock = mtime_skew_with_granularity(sidecar_mtime, source_mtime, granularity);
    let Some(recorded) = header.src_hash else {
        return match clock {
            MtimeSkew::SourceOlder => VerifyVerdict::Fresh,
            MtimeSkew::SourceNewer | MtimeSkew::Ambiguous => VerifyVerdict::Stale,
        };
    };
    let live = std::fs::read(source)
        .ok()
        .map(|bytes| common_core::hash::sha256_hex(&bytes));
    if live.as_deref() == Some(recorded.as_str()) {
        return match clock {
            MtimeSkew::SourceNewer => VerifyVerdict::Stale,
            _ => VerifyVerdict::Fresh,
        };
    }
    match clock {
        MtimeSkew::SourceOlder => VerifyVerdict::SourceChanged,
        _ => VerifyVerdict::SkewSuspect,
    }
}

/// Content tiebreak for the ambiguity window: the fresh source sha256
/// against the stored one (the `compute_diff` precedent — same hasher,
/// `common_core::hash::sha256_hex` — never a second hasher). `None`
/// stored means no evidence: fail open (not fresh — regenerate), never
/// serve ambiguous content as fresh.
///
/// Axis statement: the hash measures *task correctness* (bytes changed),
/// the complement of the window's self-doubt. It gates a regen decision
/// only — never cached, never persisted, never served as data.
///
/// Moved verbatim from `sync::staleness` (M12 single home); that module
/// re-resolves it for its inline tests.
#[must_use]
/// Content tiebreak for the ambiguity window: the fresh source sha256
/// against the stored one (the `compute_diff` precedent — same hasher,
/// `common_core::hash::sha256_hex` — never a second hasher). `None`
/// stored means no evidence: fail open (not fresh — regenerate), never
/// serve ambiguous content as fresh.
///
/// Axis statement: the hash measures *task correctness* (bytes changed),
/// the complement of the window's self-doubt. It gates a regen decision
/// only — never cached, never persisted, never served as data.
///
/// Moved verbatim from `sync::staleness` (M12 single home); that module
/// re-resolves it for its inline tests.
pub fn hash_decides_fresh(current_hash: &str, stored_hash: Option<&str>) -> bool {
    stored_hash.is_some_and(|want| want == current_hash)
}

#[cfg(test)]
#[path = "../tests/freshness_vocab.rs"]
mod freshness_vocab_tests;

#[cfg(test)]
#[path = "../tests/freshness.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/m2_mtime.rs"]
mod m2_mtime_tests;
