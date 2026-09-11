use std::path::{Path, PathBuf};

use fluent_types::GuidanceDoc;
use fluent_wvr::wrapper::Pipeline;
use thiserror::Error;

use crate::ast_parser::AstParser;
use crate::enhancer::{enhance_doc, Enhancer};
use crate::sync::comments;
use crate::sync::json_store;
use crate::sync::staleness;
use crate::walk;
use search_vector::GuidanceDb;

#[derive(Error, Debug)]
pub enum SyncEngineError {
    #[error("IO error: {0}")]
    Io(#[from] common_core::error::IoError),
    #[error("JSON error: {0}")]
    Json(#[from] json_store::JsonError),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("source file not found: {0}")]
    SourceNotFound(PathBuf),
    #[error("database error: {0}")]
    Db(String),
}

common_core::impl_from_io_error!(SyncEngineError);

#[derive(Debug, Clone, Default)]
pub struct GenConfig {
    pub db_sync: bool,
    pub db_path: Option<PathBuf>,
    pub json_base: Option<PathBuf>,
}

pub struct SyncEngine {
    pub ast_parser: AstParser,
    pub guidance_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub source_dir: PathBuf,
    pub enhancer: Option<Enhancer>,
    /// Stored source-content hashes (absolute path → sha256 at last
    /// ingest) feeding the member-JSON clock gate. Empty disables the
    /// gate: staleness falls back to mtime behavior exactly as before.
    pub source_hashes: std::collections::HashMap<String, String>,
}

struct SyncContext {
    doc: GuidanceDoc,
    source_path: PathBuf,
    source: String,
    config: GenConfig,
    source_dir: PathBuf,
    guidance_dir: PathBuf,
}

impl SyncEngine {
    pub fn new(guidance_dir: PathBuf, source_dir: PathBuf) -> Self {
        let guidance_dir = absolute_or(guidance_dir);
        let source_dir = absolute_or(source_dir);
        let workspace_root = guidance_dir
            .parent()
            .map_or_else(|| source_dir.clone(), Path::to_path_buf);
        Self {
            ast_parser: AstParser::new(),
            guidance_dir,
            workspace_root,
            source_dir,
            enhancer: None,
            source_hashes: std::collections::HashMap::new(),
        }
    }

    pub fn with_parser(guidance_dir: PathBuf, source_dir: PathBuf, ast_parser: AstParser) -> Self {
        let guidance_dir = absolute_or(guidance_dir);
        let source_dir = absolute_or(source_dir);
        let workspace_root = guidance_dir
            .parent()
            .map_or_else(|| source_dir.clone(), Path::to_path_buf);
        Self {
            ast_parser,
            guidance_dir,
            workspace_root,
            source_dir,
            enhancer: None,
            source_hashes: std::collections::HashMap::new(),
        }
    }

    /// Attach stored source-content hashes for the member-JSON clock
    /// gate (absolute path → sha256 at last ingest). Absent entries
    /// keep today's mtime behavior for those files.
    #[must_use]
    pub fn with_source_hashes(
        mut self,
        source_hashes: std::collections::HashMap<String, String>,
    ) -> Self {
        self.source_hashes = source_hashes;
        self
    }

    /// Stored hash for `source_path`, if the gate knows it.
    fn stored_hash(&self, source_path: &Path) -> Option<&str> {
        let absolute = absolute_or(source_path.to_path_buf());
        self.source_hashes
            .get(absolute.to_string_lossy().as_ref())
            .map(String::as_str)
    }

    #[must_use]
    pub fn with_enhancer(mut self, enhancer: Enhancer) -> Self {
        self.enhancer = Some(enhancer);
        self
    }

    pub fn gen(&mut self, source_path: &Path) -> Result<GuidanceDoc, SyncEngineError> {
        self.gen_with_config(source_path, &GenConfig::default())
    }

    pub fn gen_with_config(
        &mut self,
        source_path: &Path,
        config: &GenConfig,
    ) -> Result<GuidanceDoc, SyncEngineError> {
        // Absolutize up front: discovery may hand us a relative path
        // while `source_dir`/`workspace_root` are absolute (or the
        // reverse) — relativization must compare like with like, or
        // `meta` carries the raw prefix spelling (`./lib.rs`, `..lib`).
        let source_path = absolute_or(source_path.to_path_buf());
        let source = common_core::io::read_to_string_err(&source_path)?;

        let module_rel = source_path
            .strip_prefix(&self.source_dir)
            .unwrap_or(&source_path);
        let module_name = module_rel
            .to_string_lossy()
            .strip_suffix(&format!(
                ".{}",
                module_rel
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
            ))
            .unwrap_or(&module_rel.to_string_lossy())
            .replace(['/', '\\'], ".");

        let source_path_str = source_path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&source_path)
            .to_string_lossy()
            .to_string();

        let mut doc = self
            .ast_parser
            .parse_file(&source_path, &source)
            .map_err(|e| SyncEngineError::Parse(e.to_string()))?;

        doc.meta.module = module_name.as_str().into();
        doc.meta.source = source_path_str.as_str().into();

        let mut ctx = SyncContext {
            doc,
            source_path,
            source,
            config: config.clone(),
            source_dir: self.source_dir.clone(),
            guidance_dir: self.guidance_dir.clone(),
        };

        let mut pipeline =
            Self::build_pipeline(self.enhancer.as_ref(), &mut self.ast_parser, config.db_sync);
        pipeline.run(&mut ctx)?;

        Ok(ctx.doc)
    }

    fn build_pipeline<'a>(
        enhancer: Option<&'a Enhancer>,
        ast_parser: &'a mut AstParser,
        db_sync: bool,
    ) -> Pipeline<'a, SyncContext, SyncEngineError> {
        let mut p = Pipeline::new()
            .step(move |ctx: &mut SyncContext| {
                if let Some(enhancer) = enhancer {
                    if let Err(e) = enhance_doc(enhancer, &mut ctx.doc, &ctx.source) {
                        tracing::warn!("LLM enhancement failed for {:?}: {e}", ctx.source_path);
                    }
                }
                Ok(())
            })
            .step(|ctx: &mut SyncContext| {
                let json_path =
                    guidance_json_path(&ctx.source_path, &ctx.source_dir, &ctx.guidance_dir);
                json_store::save_guidance(&json_path, &ctx.doc)?;
                Ok(())
            })
            .step(move |ctx: &mut SyncContext| {
                if let Err(e) = comments::sync_comments(&ctx.source_path, &ctx.doc, ast_parser) {
                    tracing::warn!("comment sync failed for {:?}: {e}", ctx.source_path);
                }
                Ok(())
            });

