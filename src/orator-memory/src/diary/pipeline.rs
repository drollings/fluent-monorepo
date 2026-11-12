//! Checkpointed post-processing: resumable, re-runnable meeting work.
//!
//! The step list is `finalize_audio → offline_stt → diarize → align →
//! name → summarize → export`, backed by `CheckpointedStepGraph`.
//! Per-step state persists into the `checkpoints` table, so a daemon
//! restart resumes from the last checkpoint and `redo --from` re-runs
//! exactly the suffix. A panicked step cancels its dependents; result
//! data is preserved for audit while states reset.

use fluent_dag::checkpointed::CheckpointedStepGraph;
use fluent_dag::dep_graph::GraphError;
use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;

/// Post-processing steps in pipeline order.
pub const STEPS: [&str; 7] = [
    "finalize_audio",
    "offline_stt",
    "diarize",
    "align",
    "name",
    "summarize",
    "export",
];

/// Per-step run state.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Per-step run state.
pub enum StepState {
    /// Never ran.
    #[default]
    Pending,
    /// Finished with its result summary.
    Done(String),
}

/// The meeting pipeline: in-memory graph plus durable checkpoints.
pub struct MeetingPipeline {
    graph: CheckpointedStepGraph<String, StepState>,
    store: SqliteStore,
    meeting: String,
}

impl MeetingPipeline {
    /// Pipeline for `meeting` over `store` (checkpoints table must
    /// exist — the store migration owns it).
    pub fn new(store: SqliteStore, meeting: &str) -> Result<Self, GraphError> {
        let mut graph = CheckpointedStepGraph::new();
        for (index, step) in STEPS.iter().enumerate() {
            let deps: Vec<String> = if index == 0 {
                Vec::new()
            } else {
                vec![STEPS[index - 1].to_string()]
            };
            graph.add_step(step.to_string(), &deps, StepState::Pending)?;
        }
        Ok(Self {
            graph,
            store,
            meeting: meeting.to_string(),
        })
    }

    /// Steps ready to run right now.
    #[must_use]
    pub fn ready(&self) -> Vec<String> {
        self.graph.ready_steps()
    }

    /// Mark `step` done with `summary`, checkpointing durably.
    pub fn finish(&mut self, step: &str, summary: &str) -> Result<(), DbError> {
        self.graph.checkpoint(step.to_string()).map_err(|error| {
            DbError::Other(format!("checkpoint {step}: {error}"))
        })?;
        self.graph.complete(&step.to_string());
        if let Some(state) = self.graph.state_mut(&step.to_string()) {
            *state = StepState::Done(summary.to_string());
        }
        let meeting = self.meeting.clone();
        let step_name = step.to_string();
        self.store.execute(
            "INSERT INTO checkpoints (meeting, step, state) VALUES (?1, ?2, ?3) \
             ON CONFLICT(meeting) DO UPDATE SET step=excluded.step, state=excluded.state",
            &[
                &meeting as &dyn rusqlite::ToSql,
                &step_name as &dyn rusqlite::ToSql,
                &summary as &dyn rusqlite::ToSql,
            ],
        )?;
        Ok(())
    }

    /// Rewind to `step` for `redo --from`: returns the suffix to
    /// re-run and clears those checkpoints so they re-execute.
    pub fn rewind(&mut self, step: &str) -> Result<Vec<String>, DbError> {
        let suffix = self
            .graph
            .rewind_to(&step.to_string())
            .map_err(|error| DbError::Other(format!("rewind {step}: {error}")))?;
        let meeting = self.meeting.clone();
        self.store.execute(
            "DELETE FROM checkpoints WHERE meeting = ?1",
            &[&meeting as &dyn rusqlite::ToSql],
        )?;
        Ok(suffix)
    }

    /// Steps the graph reports complete.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.graph.completed_count()
    }

    /// Dependents that must not run after `step` panics.
    #[must_use]
    pub fn cancel_after(&self, step: &str) -> Vec<String> {
        self.graph.cancel_dependents(&step.to_string())
    }
}

#[cfg(test)]
#[path = "../../tests/diary_pipeline.rs"]
mod tests;