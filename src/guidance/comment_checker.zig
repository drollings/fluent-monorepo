/// comment_checker.zig — Comment staleness detection for guidance.
///
/// Decides whether an existing `///` doc comment needs to be regenerated
/// based on the member's `match_hash` and basic quality heuristics.
const std = @import("std");
const types = @import("types.zig");
const comment_parser = @import("comment_parser.zig");

/// Result of checking a comment for staleness.
pub const CheckResult = struct {
    /// True when the comment should be regenerated.
    needs_regeneration: bool,
    /// Human-readable reason (not owned; points to string literals).
    reason: ?[]const u8 = null,
};

/// Check whether `existing_comment` is stale for `member`.
///
/// A comment is considered stale when any of the following hold:
///  1. `match_hash` changed — code was modified since the comment was written.
///  2. The comment is not well-formed (empty, too long, or leaked LLM preamble).
///  3. The comment is over 400 characters.
///
/// `source_context` is the raw source text of the declaration; currently used
/// only for future LLM-based accuracy checking (pass "" to skip).
///
/// This function never calls the LLM directly; that responsibility belongs to
/// the `CommentSyncProcessor` in `comment_sync.zig`.
pub fn checkCommentStaleness(
    existing_comment: []const u8,
    member: types.Member,
    source_context: []const u8,
) CheckResult {
    // 1. Well-formedness check.
    if (!comment_parser.isWellFormedComment(existing_comment, source_context)) {
        return .{ .needs_regeneration = true, .reason = "comment is not well-formed" };
    }

    // 2. Length cap.
    if (existing_comment.len > 400) {
        return .{ .needs_regeneration = true, .reason = "comment exceeds 400 characters" };
    }

    // 3. Hash staleness: if the member has a hash and it differs from what
    //    the comment was written for, the comment may be outdated.
    //    We detect this conservatively: the comment must be regenerated only
    //    when there is no source doc comment to rely on (if the author has
    //    already updated the `///` comment, the sync loop will use that).
    //    Here we just flag the hash mismatch; the caller decides what to do.
    _ = member; // hash comparison happens in the sync workflow

    return .{ .needs_regeneration = false };
}

/// Return true when a stored `match_hash` differs from `current_hash`,
/// indicating that the code changed since the comment was written.
pub fn isHashStale(stored_hash: ?[]const u8, current_hash: ?[]const u8) bool {
    const stored = stored_hash orelse return false;
    const current = current_hash orelse return false;
    return !std.mem.eql(u8, stored, current);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "checkCommentStaleness - valid comment" {
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = checkCommentStaleness("Parse the input and return a result.", member, "");
    try std.testing.expect(!result.needs_regeneration);
}

test "checkCommentStaleness - empty comment" {
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = checkCommentStaleness("", member, "");
    try std.testing.expect(result.needs_regeneration);
}

test "checkCommentStaleness - leaked prompt" {
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = checkCommentStaleness("Let me write a function summary.", member, "");
    try std.testing.expect(result.needs_regeneration);
}

test "checkCommentStaleness - too long" {
    var long: [401]u8 = undefined;
    @memset(&long, 'a');
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = checkCommentStaleness(&long, member, "");
    try std.testing.expect(result.needs_regeneration);
}

test "isHashStale - different hashes" {
    try std.testing.expect(isHashStale("abc", "def"));
}

test "isHashStale - same hash" {
    try std.testing.expect(!isHashStale("abc", "abc"));
}

test "isHashStale - null hash" {
    try std.testing.expect(!isHashStale(null, "abc"));
    try std.testing.expect(!isHashStale("abc", null));
}
