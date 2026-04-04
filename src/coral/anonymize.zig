/// anonymize.zig — PII anonymization for frontier LLM context minimization.
///
/// Replaces email addresses, phone numbers, and credit card numbers with
/// placeholder tokens before sending context to external LLM APIs.
/// Operates on allocator-owned string copies; does not modify input.
const std = @import("std");

/// Defines anonymization rules with a fixed pattern; manages key invariants; owned by the system; supports secure data handling.
pub const AnonymizationPattern = enum {
    email,
    phone_us,
    phone_intl, // E.164 / UK / EU international formats
    credit_card,
    ssn_us, // US Social Security Number NNN-NN-NNNN
    ssn_uk, // UK National Insurance Number
    ssn_ca, // Canadian Social Insurance Number
    ipv4,
    ipv6,
    api_key_bearer, // Bearer <token>
    api_key_aws, // AKIA... AWS access key
    api_key_generic, // Generic 32+ char alphanumeric tokens

    pub fn placeholder(self: AnonymizationPattern) []const u8 {
        return switch (self) {
            .email => "[EMAIL]",
            .phone_us => "[PHONE]",
            .phone_intl => "[PHONE]",
            .credit_card => "[CARD]",
            .ssn_us => "[SSN]",
            .ssn_uk => "[NINO]",
            .ssn_ca => "[SIN]",
            .ipv4 => "[IP]",
            .ipv6 => "[IP]",
            .api_key_bearer => "[BEARER_TOKEN]",
            .api_key_aws => "[AWS_KEY]",
            .api_key_generic => "[API_KEY]",
        };
    }
};

/// Anonymizes context data by applying provided patterns to each byte in the input slice.
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

/// Replaces specified patterns in a Zig string using an allocator and returns the modified slice.
fn replacePattern(
    allocator: std.mem.Allocator,
    input: []const u8,
    pattern: AnonymizationPattern,
) ![]u8 {
    return switch (pattern) {
        .email => replaceEmails(allocator, input),
        .phone_us => replacePhones(allocator, input),
        .phone_intl => replaceIntlPhones(allocator, input),
        .credit_card => replaceCreditCards(allocator, input),
        .ssn_us => replaceSsnUs(allocator, input),
        .ssn_uk => replaceSsnUk(allocator, input),
        .ssn_ca => replaceSsnCa(allocator, input),
        .ipv4 => replaceIpv4(allocator, input),
        .ipv6 => replaceIpv6(allocator, input),
        .api_key_bearer => replaceBearerTokens(allocator, input),
        .api_key_aws => replaceAwsKeys(allocator, input),
        .api_key_generic => replaceGenericApiKeys(allocator, input),
    };
}

/// Replaces email addresses in the input slice using an allocator for memory management.
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

/// Checks if a character is a valid email character in the Zig code snippet.
fn isEmailChar(ch: u8) bool {
    return std.ascii.isAlphanumeric(ch) or ch == '_' or ch == '-' or ch == '+' or ch == '.';
}

/// Replaces phone numbers in the input slice with a placeholder, returning a modified slice.
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

/// Replaces credit card data in the input slice using an allocator for memory management.
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

