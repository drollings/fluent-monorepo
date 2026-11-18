# fluent-llm — Architecture

`forbid(unsafe_code)`. Crate `fluent-llm` (`src/llm/`). Boundary guard:
`tests/no_domain_imports.rs` forbids imports of router/coral/guidance/types/
dag/db/concept/rdf/spacy/wasm_ipc. Live-AI tests are `#[ignore]` +
`live-ai`-gated (`tests/live.rs`); hermetic suite never dials real endpoints.

Consumers: `router`, `coral`, `knowledge`, `guidance`, `orator-*`,
`fluent-onnx`, `spacy-rs` (see workspace `Cargo.toml` dependents).
`router/src/telemetry.rs` is a thin re-export of this crate's telemetry;
`coral/src/packer.rs` re-exports `ContextPacker`.

Shared-library composition rule: generic retry/hash/eviction/calibration/
orchestration come from `common-core`, `fluent-wvr`, `fluent-concurrency`.
This crate owns LLM policy and vocabulary only (`tokens` lives here, not in
`common-core`, by documented policy).

## Module index

### Transport core

- `protocol.rs` — Owned protocol + queue vocabulary (M11 canonical home;
  `fluent-concurrency::llm_queue` shims deleted). `LlmError`
  (`Api/Http/NoResponse/RateLimited`) + `is_retryable` + `From<reqwest::Error>`;
  `ChatMessage` (bon builder, `role_enum/with_role`); `LlmConfig` (bon builder:
  `api_url/model/think/timeout_ms/extra_body_params/debug/show_prompts`);
  `LlmTask`, `LlmQueueConfig` (default 1 worker/100 cap), `LlmRequestQueue`
  (thin `ResultPool` wrapper: `new/worker_count/submit/submit_with_abort`).
  Re-exported at crate root so `use fluent_llm::LlmConfig` keeps working.
- `client.rs` — Primary caller entry point. `ChatBackend` trait
  (`chat_complete/chat_complete_with_extras`); `LlmClient` (holds
  `api_base/model/config/queue/decision_transport/decide_backend`;
  constructors `new/with_queue/with_queue_and_config/with_config`;
  `chat_complete_async/chat_complete_with_extras_async` + sync twins via
  `block_on`; `decide_async/decide_prompt/decide_request/
  decide_batch_requests/probe_decision_support`). Free functions:
  `block_on`, `chat_complete_http_async/chat_complete_http`,
  `post_chat_json_async`, `merge_extra_params`, `model_name`,
  `strip_preamble/is_malformed_response/extract_comment_tag/
  is_blank_or_plausible`. Mixes transport + judgment seam + string heuristics
  in one file (~900 lines) — split with care.
- `openai.rs` — Single wire-format owner (M12): `apply_params` (merges
  `extra_body_params`, ignores `model/messages/stream`); `build_openai_chat_body`;
  `parse_openai_stream_delta` + `OpenAiDelta`; `normalize_request/
  messages_to_json/normalize_response/normalize_response_with_id/
  error_response/model_ids`; re-exports `sse::drain_sse_lines`.
- `llm_queue.rs` — Default transport binding. `default_handler` (builds the
  `Fn(LlmTask) -> BoxFuture<Result<String,LlmError>>` over
  `client::chat_complete_http_async`); `build_default_queue(runtime, config)`.
- `http_class.rs` — `HttpClass` (`HardReject/TransientFailure/
  EscalationRequired/UpstreamFailure`) + `from_status/is_retryable`;
  re-exports `common_core::telemetry::FailureClass` (single definition);
  `classify_http_status`, `From<HttpClass> for FailureClass`.
- `sse.rs` — `drain_sse_lines(buffer, chunk)`: deferred-UTF8 SSE line drainer
  (moved in as owner; no deps).
- `url.rs` — Endpoint hygiene. `UrlError`; `is_local_host/is_private_ip/
  extract_host` (bracket-naive by contract) `validate_https_or_local_http/
  chat_completions_url/derive_embeddings_url`; `HostEquivalence`
  (`LOCAL_SSRF` treats any `127.` prefix as loopback; `EXACT_LOOPBACK` does
  not) + `hosts_equivalent_with`. Dual classes are deliberate (DNS-spoof
  note); `decision/transport` forks host parsing for this reason.

