//! P3 scheduler tests: the coalescing core the `IndexCoordinator`
//! composes (per-root active + followup, retryable retry, wait, idle,
//! close). Full daemon `job-scheduler` port lands in P5.

use crate::scheduler::{
    BoxFuture, JobContext, JobError, JobReason, JobRun, JobScheduler, JobState, SubmitJob,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn run_ok() -> JobRun {
    Arc::new(|_| Box::pin(async { Ok(()) }) as BoxFuture<Result<(), JobError>>)
}

#[tokio::test]
async fn submit_runs_to_succeeded_and_wait_observes_it() {
    let scheduler = JobScheduler::new(1, 1, 1);
    let submitted = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: run_ok(),
    });
    assert!(!submitted.reused);
    let snapshot = scheduler.wait(submitted.job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Succeeded);
    assert_eq!(snapshot.attempt, 1);
    scheduler.close();
}

#[tokio::test]
async fn submit_while_running_coalesces_into_one_followup() {
    let scheduler = JobScheduler::new(1, 3, 1);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let mk_run = || {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let runs = Arc::clone(&runs);
        Arc::new(move |_: JobContext| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let runs = Arc::clone(&runs);
            Box::pin(async move {
                if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered.notify_one();
                    release.notified().await;
                }
                Ok(())
            }) as BoxFuture<Result<(), JobError>>
        })
    };
    let _first = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: mk_run(),
    });
    entered.notified().await;
    let second = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: mk_run(),
    });
    assert!(second.reused);
    let third = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: mk_run(),
    });
    assert!(third.reused);
    release.notify_one();
    scheduler.wait_for_root_idle("/repo").await;
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    scheduler.close();
}

#[tokio::test]
async fn retryable_errors_retry_the_same_run() {
    let scheduler = JobScheduler::new(1, 3, 1);
    let attempts = Arc::new(AtomicUsize::new(0));
    let run_attempts = Arc::clone(&attempts);
    let submitted = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: Arc::new(move |_: JobContext| {
            let run_attempts = Arc::clone(&run_attempts);
            Box::pin(async move {
                let attempt = run_attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    return Err(JobError::retryable("busy"));
                }
                Ok(())
            }) as BoxFuture<Result<(), JobError>>
        }),
    });
    let snapshot = scheduler.wait(submitted.job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Succeeded);
    assert_eq!(snapshot.attempt, 2);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    scheduler.close();
}

#[tokio::test]
async fn terminal_errors_fail_without_retry() {
    let scheduler = JobScheduler::new(1, 3, 1);
    let submitted = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: Arc::new(|_: JobContext| {
            Box::pin(async { Err(JobError::terminal("fatal")) })
                as BoxFuture<Result<(), JobError>>
        }),
    });
    let snapshot = scheduler.wait(submitted.job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Failed);
    assert_eq!(snapshot.attempt, 1);
    scheduler.close();
}

#[tokio::test]
async fn close_cancels_queued_followups() {
    let scheduler = JobScheduler::new(1, 1, 1);
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let entered_run = Arc::clone(&entered);
    let gate_run = Arc::clone(&gate);
    let _first = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: Arc::new(move |_: JobContext| {
            let entered_run = Arc::clone(&entered_run);
            let gate_run = Arc::clone(&gate_run);
            Box::pin(async move {
                entered_run.notify_one();
                gate_run.notified().await;
                Ok(())
            }) as BoxFuture<Result<(), JobError>>
        }),
    });
    entered.notified().await;
    // Queued behind the running first job as its followup.
    let second = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: run_ok(),
    });
    assert!(second.reused);
    tokio::time::sleep(Duration::from_millis(20)).await;
    scheduler.close();
    gate.notify_one();
    let snapshot = scheduler.wait(second.job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Cancelled, "{snapshot:?}");
}

