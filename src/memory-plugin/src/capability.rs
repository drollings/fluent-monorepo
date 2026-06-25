//! Capability token for explicit memory plugin access.
//!
//! No ambient authority — every memory operation requires passing
//! a `&MemoryCapability` or receiving one via the `WorkContext`.

use crate::registry::MemoryPluginRegistry;
use crate::traits::MemoryPlugin;
use crate::types::*;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Capability token that grants access to the memory plugin system.
///
/// Must be passed explicitly — no ambient authority. This is the
/// `fluent-concurrency` pattern for bounded resource access applied
/// to the memory subsystem.
///
/// # Usage
///
/// ```rust,ignore
/// // At startup:
/// let registry = Arc::new(RwLock::new(MemoryPluginRegistry::new()));
/// let cap = MemoryCapability::new(Arc::clone(&registry));
///
/// // In query pipeline:
/// let results = cap.search(&MemorySearchRequest { ... }).await?;
/// let context = cap.prefetch("user question", &ctx).await;
/// ```
#[derive(Clone)]
pub struct MemoryCapability {
    registry: Arc<RwLock<MemoryPluginRegistry>>,
}

impl MemoryCapability {
    /// Create a new capability bound to the given registry.
    pub fn new(registry: Arc<RwLock<MemoryPluginRegistry>>) -> Self {
        Self { registry }
    }

    /// Get the active memory plugin. Returns `None` if no plugin is active.
    pub async fn active_plugin(&self) -> Option<Arc<dyn MemoryPlugin>> {
        let reg = self.registry.read().await;
        reg.active()
    }

    /// Get a specific plugin by name.
    pub async fn get_plugin(&self, name: &str) -> Option<Arc<dyn MemoryPlugin>> {
        let reg = self.registry.read().await;
        reg.get(name)
    }

    /// Execute a search against the active plugin.
    pub async fn search(
        &self,
        req: &MemorySearchRequest,
    ) -> Result<Vec<MemoryResult>, MemoryError> {
        let plugin = self
            .active_plugin()
            .await
            .ok_or_else(|| MemoryError::NotAvailable("no active memory plugin".into()))?;
        plugin.search(req).await
    }

    /// Execute a prefetch against the active plugin.
    pub async fn prefetch(&self, query: &str, ctx: &MemoryQueryContext) -> String {
        let plugin = match self.active_plugin().await {
            Some(p) => p,
            None => return String::new(),
        };
        plugin.prefetch(query, ctx).await
    }

    /// Dispatch a tool call to the active plugin.
    pub async fn handle_tool_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<String, MemoryError> {
        let plugin = self
            .active_plugin()
            .await
            .ok_or_else(|| MemoryError::NotAvailable("no active memory plugin".into()))?;
        plugin.handle_tool_call(tool_name, args)
    }

    /// Get tool schemas from the active plugin.
    pub async fn tool_schemas(&self) -> Vec<ToolSchema> {
        let plugin = match self.active_plugin().await {
            Some(p) => p,
            None => return vec![],
        };
        plugin.tool_schemas()
    }

    /// Sync a turn with the active plugin.
    pub async fn sync_turn(
        &self,
        user_content: &str,
        assistant_content: &str,
        ctx: &MemoryQueryContext,
    ) {
        let plugin = match self.active_plugin().await {
            Some(p) => p,
            None => return,
        };
        plugin.sync_turn(user_content, assistant_content, ctx).await;
    }

    /// Notify the active plugin of session end.
    pub async fn on_session_end(&self, messages: &[TurnMessage]) {
        let plugin = match self.active_plugin().await {
            Some(p) => p,
            None => return,
        };
        plugin.on_session_end(messages).await;
    }
}
