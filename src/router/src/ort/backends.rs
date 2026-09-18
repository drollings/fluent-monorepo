//! Onnx inference backends: the registry-backed embedder/reranker builders,
//! the generative `ChatBackend` (+ its decode seam), the ColBERT chart
//! reranker, and the thin `OnnxBackend` adapter over the boot registry.

use std::sync::Arc;

use fluent_llm::backend::{BackendCaps, InferenceBackend, Readiness};
use fluent_llm::client::ChatBackend;
use fluent_llm::onnx_error::OrtError;
#[cfg(feature = "onnx")]
use fluent_llm::runtime::LlmWeights;
use fluent_wvr::prelude::*;

use crate::config::RouterConfig;
#[cfg(feature = "onnx")]
use crate::instances::manager::resume_snapshot_name;

use super::registry::OrtRegistry;
#[cfg(feature = "onnx")]
use super::weights::{onnx_role_profile_for, OnnxWeights};

/// Build the ort encoder for a model and wrap it in a `CachedEmbeddingProvider`
/// (the router is the composition root; caching is deliberately not done
/// inside fluent-onnx). `Ok(None)` when the model is not registered as a
/// `FillMask` encoder.
#[cfg(feature = "onnx")]
pub fn onnx_chart_embedder(
    registry: &OrtRegistry,
    model_key: &str,
) -> Result<Option<Arc<dyn fluent_llm::embeddings::EmbeddingProvider>>, OrtError> {
    let encoder = fluent_onnx::build_encoder_from_registry(registry, model_key)?;
    let Some(encoder) = encoder else {
        return Ok(None);
    };
    let cached = fluent_llm::embeddings::CachedEmbeddingProvider::new(encoder);
    Ok(Some(Arc::new(cached)))
}

/// ColBERT two-stage reranker: builds a `ColbertRetriever` from the registry
/// for `model_key` (must be a `LateInteraction` task). The caller wraps it
/// in a `ColbertReranker` and uses it for MaxSim-based candidate re-ranking.
///
/// Returns `Ok(Some(retriever))` when available; `Ok(None)` when the model
/// is not registered or not a `LateInteraction` task.
#[cfg(feature = "onnx")]
pub fn onnx_colbert_reranker(
    registry: &OrtRegistry,
    model_key: &str,
) -> Result<Option<fluent_onnx::ColbertRetriever>, OrtError> {
    fluent_onnx::build_colbert_from_registry(registry, model_key)
}

/// No-ort build: no ColBERT reranker available.
#[cfg(not(feature = "onnx"))]
pub fn onnx_colbert_reranker(
    _registry: &OrtRegistry,
    _model_key: &str,
) -> Result<Option<()>, OrtError> {
    Ok(None)
}

