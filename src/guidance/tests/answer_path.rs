//! Answer-path characterization: pins today's assembly and cache
//! wiring — extractive rows ride `AnswerMode::Auto` only, and the
//! answer cache persists only fully-bound synthesis output — so the
//! model-optional and cache-persistence changes flip these
//! expectations deliberately instead of drifting into them.
use super::*;
use crate::tests::common::{db_file_record, db_project_card, db_text_fragment};
use search_vector::db::GuidanceDb;
use std::cell::Cell;

/// Workspace with one code file, one doc, and the workspace card: the
/// minimum evidence set where factual intents get extractive rows and
/// synthesis has something to ground on.
fn answer_workspace() -> (tempfile::TempDir, GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(dir.path().join("lib.rs"), "pub fn alpha_sync() {}\n").expect("write");
    std::fs::create_dir(dir.path().join("doc")).expect("mkdir");
    let doc_text = "# Alpha\n\nAlpha sync engine overview.\n";
    std::fs::write(dir.path().join("doc").join("alpha.md"), doc_text).expect("write");
    let db = GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &db_file_record(dir.path(), "lib.rs"),
        &[db_text_fragment(
            "lib.rs",
            "alpha_sync",
            "pub fn alpha_sync() {}",
            1,
        )],
        &[],
    )
    .expect("seed");
    let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections("doc/alpha.md", doc_text);
    let chunk = chunks.first().expect("doc chunks");
    db.replace_project_cards(&[
        db_project_card(
            "workspace:ws",
            "workspace",
            "Workspace",
            "Ships Alpha.",
            "Cargo.toml",
            "",
        ),
        search_vector::db::ProjectCard {
            content_hash: chunk.content_hash.clone(),
            anchor: chunk.anchor.clone(),
            ..db_project_card(
                "doc:doc/alpha.md#alpha",
                "doc",
                "Alpha",
                &chunk.text,
                "doc/alpha.md",
                "",
            )
        },
    ])
    .expect("cards seed");
    (dir, db)
}

/// Counting synthesizer with a canned answer: records how often the
/// pipeline pays for generation.
struct CountingSynth {
    calls: Cell<u32>,
    answer: String,
}

impl CountingSynth {
    fn new(answer: &str) -> Self {
        Self {
            calls: Cell::new(0),
            answer: answer.to_string(),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.get()
    }
}

impl crate::read::engine::Synthesizer for CountingSynth {
    fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
        self.calls.set(self.calls.get() + 1);
        Ok(Some(self.answer.clone()))
    }
}

fn request(query: &str, mode: AnswerMode) -> ExplainRequest {
    ExplainRequest {
        query: query.to_string(),
        limit: 5,
        budget_override: None,
        no_llm: false,
        include_fixtures: false,
        answer_mode: mode,
    }
}

fn run(
    dir: &std::path::Path,
    db: &GuidanceDb,
    query: &str,
    mode: AnswerMode,
    synth: Option<&dyn crate::read::engine::Synthesizer>,
) -> ReadAnswer {
    run_with(dir, db, query, mode, false, synth)
}

fn run_with(
    dir: &std::path::Path,
    db: &GuidanceDb,
    query: &str,
    mode: AnswerMode,
    no_llm: bool,
    synth: Option<&dyn crate::read::engine::Synthesizer>,
) -> ReadAnswer {
    run_generation(dir, db, query, mode, no_llm, "", synth)
}

fn run_generation(
    dir: &std::path::Path,
    db: &GuidanceDb,
    query: &str,
    mode: AnswerMode,
    no_llm: bool,
    generation: &str,
    synth: Option<&dyn crate::read::engine::Synthesizer>,
) -> ReadAnswer {
    let engine = ReadEngine::new().with_generation(generation);
    let ctx = ExplainContext {
        workspace: dir,
        db,
        nlp: None,
        coverage: None,
    };
    let mut request = request(query, mode);
    request.no_llm = no_llm;
    engine.explain(&request, &ctx, synth)
}

fn has_answer(answer: &ReadAnswer) -> bool {
    answer.sections.iter().any(|s| s.title == "Answer")
}

