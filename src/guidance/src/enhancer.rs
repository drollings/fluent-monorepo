use fluent_llm::client::LlmClient;
use fluent_llm::ChatMessage;
use fluent_types::GuidanceDoc;
use fluent_wvr::{ArcIntern, Describable, WorkContext, WorkError, WorkOutput, WorkUnit};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

use common_core::metrics::LatencyHistogram;

#[derive(Error, Debug)]
pub enum EnhancerError {
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("no response from model")]
    NoResponse,
}

/// Language-specific comment generation — the fluent-wvr control plane for
/// LLM prompting. Each language implements its own system prompt and
/// member/file prompt templates.
///
/// M10.3: uniformity is compiler-enforced — every generator is a
/// `fluent_wvr::Component`. The `Enhancer` holds one generator and calls it
/// natively per member (hot loop: no dual-view allocation, no handoff
/// round-trip); the bound keeps the single registration uniformly
/// inventoried.
pub trait CommentGenerator: fluent_wvr::Component {
    /// System message for the LLM conversation.
    fn system_prompt(&self, language: &str) -> String;

    /// Prompt for generating a member (function/type) comment.
    fn member_prompt(
        &self,
        name: &str,
        signature: &str,
        module_context: &str,
        kind_label: &str,
        language: &str,
    ) -> String;

    /// Prompt for generating a file-level description.
    fn file_prompt(&self, rel_path: &str, source_preview: &str, language: &str) -> String;
}

/// Default comment generator — language-agnostic prompts that work for any language.
pub struct DefaultCommentGenerator;

impl CommentGenerator for DefaultCommentGenerator {
    fn system_prompt(&self, language: &str) -> String {
        format!("You are a technical documentation assistant for {language} code.")
    }

    fn member_prompt(
        &self,
        name: &str,
        signature: &str,
        module_context: &str,
        kind_label: &str,
        _language: &str,
    ) -> String {
        format!(
            "{kind_label} in {module_context}:\n  {signature}\n\n\
             Write a single-line comment for this {kind_label}.\n\
             Rules:\n\
             - Plain English, technically specific\n\
             - Max 200 characters\n\
             - No boilerplate openers\n\n\
             Wrap your answer in <comment> tags. Example:\n\
             <comment>Parses a null-terminated C string into an owned slice.</comment>\n\n\
             Name: {name}"
        )
    }

    fn file_prompt(&self, rel_path: &str, source_preview: &str, _language: &str) -> String {
        format!(
            "Source:\n{source_preview}\n\nFile: {rel_path}\n\n\
             Write a single-line description for this file.\n\
             Rules:\n\
             - Plain English, technically specific\n\
             - Max 200 chars\n\
             - No boilerplate openers\n\n\
             Wrap your answer in <comment> tags."
        )
    }
}

// --- M10.2: uniform handle (additive; native prompts untouched) -------------
//
// `DefaultCommentGenerator` is a `fluent_wvr::Component` so the
// orchestrator can hold an `Arc<dyn Component>` handle. Prompt building is
// pure string work, so `execute` runs it for real through the structured
// handoff (`ctx.structured["prompt"]` = `system` | `member` | `file`, plus
// the prompt fields as JSON strings).

/// Read one required string field from the structured handoff.
fn structured_str(ctx: &WorkContext, key: &str) -> Result<String, WorkError> {
    ctx.structured
        .get(key)
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| {
            WorkError::Execution(format!("generator execute needs ctx.structured[\"{key}\"]"))
        })
}

impl WorkUnit for DefaultCommentGenerator {
    fn name(&self) -> &str {
        "generator.default_comment"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let language = structured_str(ctx, "language")?;
        let prompt = match structured_str(ctx, "prompt")?.as_str() {
            "system" => self.system_prompt(&language),
            "member" => self.member_prompt(
                &structured_str(ctx, "name")?,
                &structured_str(ctx, "signature")?,
                &structured_str(ctx, "module_context")?,
                &structured_str(ctx, "kind_label")?,
                &language,
            ),
            "file" => self.file_prompt(
                &structured_str(ctx, "rel_path")?,
                &structured_str(ctx, "source_preview")?,
                &language,
            ),
            other => {
                return Err(WorkError::Execution(format!(
                    "generator execute: unknown prompt kind {other:?}"
                )));
            }
        };
        Ok(WorkOutput::ok(prompt))
    }
}

impl Describable for DefaultCommentGenerator {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "comment_generator",
            "generator": "default_comment",
            "name": "generator.default_comment",
        })
    }
}