/// The decode seam behind the onnx `ChatBackend`. Decouples the
/// backend's grammar-wiring + limiter from the concrete `OrtLlmSession` decode
/// so hermetic tests inject a fake decoder (no real model). `complete` runs one
/// chat call: chat-template render → tokenize → prefill → (optionally
/// grammar-constrained) decode to text.
#[cfg(feature = "onnx")]
pub(crate) trait OnnxLlmRunner: Send + Sync {
    fn complete(
        &self,
        messages: &[fluent_llm::ChatMessage],
        grammar: Option<&mut (dyn fluent_onnx::Grammar + 'static)>,
        max_tokens: Option<usize>,
        params: fluent_onnx::LlmParams,
    ) -> Result<String, fluent_llm::LlmError>;

    /// Run the chat call onto a named context: the same
    /// template→decode flow, but the KV persists in `ctx` so a follow-up call
    /// continues from where the previous stopped. Default = single-shot.
    fn complete_on_context(
        &self,
        _ctx: &fluent_onnx::OnnxContext,
        messages: &[fluent_llm::ChatMessage],
        grammar: Option<&mut (dyn fluent_onnx::Grammar + 'static)>,
        max_tokens: Option<usize>,
        params: fluent_onnx::LlmParams,
    ) -> Result<String, fluent_llm::LlmError> {
        self.complete(messages, grammar, max_tokens, params)
    }
}

/// The real runner over an `OrtLlmSession` (wraps [`fluent_onnx::OnnxChatCompletion`]).
#[cfg(feature = "onnx")]
struct RealOnnxRunner {
    session: Arc<fluent_onnx::OrtLlmSession>,
}

#[cfg(feature = "onnx")]
impl OnnxLlmRunner for RealOnnxRunner {
    fn complete(
        &self,
        messages: &[fluent_llm::ChatMessage],
        grammar: Option<&mut (dyn fluent_onnx::Grammar + 'static)>,
        max_tokens: Option<usize>,
        params: fluent_onnx::LlmParams,
    ) -> Result<String, fluent_llm::LlmError> {
        fluent_onnx::OnnxChatCompletion::new(self.session.clone())
            .complete(messages, grammar, max_tokens, params)
            .map_err(|e| fluent_llm::LlmError::Api(e.to_string()))
    }

    fn complete_on_context(
        &self,
        ctx: &fluent_onnx::OnnxContext,
        messages: &[fluent_llm::ChatMessage],
        grammar: Option<&mut (dyn fluent_onnx::Grammar + 'static)>,
        max_tokens: Option<usize>,
        params: fluent_onnx::LlmParams,
    ) -> Result<String, fluent_llm::LlmError> {
        fluent_onnx::OnnxChatCompletion::new(self.session.clone())
            .complete_on_context(ctx, messages, grammar, max_tokens, params)
            .map_err(|e| fluent_llm::LlmError::Api(e.to_string()))
    }
}

/// The onnx generative `ChatBackend`: a `CausalLm` session
/// served behind the `fluent_llm::client::ChatBackend` seam, with optional
/// grammar-constrained structured decoding.
///
/// - `chat_complete` → free text (no grammar — summarization / free generation).
/// - `chat_complete_with_extras` reads `extras["response_format"]["schema"]` and
///   builds a [`fluent_onnx::JsonObjectGrammar`] / [`fluent_onnx::JsonArrayGrammar`]
///   from it (the llama-fork `response_format` vocabulary), so a constrained
///   caller's output is structurally valid by construction. An unrepresentable
///   schema degrades to free text (the caller's post-hoc validator is the
///   backstop — the grammar is a strictness improvement, never a gate).
///
/// Every decode is bounded by a [`fluent_concurrency::pool::Limiter`] (the
/// router's thread-budget rule): a CPU decode never starves a GPU dispatch.
#[cfg(feature = "onnx")]
pub(crate) struct OnnxChatBackend {
    runner: Arc<dyn OnnxLlmRunner>,
    vocab: Arc<dyn fluent_onnx::TokenVocab>,
    params: fluent_onnx::LlmParams,
    limiter: fluent_concurrency::pool::Limiter,
    /// The role's shared context pool, when this backend is context-bound.
    /// `None` keeps the single-shot decode — byte-identical for
    /// an onnx role with no declared `instances` block.
    pool: Option<Arc<fluent_onnx::OnnxContextPool>>,
    /// The bound context name (the role's pool context). `extras["instance"]`
    /// overrides it per call; `None` + a pool still allows dynamic targeting.
    context: Option<String>,
    /// Context-name → profile resolution (the role's `instances` block).
    profile_for: Arc<dyn Fn(&str) -> fluent_onnx::OnnxContextProfile + Send + Sync>,
}

#[cfg(feature = "onnx")]
impl OnnxChatBackend {
    /// Build a single-shot backend (no named contexts).
    pub(crate) fn new(
        runner: Arc<dyn OnnxLlmRunner>,
        vocab: Arc<dyn fluent_onnx::TokenVocab>,
        params: fluent_onnx::LlmParams,
    ) -> Self {
        Self {
            runner,
            vocab,
            params,
            limiter: fluent_concurrency::pool::Limiter::new(1),
            pool: None,
            context: None,
            profile_for: Arc::new(|name| fluent_onnx::OnnxContextProfile {
                group: name.to_string(),
                n_ctx: 0,
                max_ctx: None,
                pinned: false,
                resume: false,
            }),
        }
    }

    /// Build a context-bound backend: decodes run on the role's
    /// named contexts (created on demand) so their KV persists across calls.
    pub(crate) fn with_contexts(
        runner: Arc<dyn OnnxLlmRunner>,
        vocab: Arc<dyn fluent_onnx::TokenVocab>,
        params: fluent_onnx::LlmParams,
        pool: Arc<fluent_onnx::OnnxContextPool>,
        context: Option<String>,
        profile_for: Arc<dyn Fn(&str) -> fluent_onnx::OnnxContextProfile + Send + Sync>,
    ) -> Self {
        Self {
            runner,
            vocab,
            params,
            limiter: fluent_concurrency::pool::Limiter::new(1),
            pool: Some(pool),
            context,
            profile_for,
        }
    }

    /// Run a single-shot decode through the limiter. `grammar` is `None` for
    /// free text.
    fn run_limited(
        &self,
        messages: &[fluent_llm::ChatMessage],
        mut grammar: Option<Box<dyn fluent_onnx::Grammar>>,
    ) -> Result<String, fluent_llm::LlmError> {
        let runner = Arc::clone(&self.runner);
        let params = self.params;
        let messages = messages.to_vec();
        // The sync `complete` runs in the `FnOnce` (before the async block), so
        // the grammar borrow never crosses an await boundary.
        self.limiter.run_sync(move || {
            let result = runner.complete(&messages, grammar.as_deref_mut(), None, params);
            async move { result }
        })
    }

    /// Run a decode onto a named context through the limiter.
    fn run_on_context(
        &self,
        ctx: &Arc<fluent_onnx::OnnxContext>,
        messages: &[fluent_llm::ChatMessage],
        mut grammar: Option<Box<dyn fluent_onnx::Grammar>>,
    ) -> Result<String, fluent_llm::LlmError> {
        let runner = Arc::clone(&self.runner);
        let params = self.params;
        let messages = messages.to_vec();
        let ctx_arc = Arc::clone(ctx);
        self.limiter.run_sync(move || {
            let result = runner.complete_on_context(
                &ctx_arc,
                &messages,
                grammar.as_deref_mut(),
                None,
                params,
            );
            async move { result }
        })
    }

    /// Select the target context for this call: `extras["instance"]` if
    /// present, else the backend's bound `context` (the pool context).
    fn target_context(&self, extras: &serde_json::Value) -> Option<String> {
        extras
            .get("instance")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| self.context.clone())
    }

    /// The extras-driven decode: a targeted context gets its KV
    /// restored from `extras["snapshot"]` before decode and re-snapshotted when
    /// `resume`/`save_snapshot` asks, mirroring the llama `snapshot`/`instance`/
    /// `id_slot` request fields. Absent a pool/context → single-shot.
    fn run_with_extras(
        &self,
        messages: &[fluent_llm::ChatMessage],
        extras: &serde_json::Value,
        grammar: Option<Box<dyn fluent_onnx::Grammar>>,
    ) -> Result<String, fluent_llm::LlmError> {
        let Some(pool) = &self.pool else {
            return self.run_limited(messages, grammar);
        };
        let Some(name) = self.target_context(extras) else {
            return self.run_limited(messages, grammar);
        };
        let ctx = pool.ensure_context(&name, (self.profile_for)(&name));
        // Resize-to-demand: a request that declares a bigger
        // context need than the context's allocated `n_ctx` grows the context
        // to fit (up to `max_ctx`); a need beyond `max_ctx` fails with the same
        // error shape a too-large llama request gets today (no new fail-open).
        if let Some(need) = extras.get("num_ctx").and_then(serde_json::Value::as_u64) {
            if need > ctx.n_ctx() {
                if let Some(cap) = ctx.max_ctx() {
                    if need > cap {
                        return Err(fluent_llm::LlmError::Api(format!(
                            "requested context {need} exceeds max_ctx {cap} for onnx context {name}"
                        )));
                    }
                }
                pool.resize(&name, need)
                    .map_err(|e| fluent_llm::LlmError::Api(e.to_string()))?;
            }
        }
        // Restore a prior snapshot before continuing (the resume half).
        if let Some(snap) = extras.get("snapshot").and_then(serde_json::Value::as_str) {
            ctx.kv_cache()
                .restore_sync(snap)
                .map_err(|e| fluent_llm::LlmError::Api(e.to_string()))?;
        }
        let result = self.run_on_context(&ctx, messages, grammar);
        // Snapshot the advanced KV on completion when resume/save asks.
        let save = extras
            .get("save_snapshot")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let is_resume = extras
                    .get("resume")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                is_resume.then(|| resume_snapshot_name(&name))
            });
        if let Some(snap) = save {
            let _ = ctx.kv_cache().save_sync(&snap);
        }
        result
    }
}

#[cfg(feature = "onnx")]
impl fluent_llm::client::ChatBackend for OnnxChatBackend {
    fn chat_complete(
        &self,
        messages: &[fluent_llm::ChatMessage],
    ) -> Result<String, fluent_llm::LlmError> {
        self.run_limited(messages, None)
    }

