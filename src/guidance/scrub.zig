//! scrub.zig — Synthetic comment detection and scrubbing.
//!
//! Ports the Python `is_synthetic_comment()` heuristics from
//! `bin/guidance-py:650-699` to Zig for use in the `guidance scrub` command.
//! A synthetic comment is a machine-generated placeholder or mangled LLM
//! output that provides no value to readers.

const std = @import("std");

/// Return true when `comment` is a machine-generated placeholder or mangled
/// LLM output.  Equivalent to Python's `is_synthetic_comment()`.
pub fn isSyntheticComment(comment: []const u8) bool {
    // Strip surrounding whitespace.
    var body = std.mem.trim(u8, comment, " \t\r\n");
    if (body.len == 0) return true;

    // Strip leading "[skill, skill] " skill-tag prefix, e.g. "[zig-current] ".
    if (body[0] == '[') {
        if (std.mem.indexOfScalar(u8, body, ']')) |end| {
            body = std.mem.trim(u8, body[end + 1 ..], " \t");
        }
    }
    if (body.len == 0) return true;

    // LLM thinking leakage (case-insensitive).
    if (containsIgnoreCase(body, "let me think")) return true;
    if (containsIgnoreCase(body, "let's think")) return true;
    if (containsIgnoreCase(body, "lets think")) return true;
    if (containsIgnoreCase(body, "i think")) return true;
    if (containsIgnoreCase(body, "maybe means")) return true;

    // Directive / prompt leakage.
    if (containsIgnoreCase(body, "should be concise")) return true;
    if (containsIgnoreCase(body, "so it's a")) return true;
    if (containsIgnoreCase(body, "so it is a")) return true;
    if (containsIgnoreCase(body, "so description:")) return true;

    // "We need to (guess|produce|output|describe)" — prompt bleeding.
    if (containsIgnoreCase(body, "we need to ")) {
        if (containsIgnoreCase(body, "guess") or containsIgnoreCase(body, "produce") or
            containsIgnoreCase(body, "output") or containsIgnoreCase(body, "describe"))
            return true;
    }

    // "max N chars for ..." / "(max N chars) of/for" — prompt template leakage.
    if (containsIgnoreCase(body, "max ") and containsIgnoreCase(body, " char") and
        containsIgnoreCase(body, " for"))
        return true;

    // Ends with "?" — synthetic uncertainty marker.
    const right = std.mem.trimRight(u8, body, " \t");
    if (right.len > 0 and right[right.len - 1] == '?') return true;

    // Ends with dangling preposition or article — incomplete sentence.
    if (endsWithIncomplete(right)) return true;

    // Opens with "(maybe|likely|possibly|probably|perhaps|unclear)" — hedged.
    if (body.len > 1 and body[0] == '(') {
        for ([_][]const u8{ "maybe", "likely", "possibly", "probably", "perhaps", "unclear" }) |h| {
            if (body.len > h.len + 1 and std.ascii.eqlIgnoreCase(body[1 .. 1 + h.len], h)) return true;
        }
    }

    // Starts with punctuation or digit and is very short (≤60 chars).
    if (body.len < 60 and body.len > 0) {
        const c = body[0];
        if (c == '.' or c == ',' or c == '(' or c == ')' or (c >= '0' and c <= '9')) return true;
    }

    // ": N struct" / ": N class" / ": N function" — auto-generated type ref noise.
    if (hasNumericTypeRef(body)) return true;

    return false;
}

/// Return true when `s` ends with an isolated preposition or article,
/// indicating an incomplete sentence.
fn endsWithIncomplete(s: []const u8) bool {
    const dangling = [_][]const u8{ " of", " in", " for", " from", " with", " to", " a", " an", " the" };
    for (dangling) |d| {
        if (s.len >= d.len and std.ascii.eqlIgnoreCase(s[s.len - d.len ..], d)) return true;
        // Also accept trailing period: e.g. " of."
        if (s.len > d.len and s[s.len - 1] == '.') {
            if (std.ascii.eqlIgnoreCase(s[s.len - d.len - 1 .. s.len - 1], d)) return true;
        }
    }
    return false;
}

/// Return true when `s` contains a pattern like ": N struct", ": N class",
/// or ": N function" — common in auto-generated Python type-reference strings.
fn hasNumericTypeRef(s: []const u8) bool {
    var i: usize = 0;
    while (i + 4 < s.len) : (i += 1) {
        if (s[i] != ':' or s[i + 1] != ' ') continue;
        var j = i + 2;
        if (j >= s.len or s[j] < '0' or s[j] > '9') continue;
        while (j < s.len and s[j] >= '0' and s[j] <= '9') : (j += 1) {}
        if (j >= s.len or s[j] != ' ') continue;
        const rest = s[j + 1 ..];
        if (std.mem.startsWith(u8, rest, "struct") or
            std.mem.startsWith(u8, rest, "class") or
            std.mem.startsWith(u8, rest, "function")) return true;
    }
    return false;
}

/// Case-insensitive substring search.
pub fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) return true;
    }
    return false;
}