/// Replaces all integer phone numbers in the input slice with a placeholder, returning a modified slice.
fn replaceIntlPhones(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (input[i] == '+' and i + 1 < input.len and std.ascii.isDigit(input[i + 1])) {
            // Scan forward collecting digits and allowed separators (space, -, ())
            var j = i + 1;
            var digit_count: usize = 0;
            while (j < input.len) {
                const c = input[j];
                if (std.ascii.isDigit(c)) {
                    digit_count += 1;
                } else if (c == ' ' or c == '-' or c == '(' or c == ')') {
                    // allowed separators
                } else {
                    break;
                }
                j += 1;
            }
            // E.164: country code (1-3 digits) + subscriber (4-14 digits) = 7-15 total
            if (digit_count >= 7 and digit_count <= 15) {
                try out.appendSlice(allocator, AnonymizationPattern.phone_intl.placeholder());
                i = j;
                continue;
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Replaces SSN values in the input slice using an allocator, returning a modified slice.
fn replaceSsnUs(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        // Try NNN-NN-NNNN
        if (i + 11 <= input.len and
            isNDigits(input[i .. i + 3]) and
            input[i + 3] == '-' and
            isNDigits(input[i + 4 .. i + 6]) and
            input[i + 6] == '-' and
            isNDigits(input[i + 7 .. i + 11]))
        {
            // Ensure not part of a longer number
            const before_ok = i == 0 or !std.ascii.isDigit(input[i - 1]);
            const after_ok = i + 11 >= input.len or !std.ascii.isDigit(input[i + 11]);
            if (before_ok and after_ok) {
                try out.appendSlice(allocator, AnonymizationPattern.ssn_us.placeholder());
                i += 11;
                continue;
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Replaces specified UNSCK patterns in a Zig string using an allocator.
fn replaceSsnUk(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        // AA999999A or AA 99 99 99 A (with optional spaces)
        if (i + 9 <= input.len and
            std.ascii.isAlphabetic(input[i]) and
            std.ascii.isAlphabetic(input[i + 1]) and
            std.ascii.isDigit(input[i + 2]) and
            std.ascii.isDigit(input[i + 3]) and
            std.ascii.isDigit(input[i + 4]) and
            std.ascii.isDigit(input[i + 5]) and
            std.ascii.isDigit(input[i + 6]) and
            std.ascii.isDigit(input[i + 7]) and
            std.ascii.isAlphabetic(input[i + 8]))
        {
            const before_ok = i == 0 or !std.ascii.isAlphanumeric(input[i - 1]);
            const after_ok = i + 9 >= input.len or !std.ascii.isAlphanumeric(input[i + 9]);
            if (before_ok and after_ok) {
                try out.appendSlice(allocator, AnonymizationPattern.ssn_uk.placeholder());
                i += 9;
                continue;
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Replaces SSN characters in a Zig string using an allocator and returns the modified slice.
fn replaceSsnCa(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (i + 11 <= input.len and
            isNDigits(input[i .. i + 3]) and
            input[i + 3] == '-' and
            isNDigits(input[i + 4 .. i + 7]) and
            input[i + 7] == '-' and
            isNDigits(input[i + 8 .. i + 11]))
        {
            const before_ok = i == 0 or !std.ascii.isDigit(input[i - 1]);
            const after_ok = i + 11 >= input.len or !std.ascii.isDigit(input[i + 11]);
            if (before_ok and after_ok) {
                try out.appendSlice(allocator, AnonymizationPattern.ssn_ca.placeholder());
                i += 11;
                continue;
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Checks if a slice contains only numeric characters.
fn isNDigits(s: []const u8) bool {
    for (s) |c| if (!std.ascii.isDigit(c)) return false;
    return s.len > 0;
}

/// Replaces IPv4 addresses in the input slice using an allocator.
fn replaceIpv4(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (std.ascii.isDigit(input[i])) {
            // Try to match d{1,3}.d{1,3}.d{1,3}.d{1,3}
            var j = i;
            var dots: usize = 0;
            while (j < input.len and (std.ascii.isDigit(input[j]) or input[j] == '.')) {
                if (input[j] == '.') dots += 1;
                j += 1;
            }
            const candidate = input[i..j];
            if (dots == 3 and looksLikeIpv4(candidate)) {
                const before_ok = i == 0 or !std.ascii.isDigit(input[i - 1]);
                const after_ok = j >= input.len or input[j] != '.';
                if (before_ok and after_ok) {
                    try out.appendSlice(allocator, AnonymizationPattern.ipv4.placeholder());
                    i = j;
                    continue;
                }
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Checks if a list of bytes resembles a valid IPv4 address, returning true or false.
fn looksLikeIpv4(s: []const u8) bool {
    var part_count: usize = 0;
    var part_len: usize = 0;
    for (s) |c| {
        if (c == '.') {
            if (part_len == 0 or part_len > 3) return false;
            part_count += 1;
            part_len = 0;
        } else if (std.ascii.isDigit(c)) {
            part_len += 1;
        } else {
            return false;
        }
    }
    if (part_len == 0 or part_len > 3) return false;
    return part_count == 3;
}

/// Replaces IPv6 addresses in the input slice using an allocator.
fn replaceIpv6(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (isHexChar(input[i])) {
            var j = i;
            var colons: usize = 0;
            var hex_count: usize = 0;
            while (j < input.len and (isHexChar(input[j]) or input[j] == ':')) {
                if (input[j] == ':') colons += 1 else hex_count += 1;
                j += 1;
            }
            if (colons >= 2 and hex_count >= 4) {
                const before_ok = i == 0 or (!isHexChar(input[i - 1]) and input[i - 1] != ':');
                const after_ok = j >= input.len or (!isHexChar(input[j]) and input[j] != ':');
                if (before_ok and after_ok) {
                    try out.appendSlice(allocator, AnonymizationPattern.ipv6.placeholder());
                    i = j;
                    continue;
                }
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Checks if a byte is a valid hex character for use in Zig code.
fn isHexChar(c: u8) bool {
    return std.ascii.isDigit(c) or (c >= 'a' and c <= 'f') or (c >= 'A' and c <= 'F');
}

/// Replaces bearer tokens in the input slice using an allocator for memory management.
fn replaceBearerTokens(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    const prefix = "Bearer ";
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (std.mem.startsWith(u8, input[i..], prefix)) {
            const token_start = i + prefix.len;
            var j = token_start;
            // Token: base64url chars + padding '='
            while (j < input.len and (std.ascii.isAlphanumeric(input[j]) or
                input[j] == '-' or input[j] == '_' or input[j] == '.' or
                input[j] == '+' or input[j] == '/' or input[j] == '='))
            {
                j += 1;
            }
            if (j > token_start + 8) { // at least 8 chars to be a real token
                try out.appendSlice(allocator, "Bearer ");
                try out.appendSlice(allocator, AnonymizationPattern.api_key_bearer.placeholder());
                i = j;
                continue;
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Replaces AWS keys in the input slice with a placeholder, returning a modified slice.
fn replaceAwsKeys(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    const prefix = "AKIA";
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (i + 20 <= input.len and std.mem.startsWith(u8, input[i..], prefix)) {
            const rest = input[i + 4 .. i + 20];
            var all_upper_alnum = true;
            for (rest) |c| {
                if (!std.ascii.isAlphanumeric(c) or std.ascii.isLower(c)) {
                    all_upper_alnum = false;
                    break;
                }
            }
            if (all_upper_alnum) {
                const before_ok = i == 0 or !std.ascii.isAlphanumeric(input[i - 1]);
                const after_ok = i + 20 >= input.len or !std.ascii.isAlphanumeric(input[i + 20]);
                if (before_ok and after_ok) {
                    try out.appendSlice(allocator, AnonymizationPattern.api_key_aws.placeholder());
                    i += 20;
                    continue;
                }
            }
        }
        try out.append(allocator, input[i]);
        i += 1;
    }
    return out.toOwnedSlice(allocator);
}

/// Replaces placeholder API keys in memory allocations with sanitized values.
fn replaceGenericApiKeys(allocator: std.mem.Allocator, input: []const u8) ![]u8 {
    var out: std.ArrayListUnmanaged(u8) = .{};
    errdefer out.deinit(allocator);

    var i: usize = 0;
    while (i < input.len) {
        if (std.ascii.isAlphanumeric(input[i])) {
            var j = i;
            while (j < input.len and std.ascii.isAlphanumeric(input[j])) j += 1;
            const token_len = j - i;
            if (token_len >= 32) {
                const before_ok = i == 0 or !std.ascii.isAlphanumeric(input[i - 1]);
                const after_ok = j >= input.len or !std.ascii.isAlphanumeric(input[j]);
                if (before_ok and after_ok) {
                    try out.appendSlice(allocator, AnonymizationPattern.api_key_generic.placeholder());
                    i = j;
                    continue;
                }
            }
            // Not a token — emit as-is
            try out.appendSlice(allocator, input[i..j]);
            i = j;
            continue;
        }
        try out.append(allocator, input[i]);
        i += 1;
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

test "anonymizeContext: replaces international phone E.164" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Call +44 7911 123456 or +1-800-555-0100",
        &[_]AnonymizationPattern{.phone_intl},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[PHONE]") != null);
    try testing.expect(std.mem.indexOf(u8, result, "+44") == null);
}

test "anonymizeContext: replaces US SSN" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "SSN: 123-45-6789 is confidential.",
        &[_]AnonymizationPattern{.ssn_us},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[SSN]") != null);
    try testing.expect(std.mem.indexOf(u8, result, "123-45-6789") == null);
}

test "anonymizeContext: replaces Canadian SIN" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "SIN: 046-454-286",
        &[_]AnonymizationPattern{.ssn_ca},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[SIN]") != null);
}

test "anonymizeContext: replaces IPv4" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Connect to 192.168.1.100 on port 8080.",
        &[_]AnonymizationPattern{.ipv4},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[IP]") != null);
    try testing.expect(std.mem.indexOf(u8, result, "192.168.1.100") == null);
}

test "anonymizeContext: replaces AWS key" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Key: AKIAIOSFODNN7EXAMPLE is invalid.",
        &[_]AnonymizationPattern{.api_key_aws},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[AWS_KEY]") != null);
    try testing.expect(std.mem.indexOf(u8, result, "AKIAIOSFODNN7EXAMPLE") == null);
}

test "anonymizeContext: replaces Bearer token" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        &[_]AnonymizationPattern{.api_key_bearer},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[BEARER_TOKEN]") != null);
}

test "anonymizeContext: replaces generic API key (32+ chars)" {
    const allocator = testing.allocator;
    const result = try anonymizeContext(
        allocator,
        "api_key=abcdef1234567890abcdef1234567890",
        &[_]AnonymizationPattern{.api_key_generic},
    );
    defer allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "[API_KEY]") != null);
}

test "anonymizeContext: no false positive on short alphanumeric" {
    const allocator = testing.allocator;
    const input = "version=abc123";
    const result = try anonymizeContext(
        allocator,
        input,
        &[_]AnonymizationPattern{.api_key_generic},
    );
    defer allocator.free(result);
    try testing.expectEqualStrings(input, result);
}