### Embedding plane

- `embeddings.rs` — `EmbeddingProvider` trait (`name/dimensions/embed/
  embed_batch` + async defaults + `embed_batch_with_truncation`);
  `BatchEmbedding` (`flat/count/dims`, `vector` panicking vs `try_vector`
  checked); `validate_embed_batch/check_embed_batch_output/block_on_provider`;
  `NoopEmbedding` (dims-0 sentinel), `OllamaEmbedding`
  (`with_client/embed_raw`), `OpenAiEmbedding`
  (`with_client/request_body/embeddings_url`), `CachedEmbeddingProvider<T>`
  (`new/new_with_limit/with_adapter/adapter`; dedup+shard cache fan-out);
  `do_http_post_async/do_http_post/parse_float_array/
  parse_ollama_response[_batch]/parse_openai_response[_batch]/
  create_embedding_provider`. Multi-provider god-module: Ollama and OpenAI
  sync/async bodies differ only in body/URL.
- `qwen.rs` — `QwenTextOptions` (api_key/endpoint/extra_headers/timeout_ms);
  `QwenImageFormat` + `parse`; `EmbedContent` (`Text/Image` + `image`);
  `QwenEmbedResult`; `QwenTextEmbeddingV4` / `Qwen37TextEmbedding`
  (`new/embed_texts/max_batch_size/max_input_tokens`; V4 also
  `endpoint/dimension`); `Qwen3VlEmbedding` (`embed_contents`);
  internals `QwenTextCore/QwenTextSpec/QwenVlSpec/resolve_qwen_config/
  parse_text_vectors/read_provider_error/retry_after_ms/
  validate_vl_contents/base64_encode` (hand-rolled base64; V4 vs 3.7 impls
  near-identical).
- `catalog.rs` — Static single source of truth. `EmbeddingBackend`
  (`LlamaCpp/Model2Vec/TransformersJs/Qwen`); `ArtifactMirror/
  ArtifactSources/PinnedArtifact/CatalogEntry` (20-field sparse struct, each
  backend uses a disjoint subset); `EMBEDDING_MODEL_CATALOG` (14 rows),
  `DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT/
  DEFAULT_QWEN3_VL_EMBEDDING_ENDPOINT/DEFAULT_LOCAL_EMBEDDING`;
  `list_embedding_models/get_embedding_reference`.
- `resolution.rs` — Precedence chain `explicit > existing > env > global >
  fallback`. `EMBEDDING_ENVIRONMENT_VARIABLE`; `ResolveEmbeddingReferenceOptions`
  (env injectable as map for hermetic tests); `resolve_embedding_reference`;
  unknown env value is a hard error.
- `factory.rs` — `CreateModelOptions` (api_key/endpoint/model_cache_dir/
  device/timeout_ms/extra_headers); `create_embedding_model(reference,
  options)` (two arms return `BackendNotWired`: `Model2Vec`,
  `TransformersJs`); `EmbeddingIdentity`; `RebuildField/RebuildMismatch`;
  `require_compatible_embedding` (model/endpoint drift check).
- `gguf.rs` — Local GGUF path (~1000 lines). Enums `Device` (`Auto/Cpu/
  Metal/Vulkan/Cuda`) + `parse`, `GpuSelection`, `EmbedPurpose
  (Document/Query)`, `GgufFormat (EmbeddingGemma/Qwen3)`; `GgufContextOptions/
  GgufModelSpec` (`TryFrom<&CatalogEntry>`) `/GgufArtifactRequest/
  GgufResolvedArtifacts/GgufModelOptions/GgufEmbedResult/GgufDependencies/
  ProductionGgufResolver/UnboundGgufRuntime`; seams `GgufEmbeddingContext/
  GgufModel/GgufLlamaInstance/GgufRuntimeModule/GgufRuntimeLoader/
  GgufArtifactResolver`; `GgufEmbedding` (`new/
  with_parallelism_override/prepare/embed_texts/dispose` + `EmbeddingProvider`
  impl). Single `Mutex<LoadedState>` serializes concurrent embeds
  (singleflight by design); GPU→CPU fallback checked at three layers.
