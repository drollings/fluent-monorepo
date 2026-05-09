//! test_audit.zig — Phase 2: Test file convention enforcement.
//!
//! Validates that:
//!   1. Files named `*_tests.zig` contain only test declarations (no pub fn, struct, etc.)
//!   2. Test files discovered in the workspace are covered by at least one build.zig target.
//!
//! A "non-test declaration" in a *_tests.zig file is any top-level declaration that is
//! not a `test` block. The check is AST-based for accuracy.

const std = @import("std");

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

pub const AnomalyKind = enum {
    /// A *_tests.zig file has top-level declarations that are not test blocks.
    non_test_decl_in_test_file,
    /// A *_tests.zig file is not referenced by any build.zig test target.
    uncovered_test_file,
};

pub const TestAnomaly = struct {
    kind: AnomalyKind,
    /// Workspace-relative path.
    source: []const u8,
    /// For non_test_decl: the name of the non-test declaration found.
    decl_name: ?[]const u8,
    /// 1-based line number of the anomaly.
    line: ?u32,
};

// ---------------------------------------------------------------------------
// AST helpers
// ---------------------------------------------------------------------------

/// Returns true if `source` contains any top-level declaration that is NOT a
/// `test` block or `comptime` block (comptime blocks are allowed in test files
/// to pull in inline tests from other modules).
///
/// Also allows `usingnamespace`, `@import` variable declarations (e.g.
/// `const std = @import("std");`) because these are boilerplate necessary in
/// any test file.
pub fn hasNonTestDecl(
    allocator: std.mem.Allocator,
    source: [:0]const u8,
    /// Filled with the name and line of the first offending decl (if any).
    out_name: *?[]const u8,
    out_line: *?u32,
) !bool {
    out_name.* = null;
    out_line.* = null;

    var tree = std.zig.Ast.parse(allocator, source, .zig) catch return false;
    defer tree.deinit(allocator);

    for (tree.rootDecls()) |decl| {
        const tag = tree.nodeTag(decl);
        switch (tag) {
            // Pure test blocks — acceptable.
            .test_decl => continue,
            // Comptime blocks — acceptable (used to pull in inline tests).
            .@"comptime" => continue,
            // Variable declarations that are simple @import aliases — acceptable.
            .simple_var_decl, .local_var_decl, .global_var_decl, .aligned_var_decl => {
                const full = tree.fullVarDecl(decl) orelse continue;
                if (full.ast.init_node == .none) {
                    // No initialiser — flag it.
                } else {
                    const init = full.ast.init_node.unwrap() orelse continue;
                    const init_tag = tree.nodeTag(init);
                    // Allow: `const x = @import(...)` and `const x = @This()`
                    if (init_tag == .builtin_call_two or
                        init_tag == .builtin_call_two_comma or
                        init_tag == .builtin_call or
                        init_tag == .builtin_call_comma)
                    {
                        continue;
                    }
                    // Allow: `const x = other_module.SomeType` (field access on import)
                    if (init_tag == .field_access) continue;
                    // Allow: `const x = other_module` (identifier)
                    if (init_tag == .identifier) continue;
                }
                // Anything else is a real non-test declaration.
                const name = nameOfDecl(&tree, decl);
                const line = lineOfDecl(&tree, decl);
                out_name.* = if (name) |n| try allocator.dupe(u8, n) else null;
                out_line.* = line;
                return true;
            },
            // Function declarations — always flag these.
            .fn_decl, .fn_proto, .fn_proto_one, .fn_proto_simple, .fn_proto_multi => {
                const name = nameOfDecl(&tree, decl);
                const line = lineOfDecl(&tree, decl);
                out_name.* = if (name) |n| try allocator.dupe(u8, n) else null;
                out_line.* = line;
                return true;
            },
            // Struct / enum / union declarations — flag.
            .container_decl,
            .container_decl_two,
            .container_decl_two_trailing,
            .container_decl_arg,
            .container_decl_arg_trailing,
            .tagged_union,
            .tagged_union_two,
            .tagged_union_two_trailing,
            .tagged_union_enum_tag,
            .tagged_union_enum_tag_trailing,
            => {
                const line = lineOfDecl(&tree, decl);
                out_line.* = line;
                return true;
            },
            else => continue,
        }
    }

    return false;
}

/// Extract the name token text for a var/const declaration or fn declaration.
fn nameOfDecl(tree: *const std.zig.Ast, decl: std.zig.Ast.Node.Index) ?[]const u8 {
    const tag = tree.nodeTag(decl);
    switch (tag) {
        .fn_decl => {
            var buf: [1]std.zig.Ast.Node.Index = undefined;
            const proto_node = tree.nodeData(decl).node_and_node[0];
            const proto = tree.fullFnProto(&buf, proto_node) orelse return null;
            const nt = proto.name_token orelse return null;
            return tree.tokenSlice(nt);
        },
        .simple_var_decl, .local_var_decl, .global_var_decl, .aligned_var_decl => {
            const full = tree.fullVarDecl(decl) orelse return null;
            const name_tok = full.ast.mut_token + 1;
            return tree.tokenSlice(name_tok);
        },
        else => return null,
    }
}

fn lineOfDecl(tree: *const std.zig.Ast, decl: std.zig.Ast.Node.Index) ?u32 {
    const tok = tree.nodeMainToken(decl);
    const loc = tree.tokenLocation(0, tok);
    return @intCast(loc.line + 1);
}

// ---------------------------------------------------------------------------
// Workspace audit
// ---------------------------------------------------------------------------