    fn chat_complete_with_extras(
        &self,
        messages: &[fluent_llm::ChatMessage],
        extras: &serde_json::Value,
    ) -> Result<String, fluent_llm::LlmError> {
        // The llama-fork `response_format.schema` vocabulary: a structured
        // caller declares the output shape; we constrain decode to it. An
        // absent/unrepresentable schema → free text (fail-open to the caller's
        // post-hoc validator).
        let grammar = extras
            .get("response_format")
            .and_then(|rf| rf.get("schema"))
            .and_then(|schema| {
                fluent_onnx::grammar_from_json_schema(schema, Arc::clone(&self.vocab))
            });
        self.run_with_extras(messages, extras, grammar)
    }
}

/// Build the onnx generative `ChatBackend` for `key` from the boot registry.
/// Returns `None` when `key` is not a registered `CausalLm`
/// session — fail-open, with a loud warning when the key *is* registered but
/// mistyped (a mis-configured generative role is never silent).
#[cfg(feature = "onnx")]
pub fn onnx_chat_backend(
    registry: &OrtRegistry,
    key: &str,
) -> Result<Option<Arc<dyn fluent_llm::client::ChatBackend>>, OrtError> {
    use fluent_onnx::HuggingFaceVocab;

    let Some(session) = fluent_onnx::build_llm_session(registry, key)? else {
        if registry.is_registered(key) {
            tracing::warn!(
                target: "router.ort",
                model = %key,
                "onnx_chat_backend: key registered but not a CausalLm session — \
                 no onnx backend (fail-open to HTTP/deterministic)",
            );
        }
        return Ok(None);
    };
    // Grammar-constrained decodes use the same tokenizer the session was built
    // with (the vocab the decode loop reasons over).
    let vocab: Arc<dyn fluent_onnx::TokenVocab> =
        Arc::new(HuggingFaceVocab::new(session.tokenizer().inner().clone()));
    let max_gen_tokens = session.max_gen_tokens();
    let runner: Arc<dyn OnnxLlmRunner> = Arc::new(RealOnnxRunner {
        session: Arc::new(session),
    });
    let backend = OnnxChatBackend::new(runner, vocab, fluent_onnx::LlmParams::default());
    tracing::info!(
        target: "router.ort",
        model = %key,
        max_gen_tokens = max_gen_tokens,
        "onnx generative ChatBackend built",
    );
    Ok(Some(Arc::new(backend)))
}

/// Build a **context-bound** onnx `ChatBackend` for `weights`' named context
/// `name`. Loads the role's context pool on first use (the onnx
/// lazy-residency load point — `ensure_pool` calls `build_llm_session`, which
/// loads the session through the registry) and binds the backend to that
/// context, so decodes persist their KV there. `name == pool_context` for the
/// role's default dispatch point; `local_backend_for_instance` targets any
/// declared context.
#[cfg(feature = "onnx")]
pub fn onnx_context_backend(
    weights: &OnnxWeights,
    name: &str,
) -> Result<Option<Arc<dyn fluent_llm::client::ChatBackend>>, OrtError> {
    use fluent_onnx::HuggingFaceVocab;

    let pool = weights.ensure_pool().map_err(|e| {
        // Supervisor-containment parity for onnx: a session
        // that fails to load on first use is a LOUD error, not a silent
        // fall-through — the registry's own `ensure_loaded` already logged
        // the underlying load failure.
        tracing::error!(
            target: "router.ort",
            model = %weights.model_key(),
            context = %name,
            error = %e,
            "onnx context pool load failed on first use — onnx backend unavailable (fail-open)",
        );
        OrtError::Other(format!("onnx context pool load failed: {e}"))
    })?;
    let session = pool.session();
    let vocab: Arc<dyn fluent_onnx::TokenVocab> =
        Arc::new(HuggingFaceVocab::new(session.tokenizer().inner().clone()));
    let runner: Arc<dyn OnnxLlmRunner> = Arc::new(RealOnnxRunner {
        session: session.clone(),
    });
    let role = weights.role().clone();
    let profile_for: Arc<dyn Fn(&str) -> fluent_onnx::OnnxContextProfile + Send + Sync> =
        Arc::new(move |name| onnx_role_profile_for(&role, name));
    let backend = OnnxChatBackend::with_contexts(
        runner,
        vocab,
        fluent_onnx::LlmParams::default(),
        pool.clone(),
        Some(name.to_string()),
        profile_for,
    );
    tracing::info!(
        target: "router.ort",
        model = %pool.model_key(),
        context = %name,
        "onnx context-bound ChatBackend built (KV persists on the named context)",
    );
    Ok(Some(Arc::new(backend)))
}

/// The role's **pool context** — the largest non-default `instances` group.
/// The onnx fleet's own work-context rule for role `instances` blocks (the
/// llama fleet names its work point through the roles table instead).
pub fn onnx_pool_context(role: &fluent_llm::onnx_config::OnnxRoleConfig) -> Option<String> {
    let instances = role.instances.as_ref()?;
    if instances.is_empty() {
        return None;
    }
    let first = instances.iter().next()?;
    let first_group = first.1.group.clone().unwrap_or_else(|| first.0.clone());
    // 1. The non-default profile with the largest sibling count (ties resolve
    //    to the first encountered in deterministic map order).
    let mut best_key: Option<String> = None;
    let mut best_group: Option<String> = None;
    let mut best_count: u32 = 0;
    for (key, p) in instances.iter().filter(|(_, p)| !p.default) {
        let c = p.count.max(1);
        if best_key.is_none() || c > best_count {
            best_key = Some(key.clone());
            best_group = Some(p.group.clone().unwrap_or_else(|| key.clone()));
            best_count = c;
        }
    }
    if let Some(g) = best_group {
        return Some(g);
    }
    // 2. The default profile's group.
    if let Some((key, p)) = instances.iter().find(|(_, p)| p.default) {
        return Some(p.group.clone().unwrap_or_else(|| key.clone()));
    }
    // 3. The single group shared by all profiles.
    if instances
        .iter()
        .all(|(k, p)| p.group.as_deref().unwrap_or(k.as_str()) == first_group)
    {
        Some(first_group)
    } else {
        None
    }
}

/// No-ort build: no onnx `ChatBackend` exists (fail-open). A configured onnx
/// LLM is worth a loud warning so an `onnx`-less build never silently drops the
/// generative role.
#[cfg(not(feature = "onnx"))]
pub fn onnx_chat_backend(
    _registry: &OrtRegistry,
    key: &str,
) -> Result<Option<Arc<dyn fluent_llm::client::ChatBackend>>, OrtError> {
    if !key.is_empty() {
        tracing::warn!(
            target: "router.ort",
            model = %key,
            "onnx_chat_backend requested but this build has the `onnx` feature off — \
             no onnx backend (fail-open)",
        );
    }
    Ok(None)
}

/// ColBERT-based chart candidate reranker: uses MaxSim late-interaction
/// scoring to re-rank HNSW candidates by fine-grained token-level relevance.
#[cfg(feature = "onnx")]
pub struct ColbertChartReranker {
    retriever: fluent_onnx::ColbertRetriever,
    cache: Arc<fluent_onnx::colbert::CachedColbert>,
}

#[cfg(feature = "onnx")]
impl ColbertChartReranker {
    pub fn new(retriever: fluent_onnx::ColbertRetriever) -> Self {
        Self {
            retriever,
            cache: Arc::new(fluent_onnx::colbert::CachedColbert::new(1024)),
        }
    }
}

#[cfg(feature = "onnx")]
impl crate::charts::select::ColbertRerank for ColbertChartReranker {
    fn rerank_candidates(
        &self,
        query: &str,
        candidates: &[(String, f64)],
        doc_texts: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<(String, f64)> {
        use fluent_onnx::colbert::maxsim_score;

        let query_tokens = match self.retriever.encode_query(query) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    target: "router.ort",
                    error = %e,
                    "colbert query encoding failed — falling back to HNSW order"
                );
                return candidates.to_vec();
            }
        };
        if query_tokens.is_empty() {
            return candidates.to_vec();
        }
        let query_refs: Vec<&[f32]> = query_tokens.iter().map(Vec::as_slice).collect();

