//! Golden battery over the read engine: per-task snapshots of rendered
//! sections, citation paths, max citation span, and render size. The
//! fixture workspace reproduces the measured output shapes (a
//! fixture-scope manifest card and an oversized code span) so shape
//! changes flip goldens deliberately through the expected-changes
//! manifest, never silently.
use super::*;
use crate::eval::load_eval_tasks;
use crate::query::search_db::GuidanceSearchDb;
use crate::read::render::render_answer;
use crate::tests::common::{db_file_record, db_project_card, db_text_fragment};

const BATTERY: &str = "corpora/benchmark_tasks.jsonl";
const GOLDEN_DIR: &str = "tests/goldens/explain";
const EXPECTED_CHANGES: &str = "tests/goldens/expected_changes.json";
const UPDATE_ENV: &str = "EXPLAIN_GOLDEN_UPDATE";

/// One pinned output shape for one battery task.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Golden {
    id: String,
    route: String,
    sections: Vec<String>,
    answer_present: bool,
    citations: Vec<String>,
    max_span: u64,
    fixture_paths: Vec<String>,
    rendered_bytes: usize,
    tokens_used: u64,
    gaps_count: usize,
}

impl Golden {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "route": self.route,
            "sections": self.sections,
            "answer_present": self.answer_present,
            "citations": self.citations,
            "max_span": self.max_span,
            "fixture_paths": self.fixture_paths,
            "rendered_bytes": self.rendered_bytes,
            "tokens_used": self.tokens_used,
            "gaps_count": self.gaps_count,
        })
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        let get = |key: &str| {
            value
                .get(key)
                .cloned()
                .ok_or_else(|| format!("missing {key}"))
        };
        let strings = |key: &str| -> Result<Vec<String>, String> {
            get(key)?
                .as_array()
                .ok_or_else(|| format!("{key} not an array"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{key} not text"))
                })
                .collect()
        };
        let number = |key: &str| -> Result<u64, String> {
            get(key)?
                .as_u64()
                .ok_or_else(|| format!("{key} not a number"))
        };
        Ok(Self {
            id: get("id")?.as_str().ok_or("id not text")?.to_string(),
            route: get("route")?.as_str().ok_or("route not text")?.to_string(),
            sections: strings("sections")?,
            answer_present: get("answer_present")?
                .as_bool()
                .ok_or("answer_present not a bool")?,
            citations: strings("citations")?,
            max_span: number("max_span")?,
            fixture_paths: strings("fixture_paths")?,
            rendered_bytes: number("rendered_bytes")? as usize,
            tokens_used: number("tokens_used")?,
            gaps_count: number("gaps_count")? as usize,
        })
    }
}

/// Long generated body: the oversized-span shape. One hundred thirty
/// lines is enough to blow any render window while staying fast, and
/// the header terms draw at least one battery query onto the span.
fn long_body() -> String {
    use std::fmt::Write;
    let mut body = String::from(
        "//! Entry point owning the build cache under .guidance/target;\n\
         //! clean removes it safely.\n\
         pub fn main_entry() {\n",
    );
    for i in 0..130 {
        let _ = writeln!(body, "    let step_{i} = {i};");
    }
    body.push_str("}\n");
    body
}

