//! Onnx pre-filter and scorer builders: the trained-encoder annotation seam,
//! the review PII pre-filter, and the ColBERT entity-link scorer.

use std::sync::Arc;

use fluent_llm::onnx_error::OrtError;

use super::registry::OrtRegistry;

/// Build the trained-encoder annotation seam:
/// an `EncoderFetchSync` closure that runs
/// the trained-encoder annotation worker over the doc's orth/idx and produces
/// an `AnnotationSet` aligned by construction (record `text` = spacy orth).
///
/// `Ok(Some(closure))` when a registered `FillMask` encoder with declared
/// `annotation_heads` is available; `Ok(None)` when no such encoder is
/// configured (fail-open — the encoder rung is absent from the ladder, exactly
/// the pre-M2 behavior). The closure returns `Err` on any `None`-aligned
/// token, unknown label, or session error, so the ladder falls through to
/// ArcEager (never a partial mix, never a wrong enum).
#[cfg(feature = "onnx")]
pub fn nlp_encoder_fetch(
    registry: &OrtRegistry,
    encoder_model: &str,
) -> Result<Option<spacy_rs::pipeline::EncoderFetchSync>, OrtError> {
    use spacy_rs::pipeline::{AnnotateError, EncoderFetchSync};
    use spacy_rs::{AnnotationSet, Doc, Lemmatizer};

    let Some(worker) = fluent_onnx::build_annotation_worker_from_registry(registry, encoder_model)?
    else {
        // No registered FillMask+heads encoder: the encoder rung is absent
        // (identical to the pre-M2 behavior). A declared-but-untyped model is
        // worth a warning so a mis-configured encoder is never silent.
        if registry.is_registered(encoder_model) {
            tracing::warn!(
                target: "router.ort",
                model = %encoder_model,
                "encoder_model configured but not a registered FillMask session with \
                 annotation_heads — encoder rung absent (fail-open to ArcEager)",
            );
        }
        return Ok(None);
    };

    // The deterministic rule lemmatizer supplies the lemma field (spacy-rs's
    // rule base form). The encoder predicts pos/dep/head only; lemma stays
    // deterministic (VISION: the model annotates *given* tokens, it does not
    // own lexical data).
    let lemmatizer = Lemmatizer::english_rule();
    let closure: EncoderFetchSync =
        Arc::new(move |doc: &Doc| -> Result<AnnotationSet, AnnotateError> {
            // The doc's reconstructed text is the byte source both the LFM
            // tokenizer and the spacy spans index (orth + idx → spacy_spans).
            let text = doc.text();
            let orth: Vec<String> = (0..doc.len()).map(|i| doc.token_text(i)).collect();
            let idx: Vec<usize> = (0..doc.len()).map(|i| doc.token(i).idx as usize).collect();
            let spans = fluent_onnx::SpacyTokenAligner::spacy_spans(&orth, &idx);

            // Session error → Err → the ladder falls through to ArcEager.
            let annotations = worker
                .annotate(&text, &spans)
                .map_err(|e| AnnotateError::Encoder(e.to_string()))?;
            map_annotations(doc, &lemmatizer, &annotations)
        });

    tracing::info!(
        target: "router.ort",
        model = %encoder_model,
        "encoder annotation seam built (trained UPOS/dep/head rung active)",
    );

    Ok(Some(closure))
}

/// Map `OrtAnnotationWorker` output onto an [`spacy_rs::AnnotationSet`] for
/// `doc`, aligned **by construction** (record `text` = spacy orth). Pure — no
/// session. `pos`/`dep` are the label strings; `head` is the F8 relative
/// offset (`head_abs − i`); a `None` head (special / outside every spacy span)
/// is decoded as ROOT (`dep="root"`, `head=0`); the lemma comes from the
/// deterministic rule [`spacy_rs::Lemmatizer`].
///
/// Returns `Err` on any `None`-aligned token or unknown label, so the ladder
/// falls through to ArcEager — never a partial mix, never a wrong enum.
#[cfg(feature = "onnx")]
pub(crate) fn map_annotations(
    doc: &spacy_rs::Doc,
    lemmatizer: &spacy_rs::Lemmatizer,
    annotations: &[Option<fluent_onnx::TokenAnnotation>],
) -> Result<spacy_rs::AnnotationSet, spacy_rs::pipeline::AnnotateError> {
    use spacy_rs::pipeline::AnnotateError;
    use spacy_rs::{AnnotationRecord, AnnotationSet, Upos};

    if annotations.len() != doc.len() {
        return Err(AnnotateError::Encoder(format!(
            "encoder produced {} annotations for {} spacy tokens",
            annotations.len(),
            doc.len()
        )));
    }
    let mut records = Vec::with_capacity(doc.len());
    for (i, t) in annotations.iter().enumerate() {
        let orth = doc.token_text(i);
        let t = t.as_ref().ok_or_else(|| {
            AnnotateError::Encoder(format!(
                "spacy token {i} has no covering LFM subword — encoder rung falls back"
            ))
        })?;
        // Unknown label → Err → ArcEager (the 7-check gate would reject it
        // anyway; fail-open before a wrong enum is ever written).
        let pos: Upos = t
            .pos
            .parse()
            .map_err(|_| AnnotateError::Encoder(format!("unknown UPOS label {:?}", t.pos)))?;
        let lemma = lemmatizer
            .lemmatize(&orth, pos, 0)
            .into_iter()
            .next()
            .unwrap_or_else(|| orth.to_ascii_lowercase());
        // A head that maps to a special token / outside every spacy span is
        // decoded as ROOT (head=0, dep="root"); otherwise the F8 relative
        // offset is the absolute spacy head minus the token's own index.
        let (dep, head) = match t.head_abs {
            Some(h) => (t.dep.clone(), (h as i32) - (i as i32)),
            None => ("root".to_string(), 0),
        };
        records.push(AnnotationRecord {
            text: orth,
            pos: t.pos.clone(),
            tag: String::new(),
            dep,
            head,
            lemma,
            morph: String::new(),
            ent_iob: String::new(),
            ent_type: String::new(),
        });
    }
    Ok(AnnotationSet(records))
}

