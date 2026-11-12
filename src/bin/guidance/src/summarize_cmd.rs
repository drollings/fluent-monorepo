//! `summarize` command (M5.7): background comment-summary pass over the
//! workspace. Opt-in only; the query path never calls a model.

use std::path::{Path, PathBuf};

use guidance_core::config::ProjectConfig;
use guidance_core::model_lanes::LaneGate;

/// Outcome totals for one `summarize` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Outcome totals for one `summarize` run.
pub struct SummaryReport {
    /// Files visited.
    pub files: usize,
    /// Nodes summarized this run.
    pub summarized: usize,
    /// Nodes skipped on a matching cache entry.
    pub cached: usize,
    /// Nodes that failed validation or transport.
    pub failed: usize,
    /// Nodes the model backend could not serve.
    pub unavailable: usize,
    /// Coverage gaps recorded.
    pub gaps: usize,
    /// Files whose pass failed (logged, run continues).
    pub files_failed: usize,
}

/// Run the background summary pass over `src_dirs`: parse changed files
/// once, skip cached nodes, summarize the rest through the background
/// lane, and persist sidecars plus the DB projection. Reads the generative
/// role from `cfg`; a disabled or unconfigured role is an error, never a
/// silent skip.
pub async fn run_summarize(
    json_dir: &Path,
    db_path: &str,
    src_dirs: &[PathBuf],
    cfg: &ProjectConfig,
    verbose: bool,
) -> Result<SummaryReport, String> {
    use guidance_core::sync_engine::guidance_json_path;
    let Some((base_url, model)) = cfg.generative.endpoint() else {
        return Err("generative role is disabled; enable it to summarize".to_string());
    };
    if model.is_empty() {
        return Err("generative model is not configured".to_string());
    }
    let config = fluent_llm::protocol::LlmConfig::new()
        .api_url(base_url.to_string())
        .model(model.to_string())
        .timeout_ms(cfg.generative.timeout_ms)
        .build();
    let client = fluent_llm::client::LlmClient::with_config(config);
    let db = search_vector::GuidanceDb::open(Path::new(db_path))
        .map_err(|error| format!("cannot open index at {db_path}: {error}"))?;
    let known: std::collections::HashMap<String, String> = db
        .list_files()
        .map_err(|error| format!("list files: {error}"))?
        .into_iter()
        .map(|file| (file.absolute_path, file.id))
        .collect();
    let gate = LaneGate::with_defaults();
    let mut pass = guidance_core::summary::SummaryPass::new(&client, model.to_string(), &gate, &db);
    let mut report = SummaryReport::default();
    for src_dir in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let selection = guidance_core::selection::FileSelection {
            roots: vec![src_dir.clone()],
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extra_skip_dirs: Vec::new(),
            honor_gitignore: true,
            max_bytes_override: None,
        };
        let mut diag = guidance_core::selection::ScanDiagnostics::default();
        let files = guidance_core::selection::select_files(&selection, &mut diag)
            .map_err(|error| format!("selection failed: {error}"))?;
        for selected in files {
            if selected.kind == guidance_core::search_types::FileKind::Image {
                continue;
            }
            let text = match std::fs::read_to_string(&selected.path) {
                Ok(text) => text,
                Err(_) => {
                    report.files_failed += 1;
                    continue;
                }
            };
            let Some(file_id) = known.get(&selected.path.to_string_lossy().into_owned()) else {
                continue;
            };
            let sidecar = guidance_json_path(&selected.path, &selected.root, json_dir);
            let input = guidance_core::summary::SummaryFileInput {
                file_id,
                text: &text,
                format: &selected.format,
            };
            match pass.summarize_file(&input, &sidecar).await {
                Ok(outcome) => {
                    if verbose && (outcome.summarized > 0 || outcome.failed > 0) {
                        eprintln!(
                            "summarize {}: {} summarized, {} cached, {} failed, {} unavailable, {} gaps",
                            selected.relative,
                            outcome.summarized,
                            outcome.cached,
                            outcome.failed,
                            outcome.unavailable,
                            outcome.gaps.len(),
                        );
                    }
                    report.files += 1;
                    report.summarized += outcome.summarized;
                    report.cached += outcome.cached;
                    report.failed += outcome.failed;
                    report.unavailable += outcome.unavailable;
                    report.gaps += outcome.gaps.len();
                }
                Err(error) => {
                    eprintln!("summarize failed for {}: {error}", selected.relative);
                    report.files_failed += 1;
                }
            }
        }
    }
    // Refresh the generation marker over the new sidecar headers so the
    // next sync reads them as current instead of rebuilding the index.
    let json_src = json_dir.join("src");
    if json_src.is_dir() {
        let generation = guidance_core::index_generation::generation_of(&json_src);
        let index_dir = Path::new(db_path)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        if let Err(error) =
            guidance_core::index_generation::write_generation(&index_dir, &generation)
        {
            eprintln!("warning: generation marker not written: {error}");
        }
    }
    Ok(report)
}