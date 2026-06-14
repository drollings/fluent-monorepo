use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("config not found")]
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub base_url: String,
    pub chat_endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, bon::Builder)]
pub struct ProjectConfig {
    #[serde(default = "default_guidance_dir")]
    #[builder(default = default_guidance_dir())]
    pub guidance_dir: PathBuf,

    #[serde(default)]
    pub json_base: Option<PathBuf>,

    #[serde(default)]
    pub skills_dir: Option<PathBuf>,

    #[serde(default)]
    pub inbox_dir: Option<PathBuf>,

    #[serde(default)]
    pub db_path: Option<PathBuf>,

    #[serde(default)]
    pub embedding_provider: Option<String>,

    #[serde(default)]
    pub embedding_model: Option<String>,

    #[serde(default)]
    pub embedding_dims: Option<usize>,

    #[serde(default)]
    pub capabilities_dir: Option<PathBuf>,

    #[serde(default)]
    #[builder(default)]
    pub src_dirs: Vec<PathBuf>,

    #[serde(default)]
    #[builder(default)]
    pub providers: HashMap<String, Provider>,

    #[serde(default)]
    pub model_default: Option<String>,

    #[serde(default)]
    pub model_fast: Option<String>,

    #[serde(default)]
    pub model_thinking: Option<String>,

    #[serde(default)]
    #[builder(default)]
    pub test_commands: HashMap<String, Vec<String>>,

    #[serde(default)]
    #[builder(default)]
    pub lint_commands: HashMap<String, Vec<String>>,

    #[serde(default)]
    #[builder(default)]
    pub fmt_commands: HashMap<String, Vec<String>>,

    #[serde(default)]
    pub embedding_cache_limit: Option<usize>,
}

fn default_guidance_dir() -> PathBuf {
    PathBuf::from(".guidance")
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            guidance_dir: default_guidance_dir(),
            json_base: None,
            skills_dir: None,
            inbox_dir: None,
            db_path: None,
            embedding_provider: None,
            embedding_model: None,
            embedding_dims: None,
            capabilities_dir: None,
            src_dirs: vec![],
            providers: HashMap::new(),
            model_default: None,
            model_fast: None,
            model_thinking: None,
            test_commands: HashMap::new(),
            lint_commands: HashMap::new(),
            fmt_commands: HashMap::new(),
            embedding_cache_limit: None,
        }
    }
}

/// Strip provider prefix from model reference.
/// e.g. "ollama:llama3" -> "llama3", "model" -> "model"
pub fn model_name(model_ref: &str) -> &str {
    model_ref
        .split_once(':')
        .map_or(model_ref, |(_, name)| name)
}

/// Resolve a model reference into (api_url, model_name, is_thinking).
pub fn resolve_model_url(config: &ProjectConfig) -> (String, String, bool) {
    let model_ref = config.embedding_model.as_deref().unwrap_or("default");
    let is_thinking = config.model_thinking.as_deref() == Some(model_ref);

    let (provider_name, model) = model_ref.split_once(':').unwrap_or(("default", model_ref));

    let url = config
        .providers
        .get(provider_name)
        .map(|p| {
            format!(
                "{}/{}",
                p.base_url.trim_end_matches('/'),
                p.chat_endpoint.trim_start_matches('/')
            )
        })
        .unwrap_or_default();

    (url, model.to_string(), is_thinking)
}

/// Find config file with 3-level fallback:
/// 1. {workspace}/.guidance/guidance-config.json
/// 2. ~/.config/guidance/guidance-config.json
/// 3. None
pub fn find_config_file(workspace: &Path) -> Option<PathBuf> {
    let project = workspace.join(".guidance/guidance-config.json");
    if project.is_file() {
        return Some(project);
    }

    if let Some(config_dir) = dirs::config_dir() {
        let user = config_dir.join("guidance/guidance-config.json");
        if user.is_file() {
            return Some(user);
        }
    }

    None
}