/// No-ort build: the encoder annotation seam is unavailable (fail-open).
#[cfg(not(feature = "onnx"))]
pub fn nlp_encoder_fetch(
    _registry: &OrtRegistry,
    encoder_model: &str,
) -> Result<Option<spacy_rs::pipeline::EncoderFetchSync>, OrtError> {
    if !encoder_model.is_empty() {
        tracing::warn!(
            target: "router.ort",
            model = %encoder_model,
            "encoder model declared but this build has the `onnx` feature off — \
             encoder rung unavailable (fail-open)",
        );
    }
    Ok(None)
}

/// Build the review worker's PII pre-filter:
/// the ort PII-Detector when `pii_model` names a registered
/// `TokenClassification` session, else the deterministic `RegexPiiDetector`
/// when `auto_enqueue` requires a pre-filter. Fail-open: a mis-declared or
/// absent model degrades to the regex baseline (never a boot error, never a
/// silent no-pre-filter when `auto_enqueue` is on).
///
/// M6: the shared tail of both cfg definitions below — the deterministic
/// regex baseline when `auto_enqueue` requires a pre-filter, else no
/// pre-filter. The cfg split itself stays (the onnx classifier type does not
/// exist without the feature); only the repeated tail is factored out.
fn regex_baseline_or_none(
    auto_enqueue: bool,
) -> Option<Arc<dyn fluent_llm::backend::PiiSpanDetector>> {
    if auto_enqueue {
        Some(Arc::new(fluent_llm::RegexPiiDetector))
    } else {
        None
    }
}

#[cfg(feature = "onnx")]
pub fn pii_prefilter(
    registry: Option<&OrtRegistry>,
    pii_model: Option<&str>,
    auto_enqueue: bool,
) -> Result<Option<Arc<dyn fluent_llm::backend::PiiSpanDetector>>, OrtError> {
    let regex_fallback = || regex_baseline_or_none(auto_enqueue);
    match pii_model {
        Some(key) => {
            if let Some(registry) = registry {
                if let Some(detector) = fluent_onnx::build_pii_classifier(registry, key)? {
                    tracing::info!(
                        target: "router.ort",
                        model = %key,
                        "ort PII-Detector pre-filter built",
                    );
                    Ok(Some(detector))
                } else {
                    tracing::warn!(
                        target: "router.ort",
                        model = %key,
                        "pii_model configured but not a registered TokenClassification \
                         session — falling back to the regex PII baseline",
                    );
                    Ok(if auto_enqueue { regex_fallback() } else { None })
                }
            } else {
                tracing::warn!(
                    target: "router.ort",
                    model = %key,
                    "pii_model configured but no onnx registry is available — falling back to \
                     the regex PII baseline",
                );
                Ok(if auto_enqueue { regex_fallback() } else { None })
            }
        }
        None => Ok(if auto_enqueue { regex_fallback() } else { None }),
    }
}

/// No-ort build: the ort classifier is unavailable, so the pre-filter is the
/// deterministic `RegexPiiDetector` when `auto_enqueue` requires one (fail-open).
/// M6: shares the `regex_baseline_or_none` tail with the onnx definition above.
#[cfg(not(feature = "onnx"))]
pub fn pii_prefilter(
    _registry: Option<&OrtRegistry>,
    pii_model: Option<&str>,
    auto_enqueue: bool,
) -> Result<Option<Arc<dyn fluent_llm::backend::PiiSpanDetector>>, OrtError> {
    if pii_model.is_some() {
        tracing::warn!(
            target: "router.ort",
            pii_model = pii_model,
            "pii_model declared but this build has the `onnx` feature off — using the \
             regex PII baseline",
        );
    }
    Ok(regex_baseline_or_none(auto_enqueue))
}