        let mut scored: Vec<(String, f64)> = candidates
            .iter()
            .filter_map(|(name, _hnsw_score)| {
                let text = doc_texts(name)?;
                let cache_key = format!("{}:{}", self.retriever.name(), name);
                let doc_tokens = if let Some(cached) = self.cache.get(&cache_key) {
                    cached
                } else {
                    let tokens = self.retriever.encode(&text).ok()?;
                    self.cache.insert(cache_key, tokens.clone());
                    tokens
                };
                let doc_refs: Vec<&[f32]> = doc_tokens.iter().map(Vec::as_slice).collect();
                let colbert_score = f64::from(maxsim_score(&query_refs, &doc_refs));
                // Blend: ColBERT score replaces the HNSW score for re-ranking.
                Some((name.clone(), colbert_score))
            })
            .collect();

        // Sort by descending ColBERT score.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if scored.is_empty() {
            candidates.to_vec()
        } else {
            scored
        }
    }
}

/// The onnx inference backend: a thin [`InferenceBackend`] adapter over the
/// boot-built onnx registry for the generative (`onnx/llm`) role.
/// Single-shot dispatch delegates to [`onnx_chat_backend`], named-context
/// dispatch to [`onnx_context_backend`] over the role's `OnnxWeights`, and
/// `weights()` serves that same `OnnxWeights` — the adapter adds routing,
/// never a second session or construction path.
pub struct OnnxBackend {
    llm_key: String,
    registry: OrtRegistry,
    single_shot: Option<Arc<dyn ChatBackend>>,
    pool_context: Option<String>,
    has_instances: bool,
    #[cfg(feature = "onnx")]
    llm_weights: Option<Arc<OnnxWeights>>,
}

