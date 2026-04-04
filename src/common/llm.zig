//! common — Shared utilities and LLM client for guidance, vector, and coral.
//!
//! This file is the module root for the `common` build module.
//! It re-exports from sub-modules so consumers can write
//! `@import("common").LlmClient` etc. without knowing the source file.
//!
//! Sub-modules:
//!   src/llm/root.zig       — LlmClient, LlmConfig, LlmError (pure HTTP, no str deps)
//!   src/common/embeddings  — EmbeddingProvider, Ollama/OpenAI clients, factory
//!   src/common/args        — CommonArgs, parseCommonArgs
//!   src/common/io          — WriterState, ReaderState, path helpers
//!   src/common/source      — NodeType, extractExcerpt, extractSimpleExcerpt
//!   src/common/hash        — sha256Hex, contentHashWithModel, blake3, etc.
//!   src/common/json        — jsonStringifyAlloc, jsonWriteEscaped, parseJsonFile
//!   src/common/str         — containsIgnoreCase, looksLikeIdentifier, etc.
//!   src/common/url         — isLocalHost, validateHttpsOrLocalHttp

const std = @import("std");
const args = @import("args.zig");
const io = @import("io.zig");
const source = @import("source.zig");
const hash_mod = @import("hash.zig");
const json_mod = @import("json.zig");
const str_mod = @import("str.zig");
const url_mod = @import("url.zig");
const embed_mod = @import("embeddings.zig");

// ---------------------------------------------------------------------------
// Sub-module namespace exports — structured access for Coral and future tools
// ---------------------------------------------------------------------------
/// Field-level reflection: ConstraintVTable, Accessor, Editable(T), DynamicEditable.
pub const reflection = @import("reflection");
/// String interning with arena storage + bitset ConstraintVTable bridge.
pub const interner = @import("interner.zig");
/// Target DAG registry: TargetRegistry, TargetBuilder (fluent DSL).
pub const registry = @import("registry.zig");
/// Target/TargetType/ExecutorKind value types shared across build & coral.
pub const target = @import("target.zig");
/// Hash utilities: sha256Hex, contentHashWithModel, blake3Hash, hashString.
pub const hash = @import("hash.zig");
/// BuildContext for DAG execution.
pub const context = @import("context.zig");
/// Interactive REPL for coral.
pub const repl = @import("repl.zig");
/// JSON target-file parser.
pub const json_parser = @import("json_parser.zig");

// ── LLM inference (src/llm/) ─────────────────────────────────────
const llm = @import("llm");

pub const LlmError = llm.LlmError;
pub const LlmConfig = llm.LlmConfig;
pub const LlmClient = llm.LlmClient;

// ── LLM response post-processing (depends on str_mod, lives here) ─
// These validators operate on LLM output text and use string utilities
// that are part of this module — keeping them here avoids circular deps.

/// Removes tags from a Zig string slice, returning a cleaned version.
fn stripTagBlock(text: []const u8, open: []const u8, close: []const u8) []const u8 {
    const tag_start = std.mem.indexOf(u8, text, open) orelse return text;
    if (std.mem.indexOfPos(u8, text, tag_start + open.len, close)) |close_start| {
        const after = close_start + close.len;
        if (after >= text.len) return "";
        var s = after;
        while (s < text.len and (text[s] == ' ' or text[s] == '\n')) s += 1;
        return text[s..];
    }
    return std.mem.trim(u8, text[0..tag_start], " \t\r\n");
}

/// Removes unwanted blocks from the input text slice.
pub fn stripThinkBlock(text: []const u8) []const u8 {
    if (std.mem.indexOf(u8, text, "<think>") != null)
        return stripTagBlock(text, "<think>", "</think>");
    if (std.mem.indexOf(u8, text, "[THINK]") != null)
        return stripTagBlock(text, "[THINK]", "[/THINK]");
    return text;
}

/// Removes leading zeros from a Zig string slice, returning a trimmed version.
pub fn stripPreamble(allocator: std.mem.Allocator, text: []const u8) ![]const u8 {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return allocator.dupe(u8, trimmed);

    const nl_pos = std.mem.indexOfScalar(u8, trimmed, '\n') orelse trimmed.len;
    const first_line = trimmed[0..nl_pos];

    const preambles = [_][]const u8{
        "let's ", "let me ", "we need to ",    "here's ",    "here is ",
        "i'll ",  "i will ", "the answer is ", "to answer ", "okay, ",
        "ok, ",   "sure, ",  "alright, ",
    };

    const first_lower = try std.ascii.allocLowerString(allocator, first_line);
    defer allocator.free(first_lower);

    for (preambles) |preamble| {
        if (std.mem.startsWith(u8, first_lower, preamble)) {
            if (nl_pos >= trimmed.len) return allocator.dupe(u8, "");
            const rest = std.mem.trim(u8, trimmed[nl_pos + 1 ..], " \t\r\n");
            return allocator.dupe(u8, rest);
        }
    }

    return allocator.dupe(u8, trimmed);
}

