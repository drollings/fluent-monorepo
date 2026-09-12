use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] common_core::error::IoError),
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

/// The top-level `"embed": {"dims", "cache_limit"}` object the default
/// config writes. (The flat `embedding_dims` / `embedding_cache_limit`
/// fields below are the legacy alternative shape; the object wins.)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmbedConfig {
    #[serde(default)]
    pub dims: Option<usize>,
    #[serde(default)]
    pub cache_limit: Option<usize>,
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

    /// Top-level `"embed"` object (`dims`, `cache_limit`). Preferred over
    /// the flat legacy `embedding_*` fields when both are present.
    #[serde(default)]
    pub embed: Option<EmbedConfig>,

    /// Deserializes the `"models"` map from the JSON config.
    /// Keys are role names ("default", "fast", "thinking", "batch", "embed"),
    /// values are model references like `"llama:code"`.
    #[serde(default)]
    #[builder(default)]
    pub models: HashMap<String, String>,
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
            embed: None,
            models: HashMap::new(),
        }
    }
}

/// Strip provider prefix from model reference.
/// e.g. "ollama:llama3" -> "llama3", "model" -> "model"
/// Delegates to the canonical `fluent_types::model_ref::model_name`.
pub fn model_name(model_ref: &str) -> &str {
    fluent_types::model_ref::model_name(model_ref)
}

/// Resolve a model reference for a given role.
///
/// Checks the `"models"` map first (e.g. `models["fast"]` → `"llama:code"`),
/// then falls back to the flat config fields (`model_fast`, `model_default`).
/// Returns a model reference string like `"llama:code"`.
pub fn resolve_model_ref(config: &ProjectConfig, role: &str) -> String {
    // 1. Check the nested "models" map from the JSON config.
    if let Some(val) = config.models.get(role) {
        return val.clone();
    }
    // 2. Fall back to flat fields for backwards compatibility.
    match role {
        "fast" => config
            .model_fast
            .clone()
            .or_else(|| config.model_default.clone())
            .unwrap_or_else(|| "code:latest".to_string()),
        _ => config
            .model_default
            .clone()
            .unwrap_or_else(|| "code:latest".to_string()),
    }
}

/// Resolve a model reference into (api_url, model_name, is_thinking).
///
/// A model is considered a "thinking" model when:
/// 1. Its reference matches `config.model_thinking` (flat field), OR
/// 2. Its reference matches the model in `models.thinking`, OR
/// 3. It IS the model referenced by `models.thinking` (same provider:model).
pub fn resolve_model_url(config: &ProjectConfig, model_ref: &str) -> (String, String, bool) {
    // Determine the thinking model reference from both sources.
    let thinking_ref = config
        .model_thinking
        .as_deref()
        .or_else(|| config.models.get("thinking").map(String::as_str));

    let is_thinking = thinking_ref.is_some_and(|tr| tr == model_ref);

    let parsed = fluent_types::model_ref::ModelRef::parse(model_ref);

    let url = config
        .providers
        .get(parsed.provider.as_str())
        .map(|p| {
            format!(
                "{}/{}",
                p.base_url.trim_end_matches('/'),
                p.chat_endpoint.trim_start_matches('/')
            )
        })
        .unwrap_or_default();

    (url, parsed.model, is_thinking)
}

/// Find config file with 3-level fallback:
/// 1. {workspace}/.guidance/guidance-config.json
/// 2. ~/.config/guidance/guidance-config.json
/// 3. None (precedence delegates to
///    `common_core::config::find_hierarchical`).
pub fn find_config_file(workspace: &Path) -> Option<PathBuf> {
    let mut candidates = vec![workspace.join(".guidance/guidance-config.json")];
    if let Some(config_dir) = dirs::config_dir() {
        candidates.push(config_dir.join("guidance/guidance-config.json"));
    }
    common_core::config::find_hierarchical(&candidates)
}

/// Load config with 3-level fallback: project -> user -> default.
pub fn load_config(workspace: &Path) -> Result<ProjectConfig, ConfigError> {
    let config_path = find_config_file(workspace);
    match config_path {
        Some(path) => Ok(common_core::config::load_json(&path)?),
        None => Ok(ProjectConfig::default()),
    }
}