const OWNERSHIP: &str = "Who owns the Alpha sync engine?";

#[test]
fn auto_factual_answers_extractively_without_any_model() {
    // Extractive rows are model-free: the shipped default renders an
    // answer with no synthesizer wired at all.
    let (dir, db) = answer_workspace();
    let answer = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    assert!(has_answer(&answer), "{:?}", answer.sections);
    assert!(answer.claims.is_empty(), "no claims without synthesis");
}

#[test]
fn evidence_mode_renders_the_extractive_answer() {
    // Extractive assembly is model-free by construction, so every mode
    // renders it; `evidence` still never synthesizes.
    let (dir, db) = answer_workspace();
    let auto = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    let evidence = run(dir.path(), &db, OWNERSHIP, AnswerMode::Evidence, None);
    assert!(has_answer(&auto), "auto renders: {:?}", auto.sections);
    assert!(
        has_answer(&evidence),
        "evidence mode renders the extractive answer: {:?}",
        evidence.sections
    );
    let body = |answer: &ReadAnswer| {
        answer
            .sections
            .iter()
            .find(|section| section.title == "Answer")
            .map(|section| section.body.clone())
            .unwrap_or_default()
    };
    assert_eq!(
        body(&auto),
        body(&evidence),
        "the same rows answer under both model-free modes"
    );
}

#[test]
fn no_llm_never_invokes_the_synthesizer() {
    // The seam is the only model contact: with `no_llm` the extractive
    // answer renders and the synthesizer is never touched.
    let (dir, db) = answer_workspace();
    let synth = CountingSynth::new("Alpha syncs here [C1].");
    let answer = run_with(
        dir.path(),
        &db,
        OWNERSHIP,
        AnswerMode::Auto,
        true,
        Some(&synth),
    );
    assert!(has_answer(&answer), "{:?}", answer.sections);
    assert_eq!(synth.calls(), 0, "no model calls under --no-llm");
}

#[test]
fn synthesis_mode_without_llm_falls_back_to_extractive() {
    let (dir, db) = answer_workspace();
    let synth = CountingSynth::new("Alpha syncs here [C1].");
    let answer = run_with(
        dir.path(),
        &db,
        OWNERSHIP,
        AnswerMode::Synthesis,
        true,
        Some(&synth),
    );
    assert!(has_answer(&answer), "{:?}", answer.sections);
    assert_eq!(synth.calls(), 0);
}

#[test]
fn synthesis_mode_keeps_the_generative_path() {
    let (dir, db) = answer_workspace();
    let synth = CountingSynth::new("Alpha syncs here [C1].");
    let answer = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&synth),
    );
    assert!(has_answer(&answer), "{:?}", answer.sections);
    assert_eq!(synth.calls(), 1, "the model path still runs when asked");
    assert!(!answer.claims.is_empty(), "gated claims ride the answer");
}

#[test]
fn extractive_answers_persist_and_replay() {
    // Both assembly paths share the one store: a model-free answer
    // persists and the repeat is a byte-identical replay.
    let (dir, db) = answer_workspace();
    let first = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    let second = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    assert!(has_answer(&first));
    assert!(!first.cache_hit, "first run computes");
    assert!(second.cache_hit, "repeat serves from the store");
    assert_eq!(
        crate::read::render::render_answer(&first),
        crate::read::render::render_answer(&second),
        "cached bytes reproduce the fresh answer"
    );
}

#[test]
fn mixed_claims_persist_the_gated_answer() {
    // The floor-passing gated answer persists even when the model
    // emitted an uncited sentence: the gate drops the sentence, the
    // remaining bound answer is the answer. The repeat is a hit.
    let (dir, db) = answer_workspace();
    let first_synth = CountingSynth::new("Alpha syncs here [C1]. This sentence names nothing.");
    let first = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&first_synth),
    );
    assert!(has_answer(&first), "{:?}", first.sections);
    assert_eq!(first_synth.calls(), 1);
    let second_synth = CountingSynth::new("A different answer entirely [C1].");
    let second = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&second_synth),
    );
    assert_eq!(second_synth.calls(), 0, "the repeat never regenerates");
    assert!(second.cache_hit, "the gated answer persisted");
    let rendered = crate::read::render::render_answer(&second);
    assert!(
        rendered.contains("Alpha syncs here"),
        "the stored gated body replays: {rendered}"
    );
    assert!(
        !rendered.contains("names nothing"),
        "uncited sentences never persisted: {rendered}"
    );
}