        if db_sync {
            p = p.maybe(
                |_ctx: &SyncContext| true,
                move |ctx: &mut SyncContext| {
                    let db_path = ctx
                        .config
                        .db_path
                        .clone()
                        .unwrap_or_else(|| ctx.guidance_dir.join("..").join(".guidance.db"));
                    let json_base = ctx
                        .config
                        .json_base
                        .clone()
                        .unwrap_or_else(|| ctx.guidance_dir.join("src"));
                    if let Ok(db) = GuidanceDb::open(&db_path) {
                        let _ = db.sync_from_dir(&json_base);
                    }
                    Ok(())
                },
            );
        }

        p
    }

    pub fn gen_if_stale(&mut self, source_path: &Path) -> Result<bool, SyncEngineError> {
        let json_path = self.guidance_json_path(source_path);

        if !self.should_generate_for(source_path, &json_path) {
            return Ok(false);
        }

        self.gen(source_path)?;
        Ok(true)
    }

    /// Staleness for one source file: the mtime gate, with the
    /// content-aware clock fallback when stored source hashes are
    /// attached (files without stored evidence keep mtime behavior).
    fn should_generate_for(&self, source_path: &Path, json_path: &Path) -> bool {
        let Some(stored) = self.stored_hash(source_path) else {
            return staleness::should_generate(json_path, source_path);
        };
        staleness::should_generate_with_stored(json_path, source_path, Some(stored), || {
            crate::diff::hash_file(source_path)
        })
    }

    pub fn load_doc(&self, source_path: &Path) -> Result<Option<GuidanceDoc>, SyncEngineError> {
        let json_path = self.guidance_json_path(source_path);
        let doc = json_store::load_guidance(&json_path)?;
        Ok(doc)
    }

    pub fn status(&self) -> Result<SyncStatus, SyncEngineError> {
        let mut total_files = 0;
        let mut stale_files = 0;
        let mut up_to_date = 0;

        self.walk_source_files(|source_path| {
            total_files += 1;
            let json_path = self.guidance_json_path(source_path);
            if self.should_generate_for(source_path, &json_path) {
                stale_files += 1;
            } else {
                up_to_date += 1;
            }
        });

        Ok(SyncStatus {
            total_files,
            stale_files,
            up_to_date,
        })
    }

    fn guidance_json_path(&self, source_path: &Path) -> PathBuf {
        guidance_json_path(source_path, &self.source_dir, &self.guidance_dir)
    }

    /// Path-scoped generation (P2): generate docs for every source file
    /// under `path` (file or directory), returning `(generated, failed)`.
    /// Per-file isolation holds — a failed file is counted, never thrown.
    pub fn gen_scoped(&mut self, path: &Path) -> Result<(usize, usize), SyncEngineError> {
        use crate::selection::{select_files, FileSelection};
        let mut diag = crate::selection::ScanDiagnostics::default();
        let selection = FileSelection {
            roots: vec![path.to_path_buf()],
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extra_skip_dirs: Vec::new(),
            honor_gitignore: false,
            max_bytes_override: None,
        };
        let files = select_files(&selection, &mut diag)
            .map_err(|e| SyncEngineError::Parse(e.to_string()))?;
        let mut generated = 0;
        let mut failed = 0;
        for file in files {
            // The legacy member pipeline parses zig/zon/py/rs only.
            let parseable = file
                .path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "zig" | "zon" | "py" | "rs"));
            if !parseable {
                continue;
            }
            match self.gen_if_stale(&file.path) {
                Ok(true) => generated += 1,
                Ok(false) => {} // Up to date: hash wins over mtime (Gate 0 §6).
                Err(_) => failed += 1,
            }
        }
        Ok((generated, failed))
    }

    /// Embedding-schema gate (P2): the fragment index and embedding cache
    /// tables must exist before any wave commits. Version mismatches
    /// surface here (never as a mid-run commit failure).
    pub fn check_store(db: &search_vector::GuidanceDb) -> Result<(), SyncEngineError> {
        for table in ["files", "fragments", "embedding_cache"] {
            if !db.has_table(table) {
                return Err(SyncEngineError::Parse(format!(
                    "index schema gate: missing table {table} (expected schema v{})",
                    crate::index_pipeline::INDEX_SCHEMA_VERSION
                )));
            }
        }
        Ok(())
    }

    fn walk_source_files<F>(&self, mut callback: F)
    where
        F: FnMut(&Path),
    {
        walk::walk_files(&self.source_dir, walk::SOURCE_EXTENSIONS, &mut callback);
    }
}