/// Resolve a model reference to its provider URL and model name.
/// Delegates to the canonical `fluent_types::model_ref::parse_model_ref`.
pub fn parse_model_ref(model_ref: &str) -> Option<(&str, &str)> {
    fluent_types::model_ref::parse_model_ref(model_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;

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

    // M8.1 characterization: model-ref behavior pinned verbatim before the
    // `fluent_types::model_ref` extraction. Forecast drift note: the
    // roadmap's `normalize_model_ref`, markdown model mentions, and
    // knowledge provenance parsers do not exist — this trio
    // (`model_name`, `parse_model_ref`, `resolve_model_url`) plus the
    // enhancer `<comment>` tag are the actual duplication surface.

    #[test]
    fn m8_model_name_edge_matrix() {
        assert_eq!(model_name(""), "");
        assert_eq!(model_name(":"), "");
        assert_eq!(model_name(":x"), "x");
        assert_eq!(model_name("x:"), "");
        assert_eq!(model_name("a:b:c"), "b:c");
        assert_eq!(model_name("ollama:llama3"), "llama3");
    }

    #[test]
    fn m8_parse_model_ref_edge_matrix() {
        assert_eq!(parse_model_ref("a:b:c"), Some(("a", "b:c")));
        assert_eq!(parse_model_ref(":"), Some(("", "")));
        assert_eq!(parse_model_ref(""), None);
        assert_eq!(parse_model_ref("plain"), None);
    }

    #[test]
    fn m8_resolve_model_url_edge_matrix() {
        let config = ProjectConfig::builder().build();
        // No provider segment: default provider, empty URL, model untouched.
        assert_eq!(
            resolve_model_url(&config, "m"),
            (String::new(), "m".to_string(), false)
        );
        // Unknown provider: empty URL, model is the post-colon half.
        assert_eq!(
            resolve_model_url(&config, "nope:m"),
            (String::new(), "m".to_string(), false)
        );
        // Endpoint slashes collapse to exactly one.
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "p".into(),
            Provider {
                base_url: "http://h/".into(),
                chat_endpoint: "/api/chat".into(),
            },
        );
        let config = ProjectConfig::builder().providers(providers).build();
        let (url, model, thinking) = resolve_model_url(&config, "p:m");
        assert_eq!(url, "http://h/api/chat");
        assert_eq!(model, "m");
        assert!(!thinking);
        // Thinking via the models map (third clause).
        let mut models = std::collections::HashMap::new();
        models.insert("thinking".to_string(), "p:deep".to_string());
        let config = ProjectConfig::builder().models(models).build();
        let (_, _, thinking) = resolve_model_url(&config, "p:deep");
        assert!(thinking);
        let (_, _, thinking) = resolve_model_url(&config, "p:other");
        assert!(!thinking);
    }

    #[test]
    fn test_resolve_model_url_no_providers() {
        let config = ProjectConfig::builder().build();
        let (url, model, is_thinking) = resolve_model_url(&config, "default");
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
        let config = ProjectConfig::builder().providers(providers).build();
        let (url, model, is_thinking) = resolve_model_url(&config, "ollama:llama3");
        assert_eq!(model, "llama3");
        assert_eq!(url, "http://localhost:11434/api/chat");
        assert!(!is_thinking);
    }

    #[test]
    fn test_resolve_model_url_is_thinking() {
        let config = ProjectConfig::builder()
            .model_thinking("deepseek:r1".into())
            .build();
        let (_, model, is_thinking) = resolve_model_url(&config, "deepseek:r1");
        assert_eq!(model, "r1");
        assert!(is_thinking);
    }

    #[test]
    fn test_find_config_file_project() {
        let dir = tempdir();
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        let config_path = guidance_dir.join("guidance-config.json");
        std::fs::write(&config_path, r#"{"guidance_dir": ".guidance"}"#).expect("write");

        let found = find_config_file(dir.path());
        assert!(found.is_some(), "should find project config");
    }

    #[test]
    fn test_find_config_file_not_found() {
        let dir = tempdir();
        let found = find_config_file(dir.path());
        assert!(found.is_none(), "should not find config");
    }

    // M9.1 characterization: hierarchy precedence pinned before the
    // `common_core::config::find_hierarchical` extraction. Deterministic
    // subset only — the ambient user level (`~/.config`) cannot be
    // controlled hermetically here; full precedence (project beats user
    // beats absent) is pinned at the primitive in M9.2 with tempdirs.
    #[test]
    fn m9_project_config_wins_and_returns_exact_path() {
        let dir = tempdir();
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        let config_path = guidance_dir.join("guidance-config.json");
        std::fs::write(&config_path, r#"{"guidance_dir": ".guidance"}"#).expect("write");
        // A present project file always wins, regardless of ambient state.
        assert_eq!(find_config_file(dir.path()).as_deref(), Some(config_path.as_path()));
    }

    #[test]
    fn m9_load_config_is_strict_on_invalid_json() {
        // `load_config` uses strict `load_json` (NOT load-or-default): an
        // existing-but-broken project file errors instead of defaulting.
        let dir = tempdir();
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        std::fs::write(
            guidance_dir.join("guidance-config.json"),
            r#"{"embedding_model": "#,
        )
        .expect("write");
        assert!(load_config(dir.path()).is_err());
    }

    #[test]
    fn test_load_config_defaults() {
        let dir = tempdir();
        let config = load_config(dir.path()).expect("should return default");
        assert_eq!(config.guidance_dir, PathBuf::from(".guidance"));
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = tempdir();
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
    fn test_embed_object_parses_dims_and_cache() {
        // The default config writes `"embed": {"dims", "cache_limit"}` —
        // it must round-trip instead of being silently ignored.
        let dir = tempdir();
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        let config_path = guidance_dir.join("guidance-config.json");
        std::fs::write(
            &config_path,
            r#"{"models": {"embed": "llama:embed"}, "embed": {"dims": 768, "cache_limit": 400}}"#,
        )
        .expect("write");

        let config = load_config(dir.path()).expect("should load");
        assert_eq!(
            config.models.get("embed").map(String::as_str),
            Some("llama:embed")
        );
        let embed = config.embed.expect("embed object must parse");
        assert_eq!(embed.dims, Some(768));
        assert_eq!(embed.cache_limit, Some(400));
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

    #[test]
    fn test_resolve_model_ref_from_models_map() {
        let mut models = std::collections::HashMap::new();
        models.insert("fast".to_string(), "llama:code".to_string());
        models.insert("default".to_string(), "llama:code".to_string());
        let config = ProjectConfig::builder().models(models).build();
        assert_eq!(resolve_model_ref(&config, "fast"), "llama:code");
        assert_eq!(resolve_model_ref(&config, "default"), "llama:code");
    }

    #[test]
    fn test_resolve_model_ref_falls_back_to_flat_fields() {
        let config = ProjectConfig::builder()
            .model_fast("ollama:fast-model".into())
            .build();
        assert_eq!(resolve_model_ref(&config, "fast"), "ollama:fast-model");
    }

    #[test]
    fn test_resolve_model_ref_unknown_role_returns_default() {
        let config = ProjectConfig::builder().build();
        assert_eq!(resolve_model_ref(&config, "unknown"), "code:latest");
    }

    #[test]
    fn test_load_config_deserializes_models_map() {
        let dir = tempdir();
        let guidance_dir = dir.path().join(".guidance");
        std::fs::create_dir_all(&guidance_dir).expect("create");
        let config_path = guidance_dir.join("guidance-config.json");
        std::fs::write(
            &config_path,
            r#"{"models": {"fast": "llama:code", "default": "llama:code"}}"#,
        )
        .expect("write");

        let config = load_config(dir.path()).expect("should load");
        assert_eq!(config.models.get("fast").unwrap(), "llama:code");
        assert_eq!(config.models.get("default").unwrap(), "llama:code");
    }
}