- `decomposer.rs` — `Decomposer` trait (`decompose`); `DecomposerConfig
  (llm/max_subtasks/max_depth)`; `LocalDecomposer` (`new/decompose_into`;
  LLM JSON-array plan, fallback `[task]`). Uses strict `from_str` array
  parsing rather than tolerant `parse::parse_typed` — brittle to fences/prose.

### ONNX / runtime plane

- `onnx_config.rs` — `OnnxTask` (7 variants) + `expected_architecture_families/
  required_outputs`; `Quant (Fp32/Fp16/Q8/Q4/Fp8)` + `suggested_for/is_fp32`;
  `LlmIo` (15 fields, bon builder); `AnnotationHeads/AnnotationLabels`
  (`load`); `ResidencyPolicy (Always/Unloadable)`; `OnnxConfig` (16 fields:
  `policy/resolved_tokenizer_path/config_json_path/resolve_model_file/
  validate`); `OnnxRole (Encoder/Pii/Router/Policy/Colbert/Llm)` +
  `task/registry_key/suggested_quantization/all`; `OnnxInstanceProfile
  (apply_max_ctx)`; `OnnxRoleConfig (to_onnx_config)`; `OnnxFleetConfig
  (is_empty/iter/get/has)`. `OnnxTask`/`OnnxRole` overlap 1:1.
- `onnx_session.rs` — ort-free registry + injectable loader. `SessionHandle`
  (`new/downcast_ref/downcast_arc` over `Arc<dyn Any>`);
  `SessionLoader` trait (`load`); `OrtSessionRegistry` (`new/register/
  register_with_policy/register_with_lifecycle/ensure_loaded/session/config/
  policy/is_pinned/sleep_idle_seconds/touch/last_used_of/resident_bytes/
  release/unloadable_keys/residency_report/is_registered/refuses_unload/
  model_keys/len/is_empty`); `ResidencyReportEntry`.
- `onnx_error.rs` — `OrtError` vocabulary only (`ModelFileNotFound/ConfigRead/
  ConfigParse/TaskMismatch/OutputMismatch/SessionLoad/SessionRun/
  Tokenization/Other` + `tokenization`).
- `backend.rs` — Backend-component contract (~590 lines, intentionally broad).
  `BackendCaps` (6 bools) + `chat_with_contexts`; `Readiness`;
  `ContextProfile`; `DEFAULT_CONTEXT_GROUP`; `WellKnownBackend (Llama/Onnx)`;
  `BackendLoader (load/probe_ready)`; `BackendError`; `InferenceBackend:
  Component` (`backend_id/model_keys/weights/chat_backend/
  embed_provider/capabilities/readiness`); `NamedContexts: InferenceBackend
  (ensure_context)`; `InferenceRegistry` (`register` panics on dup/
  `unregister/get/backend_ids/route_chat/route_embed/len/is_empty`,
  backed by `common_core::registry::KeyedRegistry`); `InferenceCapability`;
  neutral consumer types `PiiSpan/PiiError/PiiSpanDetector/
  RegexPiiDetector/ResidualKind/Residual/OverlayContribution/OverlayError/
  ResidualOverlay/RouteLabel/EntityLinkScorer`. Fallback walks compose
  `fluent_concurrency::ladder::first_accept_in_order_sync`.
- `runtime.rs` — Residency engine (~990 lines). `LlmRuntime (Llama/Onnx)`;
  `EvictionPolicy (FootprintColdness/LruLargest)` + `pool`;
  `MemoryPool (Vram/Ram)`; `LlmRuntimeError`; `ResidencyState
  (Loaded/Sleeping/Unloaded)`; `LlmResidencyRow` (12 fields);
  `SnapshotMeta`; `LlmWeights` (13-method wide interface:
  `model_key/weights_bytes/pinned/refuse_unload/is_loaded/
  sleep_idle_seconds/ensure_loaded/unload/touch/last_used/in_flight/
  residency_rows/context/ensure_context/eviction_policy/expire_resume/
  evict_weights`); `LlmContext` (11 methods incl. `resize/destroy/kv_cache/
  evict`); `LlmKVCache (save/restore/list/delete)`; `LlmResidencyEngine`
  (`new/new_with_clock/with_admission_budget/residency_cycle/make_room_for/
  start` over `fluent_wvr::runtime::Runtime`). Four-phase cycle
  (expire → idle-release → per-pool evict → unload-empty) composes
  `common_core::cache::{eviction_order, evict_until_fit}` and
  `retry::capped_backoff_ms`. Two `last_used` units (s/ms) deliberately
  unconverted — read the accounting code before touching.

