#[cfg(test)]
// The file is included as `mod server_tests` (lib.rs:62) and nests an inner
// `mod server_tests` to keep the harness private — intentional structure.
#[allow(clippy::module_inception)]
mod server_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fluent_wvr::prelude::*;

    use crate::config::ServerConfig;
    use crate::pipeline::PipelineOrchestrator;
    use crate::server::RouterServer;

    fn make_echo_pipeline() -> Arc<PipelineOrchestrator> {
        let stages: Vec<Arc<dyn Component>> = vec![];
        Arc::new(PipelineOrchestrator::new(stages))
    }

    #[test]
    fn router_server_creates_with_config() {
        let mut pipelines = HashMap::new();
        pipelines.insert("default".into(), make_echo_pipeline());
        let routes = HashMap::new();
        let models = HashMap::new();
        let config = ServerConfig::default();
        let server = RouterServer::new(pipelines, routes, models, &config, None);
        assert_eq!(server.name(), "router.server");
        assert!(server
            .provides()
            .contains(&ArcIntern::from("http.endpoint")));
    }

    #[test]
    fn server_config_defaults() {
        let config = ServerConfig::default();
        assert!(
            config.bind_addr.is_empty(),
            "bind_addr must be provided by config or CLI, not hardcoded"
        );
        assert_eq!(config.max_payload, 1_048_576);
    }

    /// Boot-materialized routing tables for warmup-target tests: models,
    /// roles, routes.
    type WarmupTables = (
        HashMap<String, crate::config::ModelEntry>,
        HashMap<String, crate::config::RoleEntry>,
        HashMap<String, crate::config::RouteRef>,
    );

    fn warmup_tables() -> WarmupTables {
        // Fleet shape: qwen serves pinned ledger + unpinned quick (two
        // siblings) and scratch, tiny serves unpinned work only, plain is
        // unbound (lazy base). The default route serves quick — unpinned,
        // but still a warmup target through the responsiveness set.
        use crate::config::RoleEntry;
        let roles: HashMap<String, RoleEntry> = serde_json::from_value(serde_json::json!({
            "ledger": {"pinned": true, "params": {"num_ctx": 65536}, "models": ["qwen"]},
            "quick": {"max_parallel": 2, "params": {"num_ctx": 8192}, "models": ["qwen"]},
            "scratch": {"params": {"num_ctx": 32768}, "models": ["qwen"]},
            "work": {"params": {"num_ctx": 4096}, "models": ["tiny"]}
        }))
        .expect("roles parse");
        let mut models: HashMap<String, crate::config::ModelEntry> =
            serde_json::from_value(serde_json::json!({
                "qwen": {
                    "endpoint": "http://x/v1/chat/completions",
                    "name": "qwen", "weights": "/models/qwen.gguf",
                    "intelligence": 4,
                    "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10
                },
                "tiny": {
                    "endpoint": "http://y/v1/chat/completions",
                    "name": "tiny", "weights": "/models/tiny.gguf",
                    "intelligence": 1,
                    "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10
                },
                "plain": {
                    "endpoint": "http://z/v1/chat/completions",
                    "name": "plain", "weights": "/models/plain.gguf",
                    "intelligence": 1,
                    "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10
                }
            }))
            .expect("models parse");
        // Boot-compose effective pools the way `apply_defaults` does.
        for key in ["qwen", "tiny", "plain"] {
            let entry = models.get(key).cloned().expect("member");
            let pool = crate::config::materialize_effective_pool(key, &entry, &roles);
            models.get_mut(key).expect("member").effective_profiles = Some(pool);
        }
        let routes = HashMap::from([(
            "local".to_string(),
            crate::config::RouteRef {
                role: "quick".to_string(),
                pipelines: vec!["default".into()],
                description: String::new(),
                always_route: false,
            },
        )]);
        (models, roles, routes)
    }

    #[test]
    fn warmup_targets_cover_pinned_and_default_windows() {
        let (models, roles, routes) = warmup_tables();
        let targets =
            crate::server::warmup_targets(&models, &roles, &routes, "local");
        // Pinned ledger plus both quick siblings through the default route's
        // terminal role — unpinned scratch/tiny/plain windows stay lazy.
        assert_eq!(
            targets,
            vec![
                ("qwen".to_string(), "ledger".to_string()),
                ("qwen".to_string(), "quick-0".to_string()),
                ("qwen".to_string(), "quick-1".to_string()),
            ],
            "resident set plus default-route windows, sorted"
        );
    }

    #[test]
    fn warmup_targets_without_default_route_warm_pinned_only() {
        let (models, roles, routes) = warmup_tables();
        let targets = crate::server::warmup_targets(&models, &roles, &routes, "");
        assert_eq!(
            targets,
            vec![("qwen".to_string(), "ledger".to_string())],
            "absent default route warms pinned instances only"
        );
    }
}
