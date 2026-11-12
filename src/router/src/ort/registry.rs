//! Onnx session-registry composition: the boot-built `OrtSessionRegistry`,
//! the overlay construction derived from it, and the shared registry alias.

use std::sync::Arc;

use fluent_llm::onnx_error::OrtError;
use fluent_llm::onnx_session::OrtSessionRegistry;
#[cfg(feature = "onnx")]
use tracing::info;

use crate::config::RouterConfig;

/// Convenience alias for the shared onnx registry.
pub type OrtRegistry = Arc<OrtSessionRegistry>;

/// Build the disambiguation overlays for the overlay stage:
/// one `PromptRouterOverlay` per `overlay_models`
/// key whose registered onnx model is a `ZeroShotRouting` two-tower session,
/// each scored against the given route descriptions. The router (composition
/// root) supplies the route labels from `RoutingConfig`.
#[cfg(feature = "onnx")]
/// Build the disambiguation overlays for the overlay stage:
/// one `PromptRouterOverlay` per `overlay_models`
/// key whose registered onnx model is a `ZeroShotRouting` two-tower session,
/// each scored against the given route descriptions. The router (composition
/// root) supplies the route labels from `RoutingConfig`.
/// No-ort build: no overlays can be built — the builder skips the overlay
/// stage (fail-open).
pub fn disambiguation_overlays<S: std::hash::BuildHasher>(
    registry: &OrtRegistry,
    model_keys: &[String],
    routes: &std::collections::HashMap<String, crate::config::RouteRef, S>,
) -> Result<Vec<Arc<dyn fluent_llm::backend::ResidualOverlay>>, OrtError> {
    let labels: Vec<fluent_llm::backend::RouteLabel> = routes
        .iter()
        .map(|(name, r)| fluent_llm::backend::RouteLabel {
            route: name.clone(),
            description: r.description.clone(),
        })
        .collect();
    let mut overlays = Vec::new();
    for key in model_keys {
        let Some(overlay) =
            fluent_onnx::build_prompt_router_overlay(registry, key, labels.clone())?
        else {
            continue;
        };
        tracing::info!(
            target: "router.ort",
            model = %key,
            route_labels = labels.len(),
            "prompt-router overlay built",
        );
        overlays.push(overlay);
    }
    Ok(overlays)
}

/// No-ort build: no overlays can be built — the builder skips the overlay
/// stage (fail-open).
#[cfg(not(feature = "onnx"))]
pub fn disambiguation_overlays<S: std::hash::BuildHasher>(
    _registry: &OrtRegistry,
    model_keys: &[String],
    _routes: &std::collections::HashMap<String, crate::config::RouteRef, S>,
) -> Result<Vec<Arc<dyn fluent_llm::backend::ResidualOverlay>>, OrtError> {
    if !model_keys.is_empty() {
        tracing::warn!(
            target: "router.ort",
            models = ?model_keys,
            "overlay models declared but this build has the `onnx` feature off; \
             overlay stage skipped (fail-open)",
        );
    }
    Ok(Vec::new())
}

