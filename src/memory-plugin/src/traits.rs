//! Core trait definitions for the memory plugin system.

use crate::types::*;
use fluent_wvr::Component;
use std::future::Future;
use std::pin::Pin;

/// Domain-specific operations that every memory plugin must support.
///
/// This trait is separate from `Component` to keep the domain concern
/// distinct from the orchestration concern. The `MemoryPlugin` supertrait
/// composes both.
///
/// # Design Rationale
///
/// Hermes exposes 15+ lifecycle hooks because Python ABCs encourage
/// "override what you need." In our architecture, the orchestrator owns
/// lifecycle decisions. The plugin only implements these 8 methods.
/// Orchestration hooks like `on_turn_start`, `on_session_switch`,
/// `on_pre_compress`, and `on_delegation` are handled by the guidance
/// query engine, not the memory plugin.
///
/// # Dyn Compatibility
///
/// Async methods use `Pin<Box<dyn Future>>` to be dyn-compatible (object-safe).
/// This matches the pattern used by `fluent_wvr::Runtime`.
pub trait MemoryOps: Send + Sync {
    /// Short identifier: `"holographic"`, `"hindsight"`, `"honcho"`.
    fn name(&self) -> &'static str;

    /// Health check — no network calls, only config/deps validation.
    fn is_available(&self) -> bool;

    /// One-time initialization. Called once at guidance startup.
    ///
    /// Takes `&mut self` because this is called during startup before the
    /// plugin is wrapped in `Arc`. After `initialize` returns, the plugin
    /// is shared via `Arc<dyn MemoryPlugin>` and no further `&mut` access
    /// is possible (interior mutability for post-init state changes).
    fn initialize(&mut self, ctx: &MemoryInitContext) -> Result<(), MemoryError>;

    /// Clean shutdown. Release DB connections, join background tasks.
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    // ── Retrieval ──────────────────────────────────────────────

    /// Pre-fetch context before each LLM call.
    /// Returns formatted text for injection into the system prompt.
    fn prefetch(
        &self,
        query: &str,
        ctx: &MemoryQueryContext,
    ) -> Pin<Box<dyn Future<Output = String> + Send + '_>>;

    /// Background prefetch for next turn. Fire-and-forget.
    fn queue_prefetch(&self, query: &str, ctx: &MemoryQueryContext);

    /// Structured search returning scored results.
    fn search(
        &self,
        req: &MemorySearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<MemoryResult>, MemoryError>> + Send + '_>>;

    // ── Ingestion ──────────────────────────────────────────────

    /// Persist a completed turn. Non-blocking, may enqueue to background writer.
    fn sync_turn(
        &self,
        user_content: &str,
        assistant_content: &str,
        ctx: &MemoryQueryContext,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    /// End-of-session extraction. Called on /reset, timeout, or exit.
    fn on_session_end(
        &self,
        messages: &[TurnMessage],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    // ── Tool dispatch ──────────────────────────────────────────

    /// Handle a tool call from the LLM. Returns JSON string.
    fn handle_tool_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<String, MemoryError>;

    /// Tool schemas in OpenAI function-calling format.
    fn tool_schemas(&self) -> Vec<ToolSchema>;
}

/// The unified memory plugin boundary.
///
/// Any type implementing `Component + MemoryOps` automatically gets
/// `MemoryPlugin` via the blanket impl. The orchestrator stores
/// `Arc<dyn MemoryPlugin>` and never sees concrete types.
pub trait MemoryPlugin: Component + MemoryOps {}

/// Blanket implementation: any `Component + MemoryOps` is a `MemoryPlugin`.
impl<T: Component + MemoryOps> MemoryPlugin for T {}