/// Scrub synthetic comments from a parsed JSON `Value` tree in-place.
/// Modifies `comment` fields at module and member levels.
/// Returns true if any comments were blanked.
pub fn scrubJsonValue(value: *std.json.Value) bool {
    if (value.* != .object) return false;
    var changed = false;
    const obj = &value.object;

    // Module or member-level comment field.
    if (obj.getPtr("comment")) |cv| {
        if (cv.* == .string and cv.string.len > 0 and isSyntheticComment(cv.string)) {
            cv.* = .{ .string = "" };
            changed = true;
        }
    }

    // Recursively scrub nested members.
    if (obj.getPtr("members")) |mv| {
        if (mv.* == .array) {
            for (mv.array.items) |*member| {
                if (scrubJsonValue(member)) changed = true;
            }
        }
    }

    return changed;
}

// =============================================================================
// Tests
// =============================================================================

test "isSyntheticComment: empty and whitespace" {
    const t = std.testing;
    try t.expect(isSyntheticComment(""));
    try t.expect(isSyntheticComment("   \t\n"));
}

test "isSyntheticComment: bracket-only skill tag" {
    const t = std.testing;
    try t.expect(isSyntheticComment("[zig-current]"));
    try t.expect(isSyntheticComment("[skill1, skill2]   "));
}

test "isSyntheticComment: LLM thinking markers" {
    const t = std.testing;
    try t.expect(isSyntheticComment("Let me think about this."));
    try t.expect(isSyntheticComment("I think this handles events."));
    try t.expect(isSyntheticComment("Let's think step by step."));
    try t.expect(isSyntheticComment("maybe means something else"));
}

test "isSyntheticComment: ends with question mark" {
    const t = std.testing;
    try t.expect(isSyntheticComment("Is this the right approach?"));
    try t.expect(isSyntheticComment("Handles errors?  "));
}

test "isSyntheticComment: dangling preposition" {
    const t = std.testing;
    try t.expect(isSyntheticComment("Handler responsible for"));
    try t.expect(isSyntheticComment("Returns a value of"));
    try t.expect(isSyntheticComment("Used in"));
}

test "isSyntheticComment: hedged opening" {
    const t = std.testing;
    try t.expect(isSyntheticComment("(maybe a flush routine)"));
    try t.expect(isSyntheticComment("(possibly related to config)"));
    try t.expect(isSyntheticComment("(probably unused)"));
}

test "isSyntheticComment: directive leakage" {
    const t = std.testing;
    try t.expect(isSyntheticComment("should be concise and clear"));
    try t.expect(isSyntheticComment("We need to describe the function."));
    try t.expect(isSyntheticComment("max 100 chars for the description"));
    try t.expect(isSyntheticComment("So it's a helper for the event loop."));
}

test "isSyntheticComment: numeric type ref noise" {
    const t = std.testing;
    try t.expect(isSyntheticComment("Has: 5 struct fields"));
    try t.expect(isSyntheticComment("Declares: 3 function overloads"));
}

test "isSyntheticComment: real comments pass" {
    const t = std.testing;
    try t.expect(!isSyntheticComment("Parses Zig source files using the AST."));
    try t.expect(!isSyntheticComment("Returns a vector of embeddings for the given text."));
    try t.expect(!isSyntheticComment("Initializes the database connection pool."));
    try t.expect(!isSyntheticComment("[zig-current] Manages the LRU cache eviction policy."));
    try t.expect(!isSyntheticComment("Computes cosine similarity between two f32 vectors."));
}

test "scrubJsonValue: blanks synthetic module comment" {
    const t = std.testing;
    const allocator = t.allocator;
    const json_src =
        \\{"meta":{"module":"foo","source":"foo.zig","language":"zig"},"comment":"let me think about this","members":[]}
    ;
    var parsed = try std.json.parseFromSlice(std.json.Value, allocator, json_src, .{});
    defer parsed.deinit();
    const changed = scrubJsonValue(&parsed.value);
    try t.expect(changed);
    const c = parsed.value.object.get("comment") orelse return error.MissingField;
    try t.expectEqualStrings("", c.string);
}

test "scrubJsonValue: blanks synthetic member comment" {
    const t = std.testing;
    const allocator = t.allocator;
    const json_src =
        \\{"meta":{"module":"bar","source":"bar.zig","language":"zig"},"comment":"Real module doc.","members":[{"type":"fn_decl","name":"doThing","comment":"I think this does something?","members":[]}]}
    ;
    var parsed = try std.json.parseFromSlice(std.json.Value, allocator, json_src, .{});
    defer parsed.deinit();
    const changed = scrubJsonValue(&parsed.value);
    try t.expect(changed);
    const members = parsed.value.object.get("members").?.array.items;
    const mc = members[0].object.get("comment") orelse return error.MissingField;
    try t.expectEqualStrings("", mc.string);
}

test "scrubJsonValue: preserves real comment" {
    const t = std.testing;
    const allocator = t.allocator;
    const json_src =
        \\{"meta":{"module":"baz","source":"baz.zig","language":"zig"},"comment":"Parses Zig AST nodes.","members":[]}
    ;
    var parsed = try std.json.parseFromSlice(std.json.Value, allocator, json_src, .{});
    defer parsed.deinit();
    const changed = scrubJsonValue(&parsed.value);
    try t.expect(!changed);
}
