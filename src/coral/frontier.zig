/// frontier.zig — M6: L5 Frontier Loop Context Minimization & Validation
///
/// Provides context minimization before frontier LLM calls, prompt assembly,
/// and a validation gate for LLM-generated solutions.
///
/// Key exports:
///   MinimizedContext  — compact representation of query + nodes
///   minimizeContext() — LOD-based context trimming to token budget (+ PII anonymization)
///   buildPrompt()     — assemble the LLM prompt from minimal context
///   validateSolution()— check solution JSON is well-formed (perimeter check)
///   indexSolutionWithTool() — index LLM response + attempt WASM compilation
const std = @import("std");
const coral_db = @import("coral_db");
const ContextNode = coral_db.ContextNode;
const ContextPacker = coral_db.ContextPacker;
const schema = coral_db.schema;
const anonymize_mod = @import("anonymize.zig");
const tool_compiler = @import("frontier_tool_compiler.zig");
const wasm_mod = @import("wasm");

// ---------------------------------------------------------------------------
// MinimizedContext
// ---------------------------------------------------------------------------

/// A compact context bundle ready for frontier LLM prompting.
/// Both `query` and `packed_text` are allocator-owned (PII-anonymized copies).
pub const MinimizedContext = struct {
    /// Anonymized, allocator-owned copy of the original query.
    query: []const u8,
    /// Packed LOD text (allocator-owned, PII-anonymized).
    packed_text: []const u8,
    /// Approximate token count (chars / 4).
    token_count: usize,
    /// Allocator used for `query` and `packed_text`.
    allocator: std.mem.Allocator,

    pub fn deinit(self: *MinimizedContext) void {
        self.allocator.free(self.query);
        self.allocator.free(self.packed_text);
    }
};

/// Minimize context before a frontier LLM call.
///
/// Uses ContextPacker to select LOD levels weighted by BFS graph distance,
/// then trims to `token_budget` tokens (rough estimate: chars / 4).
/// Both query and packed context are PII-anonymized before returning.
pub fn minimizeContext(
    allocator: std.mem.Allocator,
    library: *coral_db.Library,
    query: []const u8,
    focal_id: i64,
    token_budget: usize,
) !MinimizedContext {
    const all_patterns = &[_]anonymize_mod.AnonymizationPattern{
        .email, .phone_us, .credit_card,
    };

    var packer = library.createContextPacker(token_budget);
    const raw_packed = try packer.pack(focal_id);
    defer allocator.free(raw_packed);

    const packed_text = try anonymize_mod.anonymizeContext(allocator, raw_packed, all_patterns);
    errdefer allocator.free(packed_text);

    const anon_query = try anonymize_mod.anonymizeContext(allocator, query, all_patterns);
    errdefer allocator.free(anon_query);

    return MinimizedContext{
        .query = anon_query,
        .packed_text = packed_text,
        .token_count = packed_text.len / 4,
        .allocator = allocator,
    };
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Build a frontier LLM prompt from a MinimizedContext.
/// Returns an allocator-owned string; caller must free.
///
/// Prompt structure:
/// ```
/// You are a knowledge assistant. Use the provided context to answer the query.
/// Context:
/// <packed_text>
/// Query: <query>
/// Answer:
/// ```
pub fn buildPrompt(
    allocator: std.mem.Allocator,
    ctx: MinimizedContext,
) ![]const u8 {
    return std.fmt.allocPrint(
        allocator,
        "You are a knowledge assistant. Use the provided context to answer the query.\n" ++
            "Context:\n{s}\nQuery: {s}\nAnswer:",
        .{ ctx.packed_text, ctx.query },
    );
}

// ---------------------------------------------------------------------------
// Validation gate
// ---------------------------------------------------------------------------

/// Result of validating an LLM solution.
pub const ValidationResult = struct {
    valid: bool,
    reason: []const u8 = "",
};

/// Validate a solution string at the perimeter (LLM output boundary).
///
/// Checks:
/// 1. Non-empty
/// 2. Not a "sorry, I can't" refusal
/// 3. Not a raw think-block remnant
pub fn validateSolution(solution: []const u8) ValidationResult {
    const trimmed = std.mem.trim(u8, solution, " \t\r\n");
    if (trimmed.len == 0) {
        return .{ .valid = false, .reason = "empty solution" };
    }
    // Detect common refusals.
    const refusals = [_][]const u8{
        "I cannot",     "I can't",       "I am unable", "I'm unable",
        "I don't have", "I do not have",
    };
    for (refusals) |r| {
        if (std.mem.indexOf(u8, trimmed, r) != null) {
            return .{ .valid = false, .reason = "LLM refusal detected" };
        }
    }
    // Detect unconsumed think blocks.
    if (std.mem.indexOf(u8, trimmed, "<think>") != null) {
        return .{ .valid = false, .reason = "unconsumed think block" };
    }
    return .{ .valid = true };
}

// ---------------------------------------------------------------------------
// Tests — M6.1
// ---------------------------------------------------------------------------

const testing = std.testing;

test "validateSolution: empty returns invalid" {
    const r = validateSolution("");
    try testing.expect(!r.valid);
}

test "validateSolution: refusal returns invalid" {
    const r = validateSolution("I cannot answer that question.");
    try testing.expect(!r.valid);
}

test "validateSolution: think block returns invalid" {
    const r = validateSolution("<think>some reasoning</think>");
    try testing.expect(!r.valid);
}

test "validateSolution: valid prose returns valid" {
    const r = validateSolution("The capital of France is Paris.");
    try testing.expect(r.valid);
}

test "buildPrompt: contains query and context" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const ctx = MinimizedContext{
        .query = try a.dupe(u8, "Who is Ada Lovelace?"),
        .packed_text = try a.dupe(u8, "Ada Lovelace was a mathematician."),
        .token_count = 8,
        .allocator = a,
    };
    const prompt = try buildPrompt(a, ctx);
    try testing.expect(std.mem.indexOf(u8, prompt, "Ada Lovelace?") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "mathematician") != null);
}