### Decision stack (`decision/`)

Facade `decision/mod.rs` re-exports all submodules.

- `decision/types.rs` — Pure policy core. `DecisionKind (Choice/Score/Noul)`;
  `Decision (label/distribution/confidence/margin/degraded)` +
  `from_distribution` (top-1/top-2 math); `DecisionError
  (Unavailable/Rejected/Decode)` + `is_unavailable/is_retryable` (maps
  `LlmError::is_retryable`); `ThresholdSet (act/review)` + `validate`;
  `Verdict (Act/Review/Escalate)`; `decide_verdict/decide_score_verdict/
  decide_noul_verdict/noul_p_yes/score_moment/ScoreMoment` (degraded caps
  `Act→Review`).
- `decision/backend.rs` — `DecisionOption`; `DecideRequest` (owned `'static`);
  `BatchFuture`; `DecideBackend` trait (`decide/decide_batch`, default batch
  is sequential fan-out — override for speed); `ScriptedDecideBackend`
  (hermetic double, errors on exhaustion).
- `decision/transport.rs` — Safety rails. `RedactHook/AuditSink` (`Arc<dyn
  Fn>`); `anonymize_hook`; `DecisionProbe (logprobs/top_entries/
  reasoning_hijack)` + `read/degrades_to_text`; `DecisionAudit/
  PreparedPrompt` (metadata-only); `DecisionTransport` (`local_only/
  with_allow_remote/with_redact/with_audit/redact_text/prepare`,
  localhost-only by default). Own `endpoint_host/is_exact_loopback` forked
  from `crate::url` (SSRF-range note in comments).
- `decision/cache.rs` — `CacheKeyInput` + `key` (framed `blake3_hex`);
  `decision_cache_key` (legacy 8-arg ctor); `DecisionCache
  (get_or_decide/get/insert/len/is_empty/capacity)` over
  `common_core::cache::LoadCache`; caches only `Ok && !degraded`.
  Hand-rolled singleflight (`Inflight` + `Notify` + `Retire` guard) instead of
  a `fluent-concurrency` pool — most complex file in the stack.
- `decision/ladder.rs` — Canonical composition example:
  `decide_with_ladder(rule/cache/key/cacheable/breaker/model/factory/
  fallback)` via `fluent_wvr::ladder::first_accept_in_order`
  (`Unavailable` skips, only `Rejected` terminal, degraded never cached).
  Private `Rung` enum drives the walk.
- `decision/decode.rs` — `decode_choice/decode_score/decode_noul` over
  `content.logprobs.top_logprobs` (variant-sum over space/case; all-labels-
  must-appear else text fallback at 0.5/degraded prior; score needs ≥2 rungs;
  noul fixed `Yes/No`); public `match_text_label` (trim-both-ends +
  ASCII case-insensitive closed-enum match, fail-closed `None` — the
  `from_label`/`parse_triage_verdict` shape; distinct from the private
  token-level `match_label`, which keeps its trim-start contract for
  tokenizer artifacts). Internals `from_tops/match_label/canonical/
  entry_prob/parts/fallback/downgraded`; JSON fallback chains `parse_typed`
  then `decode_boundary_typed` with private `LabelDoc
  (FieldAccess/Describable)`.
