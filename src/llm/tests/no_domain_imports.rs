//! No-domain-imports guard.
//!
//! Compile-time + source-level assertion that `fluent-llm` (and
//! `common-core`) never import domain crates:
//! `router | coral | guidance | types | dag | db | concept | rdf |
//!  spacy | wasm_ipc` (plus their Cargo package-name spellings).
//! Implemented as a source grep-test over `src/llm/src` +
//! `src/common-core/src` so a future `use router::…` fails this test
//! without needing trybuild. Mirrors `bin/llm-boundary-check.sh` check 2.

use std::path::{Path, PathBuf};

/// Crate-name stems forbidden in `use` heads inside llm / common-core.
const FORBIDDEN: &[&str] = &[
    "router",
    "coral",
    "coral_context",
    "guidance",
    "guidance_core",
    "fluent_concept",
    "fluent_types",
    "fluent_dag",
    "fluent_db",
    "fluent_router",
    "concept",
    "rdf",
    "spacy",
    "spacy_rs",
    "wasm_ipc",
    "search_vector",
];

fn is_forbidden_use(line: &str) -> Option<String> {
    let t = line.trim_start();
    // Skip comments and doc examples.
    if t.starts_with("//") || t.starts_with("///") || t.starts_with("//!") || t.starts_with('*') {
        return None;
    }
    let rest = t
        .strip_prefix("pub use ")
        .or_else(|| t.strip_prefix("use "))?;
    let head = rest.split([':', ';', ' ', '{']).next().unwrap_or("").trim();
    // `fluent_llm` self-mentions in doc comments are filtered above; a real
    // `use fluent_llm::…` inside the crate itself would be a cycle worth flagging.
    let all: Vec<&str> = FORBIDDEN.iter().copied().chain(["fluent_llm"]).collect();
    if all.contains(&head) {
        Some(head.to_string())
    } else {
        None
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read_dir");
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn check_tree(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rs_files(root, &mut files);
    let mut hits = Vec::new();
    for f in files {
        let content = std::fs::read_to_string(&f).expect("read rs file");
        for (i, line) in content.lines().enumerate() {
            if let Some(head) = is_forbidden_use(line) {
                hits.push(format!(
                    "{}:{}: forbidden `use {}`",
                    f.display(),
                    i + 1,
                    head
                ));
            }
        }
    }
    hits
}

#[test]
fn no_domain_imports_in_llm_or_common_core() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let llm_src = manifest.join("src");
    let cc_src = manifest.join("../common-core/src");
    let mut hits = check_tree(&llm_src);
    if cc_src.is_dir() {
        hits.extend(check_tree(&cc_src));
    }
    assert!(
        hits.is_empty(),
        "domain-crate imports forbidden in fluent-llm/common-core:\n{}",
        hits.join("\n")
    );
}

#[test]
fn protocol_types_still_resolve_via_fluent_llm() {
    // Ownership lock (M9, completed by M11): protocol types are owned by
    // fluent_llm::protocol (M11 deleted the fluent-concurrency::llm_queue
    // shims). This test pins the canonical path.
    fn assert_same_type(c: fluent_llm::protocol::LlmConfig) -> fluent_llm::LlmConfig {
        c
    }
    let _ = assert_same_type;
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<fluent_llm::LlmConfig>();
    assert_send_sync::<fluent_llm::ChatMessage>();
    assert_send_sync::<fluent_llm::LlmTask>();
    assert_send_sync::<fluent_llm::LlmQueueConfig>();
    assert_send_sync::<fluent_llm::LlmRequestQueue>();
    let _ = fluent_llm::LlmError::NoResponse;
}

/// Consumer crates no shared crate may import (lib names): guidance and
/// coral front-ends, the router, orator/protopyne/spacy/egui/onnx
/// surfaces, the memory plugin, and the WASM IPC boundary. Shared
/// crates depending inward on any of these would invert the dependency
/// direction (consumers compose shared primitives, never the reverse).
const CONSUMER_FORBIDDEN: &[&str] = &[
    "guidance_core",
    "coral_context",
    "fluent_router",
    "orator",
    "orator_agent",
    "orator_audio",
    "orator_cli",
    "orator_daemon",
    "orator_desktop",
    "orator_memory",
    "orator_models",
    "orator_supervise",
    "orator_testkit",
    "orator_voice",
    "protopyne_types",
    "protopyne_kernels",
    "protopyne_store",
    "protopyne_strategy",
    "protopyne_venue",
    "protopyne_server",
    "protopyne_chart_model",
    "protopyne_chart_scale",
    "protopyne_view",
    "protopyne_egui",
    "spacy_rs",
    "fluent_egui",
    "fluent_onnx",
    "memory_plugin",
    "wasm_ipc",
];

/// Shared-crate roots checked for consumer-direction imports (the
/// `llm`/`common-core` pair keeps its stricter dedicated test above).
const SHARED_ROOTS: &[&str] = &[
    "../types/src",
    "../dag/src",
    "../db/src",
    "../fluent-wvr/src",
    "../fluent-wvr-macros/src",
    "../fluent-concurrency/src",
    "../search-vector/src",
    "../concept/src",
    "../knowledge/src",
    "../rdf/src",
    "../vector-math/src",
    "../wasm_ipc/src",
];

fn is_consumer_use(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with("//") || t.starts_with('*') {
        return None;
    }
    let rest = t
        .strip_prefix("pub use ")
        .or_else(|| t.strip_prefix("use "))?;
    let head = rest.split([':', ';', ' ', '{']).next().unwrap_or("").trim();
    CONSUMER_FORBIDDEN
        .iter()
        .find(|&&banned| head == banned)
        .map(std::string::ToString::to_string)
}

#[test]
fn no_consumer_imports_in_shared_crates() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    for root in SHARED_ROOTS {
        let dir = manifest.join(root);
        if !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&dir, &mut files);
        for f in files {
            let content = std::fs::read_to_string(&f).expect("read rs file");
            for (i, line) in content.lines().enumerate() {
                // `use crate::…` / `use super::…` / `use self::…` are
                // intra-crate paths, never consumer imports.
                let t = line.trim_start();
                if t.starts_with("use crate::")
                    || t.starts_with("use super::")
                    || t.starts_with("use self::")
                    || t.starts_with("pub use crate::")
                {
                    continue;
                }
                if let Some(banned) = is_consumer_use(line) {
                    hits.push(format!(
                        "{}:{}: shared crate uses consumer `{}`",
                        f.display(),
                        i + 1,
                        banned
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "shared crates must not depend on consumers:\n{}",
        hits.join("\n")
    );
}