// ---------------------------------------------------------------------------
// Solution indexing — R4
// ---------------------------------------------------------------------------
//
// After a frontier LLM response passes validation, index it as a ContextNode
// so that future semantically-similar queries resolve via L4 KNN (< 200ms)
// rather than triggering another frontier call.
//
// The solution node uses:
//   lod[0] = full LLM response (or first 800 chars)
//   lod[1] = first 800 chars  (generated by LOD truncation)
//   lod[2] = first 240 chars
//   lod[3] = first 80 chars
//   lod[4] = query text (name field for L3 graph lookup)
//
// Node id is derived from a hash of the query so re-indexing is idempotent.

/// Index a validated LLM solution as a ContextNode for future L4 KNN retrieval.
///
/// Safe to call multiple times with the same query — uses a hash-derived id so
/// the INSERT OR REPLACE is a no-op on subsequent calls.
pub fn indexSolution(
    allocator: std.mem.Allocator,
    library: *coral_db.Library,
    query: []const u8,
    response: []const u8,
) !void {
    if (query.len == 0 or response.len == 0) return;

    // Derive a stable id from the query hash (first 8 bytes of SHA-256).
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update(query);
    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    var id_bytes: [8]u8 = undefined;
    @memcpy(&id_bytes, digest[0..8]);
    const node_id: i64 = @bitCast(id_bytes);

    // Build the full text (cap at 4096 chars to stay within LOD budget).
    const full_text = if (response.len > 4096) response[0..4096] else response;

    // Generate LOD levels 1-3 via deterministic truncation.
    var lod = try coral_db.generateLodSlices(allocator, full_text);
    defer {
        // Slots 1-3 are allocator-owned; slot 4 duped below.
        for (1..4) |i| if (lod[i].len > 0) allocator.free(lod[i]);
        if (lod[4].len > 0) allocator.free(lod[4]);
    }
    // lod[0] points into `full_text` (not owned); lod[4] = query (owned copy).
    lod[0] = full_text;
    lod[4] = try allocator.dupe(u8, if (query.len > 80) query[0..80] else query);

    // Build the ContextNode using the schema-driver SharedString for lod[0].
    var node = try coral_db.ContextNode.init(node_id, lod[4], full_text, allocator);
    defer node.free(allocator);

    // Overwrite LOD slots 1-3 with our generated slices (transfer ownership).
    for (1..4) |i| {
        if (lod[i].len > 0) {
            node.setLod(@intCast(i), lod[i]);
            node.lod_owned |= @as(u8, 1) << @intCast(i);
            lod[i] = ""; // transferred — don't double-free in defer
        }
    }

    library.insertNode(node) catch |err| {
        if (err == error.AlreadyExists) return; // idempotent
        return err;
    };
}