- `decision/question.rs` — Control-plane spec. `QuestionSpec` (bon builder +
  `FieldAccess/Describable/Component/WorkUnit`; `validate/kind_enum/options/
  thresholds`; enforces single-token labels, canonicalized dedup,
  `MAX_DECISION_OPTIONS=10`); `Question` (`render` state-first/question-last
  with `(seed,salt)` rotation); `MAX_DECISION_PROMPT_BYTES=8192`;
  `rotated_options/render_prompt/DecisionOptions::labels`. Own LCG
  Fisher-Yates shuffle; rotation unknown-mode render is fail-open while
  `validate` is fail-closed.
- `decision/bulk.rs` — Fan-out (~750 lines). `decide_all
  (client/questions/cap/queue_capacity/abort)` (validate-then-fan-out via
  `ResultPool`, order-preserving; backend batch path or `OwnedJob` pool);
  `aggregate_rotations/rotation_agreement/decide_aggregated` (mass-sum merge,
  degraded-sticky); `route_hierarchical` (chunked tournament over max
  options); `filter_noul/route_choice/rank_score` (kind-checked wrappers);
  `bulk_filter/bulk_rerank/bulk_route` (pure post-processors over
  `decide_verdict`). `cap = items.len()` is unbounded by default.
- `decision/breaker.rs` — `DecisionBreaker (new/threshold/cooldown)` +
  `is_open/check/record_transport_failure/record_permanent/record_success`
  (consecutive-`Unavailable` trip, cooldown, single half-open trial;
  `Instant`-based clock, non-injectable).
- `decision/registry.rs` — Calibration persistence. `CaseOrigin
  (Human/HumanReviewed/Synthetic)`; `CaseProvenance/CorpusProvenance/Corpus`;
  `GateReport`; `RegistryEntry (calibrate/calibrate_with)`;
  `CalibrationRegistry (empty/upsert/lookup/load/save/load_corpus)`;
  `GateParams`; `gate_for_point_with`; `CalibrationStatus
  (Calibrated/ShadowOnly)`; `authorize` (human-provenance gate). Composes
  `common_core::calibration::calibrate_threshold` and `io::write_atomic`;
  `calibrate_with` records `passed` via default gate even with custom
  `GateParams`.
- `decision/eval.rs` — Thinnest wrapper: `ScoredCase`; `evaluate_point/
  control_fires/gate_for_point/render_point_table/emit_point_artifact` all
  delegate to `common_core::calibration`.
- `decision/flow.rs` — Low-boilerplate logical flow (specs built
  internally via `FlowAsk {id/version/act/timeout_ms}`; transport and
  validation failures resolve to the caller's fail-closed default, never
  an exception): `semantic_if` (gated yes/no → `bool`), `match_arm`
  (closed multi-arm dispatch → `(index, Option<Decision>)`),
  `rerank_items` (one-call Score rank over `RerankItem`s → input order,
  input order preserved on failure), `descend_tree` (per-node options +
  act gates over `TreeLevel`s → `DescentOutcome {path/decision/settled}`).
  Composes `decide_all`/`rank_score`/`match_text_label`; pure table tests
  with `ScriptedDecideBackend`, no call-site specifics.
- `decision/shadow.rs` — `ShadowOutcome` + `record`; `ShadowLog
  (record/outcomes/len/is_empty)`; `shadow_adapter` (byte-identical
  passthrough via `ComponentAdapter::with_execute_override`);
  `instrumented_shadow` (+ `Instrumented::with_metrics`). Exemplary
  `fluent-wvr` wrapper composition.
- `decision/canary.rs` — `CanaryCase`; `canary_suite` (2 fixed cases);
  `CanaryDrift`; `verify_canary` (degraded always drifts); `canary_signature`
  (blake3 over framed label + quantized mass). Composes
  `common_core::golden::compare_series`.
- `decision/bridge.rs` — Detached execution. `ShadowJob/ShadowWork`;
  `ShadowBridge (new/submit/shutdown/shutdown_blocking/dropped/completed/
  error/timeout/panic_counts)` (`Drop` aborts driver; `new` panics without
  runtime); `ReplayItem/ReplayOutcome/replay_shadow_log` (offline replay via
  `LlmClient::decide_async`). Raw `tokio::mpsc/spawn/timeout/
  catch_unwind` instead of a `fluent-concurrency` pool.
