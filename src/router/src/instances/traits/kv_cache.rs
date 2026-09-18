//! The `LlmKVCache` adapter for one llama-fork instance.

use std::path::PathBuf;
use std::sync::Arc;

use fluent_llm::runtime::{LlmKVCache, LlmRuntimeError, SnapshotMeta};

use crate::instances::client::InstanceClient;
use crate::instances::traits::common::map_instance_err;
use crate::kv_cache::{KvSnapshot, SnapshotStore};

/// The `LlmKVCache` adapter for one llama-fork instance: snapshot
/// save/list/delete round-trip through the fork's `/instances` API, with the
/// router-side `SnapshotStore` metadata index recorded when one is attached.
pub struct LlamaKVCache {
    client: InstanceClient,
    instance: String,
    model_key: String,
    store: Option<Arc<SnapshotStore>>,
    adapter: Option<String>,
    expected_identity: Option<crate::kv_cache::SnapshotIdentity>,
}

impl LlamaKVCache {
    /// Build the adapter for a fork instance. Crate-visible so tests can
    /// drive the restore identity gate against a stub fork.
    pub(crate) fn new(client: InstanceClient, instance: String, model_key: String) -> Self {
        Self {
            client,
            instance,
            model_key,
            store: None,
            adapter: None,
            expected_identity: None,
        }
    }

    /// Attach the router-side metadata index (recorded on save, consulted
    /// on restore for the identity check).
    #[must_use]
    pub fn with_store(mut self, store: Arc<SnapshotStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// The LoRA adapter this context serves under, if any. Saved records
    /// carry it so base and base+LoRA snapshots never share a key.
    #[must_use]
    pub fn with_adapter(mut self, adapter: impl Into<String>) -> Self {
        self.adapter = Some(adapter.into());
        self
    }

    /// The live identity restores must match. A stored record whose known
    /// fields differ from these rejects instead of serving stale KV.
    #[must_use]
    pub fn with_expected_identity(mut self, identity: crate::kv_cache::SnapshotIdentity) -> Self {
        self.expected_identity = Some(identity);
        self
    }
}

#[async_trait::async_trait]
impl LlmKVCache for LlamaKVCache {
    async fn save(&self, name: &str) -> Result<(), LlmRuntimeError> {
        self.client
            .save_snapshot(&self.instance, name)
            .await
            .map_err(map_instance_err)?;
        if let Some(store) = &self.store {
            let _ = store.store(KvSnapshot {
                model: self.model_key.clone(),
                adapter: self.adapter.clone(),
                session_id: self.instance.clone(),
                snapshot_name: name.to_string(),
                instance: Some(self.instance.clone()),
                file_path: PathBuf::new(),
                token_count: None,
                created_at: common_core::now_secs(),
                last_used_at: common_core::now_secs(),
                llama_cpp_version: None,
                model_quant: None,
                base_model_hash: None,
                turn_seq: None,
            });
        }
        Ok(())
    }

    async fn restore(&self, name: &str) -> Result<(), LlmRuntimeError> {
        // The fork switches a snapshot into a slot at dispatch time via the
        // `snapshot` request field; there is no management-side "restore" —
        // restore is expressed by the caller sending `snapshot=<name>`. The
        // fork's `POST /instances/:instance/snapshot` only saves. A restore
        // therefore validates the snapshot exists (loud) and is otherwise a
        // no-op — the dispatch path owns the actual slot switch.
        let snapshots = self
            .client
            .list_snapshots(&self.instance)
            .await
            .map_err(map_instance_err)?;
        if !snapshots.iter().any(|s| s.name == name) {
            return Err(LlmRuntimeError::NotLoaded(format!(
                "snapshot {name} not found for instance {}",
                self.instance
            )));
        }
        // Identity gate: when the store holds a record for this
        // (model, adapter, instance) key and a live identity is known, a
        // known-vs-known difference rejects instead of serving stale KV.
        // Either side unknown degrades to today's existence check.
        if let (Some(store), Some(expected)) = (&self.store, &self.expected_identity) {
            if let Ok(record) =
                store.retrieve(&self.model_key, self.adapter.as_deref(), &self.instance)
            {
                if let Err(e) = record.check_compatible(expected) {
                    return Err(LlmRuntimeError::NotLoaded(format!(
                        "snapshot {name} identity rejected for instance {}: {e}",
                        self.instance
                    )));
                }
            }
        }
        Ok(())
    }

    async fn list(&self) -> Vec<SnapshotMeta> {
        match self.client.list_snapshots(&self.instance).await {
            Ok(infos) => infos
                .into_iter()
                .map(|i| SnapshotMeta {
                    name: i.name,
                    size: i.size,
                    mtime: i.mtime.as_i64().unwrap_or(0),
                    n_ctx_seq: i.n_ctx_seq,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(
                    target: "router.instances",
                    instance = %self.instance,
                    error = %e,
                    "snapshot list on fork failed - returning empty",
                );
                Vec::new()
            }
        }
    }

    async fn delete(&self, name: &str) -> Result<(), LlmRuntimeError> {
        self.client
            .delete_snapshot(&self.instance, name)
            .await
            .map_err(map_instance_err)
    }
}