test "indexSolution: smoke test with in-memory library" {
    const Library = coral_db.Library;
    var lib = try Library.init(testing.allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    // Should not error; the node should be inserted.
    try indexSolution(testing.allocator, lib, "What is the capital of France?", "The capital of France is Paris, a city on the Seine.");
}

test "indexSolution: idempotent on repeated call" {
    const Library = coral_db.Library;
    var lib = try Library.init(testing.allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    const q = "Repeat me";
    const r = "Same response every time.";
    try indexSolution(testing.allocator, lib, q, r);
    try indexSolution(testing.allocator, lib, q, r); // must not error
}

// ---------------------------------------------------------------------------
// WASM tool indexing — M2.4
// ---------------------------------------------------------------------------
//
// After a frontier LLM response passes validation, attempt to extract source
// code, compile it to WASM, verify execution, and cache the compiled tool.
// Falls back to plain ContextNode indexing if compilation is unavailable or
// fails (e.g., no Zig compiler at runtime, AssemblyScript not installed).
//
// Pass a non-null `generator` to enable compilation. Pass null on edge
// deployments where no compiler is available; the source is stored as a
// ContextNode for deferred compilation later.

/// Index a validated LLM solution, attempting WASM compilation first.
///
/// Workflow:
///   1. Extract first fenced code block from `response`.
///   2. If `generator` is provided and source is valid Zig or AS, compile to WASM.
///   3. Verify the compiled tool runs without traps.
///   4. Store as a WasmTool in the library's wasm_tools table.
///   5. Fall through to plain `indexSolution` regardless of compilation outcome.
pub fn indexSolutionWithTool(
    allocator: std.mem.Allocator,
    library: *coral_db.Library,
    query: []const u8,
    response: []const u8,
    generator: ?*wasm_mod.ToolGenerator,
) !void {
    // Try to extract and compile source if a generator is available.
    if (generator) |gen| {
        if (try tool_compiler.extractFirstCodeBlock(allocator, response)) |block_val| {
            var block = block_val;
            defer block.deinit();

            if (tool_compiler.validateSource(block.source)) {
                const lang: ?wasm_mod.WasmLanguage = if (std.mem.eql(u8, block.language, "zig"))
                    .zig
                else if (std.mem.eql(u8, block.language, "typescript") or
                    std.mem.eql(u8, block.language, "assemblyscript"))
                    .assemblyscript
                else
                    null;

                if (lang) |l| {
                    if (gen.generateFromLLM(block.source, l)) |wasm_bytes| {
                        defer allocator.free(wasm_bytes);

                        const verified = gen.verifyTool(wasm_bytes, query) catch false;
                        if (verified) {
                            // Base64-encode the WASM binary for SQLite TEXT storage.
                            const b64_len = std.base64.standard.Encoder.calcSize(wasm_bytes.len);
                            const b64 = try allocator.alloc(u8, b64_len);
                            defer allocator.free(b64);
                            _ = std.base64.standard.Encoder.encode(b64, wasm_bytes);

                            // Derive a stable id from the query hash.
                            var hasher = std.crypto.hash.sha2.Sha256.init(.{});
                            hasher.update(query);
                            var digest: [32]u8 = undefined;
                            hasher.final(&digest);
                            var id_bytes: [8]u8 = undefined;
                            @memcpy(&id_bytes, digest[0..8]);
                            const tool_id: i64 = @bitCast(id_bytes);

                            try library.insertWasmTool(.{
                                .id = tool_id,
                                .target_id = 0,
                                .wasm_b64 = b64,
                                .schema_hash = "",
                                .test_passed = true,
                                .created_at = @floatFromInt(std.time.timestamp()),
                            });
                        }
                    } else |_| {} // compilation failure — fall through to text indexing
                }
            }
        }
    }

    // Always index as a ContextNode for L4 KNN retrieval regardless of WASM outcome.
    try indexSolution(allocator, library, query, response);
}

test "indexSolutionWithTool: null generator falls back to ContextNode indexing" {
    const Library = coral_db.Library;
    var lib = try Library.init(testing.allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    try indexSolutionWithTool(
        testing.allocator,
        lib,
        "What is 2 + 2?",
        "The answer is 4.",
        null,
    );
    // Verify node was indexed.
    try testing.expect(try lib.countNodes() >= 1);
}

test "indexSolutionWithTool: no code block still indexes node" {
    const Library = coral_db.Library;
    var lib = try Library.init(testing.allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    try indexSolutionWithTool(
        testing.allocator,
        lib,
        "Explain recursion",
        "Recursion is when a function calls itself.",
        null,
    );
    try testing.expect(try lib.countNodes() >= 1);
}