/// Build the ColBERT-backed entity-link scorer:
/// bakes an [`fluent_onnx::EntitySimilarityIndex`] over the concept
/// store's labels at boot, then returns an [`EntityLinkScorer`] closure that
/// encodes a span via the retriever, looks it up against the baked index, and
/// resolves each hit's canonical → `InterlinguaId` through the store.
///
/// Fail-open: no model registered / not a `LateInteraction` task / no labels →
/// an empty scorer (yields no candidates, identical to the pre-M3 stub). A
/// lookup-time encoding error also degrades to "no candidates" (loud warn) —
/// never a block, never a drop.
#[cfg(feature = "onnx")]
pub fn colbert_entity_scorer(
    registry: &OrtRegistry,
    model_key: &str,
    concept_store: &Arc<dyn fluent_concept::ConceptStore>,
    threshold: f64,
) -> Result<crate::server::entity_link::EntityLinkScorer, OrtError> {
    use fluent_onnx::colbert::{bake_entity_index, EntitySimilarityIndex};
    use fluent_types::InterlinguaId;

    let empty: crate::server::entity_link::EntityLinkScorer = Arc::new(|_text| Vec::new());
    let Some(retriever) = fluent_onnx::build_colbert_from_registry(registry, model_key)? else {
        if registry.is_registered(model_key) {
            tracing::warn!(
                target: "router.ort",
                model = %model_key,
                "entity-link scorer model configured but not a registered LateInteraction \
                 session — empty scorer (fail-open)",
            );
        }
        return Ok(empty);
    };

    // Bake once at boot over the concept store's labels: (namespace, canonical,
    // label) triples. An absent label (None) falls back to the canonical name.
    let concepts: Vec<(String, String, String)> = concept_store
        .iter_ids()
        .filter_map(|id| concept_store.get(id).ok())
        .map(|meta| {
            let namespace = format!("{:?}", meta.namespace);
            let label = meta.label.unwrap_or_else(|| meta.canonical_name.clone());
            (namespace, meta.canonical_name, label)
        })
        .collect();
    let index: EntitySimilarityIndex = bake_entity_index(&retriever, &concepts, threshold as f32)?;
    tracing::info!(
        target: "router.ort",
        model = %model_key,
        concepts = concepts.len(),
        threshold = threshold,
        "entity-link ColBERT scorer baked (data-time)",
    );

    let store = Arc::clone(concept_store);
    Ok(Arc::new(move |text: &str| -> Vec<(InterlinguaId, f64)> {
        let query_tokens = match retriever.encode_query(text) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    target: "router.ort",
                    error = %e,
                    "entity-link query encoding failed — no candidates (fail-open)",
                );
                return Vec::new();
            }
        };
        score_span(&query_tokens, &index, store.as_ref())
    }))
}

/// Score already-encoded query tokens against a baked [`fluent_onnx::EntitySimilarityIndex`]
/// and map each above-threshold hit's canonical → `InterlinguaId` through the
/// store. Pure (no session) — the encode step is the only model-dependent part
/// of the entity-link scorer, so the mapping is hermetically testable.
#[cfg(feature = "onnx")]
pub(crate) fn score_span(
    query_tokens: &[Vec<f32>],
    index: &fluent_onnx::EntitySimilarityIndex,
    store: &dyn fluent_concept::ConceptStore,
) -> Vec<(fluent_types::InterlinguaId, f64)> {
    if query_tokens.is_empty() {
        return Vec::new();
    }
    let query_refs: Vec<&[f32]> = query_tokens.iter().map(Vec::as_slice).collect();
    index
        .lookup(&query_refs)
        .into_iter()
        .filter_map(|hit| {
            store
                .resolve_name(&hit.canonical)
                .ok()
                .map(|id| (id, f64::from(hit.score)))
        })
        .collect()
}

/// No-ort build: the ColBERT scorer is unavailable — empty scorer (fail-open).
#[cfg(not(feature = "onnx"))]
pub fn colbert_entity_scorer(
    _registry: &OrtRegistry,
    model_key: &str,
    _concept_store: &Arc<dyn fluent_concept::ConceptStore>,
    _threshold: f64,
) -> Result<crate::server::entity_link::EntityLinkScorer, OrtError> {
    if !model_key.is_empty() {
        tracing::warn!(
            target: "router.ort",
            model = %model_key,
            "entity-link scorer model declared but this build has the `onnx` feature off — \
             empty scorer (fail-open)",
        );
    }
    Ok(Arc::new(|_text| Vec::new()))
}
