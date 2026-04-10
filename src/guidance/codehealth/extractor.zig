//! call_extractor.zig — AST-based call site extraction for codehealth Phase 2b.
//!
//! Extracts call sites from Zig source files and populates the `called_by` table.
//! Only run when `guidance codehealth --extract-calls` is invoked; not part of
//! the normal RALPH loop.
//!
//! Confidence levels:
//!   high   — same-file direct call or qualified import call (mod.fn())
//!   medium — method call on self (self.method())
//!   low    — unresolved / dynamic dispatch / chained call

const std = @import("std");
const vector = @import("vector");

/// A single extracted call site within a source file.
pub const CallSite = struct {
    caller_line: u32,
    /// Name of the enclosing function ("(module)" for top-level calls).
    caller_fn: []const u8,
    /// Name of the callee (may be qualified: "mod.helper").
    callee_name: []const u8,
    /// Confidence in the resolution.
    confidence: enum { high, medium, low },
};

/// Extracts call sites from a single Zig source file using the Zig AST.
pub const CallExtractor = struct {
    allocator: std.mem.Allocator,
    source: [:0]const u8,
    tree: std.zig.Ast,
    /// Map: import alias → resolved relative path (e.g. "bar" → "src/bar.zig").
    imports: std.StringHashMap([]const u8),
    file_path: []const u8,

    pub fn init(allocator: std.mem.Allocator, file_path: []const u8, source: [:0]const u8) !CallExtractor {
        const tree = try std.zig.Ast.parse(allocator, source, .zig);
        var self = CallExtractor{
            .allocator = allocator,
            .source = source,
            .tree = tree,
            .imports = std.StringHashMap([]const u8).init(allocator),
            .file_path = file_path,
        };
        try self.buildImportMap();
        return self;
    }

    pub fn deinit(self: *CallExtractor) void {
        self.tree.deinit(self.allocator);
        var it = self.imports.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            self.allocator.free(entry.value_ptr.*);
        }
        self.imports.deinit();
    }

    /// Build a map of `const alias = @import("path.zig")` declarations.
    fn buildImportMap(self: *CallExtractor) !void {
        for (self.tree.rootDecls()) |decl| {
            const tag = self.tree.nodeTag(decl);
            if (tag != .simple_var_decl and tag != .local_var_decl and
                tag != .global_var_decl and tag != .aligned_var_decl) continue;

            const full_var = self.tree.fullVarDecl(decl) orelse continue;
            if (full_var.ast.init_node == .none) continue;
            const init_node = full_var.ast.init_node.unwrap() orelse continue;

            // Check if init is a builtin call.
            const init_tag = self.tree.nodeTag(init_node);
            if (init_tag != .builtin_call_two and init_tag != .builtin_call_two_comma and
                init_tag != .builtin_call and init_tag != .builtin_call_comma) continue;

            // The builtin name is the main token.
            const builtin_tok = self.tree.nodeMainToken(init_node);
            const builtin_name = self.tree.tokenSlice(builtin_tok);
            if (!std.mem.eql(u8, builtin_name, "@import")) continue;

            // For builtin_call_two, data is opt_node_and_opt_node.
            // @import takes exactly one string argument → first opt is the arg.
            const opts = self.tree.nodeData(init_node).opt_node_and_opt_node;
            const arg_node = opts[0].unwrap() orelse continue;
            if (self.tree.nodeTag(arg_node) != .string_literal) continue;

            const path_tok = self.tree.nodeMainToken(arg_node);
            const path_raw = self.tree.tokenSlice(path_tok);
            if (path_raw.len < 2) continue;
            const path = path_raw[1 .. path_raw.len - 1]; // strip quotes

            // Alias is the identifier after `const`.
            const alias_tok = full_var.ast.mut_token + 1;
            const alias = try self.allocator.dupe(u8, self.tree.tokenSlice(alias_tok));
            errdefer self.allocator.free(alias);

            const resolved = try self.allocator.dupe(u8, path);
            try self.imports.put(alias, resolved);
        }
    }

    /// Extract all call sites from the file in a single pass over rootDecls.
    /// The caller owns the returned slice and each CallSite's string fields.
    pub fn extractAllCalls(self: *CallExtractor) ![]CallSite {
        var calls: std.ArrayList(CallSite) = .{};
        errdefer {
            for (calls.items) |cs| {
                self.allocator.free(cs.caller_fn);
                self.allocator.free(cs.callee_name);
            }
            calls.deinit(self.allocator);
        }

        for (self.tree.rootDecls()) |decl| {
            const tag = self.tree.nodeTag(decl);
            if (tag == .fn_decl) {
                try self.extractFromFnDecl(decl, &calls);
            }
        }

        return calls.toOwnedSlice(self.allocator);
    }

    fn extractFromFnDecl(
        self: *CallExtractor,
        fn_node: std.zig.Ast.Node.Index,
        calls: *std.ArrayList(CallSite),
    ) !void {
        var buf: [1]std.zig.Ast.Node.Index = undefined;
        const parts = self.tree.nodeData(fn_node).node_and_node;
        const fn_proto_node = parts[0];
        const body_node = parts[1];

        const full_proto = self.tree.fullFnProto(&buf, fn_proto_node) orelse return;
        const fn_name: []const u8 = if (full_proto.name_token) |nt|
            self.tree.tokenSlice(nt)
        else
            "(anonymous)";

        if (@intFromEnum(body_node) == 0) return;

        // Do a flat scan of the subtree for call nodes.
        // We walk node indices in the range [fn_node, body_node's last token] but
        // since AST node indices are ordered by position, we scan from fn_node onward
        // and stop when we leave the function scope. For simplicity, scan all nodes
        // and filter those whose main token falls within the function's token range.
        const fn_start = self.tree.nodeMainToken(fn_node);
        const fn_end = self.tree.nodeMainToken(body_node); // approximate

        var i: u32 = @intFromEnum(fn_node) + 1;
        while (i < self.tree.nodes.len) : (i += 1) {
            const node: std.zig.Ast.Node.Index = @enumFromInt(i);
            const node_tok = self.tree.nodeMainToken(node);
            if (node_tok < fn_start) continue;
            if (node_tok > fn_end + 5000) break;

            const node_tag = self.tree.nodeTag(node);
            switch (node_tag) {
                .call_one, .call_one_comma => {
                    var call_buf: [1]std.zig.Ast.Node.Index = undefined;
                    const call = self.tree.callOne(&call_buf, node);
                    try self.recordCallSite(node, call.ast.fn_expr, fn_name, calls);
                },
                .call, .call_comma => {
                    const call = self.tree.callFull(node);
                    try self.recordCallSite(node, call.ast.fn_expr, fn_name, calls);
                },
                else => {},
            }
        }
    }

    fn recordCallSite(
        self: *CallExtractor,
        call_node: std.zig.Ast.Node.Index,
        fn_expr: std.zig.Ast.Node.Index,
        current_fn: []const u8,
        calls: *std.ArrayList(CallSite),
    ) !void {
        const tok = self.tree.nodeMainToken(call_node);
        const loc = self.tree.tokenLocation(0, tok);
        const caller_line: u32 = @intCast(loc.line + 1);

        const fn_tag = self.tree.nodeTag(fn_expr);
        var callee_name: []const u8 = undefined;
        var confidence: @TypeOf((@as(CallSite, undefined)).confidence) = .low;
        var owned = false;

        switch (fn_tag) {
            .identifier => {
                callee_name = self.tree.tokenSlice(self.tree.nodeMainToken(fn_expr));
                confidence = .high;
            },
            .field_access => {
                const fa_parts = self.tree.nodeData(fn_expr).node_and_token;
                const lhs_node = fa_parts[0];
                const field_tok = fa_parts[1];
                const method_name = self.tree.tokenSlice(field_tok);

                if (self.tree.nodeTag(lhs_node) == .identifier) {
                    const obj_name = self.tree.tokenSlice(self.tree.nodeMainToken(lhs_node));
                    if (std.mem.eql(u8, obj_name, "self")) {
                        callee_name = method_name;
                        confidence = .medium;
                    } else if (self.imports.contains(obj_name)) {
                        const full = try std.fmt.allocPrint(
                            self.allocator,
                            "{s}.{s}",
                            .{ obj_name, method_name },
                        );
                        callee_name = full;
                        confidence = .high;
                        owned = true;
                    } else {
                        callee_name = method_name;
                        confidence = .low;
                    }
                } else {
                    callee_name = method_name;
                    confidence = .low;
                }
            },
            else => return, // Skip complex callees (e.g. derefs, comptime blocks).
        }

        const duped_fn = try self.allocator.dupe(u8, current_fn);
        errdefer self.allocator.free(duped_fn);
        const duped_callee = if (owned)
            callee_name
        else
            try self.allocator.dupe(u8, callee_name);

        try calls.append(self.allocator, .{
            .caller_line = caller_line,
            .caller_fn = duped_fn,
            .callee_name = duped_callee,
            .confidence = confidence,
        });
    }
};