/// Load config with 3-level fallback: project -> user -> default.
pub fn load_config(workspace: &Path) -> Result<ProjectConfig, ConfigError> {
    let config_path = find_config_file(workspace);
    match config_path {
        Some(path) => {
            let content = std::fs::read_to_string(&path)?;
            let config: ProjectConfig = serde_json::from_str(&content)?;
            Ok(config)
        }
        None => Ok(ProjectConfig::default()),
    }
}

/// Resolve a model reference to its provider URL and model name.
pub fn parse_model_ref(model_ref: &str) -> Option<(&str, &str)> {
    model_ref.split_once(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_name_strips_provider() {
        assert_eq!(model_name("ollama:llama3"), "llama3");
        assert_eq!(model_name("openai:gpt-4"), "gpt-4");
        assert_eq!(model_name("plain_model"), "plain_model");
        assert_eq!(model_name("a:b:c"), "b:c");
    }

    #[test]
    fn test_parse_model_ref() {
        let (provider, model) = parse_model_ref("ollama:llama3").unwrap();
        assert_eq!(provider, "ollama");
        assert_eq!(model, "llama3");
        assert!(parse_model_ref("plain").is_none());
    }

    #[test]
    fn test_resolve_model_url_no_providers() {
        let config = ProjectConfig::builder().build();
        let (url, model, is_thinking) = resolve_model_url(&config);
        assert_eq!(model, "default");
        assert!(!is_thinking);
        assert!(url.is_empty());
    }

    #[test]
    fn test_resolve_model_url_with_provider() {
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "ollama".into(),
            Provider {
                base_url: "http://localhost:11434".into(),
                chat_endpoint: "api/chat".into(),
            },
        );
        let config = ProjectConfig::builder()
            .embedding_model("ollama:llama3".into())
            .providers(providers)
            .build();
        let (url, model, is_thinking) = resolve_model_url(&config);
        assert_eq!(model, "llama3");
        assert_eq!(url, "http://localhost:11434/api/chat");
        assert!(!is_thinking);
    }

    #[test]
    fn test_resolve_model_url_is_thinking() {
        let config = ProjectConfig::builder()
            .model_thinking("deepseek:r1".into())
            .embedding_model("deepseek:r1".into())
            .build();
        let (_, model, is_thinking) = resolve_model_url(&config);
        assert_eq!(model, "r1");
        assert!(is_thinking);
    }

    #[test]
    fn test_find_config_file_project() {
        let dir = tempfile::tempdir().expect("temp dir");
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        let config_path = guidance_dir.join("guidance-config.json");
        std::fs::write(&config_path, r#"{"guidance_dir": ".guidance"}"#).expect("write");

        let found = find_config_file(dir.path());
        assert!(found.is_some(), "should find project config");
    }

    #[test]
    fn test_find_config_file_not_found() {
        let dir = tempfile::tempdir().expect("temp dir");
        let found = find_config_file(dir.path());
        assert!(found.is_none(), "should not find config");
    }

    #[test]
    fn test_load_config_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = load_config(dir.path()).expect("should return default");
        assert_eq!(config.guidance_dir, PathBuf::from(".guidance"));
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        let config_path = guidance_dir.join("guidance-config.json");
        std::fs::write(
            &config_path,
            r#"{"embedding_model": "ollama:llama3", "embedding_dims": 4096}"#,
        )
        .expect("write");

        let config = load_config(dir.path()).expect("should load");
        assert_eq!(config.embedding_model.as_deref(), Some("ollama:llama3"));
        assert_eq!(config.embedding_dims, Some(4096));
    }

    #[test]
    fn test_project_config_default_builder() {
        let config = ProjectConfig::builder().build();
        assert_eq!(config.guidance_dir, PathBuf::from(".guidance"));
        assert!(config.providers.is_empty());
        assert!(config.src_dirs.is_empty());
    }

    #[test]
    fn test_project_config_builder_with_values() {
        let config = ProjectConfig::builder()
            .guidance_dir(PathBuf::from("custom"))
            .embedding_model("ollama:llama3".into())
            .build();
        assert_eq!(config.guidance_dir, PathBuf::from("custom"));
        assert_eq!(config.embedding_model.as_deref(), Some("ollama:llama3"));
    }
}