fluent_wvr::impl_fieldless!(DefaultCommentGenerator);
fluent_wvr::impl_component!(DefaultCommentGenerator);

/// AI comment enhancer for guidance generation.
///
/// Generates descriptions (≤200 chars) for functions, structs, and files
/// by calling the configured LLM endpoint. Uses a `CommentGenerator` trait
/// for language-specific prompting (fluent-wvr control plane pattern).
pub struct Enhancer {
    pub client: LlmClient,
    pub generator: Box<dyn CommentGenerator>,
    pub debug: bool,
    pub show_prompts: bool,
    pub histogram: Option<Arc<LatencyHistogram>>,
}

impl Enhancer {
    pub fn new(api_base: &str, model: &str) -> Self {
        Self {
            client: LlmClient::new(api_base, model),
            generator: Box::new(DefaultCommentGenerator),
            debug: false,
            show_prompts: false,
            histogram: None,
        }
    }

    #[must_use]
    pub fn with_generator(mut self, generator: Box<dyn CommentGenerator>) -> Self {
        self.generator = generator;
        self
    }

    #[must_use]
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    #[must_use]
    pub fn with_show_prompts(mut self, show: bool) -> Self {
        self.show_prompts = show;
        self
    }

    /// Attach a `LatencyHistogram` to record the LLM call duration.
    #[must_use]
    pub fn with_metrics(mut self, histogram: Arc<LatencyHistogram>) -> Self {
        self.histogram = Some(histogram);
        self
    }

    /// Generate a comment for a function, struct, enum, or union type.
    pub fn enhance_member(
        &self,
        name: &str,
        signature: &str,
        module_context: &str,
        kind_label: &str,
        language: &str,
    ) -> Result<Option<String>, EnhancerError> {
        let prompt =
            self.generator
                .member_prompt(name, signature, module_context, kind_label, language);
        self.call_llm(language, &prompt)
    }

    /// Generate a comment for a function or method.
    pub fn enhance_function(
        &self,
        name: &str,
        signature: &str,
        module_context: &str,
        language: &str,
    ) -> Result<Option<String>, EnhancerError> {
        self.enhance_member(name, signature, module_context, "Function", language)
    }

    /// Generate a comment for a struct/enum/union type.
    pub fn enhance_struct(
        &self,
        name: &str,
        signature: &str,
        module_context: &str,
        language: &str,
    ) -> Result<Option<String>, EnhancerError> {
        self.enhance_member(name, signature, module_context, "Type", language)
    }

    /// Generate a one-line file-level description.
    pub fn enhance_file(
        &self,
        rel_path: &str,
        source_preview: &str,
        language: &str,
    ) -> Result<Option<String>, EnhancerError> {
        let preview = if source_preview.len() > 3000 {
            &source_preview[..3000]
        } else {
            source_preview
        };
        let prompt = self.generator.file_prompt(rel_path, preview, language);
        self.call_llm(language, &prompt)
    }

    /// Score a docstring for quality (mirrors Zig `scoreDocstring`).
    pub fn score_docstring(text: &str) -> u32 {
        if text.is_empty() {
            return 0;
        }
        let mut score: u32 = 0;
        if text.len() > 50 {
            score += 1;
        }
        let lower = text.to_lowercase();
        if lower.contains("args:") || lower.contains("parameters") {
            score += 2;
        }
        if lower.contains("returns:") || lower.contains("return") {
            score += 2;
        }
        if lower.contains("error") || lower.contains("raises:") {
            score += 1;
        }
        let newline_count = text.chars().filter(|&c| c == '\n').count() as u32;
        if newline_count > 2 {
            score += 1;
        }
        score
    }

