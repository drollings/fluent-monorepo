#[cfg(test)]
// The file is included as `mod tests` (transforms/mod.rs:10) and nests an
// inner `mod tests` to keep the harness private — intentional structure.
#[allow(clippy::module_inception)]
mod tests {
    use crate::testing::test_request;
    use crate::transforms::decompose_hypothetical::DecomposeToAnonymizedHypothetical;
    use crate::transforms::decompose_subtasks::DecomposeToSubtasks;
    use crate::transforms::none::NoTransform;
    use crate::transforms::pii_anonymize::PiiAnonymize;
    use crate::transforms::TransformStrategy;
    use fluent_types::openai_wire::RouterRequest;

    fn make_request(text: &str) -> RouterRequest {
        test_request(text)
    }

    // ── NoTransform ────────────────────────────────────────────────

    #[test]
    fn test_no_transform_preserves_all_fields() {
        let transform = NoTransform;
        let mut request = make_request("test");
        request.temperature = Some(0.7);
        request.max_tokens = Some(2048);
        request.session_id = Some("sess-1".into());
        let result = transform.transform(&request).unwrap();
        assert_eq!(result.temperature, Some(0.7));
        assert_eq!(result.max_tokens, Some(2048));
        assert_eq!(result.session_id, Some("sess-1".into()));
    }

    // ── PiiAnonymize ──────────────────────────────────────────────

    #[test]
    fn test_pii_anonymize_redacts_ssn() {
        let transform = PiiAnonymize;
        let request = make_request("My SSN is 123-45-6789. Please help.");
        let result = transform.transform(&request).unwrap();
        let output = result.messages[0].content.to_string_lossy();
        assert!(!output.contains("123-45-6789"), "SSN should be redacted");
        assert!(
            output.contains("[SSN]"),
            "output should contain [SSN] placeholder, got: {output}"
        );
    }

    #[test]
    fn test_pii_anonymize_redacts_email() {
        let transform = PiiAnonymize;
        let request = make_request("Contact me at user@example.com for details.");
        let result = transform.transform(&request).unwrap();
        let output = result.messages[0].content.to_string_lossy();
        assert!(
            !output.contains("user@example.com"),
            "email should be redacted"
        );
        assert!(
            output.contains("[EMAIL]"),
            "output should contain [EMAIL] placeholder, got: {output}"
        );
    }

    #[test]
    fn test_pii_anonymize_redacts_credit_card() {
        let transform = PiiAnonymize;
        let request = make_request("Card: 4111-1111-1111-1111 expires 12/25.");
        let result = transform.transform(&request).unwrap();
        let output = result.messages[0].content.to_string_lossy();
        assert!(
            !output.contains("4111-1111-1111-1111"),
            "card number should be redacted"
        );
    }

    #[test]
    fn test_pii_anonymize_redacts_phone() {
        let transform = PiiAnonymize;
        let request = make_request("Call me at 555-123-4567.");
        let result = transform.transform(&request).unwrap();
        let output = result.messages[0].content.to_string_lossy();
        assert!(!output.contains("555-123-4567"), "phone should be redacted");
    }

    #[test]
    fn test_pii_anonymize_stores_anonymize_map() {
        let transform = PiiAnonymize;
        let request = make_request("My email is user@example.com and SSN is 123-45-6789.");
        let result = transform.transform(&request).unwrap();
        let map = result.metadata.get("anonymize_map");
        assert!(map.is_some(), "anonymize_map should be present in metadata");
    }

    #[test]
    fn test_pii_anonymize_no_pii_passes_unchanged() {
        let transform = PiiAnonymize;
        let request = make_request("What is the capital of France?");
        let result = transform.transform(&request).unwrap();
        let output = result.messages[0].content.to_string_lossy();
        assert_eq!(output, "What is the capital of France?");
    }

    #[test]
    fn test_pii_anonymize_empty_yields_no_metadata_key() {
        // C2.M0: a request with no PII must NOT insert an empty anonymize_map
        // object into metadata (no spurious empty object).
        let transform = PiiAnonymize;
        let request = make_request("What is the capital of France?");
        let result = transform.transform(&request).unwrap();
        assert!(
            result.metadata.get("anonymize_map").is_none(),
            "no PII ⇒ no anonymize_map key"
        );
    }

    // ── DecomposeToAnonymizedHypothetical ─────────────────────────

    #[test]
    fn test_decompose_hypothetical_no_pii_in_output() {
        let transform = DecomposeToAnonymizedHypothetical;
        let request = make_request("My SSN is 123-45-6789 and email is user@example.com");
        let result = transform.transform(&request).unwrap();
        let output = result.messages[0].content.to_string_lossy();
        assert!(
            !output.contains("123-45-6789"),
            "SSN should not appear in output"
        );
        assert!(
            !output.contains("user@example.com"),
            "email should not appear in output"
        );
    }