/// Lexical absolutization (no I/O, no symlink resolution): relative
/// roots compare equal with the absolute paths discovery yields, so
/// `strip_prefix` relativization and `meta` derivation agree no matter
/// how the caller spelled the workspace. Falls back to the input when
/// the working directory is unreadable — never a construction failure.
/// Lexically absolutize `path` against the process working directory,
/// returning it unchanged when absolutization fails. Shared path-key
/// normalization for stored-hash lookups (DB keys are absolute).
#[must_use]
pub fn absolute_or(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// Compute the guidance JSON path for a source file: the path relative to
/// `source_dir`, suffixed `.json`, under `guidance_dir/src`. The single shared
/// implementation behind both `SyncEngine::guidance_json_path` and the
/// pipeline step.
fn guidance_json_path(source_path: &Path, source_dir: &Path, guidance_dir: &Path) -> PathBuf {
    let source_path = absolute_or(source_path.to_path_buf());
    let source_dir = absolute_or(source_dir.to_path_buf());
    let relative = source_path.strip_prefix(&source_dir).unwrap_or(&source_path);
    let json_name = format!("{}.json", relative.display());
    absolute_or(guidance_dir.to_path_buf()).join("src").join(&json_name)
}

#[derive(Debug, Clone)]
pub struct SyncStatus {
    pub total_files: usize,
    pub stale_files: usize,
    pub up_to_date: usize,
}

impl SyncStatus {
    pub fn is_clean(&self) -> bool {
        self.stale_files == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;

    #[test]
    fn test_gen_and_load_round_trip() {
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("create src");

        let zig_file = source_dir.join("test.zig");
        std::fs::write(&zig_file, "/// A test module\npub fn hello() void {}\n").expect("write");

        let guidance_dir = dir.path().join(".guidance");
        let mut engine = SyncEngine::new(guidance_dir.clone(), source_dir);

        let doc = engine.gen(&zig_file).expect("gen");
        assert_eq!(doc.meta.module.as_str(), "test");
        assert_eq!(doc.members.len(), 1);
        assert_eq!(doc.members[0].name.as_str(), "hello");
    }

    #[test]
    fn test_gen_if_stale() {
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("create src");

        let zig_file = source_dir.join("test.zig");
        std::fs::write(&zig_file, "pub fn foo() void {}").expect("write");

        let guidance_dir = dir.path().join(".guidance");
        let mut engine = SyncEngine::new(guidance_dir, source_dir);

        assert!(engine.gen_if_stale(&zig_file).expect("gen if stale"));
    }

    #[test]
    fn test_status() {
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("create src");

        let zig_file = source_dir.join("test.zig");
        std::fs::write(&zig_file, "pub fn bar() void {}").expect("write");

        let guidance_dir = dir.path().join(".guidance");
        let mut engine = SyncEngine::new(guidance_dir, source_dir);
        engine.gen(&zig_file).expect("gen");

        let status = engine.status().expect("status");
        assert_eq!(status.total_files, 1);
    }

    #[test]
    fn test_gen_scoped_covers_subtree() {
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir_all(source_dir.join("sub")).expect("mkdir");
        std::fs::write(source_dir.join("a.zig"), "pub fn a() void {}\n").expect("write");
        std::fs::write(source_dir.join("sub").join("b.zig"), "pub fn b() void {}\n")
            .expect("write");

        let guidance_dir = dir.path().join(".guidance");
        let mut engine = SyncEngine::new(guidance_dir, source_dir.clone());
        let (generated, failed) = engine.gen_scoped(&source_dir).expect("scoped");
        assert_eq!(generated, 2, "both files generated");
        assert_eq!(failed, 0);

        // Path-scoped to the subtree only.
        let guidance_dir2 = dir.path().join(".guidance2");
        let mut engine2 = SyncEngine::new(guidance_dir2, source_dir.clone());
        let (generated, _) = engine2.gen_scoped(&source_dir.join("sub")).expect("scoped");
        assert_eq!(generated, 1);
    }

    #[test]
    fn test_check_store_gates_schema() {
        let db = search_vector::GuidanceDb::open_in_memory().expect("db");
        assert!(SyncEngine::check_store(&db).is_ok());
    }

    #[test]
    fn test_gen_syncs_comments() {
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("create src");

        let zig_file = source_dir.join("test.zig");
        std::fs::write(&zig_file, "pub fn hello() void {}\n").expect("write");

        let guidance_dir = dir.path().join(".guidance");
        let mut engine = SyncEngine::new(guidance_dir, source_dir);

        let doc = engine.gen(&zig_file).expect("gen");
        assert_eq!(doc.members.len(), 1);

        let source_after = std::fs::read_to_string(&zig_file).expect("read");
        assert!(source_after.contains("pub fn hello() void {}"));
    }
}


#[cfg(test)]
mod regen_determinism_tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;

    #[test]
    fn test_regen_is_byte_identical_and_mtime_neutral() {
        // The M3 propagation contract: reprocessing an unchanged file
        // (e.g. via a fresh pool-regen engine, as the single-file path
        // does) must neither change bytes nor bump the mtime — otherwise
        // every sync would flap downstream fingerprints.
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("mkdir");
        let file = source_dir.join("m.rs");
        std::fs::write(&file, "pub fn f() {}\n").expect("write");
        let guidance_dir = dir.path().join(".guidance");

        let mut first = SyncEngine::new(guidance_dir.clone(), source_dir.clone());
        first.gen(&file).expect("first gen");
        let json = guidance_dir.join("src").join("m.rs.json");
        let bytes = std::fs::read(&json).expect("read");
        let mtime = std::fs::metadata(&json).expect("stat").modified().expect("mtime");
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Fresh engine, as the pool path constructs per regen.
        let mut second = SyncEngine::with_parser(
            guidance_dir.clone(),
            source_dir.clone(),
            AstParser::new(),
        );
        second.gen(&file).expect("second gen");
        assert_eq!(std::fs::read(&json).expect("read"), bytes, "regen must be byte-identical");
        assert_eq!(
            std::fs::metadata(&json).expect("stat").modified().expect("mtime"),
            mtime,
            "identical regen must not bump the mtime"
        );
    }
}

#[cfg(test)]
mod clock_gate_calibration_tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;

    #[test]
    fn test_ambiguity_window_calibration_five_unchanged_five_edited() {
        // Clock-gate calibration on a hermetic fixture, everything inside
        // the 1s ambiguity window: 5 touched-but-identical files must NOT
        // re-process (precision) and 5 sub-second real edits MUST
        // re-process (recall). Pre-registered bar: 5/5 on both arms.
        let dir = tempdir();
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("mkdir");
        let guidance_dir = dir.path().join(".guidance");
        let mut paths = Vec::new();
        for i in 0..10 {
            let file = source_dir.join(format!("c{i:02}.rs"));
            std::fs::write(&file, format!("pub fn cal{i:02}() -> u64 {{ {i} }}\n"))
                .expect("write");
            paths.push(file);
        }
        let mut engine = SyncEngine::new(guidance_dir.clone(), source_dir.clone());
        for file in &paths {
            engine.gen(file).expect("gen");
        }
        // Stored evidence: sha256 of the v1 bytes, as last-ingest records.
        let hashes: std::collections::HashMap<String, String> = paths
            .iter()
            .map(|file| {
                let bytes = std::fs::read(file).expect("read");
                (
                    absolute_or(file.clone()).to_string_lossy().into_owned(),
                    common_core::hash::sha256_hex(&bytes),
                )
            })
            .collect();
        // Control arm first (read-only): without stored evidence the gate
        // keeps today's mtime behavior — inside-window edits skip. The
        // stored map is the evidence channel that enables firing.
        let mut bare = SyncEngine::new(guidance_dir.clone(), source_dir.clone());
        for file in &paths[5..] {
            std::fs::write(
                file,
                format!(
                    "pub fn edited{}() -> u64 {{ 999 }}\n",
                    file.file_stem().unwrap().to_string_lossy()
                ),
            )
            .expect("edit");
        }
        for file in &paths[..5] {
            let bytes = std::fs::read(file).expect("read");
            std::fs::write(file, &bytes).expect("touch");
        }
        for file in &paths[5..] {
            assert!(
                !bare.gen_if_stale(file).expect("gate"),
                "control: no stored evidence means mtime behavior (skip): {file:?}"
            );
        }
        // Gated arms: precision then recall.
        let mut gated = SyncEngine::new(guidance_dir.clone(), source_dir.clone())
            .with_source_hashes(hashes);
        for file in &paths[..5] {
            assert!(
                !gated.gen_if_stale(file).expect("gate"),
                "touched-identical must NOT re-process: {file:?}"
            );
        }
        for file in &paths[5..] {
            assert!(
                gated.gen_if_stale(file).expect("gate"),
                "sub-second edit MUST re-process: {file:?}"
            );
        }
    }
}
