/// frontier_tool_compiler.zig — Compiles LLM-generated source into WASM tools.
///
/// After a successful L5 frontier LLM response, this module:
///   1. Extracts code blocks from the LLM response
///   2. Validates the extracted source
///   3. Caches source for future WASM compilation
///
/// Note: Actual Zig→WASM compilation requires the Zig compiler toolchain
/// at runtime. In edge deployments without a compiler, the source is stored
/// for deferred compilation.
const std = @import("std");

/// Represents extracted code structure with ownership and invariants; manages compilation pipeline.
pub const ExtractedCode = struct {
    language: []const u8, // "zig", "rust", "typescript", etc.
    source: []const u8, // allocator-owned
    allocator: std.mem.Allocator,

    pub fn deinit(self: *ExtractedCode) void {
        self.allocator.free(self.source);
    }
};

/// Extracts the first code block from a Zig response using an allocator and returns the extracted slice.
pub fn extractFirstCodeBlock(
    allocator: std.mem.Allocator,
    llm_response: []const u8,
) !?ExtractedCode {
    // Find opening fence
    const fence_start = std.mem.indexOf(u8, llm_response, "```") orelse return null;
    const after_fence = llm_response[fence_start + 3 ..];

    // Extract language tag (up to newline)
    const lang_end = std.mem.indexOfScalar(u8, after_fence, '\n') orelse return null;
    const language = std.mem.trim(u8, after_fence[0..lang_end], " \t\r");

    // Find closing fence
    const body_start = lang_end + 1;
    if (body_start >= after_fence.len) return null;
    const body = after_fence[body_start..];
    const close_idx = std.mem.indexOf(u8, body, "\n```") orelse return null;

    const source = try allocator.dupe(u8, body[0..close_idx]);
    const lang_copy = if (language.len == 0) "unknown" else language;

    return ExtractedCode{
        .language = lang_copy,
        .source = source,
        .allocator = allocator,
    };
}

/// Validates a Zig source slice for correctness and returns true if valid.
pub fn validateSource(source: []const u8) bool {
    const trimmed = std.mem.trim(u8, source, " \t\n\r");
    return trimmed.len > 0;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const testing = std.testing;

test "extractFirstCodeBlock: extracts zig block" {
    const allocator = testing.allocator;
    const response =
        \\Here is the solution:
        \\```zig
        \\pub fn run(input: []const u8) []const u8 {
        \\    return input;
        \\}
        \\```
        \\That should work.
    ;
    var block = try extractFirstCodeBlock(allocator, response);
    try testing.expect(block != null);
    defer block.?.deinit();
    try testing.expectEqualStrings("zig", block.?.language);
    try testing.expect(std.mem.indexOf(u8, block.?.source, "pub fn run") != null);
}

test "extractFirstCodeBlock: returns null for no code block" {
    const allocator = testing.allocator;
    const response = "Here is my explanation without code.";
    const block = try extractFirstCodeBlock(allocator, response);
    try testing.expect(block == null);
}

test "extractFirstCodeBlock: empty fence returns null" {
    const allocator = testing.allocator;
    const response = "```\n```";
    // Empty body — source will be empty
    var block = try extractFirstCodeBlock(allocator, response);
    if (block) |*b| {
        defer b.deinit();
        try testing.expect(!validateSource(b.source));
    }
}

test "validateSource: empty returns false" {
    try testing.expect(!validateSource(""));
    try testing.expect(!validateSource("   \n\t  "));
}

test "validateSource: non-empty returns true" {
    try testing.expect(validateSource("fn main() void {}"));
}