/// Patterns that, when found anywhere in an LLM response, indicate it is a
/// preamble / meta-commentary rather than a usable doc comment.
/// Adding a new pattern is a single-line change here; no if-chain to edit.
const llm_preamble_patterns = [_][]const u8{
    "here's a",    "here is a",   "i'll ",        "to summarize",
    "okay,",       "ok,",         "we need ",     "let's think",
    "let's craft", "let's count", "let me think", "i need to ",
};

/// Checks if the provided text slice meets Zig's format requirements and returns true for malformed inputs.
pub fn isMalformedResponse(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;
    if (llmHasDanglingEnd(trimmed)) return true;

    const rtrimmed = std.mem.trimRight(u8, trimmed, " \t");
    if (rtrimmed.len > 0 and rtrimmed[rtrimmed.len - 1] == '?') return true;

    if (llmIsGenericSelfRef(trimmed)) return true;
    if (llmIsOverlyGeneric(trimmed)) return true;

    inline for (llm_preamble_patterns) |pattern| {
        if (str_mod.containsIgnoreCase(trimmed, pattern)) return true;
    }
    return false;
}

/// Checks if a Zig string ends with a null byte, returning true if so.
pub fn llmHasDanglingEnd(body: []const u8) bool {
    const trimmed = std.mem.trimRight(u8, body, " \t.?");
    if (trimmed.len == 0) return false;
    var i: usize = trimmed.len;
    while (i > 0 and trimmed[i - 1] != ' ') i -= 1;
    const last_word = trimmed[i..];
    const danglers = [_][]const u8{ "of", "in", "for", "from", "with", "to", "a", "an", "the" };
    for (danglers) |d| {
        if (std.ascii.eqlIgnoreCase(last_word, d)) return true;
    }
    return false;
}

/// Checks if a Zig body slice represents a valid self-referential structure, returning true if it matches the expected pattern.
pub fn llmIsGenericSelfRef(body: []const u8) bool {
    const patterns = [_][]const u8{
        "this function", "this method", "this class",
        "this struct",   "this type",   "this module",
    };
    const trimmed = std.mem.trim(u8, body, " \t\r\n.");
    for (patterns) |p| {
        if (std.ascii.eqlIgnoreCase(trimmed, p)) return true;
    }
    return false;
}

/// Checks if the provided body is overly generic by evaluating its structure and returns true if it matches.
pub fn llmIsOverlyGeneric(body: []const u8) bool {
    const generics = [_][]const u8{
        "function", "method",   "helper",  "util",           "utility",
        "handler",  "callback", "wrapper", "implementation",
    };
    const trimmed = std.mem.trim(u8, body, " \t\r\n.");
    if (trimmed.len > 20) return false;
    if (std.mem.indexOfScalar(u8, trimmed, ' ') != null) return false;
    for (generics) |g| {
        if (std.ascii.eqlIgnoreCase(trimmed, g)) return true;
    }
    return false;
}

/// Extracts the comment tag from a Zig string slice, returning its slice.
pub fn extractCommentTag(text: []const u8) ?[]const u8 {
    const open = "<comment>";
    const close = "</comment>";
    const start = std.mem.indexOf(u8, text, open) orelse return null;
    const content_start = start + open.len;
    const end = std.mem.indexOfPos(u8, text, content_start, close) orelse return null;
    const content = std.mem.trim(u8, text[content_start..end], " \t\r\n");
    if (content.len == 0) return null;
    return content;
}

// ── Embedding providers (src/common/embeddings.zig) ──────────────
pub const EmbeddingProvider = embed_mod.EmbeddingProvider;
pub const NoopEmbedding = embed_mod.NoopEmbedding;
pub const OllamaEmbedding = embed_mod.OllamaEmbedding;
pub const OpenAiEmbedding = embed_mod.OpenAiEmbedding;
pub const createEmbeddingProvider = embed_mod.createEmbeddingProvider;
pub const parseOllamaResponse = embed_mod.parseOllamaResponse;
pub const parseOpenAiResponse = embed_mod.parseOpenAiResponse;

// ── CLI args ─────────────────────────────────────────────────────
pub const CommonArgs = args.CommonArgs;
pub const parseCommonArgs = args.parseCommonArgs;

// ── I/O helpers ──────────────────────────────────────────────────
pub const WriterState = io.WriterState;
pub const ReaderState = io.ReaderState;
pub const makePathAbsolute = io.makePathAbsolute;
pub const readFileAlloc = io.readFileAlloc;
pub const readFileAllocErr = io.readFileAllocErr;
pub const resolvePath = io.resolvePath;
pub const stripPathPrefix = io.stripPathPrefix;
pub const DEFAULT_MAX_FILE_SIZE = io.DEFAULT_MAX_FILE_SIZE;

// ── Source excerpt extraction ─────────────────────────────────────
pub const NodeType = source.NodeType;
pub const DEFAULT_MAX_LINES = source.DEFAULT_MAX_LINES;
pub const extractExcerpt = source.extractExcerpt;
pub const extractSimpleExcerpt = source.extractSimpleExcerpt;

// ── Hash utilities ────────────────────────────────────────────────
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