/// Build the `OrtSessionRegistry` from the role-based ONNX fleet in `config`.
///
/// Each configured role (`encoder`/`pii`/`router`/`policy`/`colbert`) is
/// registered under its stable `OnnxRole::registry_key()` with the task implied
/// by the role. Lifecycle semantics parallel the llama.cpp fleet:
///
/// - `resident: true` (Always) roles load at boot (loud error on a missing
///   model file — the registry refuses to boot half-configured).
/// - `resident: false` (Unloadable) roles register lazily and load on first
///   use.
/// - Task-vs-`config.json` mismatches are loud boot errors.
///
/// Absent ONNX config yields `None` (fully fail-open — the pipeline is
/// pure-deterministic). When the crate is built without the `onnx` feature,
/// `None` is returned.
#[cfg(feature = "onnx")]
/// Build the `OrtSessionRegistry` from the role-based ONNX fleet in `config`.
/// 
/// Each configured role (`encoder`/`pii`/`router`/`policy`/`colbert`) is
/// registered under its stable `OnnxRole::registry_key()` with the task implied
/// by the role. Lifecycle semantics parallel the llama.cpp fleet:
/// 
/// - `resident: true` (Always) roles load at boot (loud error on a missing
/// model file — the registry refuses to boot half-configured).
/// - `resident: false` (Unloadable) roles register lazily and load on first
/// use.
/// - Task-vs-`config.json` mismatches are loud boot errors.
/// 
/// Absent ONNX config yields `None` (fully fail-open — the pipeline is
/// pure-deterministic). When the crate is built without the `onnx` feature,
/// `None` is returned.
/// No-ort build: the ONNX fleet degrades to nothing (fail-open), but its
/// presence is worth a loud warning so a config that expects ort wiring is not
/// silently degraded.
pub fn build_onnx_registry(
    config: &RouterConfig,
) -> Result<Option<Arc<OrtSessionRegistry>>, OrtError> {
    let Some(fleet) = config.onnx.as_ref() else {
        return Ok(None);
    };
    if fleet.is_empty() {
        return Ok(None);
    }
    // Resolve tokenizer paths: roles like router and PII often share the
    // encoder's tokenizer. When a role has no tokenizer_path, inherit it
    // from the encoder role. This keeps the simplified config format concise.
    // The generative `Llm` role is exempt — its tokenizer is its own (the
    // model's `tokenizer.json`), never the encoder's.
    let encoder_tokenizer = fleet
        .encoder
        .as_ref()
        .and_then(|e| e.model.tokenizer_path.clone());
    let registry = OrtSessionRegistry::new(Arc::new(fluent_onnx::OrtSessionLoader));
    for (role, role_cfg) in fleet.iter() {
        let mut resolved_cfg = role_cfg.clone();
        if resolved_cfg.model.tokenizer_path.is_none()
            && role != fluent_llm::onnx_config::OnnxRole::Llm
        {
            if let Some(ref tok) = encoder_tokenizer {
                resolved_cfg.model.tokenizer_path = Some(tok.clone());
                info!(
                    target: "router.ort",
                    role = ?role,
                    tokenizer = %tok.display(),
                    "tokenizer_path inherited from encoder role",
                );
            }
        }
        let mut model = resolved_cfg.to_onnx_config(role);
        // `onnx_dir` prefixes every relative ONNX file declaration
        // (absolute paths pass through) so the session loaders below see
        // fully resolved paths. Applied after the tokenizer inheritance
        // above, so an inherited relative path resolves identically.
        model.model_path = config.resolve_onnx_path(&model.model_path);
        model.tokenizer_path = model
            .tokenizer_path
            .as_deref()
            .map(|p| config.resolve_onnx_path(p));
        model.label_source = model
            .label_source
            .as_deref()
            .map(|p| config.resolve_onnx_path(p));
        model.policy_labels = model
            .policy_labels
            .as_deref()
            .map(|p| config.resolve_onnx_path(p));
        model.validate()?;
        info!(
            target: "router.ort",
            role = ?role,
            key = role.registry_key(),
            task = ?model.task,
            resident = model.resident,
            pinned = role_cfg.pinned,
            sleep_idle_seconds = role_cfg.sleep_idle_seconds,
            model_path = %model.model_path.display(),
            "onnx role registered",
        );
        // Register with the full residency lifecycle so the onnx residency
        // loop honors the role's `pinned` / `sleep_idle_seconds` directly from
        // the registry entry — no parallel table.
        let policy = model.policy();
        registry.register_with_lifecycle(
            role.registry_key().to_string(),
            model,
            policy,
            role_cfg.pinned,
            role_cfg.sleep_idle_seconds,
        )?;
    }
    Ok(Some(Arc::new(registry)))
}

/// No-ort build: the ONNX fleet degrades to nothing (fail-open), but its
/// presence is worth a loud warning so a config that expects ort wiring is not
/// silently degraded.
#[cfg(not(feature = "onnx"))]
pub fn build_onnx_registry(
    config: &RouterConfig,
) -> Result<Option<Arc<OrtSessionRegistry>>, OrtError> {
    if let Some(fleet) = config.onnx.as_ref() {
        if !fleet.is_empty() {
            let roles: Vec<&'static str> = fluent_llm::onnx_config::OnnxRole::all()
                .iter()
                .filter(|r| fleet.has(**r))
                .map(|r| r.registry_key())
                .collect();
            tracing::warn!(
                target: "router.ort",
                roles = ?roles,
                "onnx roles declared but this build has the `onnx` feature off; \
                 treating them as unavailable (fail-open)",
            );
        }
    }
    Ok(None)
}