#[test]
fn mode_discriminator_separates_assembly_paths() {
    // Same evidence, different assembly: extraction and synthesis are
    // different answers and therefore different keys.
    let (dir, db) = answer_workspace();
    let extractive = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    assert!(!extractive.cache_hit);
    let synth = CountingSynth::new("Alpha syncs here [C1].");
    let generated = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&synth),
    );
    assert!(!generated.cache_hit, "a different mode is a different key");
    assert_eq!(synth.calls(), 1);
    let replay = CountingSynth::new("unused");
    let again = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&replay),
    );
    assert!(again.cache_hit);
    assert_eq!(replay.calls(), 0);
}

#[test]
fn source_mutation_misses_the_cache() {
    let (dir, db) = answer_workspace();
    let first = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    assert!(!first.cache_hit);
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn alpha_sync() { /* moved */ }
",
    )
    .expect("write");
    let second = run(dir.path(), &db, OWNERSHIP, AnswerMode::Auto, None);
    assert!(!second.cache_hit, "changed evidence is a new manifest");
}

#[test]
fn generation_change_invalidates_the_cache() {
    let (dir, db) = answer_workspace();
    let first = run_generation(
        dir.path(),
        &db,
        OWNERSHIP,
        AnswerMode::Auto,
        false,
        "g1",
        None,
    );
    assert!(!first.cache_hit);
    let same = run_generation(
        dir.path(),
        &db,
        OWNERSHIP,
        AnswerMode::Auto,
        false,
        "g1",
        None,
    );
    assert!(same.cache_hit, "same generation replays");
    let moved = run_generation(
        dir.path(),
        &db,
        OWNERSHIP,
        AnswerMode::Auto,
        false,
        "g2",
        None,
    );
    assert!(
        !moved.cache_hit,
        "a new generation never serves the old row"
    );
}

#[test]
fn fully_bound_synthesis_persists_and_replays() {
    // The one case the cache serves today; the contract the
    // persistence change must preserve on every assembly path.
    let (dir, db) = answer_workspace();
    let answer_text = "Alpha syncs here [C1]. The workspace ships it [D1].";
    let first_synth = CountingSynth::new(answer_text);
    let first = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&first_synth),
    );
    assert!(has_answer(&first));
    assert_eq!(first_synth.calls(), 1);
    assert!(!first.cache_hit, "first run computes");
    let second_synth = CountingSynth::new(answer_text);
    let second = run(
        dir.path(),
        &db,
        "How does Alpha sync work",
        AnswerMode::Synthesis,
        Some(&second_synth),
    );
    assert!(second.cache_hit, "repeat serves from the store");
    assert_eq!(second_synth.calls(), 0, "replay never regenerates");
    let rendered_first = crate::read::render::render_answer(&first);
    let rendered_second = crate::read::render::render_answer(&second);
    let body = |text: &str| {
        text.split("### Coverage")
            .next()
            .unwrap_or(text)
            .to_string()
    };
    assert_eq!(
        body(&rendered_first),
        body(&rendered_second),
        "cached bytes reproduce the fresh answer"
    );
}

fn finding(id: &str, path: &str, start: u32, end: u32, text: &str) -> crate::read::engine::Finding {
    crate::read::engine::Finding {
        id: id.to_string(),
        producer: crate::read::engine::FindingProducer::Database,
        kind: crate::read::engine::FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new(path),
        ),
        start_line: Some(start),
        end_line: Some(end),
        text: text.to_string(),
        producer_score: 1.0,
        provenance: "database".to_string(),
    }
}

fn doc_hit(path: &str, anchor: &str, body: &str) -> crate::read::planes::docs::DocHit {
    crate::read::planes::docs::DocHit {
        key: format!("doc:{path}#{anchor}"),
        title: anchor.to_string(),
        source_path: path.to_string(),
        anchor: anchor.to_string(),
        body: Some(body.to_string()),
        verified: true,
        code_paths: Vec::new(),
    }
}

