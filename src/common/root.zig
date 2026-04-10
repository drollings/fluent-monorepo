//! common — Module umbrella root.
//!
//! This is the root of the `common` build module.  It re-exports all public
//! symbols from internal files for convenient access.
//!
//! LLM types (LlmClient, LlmConfig, LlmError, etc.) have been moved to
//! the `llm` module.  Import via `@import("llm")` instead.
//!
//! Additional structured sub-module namespaces:
//!   common.embeddings — EmbeddingProvider and backends

const std = @import("std");

// ── Named module imports (from build.zig) ───────────────────────────────────────
const embed_mod = @import("embeddings.zig");
const args_mod = @import("args.zig");
const io_mod = @import("io.zig");
const source_mod = @import("source.zig");
const hash_mod = @import("hash.zig");
const json_mod = @import("json.zig");
const string_mod = @import("string.zig");
const url_mod = @import("url.zig");
const builder_error_mod = @import("builder_error.zig");

// ── Named sub-module namespaces ───────────────────────────────────────────────
/// I/O helpers: WriterState, ReaderState, readFileAlloc, etc.
pub const io = io_mod;
/// Embedding providers (Noop, Ollama, OpenAI) and factory.
pub const embeddings = embed_mod;
/// Field-level reflection: ConstraintVTable, Accessor, Editable(T), DynamicEditable.
pub const reflection = @import("reflection");
/// String interning with arena storage + bitset ConstraintVTable bridge.
pub const interner = @import("interner.zig");
/// Hash utilities: sha256Hex, contentHashWithModel, blake3Hash, hashString.
pub const hash = hash_mod;
/// Builder error types for fluent builder chains (Phase, BuilderError, etc.).
pub const builder_error = builder_error_mod;

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
pub const readFileOpt = io_mod.readFileOpt;
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
/// Imported from the external zigsharedstring package.
pub const SharedString = @import("zigsharedstring").SharedString;

// ── Reference counting (zigrc) ────────────────────────────────────────────────
/// Single-threaded reference counting: Rc(T), SecureRc(T).
pub const rc = @import("zigrc");
/// Convenience re-exports from zigrc for direct access.
pub const Rc = rc.Rc;
pub const SecureRc = rc.SecureRc;
pub const RcAligned = rc.RcAligned;
pub const RcUnmanaged = rc.RcUnmanaged;
pub const RcAlignedUnmanaged = rc.RcAlignedUnmanaged;
pub const Arc = rc.Arc;
pub const SecureArc = rc.SecureArc;
pub const ArcAligned = rc.ArcAligned;
pub const ArcUnmanaged = rc.ArcUnmanaged;
pub const ArcAlignedUnmanaged = rc.ArcAlignedUnmanaged;

// ── Content node ──────────────────────────────────────────────────────────────
/// LOD text pyramid backed by a ref-counted SharedString.  Common primitive
/// for ContextNode and any subsystem needing multi-level text representation.
pub const ContentNode = @import("content_node.zig").ContentNode;

// ── LOD count ─────────────────────────────────────────────────────────────────
/// Number of LOD text slots per ContentNode (= 6).
pub const LOD_COUNT = @import("types.zig").LOD_COUNT;

// ── String utilities ──────────────────────────────────────────────────────────
pub const looksLikeIdentifier = string_mod.looksLikeIdentifier;
pub const isTestPath = string_mod.isTestPath;
pub const skillNameFromRef = string_mod.skillNameFromRef;
pub const containsIgnoreCase = string_mod.containsIgnoreCase;
pub const containsWord = string_mod.containsWord;
pub const containsAny = string_mod.containsAny;
pub const containsAnyWord = string_mod.containsAnyWord;
pub const hasExtension = string_mod.hasExtension;
pub const isPathToken = string_mod.isPathToken;
pub const langFromPath = string_mod.langFromPath;
pub const dupeStrings = string_mod.dupeStrings;
pub const slugify = string_mod.slugify;
pub const stripNlPrefix = string_mod.stripNlPrefix;
pub const STOP_WORDS = string_mod.STOP_WORDS;
pub const stripBoilerplate = string_mod.stripBoilerplate;
pub const isNoisyComment = string_mod.isNoisyComment;
pub const truncateAtSentence = string_mod.truncateAtSentence;
pub const firstCommentLine = string_mod.firstCommentLine;

// ── Reference-counted VTable handles (M7) ─────────────────────────────────────
pub const refcount = @import("refcount.zig");
pub const RefCounted = refcount.RefCounted;

// ── Conditional wrappers and call helpers (M9) ────────────────────────────────
pub const wrapper = @import("wrapper.zig");
pub const wrapIf = wrapper.wrapIf;
pub const retryCall = wrapper.retryCall;
pub const WrapperKind = wrapper.WrapperKind;
pub const Pipeline = wrapper.Pipeline;

// ── Structured error context ─────────────────────────────────────────────────────
pub const error_context = @import("error_context.zig");
pub const ErrorContext = error_context.ErrorContext;
pub const ArenaErrorContext = error_context.ArenaErrorContext;

// ── Structured logging context (M8) ──────────────────────────────────────────
pub const logging = @import("logging.zig");
pub const LogContext = logging.LogContext;
pub const LogScope = logging.Scope;
pub const callLogged = logging.callLogged;

// ── URL utilities ─────────────────────────────────────────────────────────────
pub const isLocalHost = url_mod.isLocalHost;
pub const isPrivateIp = url_mod.isPrivateIp;
pub const validateHttpsOrLocalHttp = url_mod.validateHttpsOrLocalHttp;

// ── Token budget estimation ───────────────────────────────────────────────────
/// Moved to the llm module. Access via @import("llm").token_budget.

// ── BitSet DRIFT ──────────────────────────────────────────────────────────────
/// Deterministic follow-up query generation (shared by guidance and coral).
pub const drift = @import("drift.zig");

// ── Constants ───────────────────────────────────────────────────────────
/// Shared size/count caps for file reads, MCP requests, KNN scans, etc.
pub const constants = @import("constants.zig");

// ── Shell command parser ──────────────────────────────────────────────────────
/// Safe command-string tokenizer (no shell intermediary).
pub const shell_parser = @import("shell_parser.zig");

// ── Shell command execution ─────────────────────────────────────────────────────
pub const shell = @import("shell.zig");

// ── Typed ID handles ──────────────────────────────────────────────────────────
pub const types = @import("types.zig");

// ── Pattern detection ──────────────────────────────────────────────────────────
pub const pattern = @import("pattern.zig");

// ── Latency metrics (LatencyHistogram, BUCKET_MS, BUCKET_COUNT) ───────────────
pub const metrics = @import("metrics.zig");

// ── Agent delegation (DelegationConfig, DelegationResult, Delegation) ─────────
pub const delegation = @import("delegation.zig");

// ── FNV-1a hash (fnv1a64) ─────────────────────────────────────────────────────
pub const fnv1a64 = hash_mod.fnv1a64;