// ---------------------------------------------------------------------------
// Workspace extraction
// ---------------------------------------------------------------------------

/// Walk all `.zig` source files under `workspace` and populate `called_by`.
pub fn extractCallsFromWorkspace(
    allocator: std.mem.Allocator,
    db: *vector.GuidanceDb,
    workspace: []const u8,
) !void {
    var dir = std.fs.cwd().openDir(workspace, .{ .iterate = true }) catch |err| {
        std.debug.print("[call_extractor] cannot open workspace '{s}': {s}\n", .{ workspace, @errorName(err) });
        return err;
    };
    defer dir.close();

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    var files_processed: usize = 0;
    var call_sites_found: usize = 0;

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;
        if (std.mem.endsWith(u8, entry.path, "_test.zig")) continue;

        const abs_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ workspace, entry.path });
        defer allocator.free(abs_path);

        const source_raw = std.fs.cwd().readFileAlloc(allocator, abs_path, 5 * 1024 * 1024) catch continue;
        defer allocator.free(source_raw);

        const source = try allocator.dupeZ(u8, source_raw);
        defer allocator.free(source);

        var extractor = CallExtractor.init(allocator, entry.path, source) catch continue;
        defer extractor.deinit();

        const sites = extractor.extractAllCalls() catch continue;
        defer {
            for (sites) |cs| {
                allocator.free(cs.caller_fn);
                allocator.free(cs.callee_name);
            }
            allocator.free(sites);
        }

        const confidence_strs = [_][]const u8{ "high", "medium", "low" };
        for (sites) |cs| {
            db.insertCalledBy(
                entry.path,
                cs.caller_line,
                cs.caller_fn,
                cs.callee_name,
                null,
                null,
                confidence_strs[@intFromEnum(cs.confidence)],
            );
        }

        files_processed += 1;
        call_sites_found += sites.len;
    }

    std.debug.print("[call_extractor] processed {d} files, found {d} call sites\n", .{
        files_processed, call_sites_found,
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