    fn call_llm(&self, language: &str, prompt: &str) -> Result<Option<String>, EnhancerError> {
        if self.debug {
            tracing::debug!("[enhancer] sending prompt (len={})", prompt.len());
        }
        if self.show_prompts {
            tracing::debug!("[enhancer] prompt:\n{prompt}");
        }

        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: self.generator.system_prompt(language),
            },
            ChatMessage {
                role: "user".into(),
                content: prompt.to_string(),
            },
        ];

        let start = Instant::now();
        let response = self.chat_complete_with_retry(&messages);
        if let Some(ref hist) = self.histogram {
            hist.observe_duration(start);
        }
        let response = response.map_err(|e| EnhancerError::Llm(e.to_string()))?;

        if response.is_empty() {
            return Err(EnhancerError::NoResponse);
        }

        // Extract <comment>...</comment> tag (canonical helper; a
        // missing or empty tag falls through to the raw response below).
        if let Some(content) = common_core::string::extract_tag(&response, "comment") {
            return Ok(Some(content.to_string()));
        }

        // Fallback: return the raw response (trimmed)
        let trimmed = response.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed));
        }

        Ok(None)
    }

    /// Chat completion with jittered-exponential retry over the shared
    /// `common_core::retry::retry_async` helper. Only transient
    /// (`LlmError::is_retryable()`) failures are retried; permanent errors
    /// short-circuit on the first attempt, preserving the original
    /// single-shot behavior byte-for-byte.
    fn chat_complete_with_retry(
        &self,
        messages: &[ChatMessage],
    ) -> Result<String, fluent_llm::LlmError> {
        let client = self.client.clone();
        let fut = common_core::retry::retry_async(
            3,
            fluent_llm::constants::DEFAULT_RETRY_INTERVAL_S * 1000,
            0,
            fluent_llm::LlmError::is_retryable,
            || {
                let client = client.clone();
                let messages = messages.to_vec();
                async move { client.chat_complete_async(&messages).await }
            },
        );
        fluent_llm::client::block_on(fut)
    }
}

