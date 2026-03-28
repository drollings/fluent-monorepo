//! common — Module umbrella root.
//!
//! This is the root of the `common` build module.  It re-exports all public
//! symbols that were previously exported directly from llm.zig so that
//! all existing `@import("common").Foo` call sites continue to work unchanged.
//!
//! Additional structured sub-module namespaces (new in P2.1):
//!   common.llm        — LLM client and response post-processing
//!   common.embeddings — EmbeddingProvider and backends

const std = @import("std");
const llm_file = @import("llm.zig");
const llm_mod = @import("llm");
const embed_mod = @import("embeddings.zig");
const args_mod = @import("args.zig");
const io_mod = @import("io.zig");
const source_mod = @import("source.zig");
const hash_mod = @import("hash.zig");
const json_mod = @import("json.zig");
const str_mod = @import("str.zig");
const url_mod = @import("url.zig");

// ── Named sub-module namespaces ───────────────────────────────────────────────
/// LLM inference client and response post-processing.
pub const llm = llm_file;
/// Embedding providers (Noop, Ollama, OpenAI) and factory.
pub const embeddings = embed_mod;
/// Field-level reflection: ConstraintVTable, Accessor, Editable(T), DynamicEditable.
pub const reflection = @import("reflection");
/// String interning with arena storage + bitset ConstraintVTable bridge.
pub const interner = @import("interner.zig");
/// Target DAG registry: TargetRegistry, TargetBuilder (fluent DSL).
pub const registry = @import("registry.zig");
/// Target/TargetType/ExecutorKind value types shared across build & coral.
pub const target = @import("target.zig");
/// Hash utilities: sha256Hex, contentHashWithModel, blake3Hash, hashString.
pub const hash = hash_mod;
/// BuildContext for DAG execution.
pub const context = @import("context.zig");
/// Interactive REPL for coral.
pub const repl = @import("repl.zig");
/// JSON target-file parser.
pub const json_parser = @import("json_parser.zig");

// ── LLM types (flat re-exports for backward compatibility) ────────────────────
pub const LlmError = llm_mod.LlmError;
pub const LlmConfig = llm_mod.LlmConfig;
pub const LlmClient = llm_mod.LlmClient;

// ── LLM response post-processing (backward compat flat re-exports) ────────────
pub const stripThinkBlock = llm_file.stripThinkBlock;
pub const stripPreamble = llm_file.stripPreamble;
pub const isMalformedResponse = llm_file.isMalformedResponse;
pub const extractCommentTag = llm_file.extractCommentTag;

// ── Embedding providers (backward compat flat re-exports) ─────────────────────
pub const EmbeddingProvider = embed_mod.EmbeddingProvider;
pub const NoopEmbedding = embed_mod.NoopEmbedding;
pub const OllamaEmbedding = embed_mod.OllamaEmbedding;
pub const OpenAiEmbedding = embed_mod.OpenAiEmbedding;
pub const createEmbeddingProvider = embed_mod.createEmbeddingProvider;
pub const parseOllamaResponse = embed_mod.parseOllamaResponse;
pub const parseOpenAiResponse = embed_mod.parseOpenAiResponse;

// ── CLI args ──────────────────────────────────────────────────────────────────
pub const CommonArgs = args_mod.CommonArgs;
pub const parseCommonArgs = args_mod.parseCommonArgs;

// ── I/O helpers ───────────────────────────────────────────────────────────────
pub const WriterState = io_mod.WriterState;
pub const ReaderState = io_mod.ReaderState;
pub const makePathAbsolute = io_mod.makePathAbsolute;
pub const readFileAlloc = io_mod.readFileAlloc;
pub const readFileAllocErr = io_mod.readFileAllocErr;
pub const resolvePath = io_mod.resolvePath;
pub const stripPathPrefix = io_mod.stripPathPrefix;
pub const DEFAULT_MAX_FILE_SIZE = io_mod.DEFAULT_MAX_FILE_SIZE;