// --- M4.3 pin: scheduler delay composes backoff_ms exactly ---
#[test]
fn m4_scheduler_delay_matches_backoff_composition() {
    // Historical formula `base * 2^min(attempt-1, 8)` vs the migrated
    // `backoff_ms(base, min(attempt, 9), 0)`: bit-equal on the whole u32
    // domain, so the M4.3 migration is behavior-preserving by construction.
    // If this ever diverges, behavior wins: revert, do not "fix" the pin.
    for base in [0u64, 1, 100, 500] {
        for attempt in (0u32..=20).chain([21, 100, u32::MAX]) {
            let historical = base.saturating_mul(
                2u64.pow(attempt.saturating_sub(1).min(8)),
            );
            let composed =
                common_core::retry::backoff_ms(base, attempt.min(9), 0);
            assert_eq!(historical, composed, "base {base} attempt {attempt}");
        }
    }
}

// --- M11.1 characterization: exhaustion, close fail-fast, followup merge ---

#[tokio::test]
async fn retryable_exhaustion_fails_at_max_attempts() {
    // Always-retryable body with max_attempts=3: exactly 3 runs, then
    // Failed (no fourth attempt, no success).
    let scheduler = JobScheduler::new(1, 3, 1);
    let runs = Arc::new(AtomicUsize::new(0));
    let run_count = Arc::clone(&runs);
    let submitted = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: Arc::new(move |_: JobContext| {
            let run_count = Arc::clone(&run_count);
            Box::pin(async move {
                run_count.fetch_add(1, Ordering::SeqCst);
                Err(JobError::retryable("busy"))
            }) as BoxFuture<Result<(), JobError>>
        }),
    });
    let snapshot = scheduler.wait(submitted.job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Failed);
    assert_eq!(snapshot.attempt, 3);
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    scheduler.close();
}

#[tokio::test]
async fn post_close_submit_fails_fast_cancelled() {
    // After close, submits never queue: Cancelled snapshot, not reused,
    // attempt 0 — and idle resolves immediately.
    let scheduler = JobScheduler::new(1, 1, 1);
    scheduler.close();
    let submitted = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Manual,
        followup_if_running: false,
        run: run_ok(),
    });
    assert!(!submitted.reused);
    assert_eq!(submitted.job.state, JobState::Cancelled);
    assert_eq!(submitted.job.attempt, 0);
    scheduler.wait_for_root_idle("/repo").await;
}

#[tokio::test]
async fn wait_on_unknown_id_returns_none() {
    let scheduler = JobScheduler::new(1, 1, 1);
    assert_eq!(scheduler.wait(999_999).await, None);
    scheduler.close();
}

#[tokio::test]
async fn wait_for_root_idle_resolves_when_idle() {
    // No jobs ever submitted: must resolve without hanging.
    let scheduler = JobScheduler::new(1, 1, 1);
    tokio::time::timeout(
        Duration::from_millis(100),
        scheduler.wait_for_root_idle("/repo"),
    )
    .await
    .expect("idle resolves");
    scheduler.close();
}

#[tokio::test]
async fn followup_merges_latest_run_and_escalates_reason() {
    // First job blocks; two followups merge into ONE followup id, the
    // latest run wins, and a Manual merge escalates the queued reason.
    let scheduler = JobScheduler::new(1, 3, 1);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mk_run = |marker: &'static str| {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let ran = Arc::clone(&ran);
        let first = marker == "first";
        Arc::new(move |_: JobContext| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let ran = Arc::clone(&ran);
            Box::pin(async move {
                if first {
                    entered.notify_one();
                    release.notified().await;
                }
                ran.lock().expect("ran").push(marker);
                Ok(())
            }) as BoxFuture<Result<(), JobError>>
        })
    };
    let _first = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: mk_run("first"),
    });
    entered.notified().await;
    let second = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Watch,
        followup_if_running: true,
        run: mk_run("stale"),
    });
    assert!(second.reused);
    let third = scheduler.submit(SubmitJob {
        root: "/repo".to_string(),
        reason: JobReason::Manual,
        followup_if_running: true,
        run: mk_run("latest"),
    });
    assert!(third.reused);
    assert_eq!(third.job.id, second.job.id, "one followup slot");
    assert_eq!(third.job.reason, JobReason::Manual, "reason escalates");
    release.notify_one();
    scheduler.wait_for_root_idle("/repo").await;
    assert_eq!(ran.lock().expect("ran").as_slice(), &["first", "latest"]);
    scheduler.close();
}
