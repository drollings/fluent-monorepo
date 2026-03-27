/// anonymize.zig — PII anonymization for frontier LLM context minimization.
///
/// Replaces email addresses, phone numbers, and credit card numbers with
/// placeholder tokens before sending context to external LLM APIs.
/// Operates on allocator-owned string copies; does not modify input.
const std = @import("std");

/// A simple anonymization pattern: replace exact substring matches for
/// well-known patterns detected by manual scanning.
///
/// For a production system, regex-based replacement would be preferable.
/// This implementation uses conservative string scanning to avoid adding
/// a regex dependency.
pub const AnonymizationPattern = enum {
    email,
    phone_us,
    credit_card,

    pub fn placeholder(self: AnonymizationPattern) []const u8 {
        return switch (self) {
            .email => "[EMAIL]",
            .phone_us => "[PHONE]",
            .credit_card => "[CARD]",
        };
    }
};

/// Anonymize a context string by replacing detected PII patterns.
/// Returns an allocator-owned string; caller must free.
pub fn anonymizeContext(
    allocator: std.mem.Allocator,
    context: []const u8,
    patterns: []const AnonymizationPattern,
) ![]u8 {
    var result = try allocator.dupe(u8, context);
    for (patterns) |pattern| {
        const replaced = try replacePattern(allocator, result, pattern);
        allocator.free(result);
        result = replaced;
    }
    return result;
}

/// Replace occurrences of `pattern` in `input`.
/// Returns an allocator-owned string.
fn replacePattern(
    allocator: std.mem.Allocator,
    input: []const u8,
    pattern: AnonymizationPattern,
) ![]u8 {
    return switch (pattern) {
        .email => replaceEmails(allocator, input),
        .phone_us => replacePhones(allocator, input),
        .credit_card => replaceCreditCards(allocator, input),
    };
}

/// Replace email-like tokens (word@word.tld) with [EMAIL].
fn replaceEmails(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        // Look for '@' as email indicator
        const at = std.mem.indexOfScalarPos(u8, input, i, '@') orelse {
            try out.appendSlice(allocator, input[i..]);
            break;
        };
        // Scan backwards for start of local part
        var start = at;
        while (start > i and isEmailChar(input[start - 1])) {
            start -= 1;
        }
        // Scan forwards for end of domain part
        var end = at + 1;
        var dot_seen = false;
        while (end < input.len and (isEmailChar(input[end]) or input[end] == '.')) {
            if (input[end] == '.') dot_seen = true;
            end += 1;
        }
        if (dot_seen and end > at + 2) {
            // Looks like an email
            try out.appendSlice(allocator, input[i..start]);
            try out.appendSlice(allocator, AnonymizationPattern.email.placeholder());
            i = end;
        } else {
            try out.appendSlice(allocator, input[i .. at + 1]);
            i = at + 1;
        }
    }
    return out.toOwnedSlice(allocator);
}

fn isEmailChar(ch: u8) bool {
    return std.ascii.isAlphanumeric(ch) or ch == '_' or ch == '-' or ch == '+' or ch == '.';
}

/// Replace US phone patterns (10-digit with optional separators) with [PHONE].
fn replacePhones(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (std.ascii.isDigit(input[i])) {
            // Try to match NNN-NNN-NNNN or NNN.NNN.NNNN or NNNNNNNNNN
            var j = i;
            var digit_count: usize = 0;
            while (j < input.len and (std.ascii.isDigit(input[j]) or input[j] == '-' or input[j] == '.')) {
                if (std.ascii.isDigit(input[j])) digit_count += 1;
                j += 1;
            }
            if (digit_count == 10) {
                try out.appendSlice(allocator, AnonymizationPattern.phone_us.placeholder());
                i = j;
            } else {
                try out.appendSlice(allocator, input[i .. i + 1]);
                i += 1;
            }
        } else {
            try out.appendSlice(allocator, input[i .. i + 1]);
            i += 1;
        }
    }
    return out.toOwnedSlice(allocator);
}

/// Replace credit card patterns (16 digits with optional spaces/dashes) with [CARD].
fn replaceCreditCards(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (std.ascii.isDigit(input[i])) {
            var j = i;
            var digit_count: usize = 0;
            while (j < input.len and (std.ascii.isDigit(input[j]) or input[j] == '-' or input[j] == ' ')) {
                if (std.ascii.isDigit(input[j])) digit_count += 1;
                j += 1;
            }
            if (digit_count == 16) {
                try out.appendSlice(allocator, AnonymizationPattern.credit_card.placeholder());
                i = j;
            } else {
                try out.appendSlice(allocator, input[i .. i + 1]);
                i += 1;
            }
        } else {
            try out.appendSlice(allocator, input[i .. i + 1]);
            i += 1;
        }
    }
    return out.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const testing = std.testing;

test "anonymizeContext: replaces email" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Contact us at user@example.com for details.",
        &[_]AnonymizationPattern{.email},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[EMAIL]") != null);
    try testing.expect(std.mem.indexOf(u8, result, "user@example.com") == null);
}

test "anonymizeContext: replaces US phone" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Call 555-123-4567 now!",
        &[_]AnonymizationPattern{.phone_us},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[PHONE]") != null);
}

test "anonymizeContext: no PII passes through unchanged" {
    const allocator = testing.allocator;
    const input = "The answer is 42 and pi is 3.14.";
    const result = try anonymizeContext(
        allocator,
        input,
        &[_]AnonymizationPattern{ .email, .phone_us, .credit_card },
    );
    defer allocator.free(result);
    try testing.expectEqualStrings(input, result);
}

test "anonymizeContext: multiple patterns applied in order" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Email: foo@bar.com, Card: 1234-5678-9012-3456",
        &[_]AnonymizationPattern{ .email, .credit_card },
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[EMAIL]") != null);
    try testing.expect(std.mem.indexOf(u8, result, "[CARD]") != null);
}