// ── Source excerpt extraction ─────────────────────────────────────────────────
pub const NodeType = source_mod.NodeType;
pub const DEFAULT_MAX_LINES = source_mod.DEFAULT_MAX_LINES;
pub const extractExcerpt = source_mod.extractExcerpt;
pub const extractSimpleExcerpt = source_mod.extractSimpleExcerpt;

// ── Hash utilities ────────────────────────────────────────────────────────────
pub const sha256Hex = hash_mod.sha256Hex;
pub const contentHashWithModel = hash_mod.contentHashWithModel;
pub const HashAlgorithm = hash_mod.HashAlgorithm;
pub const hashFile = hash_mod.hashFile;
pub const hashBatch = hash_mod.hashBatch;
pub const hashString = hash_mod.hashString;
pub const blake3Hash = hash_mod.blake3Hash;
pub const blake3Hex = hash_mod.blake3Hex;
pub const HashState = hash_mod.HashState;
pub const BatchHashResult = hash_mod.BatchHashResult;

// ── JSON utilities ────────────────────────────────────────────────────────────
pub const jsonStringifyAlloc = json_mod.jsonStringifyAlloc;
pub const jsonWriteEscaped = json_mod.writeEscaped;
pub const jsonAppendEscaped = json_mod.appendEscaped;
pub const parseJsonFile = json_mod.parseJsonFile;

// ── Shared string ─────────────────────────────────────────────────────────────
/// Reference-counted immutable string.  Use SharedString.Ref as the handle.
pub const SharedString = @import("shared_string.zig").SharedString;

// ── String utilities ──────────────────────────────────────────────────────────
pub const looksLikeIdentifier = str_mod.looksLikeIdentifier;
pub const isTestPath = str_mod.isTestPath;
pub const skillNameFromRef = str_mod.skillNameFromRef;
pub const containsIgnoreCase = str_mod.containsIgnoreCase;
pub const containsWord = str_mod.containsWord;
pub const containsAny = str_mod.containsAny;
pub const containsAnyWord = str_mod.containsAnyWord;
pub const hasExtension = str_mod.hasExtension;
pub const isPathToken = str_mod.isPathToken;
pub const langFromPath = str_mod.langFromPath;
pub const dupeStrings = str_mod.dupeStrings;
pub const stripNlPrefix = str_mod.stripNlPrefix;
pub const STOP_WORDS = str_mod.STOP_WORDS;
pub const stripBoilerplate = str_mod.stripBoilerplate;
pub const isNoisyComment = str_mod.isNoisyComment;

// ── Reference-counted VTable handles (M7) ─────────────────────────────────────
pub const refcount = @import("refcount.zig");
pub const RefCounted = refcount.RefCounted;

// ── Conditional wrappers and call helpers (M9) ────────────────────────────────
pub const wrapper = @import("wrapper.zig");
pub const wrapIf = wrapper.wrapIf;
pub const retryCall = wrapper.retryCall;
pub const WrapperKind = wrapper.WrapperKind;
pub const Pipeline = wrapper.Pipeline;

// ── Structured logging context (M8) ──────────────────────────────────────────
pub const logging = @import("logging.zig");
pub const LogContext = logging.LogContext;
pub const LogScope = logging.Scope;
pub const callLogged = logging.callLogged;

// ── URL utilities ─────────────────────────────────────────────────────────────
pub const isLocalHost = url_mod.isLocalHost;
pub const isPrivateIp = url_mod.isPrivateIp;
pub const validateHttpsOrLocalHttp = url_mod.validateHttpsOrLocalHttp;

// ── Resource limits ───────────────────────────────────────────────────────────
/// Shared size/count caps for file reads, MCP requests, KNN scans, etc.
pub const limits = @import("limits.zig");

// ── Shell command parser ──────────────────────────────────────────────────────
/// Safe command-string tokenizer (no shell intermediary).
pub const shell_parser = @import("shell_parser.zig");

// ── Typed ID handles ──────────────────────────────────────────────────────────
pub const types = @import("types.zig");