- `decision/wvr.rs` — Pure `WorkUnit`s over typed `WorkContext` channels:
  `RenderQuestionUnit` (state/question → prompt), `DecodeDecisionUnit`
  (raw_response → `Decision`, choice-only), `PolicyVerdictUnit` (decision →
  `Verdict`). Full `impl_fieldless/impl_component/Describable/
  describe_work_unit` stack.
- `decision/systemone.rs` — Native `/v1/systemone` decision transport:
  `SystemOneBackend` (bounded classifier over shared `state` + named
  `choice`/`score`/`noul` questions) mapped onto the `DecideBackend` seam,
  so decision points never send chat-completion JSON to a non-generative
  model. Loopback-only transport by default.

### Utilities

- `tokens.rs` — `estimate_tokens/estimate_tokens_floor/estimate_tokens_with/
  estimate_chars_for_tokens`; `AtomicTokenBudget (reserve/release/remaining)`;
  `TokenBudget::truncate_to_budget` (ratio-based char cut + `"..."`, not
  token-accurate); `truncate_middle` (documented overlap footgun);
  `ChunkConfig/chunk_document/split_into_sentences/build_overlap`.
- `context_packer.rs` — `ContextPacker (new/max_tokens/estimate_tokens/
  select_lod_by_distance/pack_context/ffd_pack)` over `tokens`. System/query
  bypass the budget (only context truncates); distance thresholds are magic
  numbers.
- `parse.rs` — `JsonParseError (NoJson/Serde)`; `strip_json_fence/
  extract_first_json_value/repair_json/close_open_containers/
  drop_incomplete_tail/parse_json_response/parse_typed/parse_rerank_output`
  plus the strict model-output span contract: `extract_first_json_span`
  (first balanced `{...}`/`[...]` as an unparsed slice; stack-matched,
  fails the whole scan on the first unbalanced opener — unlike
  `extract_first_json_value`, which parses and skips past unparseable
  regions) and `extract_fence` (first ```lang fence, ```-fallback,
  `None` on missing close/empty body).
  Multi-stage fallback (direct → extract → repair → close → tail-drop) over
  `fluent_wvr::boundary::repair_boundary` + `coerce::escaped_char`; fast path
  bypasses defaults/sanitize.
- `prompt.rs` — `RouteEntry/render_available_routes/substitute_thresholds/
  build_self_assessment_prompt` (pure string, moved verbatim from router).
- `thinking.rs` — `strip_thinking_blocks/strip_think_block/strip_tag_pairs/
  strip_plain_thinking`; `StreamingThinkFilter (push/finish)` with split-tag
  holdback (streaming covers `<think>/<thinking>` only; hex-escaped literals
  hide tags in source).
- `roles.rs` — `ChatRole (System/User/Assistant/Tool)` +
  `as_str/FromStr/TryFrom/Display`; `RoleParseError`. Closed vocab over the
  `String` wire type.
- `constants.rs` — `MAX_EMBEDDING_DIMENSIONS`,
  `DEFAULT_TOTAL_TIMEOUT_MS/DEFAULT_IDLE_TIMEOUT_MS/RETRY_INTERVAL_S`.
- `error.rs` — Alias shim only: `type EmbedError = EmbeddingError`.
- `telemetry.rs` — `ToolName/ProviderCategory/FeatureName`;
  `TelemetryEvent (ToolInvoked/ToolCompleted/Routing/Error/FeatureUsed)`;
  `TelemetrySink` trait; `TracingSink/NoopSink`. Composes
  `http_class::FailureClass`.
- `testutil.rs` — `StubBackend (new/served_keys)`;
  `CountingStubBackend (new/failed/consults)`; `StubSessionLoader`;
  `Box::leak` for `&'static` ids is intentional.
- `anonymize.rs` — `anonymize` (12 sequential regex rewrites via
  `pii_patterns::anonymize_actions`); `build_anonymize_map` (byte-diff
  reverse map; `find_matching_len` takes 1 alnum/punct char per placeholder).
- `pii_patterns.rs` — 12 `LazyLock<Regex>` statics;
  `PiiPattern/pii_patterns` (locked 5) `/pii_patterns_all` (12)/
  `AnonymizeAction/anonymize_actions`; `GENERIC_API_KEY` overfires by design;
  IPV4 has no octet-range check.