impl OnnxBackend {
    /// Build the adapter from the boot registry + config. `None` when no
    /// generative llm role is configured (fail-open — the backend simply is
    /// not registered).
    pub fn from_config(config: &RouterConfig, registry: &OrtRegistry) -> Option<Self> {
        let llm_key = fluent_llm::onnx_config::OnnxRole::Llm
            .registry_key()
            .to_string();
        let role = config.onnx.as_ref()?.llm.as_ref()?.clone();
        let single_shot = onnx_chat_backend(registry, &llm_key).ok().flatten();
        #[cfg(feature = "onnx")]
        let llm_weights = Some(Arc::new(OnnxWeights::new(
            llm_key.clone(),
            Arc::clone(registry),
            role.clone(),
        )));
        let has_instances = role.instances.as_ref().is_some_and(|m| !m.is_empty());
        let pool_context = onnx_pool_context(&role);
        if single_shot.is_some() {
            tracing::info!(
                target: "router.ort",
                model = %llm_key,
                has_instances = has_instances,
                "onnx generative backend wired as the default local backend",
            );
        }
        Some(Self {
            llm_key,
            registry: Arc::clone(registry),
            single_shot,
            pool_context,
            has_instances,
            #[cfg(feature = "onnx")]
            llm_weights,
        })
    }