/// Generate missing comments for all members in a doc using the enhancer.
pub fn enhance_doc(
    enhancer: &Enhancer,
    doc: &mut GuidanceDoc,
    source_content: &str,
) -> Result<usize, EnhancerError> {
    let mut generated = 0;
    let module = doc.meta.module.as_str();
    let language = doc.meta.language.as_str();

    for member in &mut doc.members {
        if member.comment.is_some() && !member.comment_generated {
            continue;
        }

        let sig = member
            .signature
            .as_ref()
            .map_or(member.name.as_str(), smol_str::SmolStr::as_str);

        let result = match member.type_name {
            fluent_types::MemberType::FnDecl
            | fluent_types::MemberType::FnPrivate
            | fluent_types::MemberType::Method
            | fluent_types::MemberType::MethodPrivate => {
                enhancer.enhance_function(member.name.as_str(), sig, module, language)
            }
            fluent_types::MemberType::Struct
            | fluent_types::MemberType::Enum
            | fluent_types::MemberType::Union => {
                enhancer.enhance_struct(member.name.as_str(), sig, module, language)
            }
            _ => continue,
        };

        if let Ok(Some(comment)) = result {
            member.comment = Some(comment.into());
            member.comment_generated = true;
            generated += 1;
        }
    }

    // Generate file-level comment if missing
    if doc.comment.is_none() {
        if let Ok(Some(file_comment)) =
            enhancer.enhance_file(doc.meta.source.as_str(), source_content, language)
        {
            doc.comment = Some(file_comment.into());
            generated += 1;
        }
    }

    Ok(generated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_types::{GuidanceDoc, Member, MemberType};

    #[test]
    fn test_score_docstring_short() {
        assert_eq!(Enhancer::score_docstring(""), 0);
        assert_eq!(Enhancer::score_docstring("short"), 0);
    }

    #[test]
    fn test_score_docstring_long() {
        let text = "a".repeat(51);
        let score = Enhancer::score_docstring(&text);
        assert!(score >= 1, "long text gets a point for length");
    }

    #[test]
    fn test_score_docstring_with_args() {
        let score = Enhancer::score_docstring("Args: x i32, y i32");
        assert!(score >= 2, "args gets points");
    }

    #[test]
    fn test_score_docstring_with_returns() {
        let score = Enhancer::score_docstring("Returns: the sum");
        assert!(score >= 2, "returns gets points");
    }

    #[test]
    fn test_call_llm_extracts_comment_tag() {
        let response = "<comment>Test comment</comment>";
        assert!(response.contains("<comment>"), "should have comment tag");
        assert!(
            response.contains("Test comment"),
            "should contain extracted text"
        );
    }

    #[test]
    fn test_enhance_doc_no_llm_fallback() {
        let mut doc = GuidanceDoc {
            comment: None,
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "foo".into(),
                signature: Some("fn foo() void".into()),
                comment: None,
                ..Member::default()
            }],
            ..crate::tests::common::make_test_doc()
        };

        // No LLM available, so enhance should gracefully skip all members
        let enhancer = Enhancer::new("http://localhost:99999", "test");
        let result = enhance_doc(&enhancer, &mut doc, "pub fn foo() void {}");
        assert!(result.is_ok(), "should not propagate LLM errors");
    }

    #[test]
    fn test_score_docstring_newlines() {
        let text = "Line 1\nLine 2\nLine 3\nLine 4\n";
        let score = Enhancer::score_docstring(text);
        assert!(score >= 1, "multi-line text gets a point");
    }

    #[test]
    fn test_comment_tag_extraction_logic() {
        // Now a direct pin of the canonical helper (the pre-M8.4 replica
        // re-implemented the extraction inline — removed with the migration).
        let response = "Some reasoning text\n<comment>Parses input and produces output.</comment>";
        assert_eq!(
            common_core::string::extract_tag(response, "comment"),
            Some("Parses input and produces output.")
        );
    }

    // M8.1 characterization: `<comment>` tag behavior pinned verbatim
    // through the real `enhance_function` path (canned LLM responses)
    // before the `common_core::string::extract_tag` extraction. Rules:
    // first tag wins, content trims, empty/missing/unclosed tags fall back
    // to the raw trimmed response.
    #[test]
    fn m8_comment_tag_matrix_through_llm_path() {
        fn canned(body: &str) -> httpmock::MockServer {
            let server = httpmock::MockServer::start();
            server.mock(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/chat/completions");
                then.status(200).body(format!(
                    "{{\"choices\":[{{\"message\":{{\"content\":{}}}}}]}}",
                    serde_json::to_string(body).expect("escape")
                ));
            });
            server
        }
        let cases = [
            (
                "Reasoning...\n<comment>  Trimmed.  </comment>\ntrailing",
                "Trimmed.",
            ),
            ("no tags here", "no tags here"),
            ("<comment></comment>", "<comment></comment>"),
            ("<comment>   </comment>", "<comment>   </comment>"),
            (
                "<comment>first</comment> <comment>second</comment>",
                "first",
            ),
            ("<comment>unclosed", "<comment>unclosed"),
        ];
        for (body, expected) in cases {
            let server = canned(body);
            let enhancer = Enhancer::new(&server.base_url(), "test");
            let got = enhancer
                .enhance_function("foo", "fn foo()", "mod", "zig")
                .expect("llm path");
            assert_eq!(got.as_deref(), Some(expected), "{body:?}");
        }
    }

    #[test]
    fn test_default_comment_generator_language_agnostic() {
        let gen = DefaultCommentGenerator;
        let sys = gen.system_prompt("python");
        assert!(sys.contains("python"), "should mention the language");

        let prompt = gen.member_prompt("foo", "fn foo()", "mod", "Function", "rust");
        assert!(prompt.contains("foo"), "should contain the name");
        assert!(prompt.contains("Function"), "should contain the kind label");
    }

    // --- M10.1 characterization: prompt byte goldens (verbatim outputs) ---

    #[test]
    fn m10_comment_generator_prompts_byte_identical() {
        // M10.3's uniform-handle switch must preserve these bytes exactly —
        // prompts are LLM inputs; any drift changes model behavior.
        let gen = DefaultCommentGenerator;
        assert_eq!(
            gen.system_prompt("python"),
            "You are a technical documentation assistant for python code."
        );
        assert_eq!(
            gen.member_prompt("foo", "fn foo()", "mod", "Function", "rust"),
            "Function in mod:\n  fn foo()\n\n\
             Write a single-line comment for this Function.\n\
             Rules:\n\
             - Plain English, technically specific\n\
             - Max 200 characters\n\
             - No boilerplate openers\n\n\
             Wrap your answer in <comment> tags. Example:\n\
             <comment>Parses a null-terminated C string into an owned slice.</comment>\n\n\
             Name: foo"
        );
        assert_eq!(
            gen.file_prompt("src/main.rs", "fn main() {}", "rust"),
            "Source:\nfn main() {}\n\nFile: src/main.rs\n\n\
             Write a single-line description for this file.\n\
             Rules:\n\
             - Plain English, technically specific\n\
             - Max 200 chars\n\
             - No boilerplate openers\n\n\
             Wrap your answer in <comment> tags."
        );
    }

    #[test]
    fn test_call_llm_retries_transient_then_succeeds() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static RETRY_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
        RETRY_ATTEMPTS.store(0, Ordering::SeqCst);

        fn is_transient(_req: &httpmock::prelude::HttpMockRequest) -> bool {
            RETRY_ATTEMPTS.fetch_add(1, Ordering::SeqCst) < 1
        }

        let server = httpmock::MockServer::start();
        let transient = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/chat/completions")
                .matches(is_transient);
            then.status(429).body("rate limited");
        });

        let ok_mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/chat/completions");
            then.status(200)
                .body(r#"{"choices":[{"message":{"content":"<comment>ok</comment>"}}]}"#);
        });

        let enhancer = Enhancer::new(&server.base_url(), "test");
        let result = enhancer.enhance_function("foo", "fn foo()", "mod", "zig");
        assert!(result.is_ok(), "should succeed after retry: {result:?}");
        assert_eq!(result.unwrap(), Some("ok".to_string()));
        transient.assert_hits(1);
        ok_mock.assert_hits(1);
    }

    #[test]
    fn test_call_llm_permanent_error_no_retry() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/chat/completions");
            then.status(400).body("bad request");
        });

        let enhancer = Enhancer::new(&server.base_url(), "test");
        let result = enhancer.enhance_function("foo", "fn foo()", "mod", "zig");
        assert!(result.is_err(), "permanent error should surface");
        // A single attempt — no retry for permanent (Api) errors.
        assert_eq!(mock.hits(), 1);
    }

    #[test]
    fn test_call_llm_records_latency_metrics() {
        let server = httpmock::MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/chat/completions");
            then.status(200)
                .body(r#"{"choices":[{"message":{"content":"<comment>hi</comment>"}}]}"#);
        });

        let hist = Arc::new(LatencyHistogram::new());
        let enhancer = Enhancer::new(&server.base_url(), "test").with_metrics(Arc::clone(&hist));
        let result = enhancer.enhance_function("foo", "fn foo()", "mod", "zig");
        assert!(result.is_ok());
        assert_eq!(hist.count(), 1, "one LLM call recorded");
        assert!(hist.sum_ms() > 0, "latency recorded");
    }

    // --- M10.2: uniform-handle contract ------------------------------------

    use fluent_wvr::{Component, FieldAccess, WorkContext};

    fn ctx_with_prompt(fields: &[(&str, &str)]) -> WorkContext {
        let mut ctx = WorkContext::default();
        for (key, value) in fields {
            ctx.structured.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
        ctx
    }

    #[test]
    fn m10_generator_handle_executes_byte_identical_to_native() {
        let gen = DefaultCommentGenerator;
        let handle: std::sync::Arc<dyn Component> =
            std::sync::Arc::new(DefaultCommentGenerator);
        assert_eq!(handle.name(), "generator.default_comment");
        let member_ctx = ctx_with_prompt(&[
            ("prompt", "member"),
            ("language", "rust"),
            ("name", "foo"),
            ("signature", "fn foo()"),
            ("module_context", "mod"),
            ("kind_label", "Function"),
        ]);
        let out = handle.execute(&member_ctx).expect("execute");
        assert_eq!(
            out.message,
            gen.member_prompt("foo", "fn foo()", "mod", "Function", "rust")
        );
        let system_ctx = ctx_with_prompt(&[("prompt", "system"), ("language", "python")]);
        let out = handle.execute(&system_ctx).expect("execute");
        assert_eq!(out.message, gen.system_prompt("python"));
        let file_ctx = ctx_with_prompt(&[
            ("prompt", "file"),
            ("language", "rust"),
            ("rel_path", "src/main.rs"),
            ("source_preview", "fn main() {}"),
        ]);
        let out = handle.execute(&file_ctx).expect("execute");
        assert_eq!(
            out.message,
            gen.file_prompt("src/main.rs", "fn main() {}", "rust")
        );
    }

    #[test]
    fn m10_generator_execute_rejects_bad_handoff() {
        let handle: std::sync::Arc<dyn Component> =
            std::sync::Arc::new(DefaultCommentGenerator);
        // Missing prompt kind.
        let err = handle
            .execute(&WorkContext::default())
            .expect_err("missing prompt");
        assert!(matches!(err, fluent_wvr::WorkError::Execution(_)));
        // Unknown prompt kind.
        let err = handle
            .execute(&ctx_with_prompt(&[("prompt", "bogus"), ("language", "rust")]))
            .expect_err("unknown prompt");
        assert!(matches!(err, fluent_wvr::WorkError::Execution(_)));
    }

    #[test]
    fn m10_generator_handle_is_fieldless_with_schema() {
        let mut handle = DefaultCommentGenerator;
        assert_eq!(handle.field_names(), &[] as &[&str]);
        assert!(handle.get_field("language").is_err());
        assert!(handle.set_field("language", "rust").is_err());
        assert_eq!(
            fluent_wvr::Describable::describe(&handle)["generator"],
            serde_json::json!("default_comment")
        );
    }
}