// ── JSON utilities ────────────────────────────────────────────────
pub const jsonStringifyAlloc = json_mod.jsonStringifyAlloc;
pub const jsonWriteEscaped = json_mod.writeEscaped;
pub const jsonAppendEscaped = json_mod.appendEscaped;
pub const parseJsonFile = json_mod.parseJsonFile;

// ── String utilities ──────────────────────────────────────────────
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

// ── URL utilities ─────────────────────────────────────────────────
pub const isLocalHost = url_mod.isLocalHost;
pub const validateHttpsOrLocalHttp = url_mod.validateHttpsOrLocalHttp;

// ── Tests for response post-processing ───────────────────────────

test "stripThinkBlock removes think tags" {
    const text1 = "<think>Some thinking</think>\nActual response";
    try std.testing.expectEqualStrings("Actual response", stripThinkBlock(text1));

    const text2 = "No think tags here";
    try std.testing.expectEqualStrings(text2, stripThinkBlock(text2));

    const text3 = "<think>Only think</think>";
    try std.testing.expectEqualStrings("", stripThinkBlock(text3));
}

test "stripThinkBlock handles unclosed think tag" {
    try std.testing.expectEqualStrings("", stripThinkBlock("<think>Reasoning that never ends"));
}

test "stripThinkBlock handles [THINK] tags" {
    try std.testing.expectEqualStrings("Actual answer", stripThinkBlock("[THINK]reasoning here[/THINK]\nActual answer"));
}

test "stripPreamble removes leading preamble line" {
    const allocator = std.testing.allocator;

    const r1 = try stripPreamble(allocator, "Let's analyze this function.\nParses JSON from a byte slice.");
    defer allocator.free(r1);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", r1);

    const r2 = try stripPreamble(allocator, "Here's the description:\nBuilds the dep graph.");
    defer allocator.free(r2);
    try std.testing.expectEqualStrings("Builds the dep graph.", r2);

    const r3 = try stripPreamble(allocator, "Parses JSON tokens efficiently.");
    defer allocator.free(r3);
    try std.testing.expectEqualStrings("Parses JSON tokens efficiently.", r3);
}

test "isMalformedResponse: empty is malformed" {
    try std.testing.expect(isMalformedResponse(""));
    try std.testing.expect(isMalformedResponse("   "));
}

test "isMalformedResponse: dangling preposition" {
    try std.testing.expect(isMalformedResponse("Parses the input from"));
    try std.testing.expect(isMalformedResponse("Returns the value of"));
}

test "isMalformedResponse: ends with question mark" {
    try std.testing.expect(isMalformedResponse("Does something?"));
}

test "isMalformedResponse: generic self-reference" {
    try std.testing.expect(isMalformedResponse("this function"));
    try std.testing.expect(isMalformedResponse("This Method"));
    try std.testing.expect(!isMalformedResponse("this function parses JSON efficiently"));
}

test "isMalformedResponse: overly generic single word" {
    try std.testing.expect(isMalformedResponse("helper"));
    try std.testing.expect(isMalformedResponse("wrapper"));
    try std.testing.expect(!isMalformedResponse("Parses"));
}

test "isMalformedResponse: LLM preamble phrases" {
    try std.testing.expect(isMalformedResponse("Here's a description of the function"));
    try std.testing.expect(isMalformedResponse("I'll explain what this does"));
    try std.testing.expect(isMalformedResponse("To summarize, this parses JSON"));
    try std.testing.expect(isMalformedResponse("Okay, so this function"));
}

test "isMalformedResponse: valid responses are NOT malformed" {
    try std.testing.expect(!isMalformedResponse("Parses Zig AST tokens and extracts public members."));
    try std.testing.expect(!isMalformedResponse("Ring buffer for streaming price data."));
    try std.testing.expect(!isMalformedResponse("Builds incremental dependency graph from @import declarations."));
}

test "isMalformedResponse: reasoning-model chain-of-thought phrases" {
    try std.testing.expect(isMalformedResponse("we need to write a comment for this type"));
    try std.testing.expect(isMalformedResponse("let's think about what this does"));
    try std.testing.expect(isMalformedResponse("let me think about the ownership model"));
    try std.testing.expect(isMalformedResponse("i need to mention that it owns the allocator"));
}

test "extractCommentTag: returns tag content" {
    const text = "some reasoning\n<comment>Parses JSON from a byte slice.</comment>\nmore text";
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", result.?);
}

test "extractCommentTag: returns null when no tag present" {
    try std.testing.expect(extractCommentTag("we need to write a comment") == null);
    try std.testing.expect(extractCommentTag("") == null);
}

test "extractCommentTag: returns null for empty tag" {
    try std.testing.expect(extractCommentTag("<comment>   </comment>") == null);
}

test "extractCommentTag: chain-of-thought before tag is ignored" {
    const text =
        \\We need to write a comment for DepsGenerator.
        \\<comment>[skills: zig-current] Walks src/ and resolves @import paths to build a dep graph.</comment>
    ;
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("[skills: zig-current] Walks src/ and resolves @import paths to build a dep graph.", result.?);
}
