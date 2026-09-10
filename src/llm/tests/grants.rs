//! Ported from zvec-grep authorization semantics
//! (`authorization/store.ts` + `manager.ts` + `prompt.ts`): workspace grants
//! for remote embedding — HMAC-signed `authorization.json` documents,
//! grant/revoke/has-grant, one-shot `--allow-remote`, and the elicitation
//! seam. No remote data flows without a stored grant or an explicit
//! one-shot/elicited approval.

use fluent_llm::catalog::DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT;
use fluent_llm::grants::{
    ElicitationDecision, ElicitationRequest, Elicitor, RemoteEmbeddingAuthorizationStore,
    RemoteEmbeddingTarget, ensure_remote_embedding_authorized,
};
use tempfile::TempDir;

fn store_in(dir: &TempDir) -> RemoteEmbeddingAuthorizationStore {
    RemoteEmbeddingAuthorizationStore::with_signing_key_path(
        dir.path().join("signing.key"),
    )
}

fn target(roots: &[std::path::PathBuf]) -> RemoteEmbeddingTarget {
    RemoteEmbeddingTarget::new(
        roots.to_vec(),
        "qwen",
        "text-embedding-v4",
        DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT,
    )
}

#[tokio::test]
async fn grant_then_has_grant_with_fingerprint_isolation() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join(".zvec-grep")).unwrap();

    let granted = target(&[root.clone()]);
    assert!(!store.has_grant(&granted).unwrap());
    store.grant(&granted).await.unwrap();
    assert!(store.has_grant(&granted).unwrap());

    // A different endpoint is a different target: no grant.
    let other_endpoint = RemoteEmbeddingTarget::new(
        vec![root.clone()],
        "qwen",
        "text-embedding-v4",
        "https://example.test/embeddings",
    );
    assert!(!store.has_grant(&other_endpoint).unwrap());

    // A different model is a different target: no grant.
    let other_model = RemoteEmbeddingTarget::new(
        vec![root.clone()],
        "qwen",
        "qwen3.7-text-embedding",
        DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT,
    );
    assert!(!store.has_grant(&other_model).unwrap());
}

#[tokio::test]
async fn tampered_documents_do_not_authorize() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join(".zvec-grep")).unwrap();
    let granted = target(&[root.clone()]);
    store.grant(&granted).await.unwrap();
    assert!(store.has_grant(&granted).unwrap());

    let document_path = root.join(".zvec-grep").join("authorization.json");
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&document_path).unwrap()).unwrap();
    document["grants"][0]["model"] = serde_json::json!("tampered-model");
    std::fs::write(&document_path, serde_json::to_string(&document).unwrap()).unwrap();
    assert!(
        !store.has_grant(&granted).unwrap(),
        "tampered grant must not authorize"
    );
}

#[tokio::test]
async fn missing_key_and_missing_document_never_authorize() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    let granted = target(&[root]);
    // No signing key, no document: a clean negative, never an error.
    assert!(!store.has_grant(&granted).unwrap());
}

#[tokio::test]
async fn revoke_and_revoke_all() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join(".zvec-grep")).unwrap();
    let first = target(&[root.clone()]);
    let second = RemoteEmbeddingTarget::new(
        vec![root.clone()],
        "qwen",
        "qwen3.7-text-embedding",
        DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT,
    );
    store.grant(&first).await.unwrap();
    store.grant(&second).await.unwrap();
    assert!(store.revoke(&first).await.unwrap());
    assert!(!store.has_grant(&first).unwrap());
    assert!(store.has_grant(&second).unwrap());
    assert!(!store.revoke(&first).await.unwrap(), "second revoke is a no-op");
    assert_eq!(store.revoke_all(&root).await.unwrap(), 1);
    assert!(!store.has_grant(&second).unwrap());
}

#[tokio::test]
async fn grant_files_are_owner_only() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join(".zvec-grep")).unwrap();
    store.grant(&target(&[root.clone()])).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(root.join(".zvec-grep").join("authorization.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "grant document must be owner-only");
        let key_mode = std::fs::metadata(dir.path().join("signing.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(key_mode, 0o600, "signing key must be owner-only");
    }
}

struct StubElicitor {
    approve: bool,
    asked: std::sync::Mutex<Vec<String>>,
}

impl Elicitor for StubElicitor {
    fn elicit(&self, request: &ElicitationRequest) -> ElicitationDecision {
        self.asked
            .lock()
            .unwrap()
            .push(request.target.target_fingerprint.clone());
        if self.approve {
            ElicitationDecision::Allow
        } else {
            ElicitationDecision::Deny
        }
    }
}

#[tokio::test]
async fn one_shot_allow_remote_bypasses_the_store_without_writing() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join(".zvec-grep")).unwrap();
    let granted = target(&[root.clone()]);
    let outcome = ensure_remote_embedding_authorized(&store, &granted, true, None).await.unwrap();
    assert!(matches!(
        outcome,
        fluent_llm::grants::RemoteAuthorization::OneShot
    ));
    assert!(
        !root.join(".zvec-grep").join("authorization.json").exists(),
        "one-shot approval must not persist"
    );
}

#[tokio::test]
async fn elicitation_approval_persists_a_grant_and_denial_names_the_path() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join(".zvec-grep")).unwrap();
    let granted = target(&[root.clone()]);

    let approver = StubElicitor {
        approve: true,
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let outcome =
        ensure_remote_embedding_authorized(&store, &granted, false, Some(&approver)).await.unwrap();
    assert!(matches!(
        outcome,
        fluent_llm::grants::RemoteAuthorization::Stored
    ));
    assert!(store.has_grant(&granted).unwrap());
    assert_eq!(approver.asked.lock().unwrap().len(), 1);

    // A stored grant short-circuits: no second elicitation.
    let outcome =
        ensure_remote_embedding_authorized(&store, &granted, false, Some(&approver)).await.unwrap();
    assert!(matches!(
        outcome,
        fluent_llm::grants::RemoteAuthorization::Stored
    ));
    assert_eq!(approver.asked.lock().unwrap().len(), 1);

    let denier = StubElicitor {
        approve: false,
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let other = RemoteEmbeddingTarget::new(
        vec![root.clone()],
        "qwen",
        "qwen3.7-text-embedding",
        DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT,
    );
    let error =
        ensure_remote_embedding_authorized(&store, &other, false, Some(&denier)).await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("authorization.json"), "{message}");
    assert!(message.contains("qwen3.7-text-embedding"), "{message}");

    // No elicitor and no grant: the same named-path error, no data sent.
    let error = ensure_remote_embedding_authorized(&store, &other, false, None).await.unwrap_err();
    assert!(error.to_string().contains("authorization.json"), "{error}");
}

#[test]
fn target_fingerprint_is_stable_and_endpoint_sensitive() {
    let roots = vec![std::path::PathBuf::from("/repo")];
    let first = target(&roots);
    let same = target(&roots);
    assert_eq!(first.target_fingerprint, same.target_fingerprint);
    let different = RemoteEmbeddingTarget::new(
        roots,
        "qwen",
        "text-embedding-v4",
        "https://example.test/other",
    );
    assert_ne!(first.target_fingerprint, different.target_fingerprint);
    assert_eq!(first.workspace_roots.len(), 1);
}