- `pii_label.rs` — `PiiLabel` (12 variants) + `as_str/FromStr/Display`;
  `PiiLabelParseError`. Mirrors `ChatRole` closed-vocab pattern.
- `pii_calibration.rs` — Doc-only module; behavior lives in
  `tests/pii_calibration.rs`.
- `artifact.rs` — Largest module (~1900 lines): `SourceKind
  (HuggingFace/ModelScope)`; `ModelArtifactSource/ModelArtifact/
  ModelArtifactDownloadProgress/ResolvedModelArtifacts`; `FailureKind/
  ArtifactDownloadError/ArtifactError/ArtifactFetchResponse/ArtifactFetchError`;
  `ArtifactBody/ArtifactFetcher/ReqwestArtifactFetcher/ArtifactTimeouts/
  ResolveArtifactsOptions`; `model_artifact_url/snapshot_fingerprint/
  snapshot_marker_path/snapshot_lock_path`; `DownloadProgressReporter/
  EmbeddingProgress`. Resolution + hashing + fallback + progress in one file;
  storage seam is the injected `ArtifactFetcher`.
- `artifact_lock.rs` — `LockOptions` (poll 250ms/stale 10m/heartbeat 30s);
  `LockError (Io/NotOwned)`; `ArtifactCacheLock
  (touch/assert_owned/release)`; `acquire_artifact_cache_lock` (async poll +
  PID-liveness takeover); `local_hostname`. Uses `common_core::io`
  ns-resolution timestamps; sync heartbeats + async acquire mix.
- `grants.rs` — Remote-embedding authorization. `REMOTE_EMBEDDING_CAPABILITY`;
  `RemoteEmbeddingTarget`; `GrantError`; `RemoteEmbeddingWorkspaceGrant`;
  `RemoteEmbeddingAuthorizationStore (grant_path/has_grant/grant/revoke/
  revoke_all)`; `RemoteAuthorization (Stored/OneShot)`; `ElicitationRequest/
  ElicitationDecision/Elicitor`; `ensure_remote_embedding_authorized`. Own
  HMAC-signed JSON doc store (`hmac_sha256`, constant-time compare) with
  per-root lock fan-out.
- `cache.rs` — `CachedResponse`; `ResponseCache
  (new/with_delete/with_clear/get/set/invalidate_key/
  invalidate_key_raw/invalidate_all)`; key is
  `"{model}:{sha256(request_json)}"`. Closure-injected storage, lazy TTL
  only, no size bound, `set` best-effort.

## Key flows (where to start reading)

- Chat: `LlmClient::chat_complete_async` → `chat_complete_http_async` →
  `openai::build_openai_chat_body` → shared HTTP client
  (`common_core::http`) + `retry_async` → `thinking::strip_think_block` →
  `parse::parse_typed`. Queue variant: `llm_queue::build_default_queue` →
  `protocol::LlmRequestQueue` (`ResultPool`) → `default_handler`.
- Decision: `LlmClient::decide_async` → `decision/transport::DecisionTransport::
  prepare` (redact/audit) → `decision/ladder::decide_with_ladder`
  (rule → `decision/cache::DecisionCache` → model via `DecideBackend` →
  fallback) → `decision/decode` (logprob → `Decision`) →
  `decision/types::decide_verdict` (→ `Verdict`). Batch: `decision/bulk::
  decide_all` over `ResultPool`. Calibration: `decision/eval` →
  `common-core::calibration`; persistence in `decision/registry`; drift via
  `decision/canary`; observation via `decision/shadow` + `decision/bridge`.
- Embeddings: `factory::create_embedding_model` (catalog → reference) →
  `embeddings::EmbeddingProvider` (Ollama/OpenAI/GGUF/Qwen) →
  `CachedEmbeddingProvider` → `BatchEmbedding`. Local weights:
  `artifact::resolve_model_artifacts` (+ `artifact_lock`) → `gguf` or
  `onnx_session` → `backend::InferenceRegistry` → `runtime::LlmResidencyEngine`.