fn card(id: &str, body: &str) -> crate::read::planes::orientation::CardFinding {
    crate::read::planes::orientation::CardFinding {
        id: id.to_string(),
        kind: "workspace".to_string(),
        title: id.to_string(),
        body: body.to_string(),
        source: "README.md".to_string(),
        code_paths: Vec::new(),
    }
}

fn manifest_hash(
    findings: &[crate::read::engine::Finding],
    docs: &[crate::read::planes::docs::DocHit],
    cards: &[crate::read::planes::orientation::CardFinding],
) -> String {
    crate::grounding::evidence_manifest_hash(
        findings,
        docs,
        cards,
        "test-model",
        crate::read::admission::ANSWER_PROMPT_VERSION,
        "synthesis",
    )
}

#[test]
fn evidence_manifest_hash_is_order_insensitive_and_stable() {
    let findings = vec![finding("a", "src/a.rs", 1, 3, "fn a() {}")];
    let docs = vec![doc_hit("doc/x.md", "x", "body")];
    let cards = vec![card("workspace:ws", "map body")];
    let first = manifest_hash(&findings, &docs, &cards);
    let second = manifest_hash(&findings, &docs, &cards);
    assert_eq!(first, second, "same inputs, same hash");

    let mut shuffled_findings = vec![
        finding("b", "src/b.rs", 2, 4, "fn b() {}"),
        finding("a", "src/a.rs", 1, 3, "fn a() {}"),
    ];
    shuffled_findings.sort_by(|l, r| l.id.cmp(&r.id));
    let reordered = manifest_hash(&shuffled_findings, &docs, &cards);
    let mut with_a_first = shuffled_findings.clone();
    with_a_first.reverse();
    assert_eq!(
        manifest_hash(&with_a_first, &docs, &cards),
        reordered,
        "input order never moves the hash"
    );
}

#[test]
fn evidence_manifest_hash_tracks_evidence_bytes() {
    let findings = vec![finding("a", "src/a.rs", 1, 3, "fn a() {}")];
    let docs = vec![doc_hit("doc/x.md", "x", "body")];
    let cards = vec![card("workspace:ws", "map body")];
    let base = manifest_hash(&findings, &docs, &cards);
    let changed_finding = vec![finding("a", "src/a.rs", 1, 3, "fn a() { edited }")];
    assert_ne!(base, manifest_hash(&changed_finding, &docs, &cards));
    let changed_doc = vec![doc_hit("doc/x.md", "x", "edited body")];
    assert_ne!(base, manifest_hash(&findings, &changed_doc, &cards));
    let changed_card = vec![card("workspace:ws", "edited map")];
    assert_ne!(base, manifest_hash(&findings, &docs, &changed_card));
    assert_ne!(
        base,
        crate::grounding::evidence_manifest_hash(
            &findings,
            &docs,
            &cards,
            "other-model",
            crate::read::admission::ANSWER_PROMPT_VERSION,
            "synthesis",
        ),
        "model identity keys the entry"
    );
}

#[test]
fn extractive_rows_window_oversized_bodies() {
    // Doc and card rows carry prose bodies the code clipper never saw;
    // the answer rows must window them the same way.
    use std::fmt::Write;
    let mut giant = String::from("//! Signature line\n");
    for i in 0..200 {
        let _ = writeln!(giant, "    let pad_{i} = {i};");
    }
    let rendered = render_extractive(&[("[D1] doc/x.md#x".to_string(), giant)]);
    assert!(rendered.contains("//! Signature line"), "{rendered}");
    assert!(rendered.contains("… [truncated]"), "{rendered}");
    assert!(!rendered.contains("pad_199"), "{rendered}");
}

#[test]
fn extractive_rows_pass_small_bodies_verbatim() {
    let rendered =
        render_extractive(&[("[C1] src/a.rs:1-1".to_string(), "fn tiny() {}".to_string())]);
    assert!(rendered.contains("fn tiny() {}"), "{rendered}");
    assert!(!rendered.contains("… [truncated]"), "{rendered}");
}