/// Walk `workspace` and report test-file anomalies.
/// Caller owns the returned slice and each TestAnomaly's string fields.
pub fn auditTestFiles(
    allocator: std.mem.Allocator,
    workspace: []const u8,
) ![]TestAnomaly {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    // ── Collect build.zig path refs ───────────────────────────────────────────
    var build_paths = std.StringHashMap(void).init(aa);
    defer build_paths.deinit();

    {
        const build_zig_path = try std.fmt.allocPrint(aa, "{s}/build.zig", .{workspace});
        const build_src = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), build_zig_path, aa, .limited(5 * 1024 * 1024)) catch null;
        if (build_src) |src| {
            const needle = ".path(\"";
            var pos: usize = 0;
            while (pos < src.len) {
                const found = std.mem.indexOfPos(u8, src, pos, needle) orelse break;
                const start = found + needle.len;
                const end = std.mem.indexOfScalarPos(u8, src, start, '"') orelse break;
                const p = src[start..end];
                if (p.len > 0) try build_paths.put(p, {});
                pos = end + 1;
            }
        }
    }

    // ── Walk workspace for *_tests.zig files ─────────────────────────────────
    var anomalies: std.ArrayList(TestAnomaly) = .empty;
    errdefer {
        for (anomalies.items) |a| {
            allocator.free(a.source);
            if (a.decl_name) |n| allocator.free(n);
        }
        anomalies.deinit(allocator);
    }

    {
        const io = std.Io.Threaded.global_single_threaded.io();
        var base_dir = std.Io.Dir.cwd().openDir(io, workspace, .{ .iterate = true }) catch |err| {
            std.debug.print("[test_audit] cannot open workspace '{s}': {s}\n", .{ workspace, @errorName(err) });
            return allocator.alloc(TestAnomaly, 0);
        };
        defer base_dir.close(io);

        var walker = try base_dir.walk(aa);
        defer walker.deinit();

        while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
            if (entry.kind != .file) continue;
            const basename = std.fs.path.basename(entry.path);
            if (!std.mem.endsWith(u8, basename, "_tests.zig")) continue;
            // Skip zig-out and zig-cache.
            if (std.mem.startsWith(u8, entry.path, "zig-")) continue;

            const rel_path = entry.path;
            const abs_path = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, rel_path });

            // Check 1: does the file contain non-test declarations?
            const src_raw = std.Io.Dir.cwd().readFileAlloc(io, abs_path, aa, .limited(5 * 1024 * 1024)) catch continue;
            const src_z = try aa.dupeZ(u8, src_raw);

            var out_name: ?[]const u8 = null;
            var out_line: ?u32 = null;
            const has_bad = try hasNonTestDecl(allocator, src_z, &out_name, &out_line);
            if (has_bad) {
                try anomalies.append(allocator, .{
                    .kind = .non_test_decl_in_test_file,
                    .source = try allocator.dupe(u8, rel_path),
                    .decl_name = out_name,
                    .line = out_line,
                });
            } else {
                if (out_name) |n| allocator.free(n);
            }

            // Check 2: is the file covered by a build.zig target?
            if (!build_paths.contains(rel_path)) {
                try anomalies.append(allocator, .{
                    .kind = .uncovered_test_file,
                    .source = try allocator.dupe(u8, rel_path),
                    .decl_name = null,
                    .line = null,
                });
            }
        }
    }

    return anomalies.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// Write AI-format section for test anomalies to `w`.
pub fn writeAiOutput(w: *std.Io.Writer, anomalies: []const TestAnomaly) !void {
    if (anomalies.len == 0) return;

    try w.writeAll(
        \\## ⚠️ TEST ORGANISATION ISSUES
        \\
        \\
    );
    for (anomalies) |a| {
        switch (a.kind) {
            .non_test_decl_in_test_file => {
                try w.print("### Non-test code in `{s}`", .{a.source});
                if (a.line) |l| try w.print(" (line {d})", .{l});
                try w.writeByte('\n');
                if (a.decl_name) |n| {
                    try w.print("**Declaration:** `{s}`\n", .{n});
                }
                try w.writeAll("**Action:** Move non-test declarations to a dedicated source file.\n\n---\n\n");
            },
            .uncovered_test_file => {
                try w.print("### `{s}` not in build.zig\n\n", .{a.source});
                try w.writeAll("**Action:** Add a test target to build.zig or remove the file.\n\n---\n\n");
            },
        }
    }
}

/// Write human-format section for test anomalies.
pub fn writeHumanOutput(w: *std.Io.Writer, anomalies: []const TestAnomaly) !void {
    if (anomalies.len == 0) return;
    try w.print("\nTest organisation issues ({d}):\n", .{anomalies.len});
    for (anomalies) |a| {
        switch (a.kind) {
            .non_test_decl_in_test_file => try w.print("  [non-test decl] {s}\n", .{a.source}),
            .uncovered_test_file => try w.print("  [uncovered] {s}\n", .{a.source}),
        }
    }
}

/// Append a JSON `"test_anomalies":[...]` array fragment (no surrounding braces).
pub fn writeJsonFragment(w: *std.Io.Writer, anomalies: []const TestAnomaly) !void {
    try w.writeAll("\"test_anomalies\":[");
    for (anomalies, 0..) |a, i| {
        if (i > 0) try w.writeAll(",");
        try w.print("{{\"kind\":\"{s}\",\"source\":\"{s}\"", .{ @tagName(a.kind), a.source });
        if (a.decl_name) |n| try w.print(",\"decl_name\":\"{s}\"", .{n});
        if (a.line) |l| try w.print(",\"line\":{d}", .{l});
        try w.writeAll("}");
    }
    try w.writeAll("]");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
