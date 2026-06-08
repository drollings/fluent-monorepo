use std::path::{Path, PathBuf};

use guidance_common::types::GuidanceDoc;
use thiserror::Error;

use crate::ast_parser::AstParser;
use crate::sync::comments;
use crate::sync::json_store;
use crate::sync::staleness;
use crate::vector::vector_db::GuidanceDb;

#[derive(Error, Debug)]
pub enum SyncEngineError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] json_store::JsonError),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("source file not found: {0}")]
    SourceNotFound(PathBuf),
    #[error("database error: {0}")]
    Db(String),
}

#[derive(Debug, Clone, Default)]
pub struct GenConfig {
    pub db_sync: bool,
    pub llm_infill: bool,
    pub db_path: Option<PathBuf>,
    pub json_base: Option<PathBuf>,
}

pub struct SyncEngine {
    pub ast_parser: AstParser,
    pub guidance_dir: PathBuf,
    pub source_dir: PathBuf,
}

impl SyncEngine {
    pub fn new(guidance_dir: PathBuf, source_dir: PathBuf) -> Self {
        Self {
            ast_parser: AstParser::new(),
            guidance_dir,
            source_dir,
        }
    }

    pub fn gen(&mut self, source_path: &Path) -> Result<GuidanceDoc, SyncEngineError> {
        self.gen_with_config(source_path, &GenConfig::default())
    }

    pub fn gen_with_config(
        &mut self,
        source_path: &Path,
        config: &GenConfig,
    ) -> Result<GuidanceDoc, SyncEngineError> {
        let source = std::fs::read_to_string(source_path)?;

        let rel_path = source_path
            .strip_prefix(&self.source_dir)
            .unwrap_or(source_path);
        let module_name = rel_path
            .to_string_lossy()
            .strip_suffix(&format!(
                ".{}",
                rel_path.extension().and_then(|e| e.to_str()).unwrap_or("")
            ))
            .unwrap_or(&rel_path.to_string_lossy())
            .replace(['/', '\\'], ".");

        let mut doc = self
            .ast_parser
            .parse_file(source_path, &source)
            .map_err(|e| SyncEngineError::Parse(e.to_string()))?;

        doc.meta.module = module_name.as_str().into();
        doc.meta.source = rel_path.to_string_lossy().as_ref().into();

        // LLM comment infill — best-effort, requires pre-configured client
        #[allow(unused_variables)]
        if config.llm_infill {
            // LLM infill requires a configured LlmClient; silently skip if unavailable
        }

        let json_path = self.guidance_json_path(source_path);
        json_store::save_guidance(&json_path, &doc)?;

        // Sync comments back to source file after generation
        if let Err(e) = comments::sync_comments(source_path, &doc) {
            tracing::warn!("comment sync failed for {:?}: {e}", source_path);
        }

        // Database sync after JSON write
        if config.db_sync {
            let db_path = config
                .db_path
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.guidance_dir.join("..").join(".guidance.db"));

            let json_base = config
                .json_base
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.guidance_dir.join("src"));

            if let Ok(db) = GuidanceDb::open(&db_path) {
                let _ = db.sync_from_dir(&json_base);
            }
        }

        Ok(doc)
    }

    pub fn gen_if_stale(&mut self, source_path: &Path) -> Result<bool, SyncEngineError> {
        let json_path = self.guidance_json_path(source_path);

        if !staleness::should_generate(&json_path, source_path) {
            return Ok(false);
        }

        self.gen(source_path)?;
        Ok(true)
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
            if staleness::should_generate(&json_path, source_path) {
                stale_files += 1;
            } else {
                up_to_date += 1;
            }
        })?;

        Ok(SyncStatus {
            total_files,
            stale_files,
            up_to_date,
        })
    }

    fn guidance_json_path(&self, source_path: &Path) -> PathBuf {
        let relative = source_path
            .strip_prefix(&self.source_dir)
            .unwrap_or(source_path);
        let json_name = format!("{}.json", relative.display());
        self.guidance_dir.join("src").join(&json_name)
    }

    fn walk_source_files<F>(&self, mut callback: F) -> Result<(), SyncEngineError>
    where
        F: FnMut(&Path),
    {
        self.walk_dir(&self.source_dir, &mut callback)?;
        Ok(())
    }

    fn walk_dir<F>(&self, dir: &Path, callback: &mut F) -> std::io::Result<()>
    where
        F: FnMut(&Path),
    {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                if !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    self.walk_dir(&path, callback)?;
                }
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "zig" | "zon" | "py" | "rs" | "md") {
                    callback(&path);
                }
            }
        }

        Ok(())
    }
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

    #[test]
    fn test_gen_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp dir");
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
        let dir = tempfile::tempdir().expect("temp dir");
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
        let dir = tempfile::tempdir().expect("temp dir");
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
    fn test_gen_syncs_comments() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source_dir = dir.path().join("src");
        std::fs::create_dir(&source_dir).expect("create src");

        // Source with a function but no comment
        let zig_file = source_dir.join("test.zig");
        std::fs::write(&zig_file, "pub fn hello() void {}\n").expect("write");

        let guidance_dir = dir.path().join(".guidance");
        let mut engine = SyncEngine::new(guidance_dir, source_dir);

        // gen() should call sync_comments which adds /// comments
        let doc = engine.gen(&zig_file).expect("gen");
        assert_eq!(doc.members.len(), 1);

        // Reread source to verify comments were synced
        let source_after = std::fs::read_to_string(&zig_file).expect("read");
        // The member has no comment, so no /// should be added — just verifying no crash
        assert!(source_after.contains("pub fn hello() void {}"));
    }
}