/// Fixture workspace + seeded index reproducing the measured shapes:
/// a workspace/doc/code battery plus one fixture-scope manifest card
/// and one oversized code span.
pub(super) fn golden_workspace() -> (tempfile::TempDir, GuidanceSearchDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(
        root.join("README.md"),
        "# Fixture Monorepo\n\nMember projects: guidance, coral-router.\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join("src/guidance/src/query")).expect("mkdir");
    std::fs::create_dir_all(root.join("src/bin/guidance/src")).expect("mkdir");
    std::fs::write(
        root.join("src/guidance/src/freshness.rs"),
        "//! Staleness decisions: which files sync reprocesses after an edit.\n\
         pub fn stale_files() -> usize {\n    0\n}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("src/guidance/src/sync_engine.rs"),
        "//! Sync engine: reprocess after an edit, stale_files and status.\n\
         pub fn status() -> usize {\n    0\n}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("src/guidance/src/query/capability_match.rs"),
        "//! Capability brief ranking.\n\
         pub fn rank_briefs() -> usize {\n    0\n}\n",
    )
    .expect("write");
    let long = long_body();
    std::fs::write(root.join("src/bin/guidance/src/main.rs"), &long).expect("write");
    std::fs::create_dir_all(root.join("src/guidance/fixture_read/src/common-core")).expect("mkdir");
    std::fs::write(
        root.join("src/guidance/fixture_read/src/common-core/Cargo.toml"),
        "[package]\nname = \"fixture-core\"\n",
    )
    .expect("write");
    let doc_text = "# Calibration\n\nDecision point promotion needs a passing registry entry.\n";
    std::fs::create_dir_all(root.join("doc/guidance")).expect("mkdir");
    std::fs::write(root.join("doc/guidance/calibration.md"), doc_text).expect("write");

    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let seed = |name: &str, symbol: &str, text: &str, end_line: u32| {
        db.upsert_fragments(
            &db_file_record(root, name),
            &[db_text_fragment(name, symbol, text, end_line)],
            &[],
        )
        .expect("seed fragment");
    };
    seed(
        "src/guidance/src/freshness.rs",
        "stale_files",
        "//! Staleness decisions: which files sync reprocesses after an edit. Freshness decides what to reprocess.",
        1,
    );
    seed(
        "src/guidance/src/sync_engine.rs",
        "status",
        "//! Sync engine: reprocess after an edit, stale_files and status.",
        1,
    );
    seed(
        "src/guidance/src/query/capability_match.rs",
        "rank_briefs",
        "//! Capability brief ranking: rank_briefs scores capability briefs against the query.",
        1,
    );
    seed("src/bin/guidance/src/main.rs", "main_entry", &long, 133);

    let (chunks, _) =
        crate::read::doc_cards::chunk_markdown_sections("doc/guidance/calibration.md", doc_text);
    let chunk = chunks.first().expect("doc chunks");
    let mut cards = vec![
        db_project_card(
            "workspace:ws",
            "workspace",
            "Workspace",
            "Fixture Monorepo member projects: guidance, coral-router.",
            "README.md",
            "",
        ),
        db_project_card(
            "crate:fixture-core",
            "crate",
            "fixture-core",
            "Fixture-scope manifest row that must never render as workspace evidence.",
            "src/guidance/fixture_read/src/common-core/Cargo.toml",
            "",
        ),
    ];
    cards.push(crate::query::search_db::ProjectCard {
        content_hash: chunk.content_hash.clone(),
        anchor: chunk.anchor.clone(),
        ..db_project_card(
            "doc:doc/guidance/calibration.md#calibration",
            "doc",
            "Calibration",
            &chunk.text,
            "doc/guidance/calibration.md",
            "",
        )
    });
    db.replace_project_cards(&cards).expect("cards seed");
    (dir, db)
}

fn snapshot(task: &crate::eval::EvalTask, root: &std::path::Path, db: &GuidanceSearchDb) -> Golden {
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: root,
        db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: task.query.clone(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = render_answer(&answer);
    let citations: Vec<String> = answer
        .citations
        .iter()
        .map(crate::source_ref::Citation::text)
        .collect();
    let fixture_paths: Vec<String> = citations
        .iter()
        .filter(|c| crate::source_scope::is_fixture_path(c))
        .cloned()
        .collect();
    let max_span = answer
        .citations
        .iter()
        .map(|c| u64::from(c.end.unwrap_or(0).saturating_sub(c.start.unwrap_or(0))))
        .max()
        .unwrap_or(0);
    Golden {
        id: task.id.clone(),
        route: answer.route.clone(),
        sections: answer.sections.iter().map(|s| s.title.clone()).collect(),
        answer_present: answer.sections.iter().any(|s| s.title == "Answer"),
        citations,
        max_span,
        fixture_paths,
        rendered_bytes: rendered.len(),
        tokens_used: answer.tokens_used,
        gaps_count: answer.gaps.len(),
    }
}

pub(super) fn battery() -> Vec<crate::eval::EvalTask> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(BATTERY);
    load_eval_tasks(&path).expect("battery loads")
}

fn golden_path(id: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(GOLDEN_DIR)
        .join(format!("{id}.json"))
}

fn update_mode() -> bool {
    std::env::var(UPDATE_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

#[test]
fn battery_loads_orientation_and_controls() {
    let tasks = battery();
    assert!(tasks.len() >= 9, "{} tasks", tasks.len());
    assert!(
        tasks.iter().filter(|t| t.control).count() >= 3,
        "controls present"
    );
    assert!(
        tasks.iter().filter(|t| !t.control).count() >= 6,
        "orientation present"
    );
}

#[test]
fn snapshots_are_deterministic_across_independent_fixtures() {
    // Two fresh workspaces must agree byte for byte: any dependence on
    // the random tempdir path would leak in here.
    let tasks = battery();
    let (a_dir, a_db) = golden_workspace();
    let (b_dir, b_db) = golden_workspace();
    for task in &tasks {
        let a = snapshot(task, a_dir.path(), &a_db);
        let b = snapshot(task, b_dir.path(), &b_db);
        assert_eq!(a, b, "snapshot drifts for {}", task.id);
    }
}

#[test]
fn snapshots_match_checked_in_goldens() {
    let tasks = battery();
    let mut mismatches = Vec::new();
    let mut written = Vec::new();
    for task in &tasks {
        let (dir, db) = golden_workspace();
        let live = snapshot(task, dir.path(), &db);
        let path = golden_path(&task.id);
        if update_mode() {
            std::fs::create_dir_all(path.parent().expect("golden parent")).expect("golden dir");
            std::fs::write(&path, format!("{}\n", live.to_json())).expect("golden write");
            written.push(task.id.clone());
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "golden missing for {} — run with {UPDATE_ENV}=1 to record, then review the diff",
                task.id
            )
        });
        let stored = Golden::from_json(&serde_json::from_str(&raw).expect("golden parses"))
            .expect("golden shape");
        if stored != live {
            mismatches.push(format!(
                "{}:\n  stored: {:?}\n  live:   {:?}",
                task.id, stored, live
            ));
        }
    }
    if update_mode() {
        assert!(!written.is_empty(), "update wrote goldens");
        return;
    }
    assert!(
        mismatches.is_empty(),
        "output shape drifted (apply the owned change via {}):\n{}",
        EXPECTED_CHANGES,
        mismatches.join("\n")
    );
}

#[test]
fn default_output_carries_no_fixture_paths() {
    // Fixture-scope rows are spam in every default section: the
    // recorded `fixture_paths` must stay empty on the whole battery.
    for task in &battery() {
        let (dir, db) = golden_workspace();
        let live = snapshot(task, dir.path(), &db);
        assert!(
            live.fixture_paths.is_empty(),
            "fixture scope leaked into {}",
            task.id
        );
    }
}

#[test]
fn expected_changes_manifest_uses_change_ids_only() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(EXPECTED_CHANGES);
    let raw = std::fs::read_to_string(&path).expect("manifest reads");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("manifest parses");
    let changes = value
        .get("changes")
        .and_then(serde_json::Value::as_array)
        .expect("changes array");
    assert!(!changes.is_empty(), "manifest lists pending changes");
    for change in changes {
        let id = change
            .get("id")
            .and_then(serde_json::Value::as_str)
            .expect("id");
        assert!(!id.is_empty(), "change id non-empty");
        assert!(
            !id.starts_with('M') || !id[1..].starts_with(|c: char| c.is_ascii_digit()),
            "change ids stay semantic, never milestone ids: {id}"
        );
        assert!(change.get("why").is_some(), "{id} states why");
        assert!(change.get("goldens").is_some(), "{id} names goldens");
    }
}

#[test]
fn rendered_bytes_and_citations_stay_within_class_ceilings() {
    // Payload ratchet: markdown for one query class stays inside its
    // byte ceiling and the citation list stays short. Recall is pinned
    // by the exact golden citation sets.
    for task in &battery() {
        let (dir, db) = golden_workspace();
        let live = snapshot(task, dir.path(), &db);
        let ceiling = match live.route.as_str() {
            "ownership" => 1_800,
            "symbol" => 1_500,
            _ => 1_600,
        };
        assert!(
            live.rendered_bytes <= ceiling,
            "{}: {} bytes over the {ceiling} ceiling",
            task.id,
            live.rendered_bytes
        );
        assert!(
            live.citations.len() <= 8,
            "{}: {} citations",
            task.id,
            live.citations.len()
        );
    }
}