    #[test]
    fn test_decompose_hypothetical_creates_system_and_user_messages() {
        let transform = DecomposeToAnonymizedHypothetical;
        let request = make_request("What is Rust?");
        let result = transform.transform(&request).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "system");
        assert_eq!(result.messages[1].role, "user");
    }

    #[test]
    fn test_decompose_hypothetical_contains_rubric() {
        let transform = DecomposeToAnonymizedHypothetical;
        let request = make_request("Help me with my code.");
        let result = transform.transform(&request).unwrap();
        let system_content = result.messages[0].content.to_string_lossy();
        assert!(
            system_content.contains("Rubric"),
            "system message should contain rubric"
        );
        assert!(
            system_content.contains("PII"),
            "system message should mention PII"
        );
    }

    #[test]
    fn test_decompose_hypothetical_metadata() {
        let transform = DecomposeToAnonymizedHypothetical;
        let request = make_request("Test query");
        let result = transform.transform(&request).unwrap();
        let transform_val = result.metadata.get("transform");
        assert!(transform_val.is_some());
        assert_eq!(
            transform_val.unwrap().as_str(),
            Some("decompose_to_anonymized_hypothetical")
        );
    }

    // ── DecomposeToSubtasks ───────────────────────────────────────

    #[test]
    fn test_decompose_subtasks_with_stub_decomposer() {
        use fluent_llm::Decomposer;

        let subtasks = vec![
            "Research the topic".to_string(),
            "Write the code".to_string(),
            "Test the solution".to_string(),
        ];

        struct StubDecomposer {
            output: Vec<String>,
        }

        impl Decomposer for StubDecomposer {
            fn decompose(&self, _task: &str) -> Vec<String> {
                self.output.clone()
            }
        }

        let decomposer = StubDecomposer {
            output: subtasks.clone(),
        };

        let transform = DecomposeToSubtasks::new(Box::new(decomposer));
        let request = make_request("Build a web app");
        let result = transform.transform(&request).unwrap();

        // Should have system + user messages
        assert_eq!(result.messages.len(), 2);
        let user_content = result.messages[1].content.to_string_lossy();
        assert!(
            user_content.contains("1."),
            "should contain numbered subtasks"
        );
        assert!(
            user_content.contains("Research"),
            "should contain first subtask"
        );
        assert!(user_content.contains("Test"), "should contain last subtask");

        // Should have subtasks in metadata
        let subtasks_val = result.metadata.get("subtasks").unwrap();
        assert_eq!(subtasks_val["count"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_decompose_subtasks_empty_input_returns_not_applicable() {
        struct EmptyDecomposer;
        impl fluent_llm::Decomposer for EmptyDecomposer {
            fn decompose(&self, _task: &str) -> Vec<String> {
                vec![]
            }
        }

        let transform = DecomposeToSubtasks::new(Box::new(EmptyDecomposer));
        let request = make_request(""); // empty user message triggers NotApplicable
        let result = transform.transform(&request);
        assert!(result.is_err(), "empty user text should error");
    }

    // ── Shared helper coverage (`crate::transforms` interface) ──

    use crate::transforms::{insert_string_map, rewrite_text_messages, TransformError};
    use fluent_types::openai_wire::{ContentPart, RouterMessage, RouterMessageContent};

    fn request_with(text: &str) -> RouterRequest {
        use crate::testing::test_request;
        let mut req = test_request(text);
        // `test_request` builds the standard shape; pin the literal fields the
        // helper tests assert on.
        req.messages = vec![RouterMessage {
            role: "user".into(),
            content: RouterMessageContent::Text(text.into()),
            tool_calls: None,
            tool_call_id: None,
        }];
        req
    }

    #[test]
    fn test_rewrite_leaves_parts_and_aborts_on_error() {
        let mut req = request_with("hi");
        req.messages.push(RouterMessage {
            role: "user".into(),
            content: RouterMessageContent::Parts(vec![ContentPart::Text {
                text: "kept".into(),
            }]),
            tool_calls: None,
            tool_call_id: None,
        });
        let out = rewrite_text_messages(&req, |t| Ok(t.to_uppercase())).expect("rewrite");
        assert_eq!(out.messages[0].content.as_text(), "HI");
        assert_eq!(out.messages[1].content.to_string_lossy(), "kept");
        let err = rewrite_text_messages(&req, |_| Err(TransformError::Failed("x".into())))
            .expect_err("aborts");
        assert!(matches!(err, TransformError::Failed(_)));
    }

    #[test]
    fn test_empty_map_inserts_nothing() {
        let mut req = request_with("hi");
        insert_string_map(&mut req, "k", std::collections::HashMap::new());
        assert!(req.metadata.get("k").is_none());
    }

    #[test]
    fn test_insert_map_object_shape_and_existing_keys() {
        let mut req = request_with("hi");
        req.metadata
            .insert("pre".into(), serde_json::json!({"x": 1}));
        let map: std::collections::HashMap<String, String> =
            [("café", "北京"), ("email", "u@e.com")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        insert_string_map(&mut req, "map", map);
        let obj = req.metadata.get("map").unwrap().as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(obj["café"], serde_json::Value::String("北京".into()));
        assert!(req.metadata.contains_key("pre"));
    }

    #[test]
    fn test_rewrite_empty_messages_passthrough() {
        let mut req = request_with("hi");
        req.messages.clear();
        let out = rewrite_text_messages(&req, |t| Ok(t.to_uppercase())).expect("empty passthrough");
        assert!(out.messages.is_empty());
    }

    // ── No LLM calls in any test ──────────────────────────────────
    // All tests above use only fixture data — no live models or network.
}
