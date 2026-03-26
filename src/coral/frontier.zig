/// frontier.zig — M6: L5 Frontier Loop Context Minimization & Validation
///
/// Provides context minimization before frontier LLM calls, prompt assembly,
/// and a validation gate for LLM-generated solutions.
///
/// Key exports:
///   MinimizedContext  — compact representation of query + nodes
///   minimizeContext() — LOD-based context trimming to token budget
///   buildPrompt()     — assemble the LLM prompt from minimal context
///   validateSolution()— check solution JSON is well-formed (perimeter check)
const std = @import("std");
const coral_db = @import("coral_db");
const ContextNode = coral_db.ContextNode;
const ContextPacker = coral_db.ContextPacker;
const schema = coral_db.schema;

// ---------------------------------------------------------------------------
// MinimizedContext
// ---------------------------------------------------------------------------

/// A compact context bundle ready for frontier LLM prompting.
pub const MinimizedContext = struct {
    query: []const u8,
    /// Packed LOD text (allocator-owned).
    packed_text: []const u8,
    /// Approximate token count (chars / 4).
    token_count: usize,
    /// Allocator used for `packed_text`.
    allocator: std.mem.Allocator,

    pub fn deinit(self: *MinimizedContext) void {
        self.allocator.free(self.packed_text);
    }
};

/// Minimize context before a frontier LLM call.
///
/// Uses ContextPacker to select LOD levels weighted by BFS graph distance,
/// then trims to `token_budget` tokens (rough estimate: chars / 4).
pub fn minimizeContext(
    allocator: std.mem.Allocator,
    library: *coral_db.Library,
    query: []const u8,
    focal_id: i64,
    token_budget: usize,
) !MinimizedContext {
    var packer = library.createContextPacker(token_budget);
    const packed_text = try packer.pack(focal_id);
    const token_count = packed_text.len / 4;
    return MinimizedContext{
        .query = query,
        .packed_text = packed_text,
        .token_count = token_count,
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
        "I cannot", "I can't", "I am unable", "I'm unable",
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
        .query = "Who is Ada Lovelace?",
        .packed_text = "Ada Lovelace was a mathematician.",
        .token_count = 8,
        .allocator = a,
    };
    const prompt = try buildPrompt(a, ctx);
    try testing.expect(std.mem.indexOf(u8, prompt, "Ada Lovelace?") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "mathematician") != null);
}