    /// The role's single-shot backend (the default dispatch point when the
    /// role declares no `instances` block). The composition root wires this
    /// into the server's explicit onnx-llm seam.
    pub fn single_shot(&self) -> Option<Arc<dyn ChatBackend>> {
        self.single_shot.clone()
    }
}

impl FieldAccess for OnnxBackend {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound(name.into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for OnnxBackend {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "description": "onnx generative inference backend",
        })
    }
}

impl WorkUnit for OnnxBackend {
    fn name(&self) -> &str {
        "onnx"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("onnx backend adapter"))
    }
}

fluent_wvr::impl_component!(OnnxBackend);

impl InferenceBackend for OnnxBackend {
    fn backend_id(&self) -> &'static str {
        "onnx"
    }
    fn model_keys(&self) -> Vec<String> {
        vec![self.llm_key.clone()]
    }
    fn weights(&self, key: &str) -> Option<Arc<dyn fluent_llm::runtime::LlmWeights>> {
        #[cfg(feature = "onnx")]
        {
            if key != self.llm_key || !self.registry.is_registered(key) {
                return None;
            }
            self.llm_weights.clone().map(|w| w as _)
        }
        #[cfg(not(feature = "onnx"))]
        {
            let _ = key;
            None
        }
    }
    fn chat_backend(&self, key: &str, instance: Option<&str>) -> Option<Arc<dyn ChatBackend>> {
        if key != self.llm_key {
            return None;
        }
        match instance {
            #[cfg(feature = "onnx")]
            Some(name) => self
                .llm_weights
                .as_ref()
                .and_then(|w| onnx_context_backend(w, name).ok().flatten()),
            #[cfg(not(feature = "onnx"))]
            Some(_) => None,
            None if self.has_instances => {
                let ctx = self.pool_context.clone()?;
                #[cfg(feature = "onnx")]
                {
                    self.llm_weights
                        .as_ref()
                        .and_then(|w| onnx_context_backend(w, &ctx).ok().flatten())
                }
                #[cfg(not(feature = "onnx"))]
                {
                    let _ = ctx;
                    None
                }
            }
            None => self.single_shot.clone(),
        }
    }
    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            named_contexts: true,
            kv_snapshot: true,
            grammar_constrained: cfg!(feature = "onnx"),
            ..BackendCaps::default()
        }
    }
    fn readiness(&self, key: &str) -> Readiness {
        if key != self.llm_key {
            return Readiness::Unloaded;
        }
        if self
            .registry
            .residency_report()
            .iter()
            .any(|r| r.key == self.llm_key && r.loaded)
        {
            return Readiness::Loaded;
        }
        // Registered but lazy, or (no-ort builds) never loadable: known but
        // not resident. Unregistered keys stay fail-open `Unloaded` too.
        Readiness::Unloaded
    }
}
