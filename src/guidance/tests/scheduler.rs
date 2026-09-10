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
