//! fsm.zig — Main FSM loop for the deterministic-first subagent.
//!
//! Implements state-driven switch dispatch for the INTAKE→CLASSIFY→ROUTE→
//! VALIDATE→EXECUTE→REFLECT→SYNTH→INTAKE cycle. Uses SyncBackend for
//! deterministic tests and ZioBackend for production concurrent execution.
//!
//! Integration points are provided via dependency injection (ExplainFn,
//! LlmFn callbacks) to avoid circular imports with the guidance module.
//!
//! M10: Per-iteration arena reset, profiling, and backend selection.

const std = @import("std");
const types = @import("types.zig");
const classify_mod = @import("classify.zig");
const route_mod = @import("route.zig");
const validate_mod = @import("validate.zig");
const execute_mod = @import("execute.zig");
const synthesize_mod = @import("synthesize.zig");
const guardrails_mod = @import("guardrails.zig");
const reflect_mod = @import("reflect.zig");
const concurrency = @import("concurrency");
const common_io = @import("common").io;

fn dispatchTool(
    allocator: std.mem.Allocator,
    config: types.SubagentConfig,
    params: *const types.ToolParams,
    io: std.Io,
) anyerror!types.ToolResult {
    const action = params.action;
    switch (action) {
        .bash => {
            const command = params.command orelse return error.MissingCommand;
            var parsed: std.ArrayList([]const u8) = .empty;
            defer {
                for (parsed.items) |arg| allocator.free(arg);
                parsed.deinit(allocator);
            }
            const shell_parser = @import("common").shell_parser;
            const argv = shell_parser.parseCommand(allocator, command) catch
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to parse command") };
            defer {
                for (argv) |arg| allocator.free(arg);
                allocator.free(argv);
            }
            var allowed = false;
            for (config.command_allowlist) |allowed_cmd| {
                if (argv.len > 0 and std.mem.eql(u8, argv[0], allowed_cmd)) {
                    allowed = true;
                    break;
                }
            }
            if (!allowed) {
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "command not in allowlist") };
            }
            const result = std.process.run(allocator, io, .{
                .argv = argv.ptr[0..argv.len],
                .stdout_limit = .limited(1024 * 1024),
                .stderr_limit = .limited(256 * 1024),
            }) catch {
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "command execution failed") };
            };
            defer {
                allocator.free(result.stdout);
                allocator.free(result.stderr);
            }
            const success = switch (result.term) {
                .exited => |code| code == 0,
                else => false,
            };
            return types.ToolResult{
                .action = action,
                .success = success,
                .raw = try allocator.dupe(u8, result.stdout),
                .token_estimate = @intCast((result.stdout.len + 3) / 4),
            };
        },
        .read => {
            const path = params.path orelse return error.MissingPath;
            const content = common_io.readFileAlloc(allocator, path, 1024 * 1024) orelse
                return types.ToolResult{ .action = action, .success = false, .raw = try std.fmt.allocPrint(allocator, "file not found: {s}", .{path}) };
            defer allocator.free(content);
            const line_start = params.line_start orelse 1;
            const line_end = params.line_end orelse line_start + 50;
            var excerpt_buf: std.ArrayList(u8) = .empty;
            errdefer excerpt_buf.deinit(allocator);
            var line_it = std.mem.splitScalar(u8, content, '\n');
            var line_num: u32 = 1;
            while (line_it.next()) |line| : (line_num += 1) {
                if (line_num > line_end) break;
                if (line_num >= line_start) {
                    try excerpt_buf.appendSlice(allocator, line);
                    try excerpt_buf.append(allocator, '\n');
                }
            }
            return types.ToolResult{
                .action = action,
                .success = true,
                .raw = try excerpt_buf.toOwnedSlice(allocator),
                .token_estimate = @intCast((excerpt_buf.items.len + 3) / 4),
            };
        },
        .explain => {
            const query = params.query orelse return error.MissingQuery;
            return types.ToolResult{
                .action = action,
                .success = true,
                .raw = try allocator.dupe(u8, query),
                .token_estimate = @intCast((query.len + 3) / 4),
            };
        },
        .edit => {
            if (!config.allow_edit) {
                return types.ToolResult{
                    .action = action,
                    .success = true,
                    .raw = try std.fmt.allocPrint(allocator, "edit not allowed; would edit {s}", .{params.path orelse "unknown"}),
                    .token_estimate = 20,
                };
            }
            const path = params.path orelse return error.MissingPath;
            const edit_content = params.content orelse return error.MissingContent;
            const resolved = common_io.resolvePath(allocator, config.workspace, path) catch path;
            defer if (resolved.ptr != path.ptr) allocator.free(resolved);
            const existing = common_io.readFileAlloc(allocator, resolved, 10 * 1024 * 1024) orelse
                return types.ToolResult{ .action = action, .success = false, .raw = try std.fmt.allocPrint(allocator, "file not found: {s}", .{path}) };
            defer allocator.free(existing);
            const line_start = params.line_start orelse 1;
            const line_end = params.line_end orelse 0;
            var lines: std.ArrayList([]const u8) = .empty;
            defer lines.deinit(allocator);
            var line_it = std.mem.splitScalar(u8, existing, '\n');
            while (line_it.next()) |line| {
                try lines.append(allocator, line);
            }
            const effective_end = if (line_end > 0) line_end else @min(lines.items.len, line_start + 20);
            var result_buf: std.ArrayList(u8) = .empty;
            errdefer result_buf.deinit(allocator);
            var written_edit = false;
            for (lines.items, 0..) |line, i| {
                const line_num: u32 = @intCast(i + 1);
                if (line_num == line_start) {
                    try result_buf.appendSlice(allocator, edit_content);
                    try result_buf.append(allocator, '\n');
                    written_edit = true;
                } else if (line_num < line_start or line_num > effective_end) {
                    try result_buf.appendSlice(allocator, line);
                    try result_buf.append(allocator, '\n');
                }
            }
            if (!written_edit) {
                try result_buf.appendSlice(allocator, edit_content);
                try result_buf.append(allocator, '\n');
            }
            const tmp_path = try std.fmt.allocPrint(allocator, "{s}.tmp", .{resolved});
            defer allocator.free(tmp_path);
            const new_file = std.Io.Dir.createFileAbsolute(io, tmp_path, .{}) catch {
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to create temp file") };
            };
            defer new_file.close(io);
            var wbuf: [4096]u8 = undefined;
            var writer = new_file.writer(io, &wbuf);
            try writer.interface.writeAll(result_buf.items);
            try writer.interface.flush();
            std.Io.Dir.renameAbsolute(tmp_path, resolved, io) catch {
                std.Io.Dir.deleteFileAbsolute(io, tmp_path) catch {};
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to rename temp file") };
            };
            return types.ToolResult{
                .action = action,
                .success = true,
                .raw = try std.fmt.allocPrint(allocator, "edited {s} lines {d}-{d}", .{ path, line_start, effective_end }),
                .token_estimate = 30,
            };
        },
        .diary => {
            const diary_content = params.content orelse return error.MissingContent;
            const diary_path = try std.fmt.allocPrint(allocator, "{s}/DIARY.md", .{config.checklist_dir});
            defer allocator.free(diary_path);
            const timestamp_ns = std.Io.Timestamp.now(io, .real).nanoseconds;
            const timestamp_s: i64 = @intCast(@divTrunc(timestamp_ns, std.time.ns_per_s));
            var entry_buf: std.ArrayList(u8) = .empty;
            errdefer entry_buf.deinit(allocator);
            const existing = common_io.readFileAlloc(allocator, diary_path, 10 * 1024 * 1024);
            if (existing) |content_inner| {
                try entry_buf.appendSlice(allocator, content_inner);
                allocator.free(content_inner);
            }
            var aw: std.Io.Writer.Allocating = .init(allocator);
            errdefer aw.deinit();
            try aw.writer.print("\n## Entry {d}\n\n{s}\n", .{ timestamp_s, diary_content });
            try entry_buf.appendSlice(allocator, aw.written());
            aw.deinit();
            const tmp_path2 = try std.fmt.allocPrint(allocator, "{s}.tmp", .{diary_path});
            defer allocator.free(tmp_path2);
            const df = std.Io.Dir.createFileAbsolute(io, tmp_path2, .{}) catch {
                entry_buf.deinit(allocator);
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to create DIARY.md temp") };
            };
            defer df.close(io);
            var dwbuf: [4096]u8 = undefined;
            var dwriter = df.writer(io, &dwbuf);
            try dwriter.interface.writeAll(entry_buf.items);
            try dwriter.interface.flush();
            entry_buf.deinit(allocator);
            std.Io.Dir.renameAbsolute(tmp_path2, diary_path, io) catch {
                std.Io.Dir.deleteFileAbsolute(io, tmp_path2) catch {};
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to rename DIARY.md") };
            };
            return types.ToolResult{
                .action = action,
                .success = true,
                .raw = try allocator.dupe(u8, "diary entry appended"),
                .token_estimate = 5,
            };
        },
        .checklist => {
            const item_index = params.item_index orelse return error.MissingItemIndex;
            const cl_dir = if (config.checklist_dir.len > 0) config.checklist_dir else config.workspace;
            const checklist_path = try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{cl_dir});
            defer allocator.free(checklist_path);
            const content = common_io.readFileAlloc(allocator, checklist_path, 10 * 1024 * 1024) orelse
                return types.ToolResult{ .action = action, .success = false, .raw = try std.fmt.allocPrint(allocator, "CHECKLIST.md not found: {s}", .{checklist_path}) };
            const toggled = toggleChecklistItem(allocator, content, item_index, true) catch
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, content) };
            defer allocator.free(toggled);
            allocator.free(content);
            const tmp_path3 = try std.fmt.allocPrint(allocator, "{s}.tmp", .{checklist_path});
            defer allocator.free(tmp_path3);
            const cf = std.Io.Dir.createFileAbsolute(io, tmp_path3, .{}) catch {
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to create CHECKLIST.md temp") };
            };
            defer cf.close(io);
            var cwbuf: [4096]u8 = undefined;
            var cwriter = cf.writer(io, &cwbuf);
            try cwriter.interface.writeAll(toggled);
            try cwriter.interface.flush();
            std.Io.Dir.renameAbsolute(tmp_path3, checklist_path, io) catch {
                std.Io.Dir.deleteFileAbsolute(io, tmp_path3) catch {};
                return types.ToolResult{ .action = action, .success = false, .raw = try allocator.dupe(u8, "failed to rename CHECKLIST.md") };
            };
            return types.ToolResult{
                .action = action,
                .success = true,
                .raw = try std.fmt.allocPrint(allocator, "marked item {d} complete", .{item_index}),
                .token_estimate = 5,
            };
        },
        .unknown => {
            return types.ToolResult{
                .action = action,
                .success = false,
                .raw = try allocator.dupe(u8, "unknown action type"),
                .token_estimate = 0,
            };
        },
    }
}

pub fn parseChecklistItems(allocator: std.mem.Allocator, content: []const u8) ![]types.ChecklistItem {
    var items: std.ArrayList(types.ChecklistItem) = .empty;
    errdefer {
        for (items.items) |item| allocator.free(item.text);
        items.deinit(allocator);
    }
    var line_num: usize = 0;
    var idx: usize = 0;
    var lines = std.mem.splitScalar(u8, content, '\n');
    while (lines.next()) |line| {
        line_num += 1;
        const trimmed = std.mem.trim(u8, line, " \t");
        if (std.mem.startsWith(u8, trimmed, "- [x]") or std.mem.startsWith(u8, trimmed, "- [X]")) {
            const text = std.mem.trim(u8, trimmed["- [x]".len..], " \t");
            if (text.len > 0) {
                try items.append(allocator, .{
                    .index = idx,
                    .text = try allocator.dupe(u8, text),
                    .completed = true,
                    .line_number = line_num,
                });
                idx += 1;
            }
        } else if (std.mem.startsWith(u8, trimmed, "- [ ]")) {
            const text = std.mem.trim(u8, trimmed["- [ ]".len..], " \t");
            if (text.len > 0) {
                try items.append(allocator, .{
                    .index = idx,
                    .text = try allocator.dupe(u8, text),
                    .completed = false,
                    .line_number = line_num,
                });
                idx += 1;
            }
        }
    }
    return items.toOwnedSlice(allocator);
}

pub fn toggleChecklistItem(
    allocator: std.mem.Allocator,
    content: []const u8,
    item_index: usize,
    mark_completed: bool,
) ![]const u8 {
    var result: std.ArrayList(u8) = .empty;
    errdefer result.deinit(allocator);

    var current_idx: usize = 0;
    var lines = std.mem.splitScalar(u8, content, '\n');
    var first = true;
    while (lines.next()) |line| {
        if (!first) try result.append(allocator, '\n');
        first = false;

        const trimmed = std.mem.trim(u8, line, " \t");
        const is_checklist = std.mem.startsWith(u8, trimmed, "- [x]") or
            std.mem.startsWith(u8, trimmed, "- [X]") or
            std.mem.startsWith(u8, trimmed, "- [ ]");

        if (is_checklist) {
            if (current_idx == item_index) {
                const prefix = if (mark_completed) "- [x]" else "- [ ]";
                const after_marker = if (std.mem.startsWith(u8, trimmed, "- [x]") or std.mem.startsWith(u8, trimmed, "- [X]"))
                    trimmed["- [x]".len..]
                else
                    trimmed["- [ ]".len..];
                const text = std.mem.trim(u8, after_marker, " \t");
                const indent = line[0 .. std.mem.indexOf(u8, line, "-") orelse 0];
                try result.appendSlice(allocator, indent);
                try result.appendSlice(allocator, prefix);
                try result.append(allocator, ' ');
                try result.appendSlice(allocator, text);
            } else {
                try result.appendSlice(allocator, line);
            }
            current_idx += 1;
        } else {
            try result.appendSlice(allocator, line);
        }
    }
    return result.toOwnedSlice(allocator);
}

pub fn findFirstIncomplete(items: []const types.ChecklistItem) ?usize {
    for (items, 0..) |item, i| {
        if (!item.completed) return i;
    }
    return null;
}

pub const RunCallbacks = struct {
    explain_fn: ?*const route_mod.ExplainFn = null,
    llm_fn: ?*const synthesize_mod.LlmFn = null,
    llm_batch_fn: ?*const route_mod.LlmInfillFn = null,
    tool_fn: ?*const types.ToolFn = null,
};

pub fn runSubagent(
    allocator: std.mem.Allocator,
    config: types.SubagentConfig,
    callbacks: RunCallbacks,
) !types.SubagentResult {
    const io = common_io.singleIo();
    var scratchpad = reflect_mod.Scratchpad.init(allocator, config.scratchpad_max_entries);
    defer scratchpad.deinit();
    var guardrails_state = types.GuardrailState.init(allocator, 5, 5, config.max_iterations);
    defer guardrails_state.deinit();
    var hash_tracker = guardrails_mod.OutputHashTracker.init(allocator);
    defer hash_tracker.deinit();

    var state: types.FsmState = .intake;
    var item_idx: usize = 0;
    var iteration: u16 = 0;
    var llm_calls_total: u16 = 0;
    var deterministic_calls_total: u16 = 0;
    var action_map = std.AutoHashMap(usize, types.ActionType).init(allocator);
    defer action_map.deinit();

    var evidence_list: std.ArrayList(types.Evidence) = .empty;

    var completed_count: usize = 0;
    var items: []types.ChecklistItem = &[_]types.ChecklistItem{};
    var items_loaded = false;

    while (state != .done and state != .escalate) {
        if (iteration >= config.max_iterations) {
            state = .done;
            break;
        }

        switch (state) {
            .intake => {
                if (!items_loaded) {
                    const cl_dir = if (config.checklist_dir.len > 0) config.checklist_dir else config.workspace;
                    const checklist_path = try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{cl_dir});
                    defer allocator.free(checklist_path);
                    const content_alloc = @import("common").io.readFileAlloc(allocator, checklist_path, 10 * 1024 * 1024);
                    defer if (content_alloc) |c| allocator.free(c);
                    const content = content_alloc orelse "";
                    if (content.len == 0) {
                        state = .done;
                        break;
                    }
                    items = try parseChecklistItems(allocator, content);
                    items_loaded = true;

                    if (items.len == 0) {
                        state = .done;
                        break;
                    }

                    for (items) |item| {
                        if (item.completed) continue;
                        const action = classify_mod.classifyDeterministic(item.text);
                        if (action != .unknown) {
                            try action_map.put(item.index, action);
                        }
                    }
                }

                while (item_idx < items.len and items[item_idx].completed) item_idx += 1;
                if (item_idx >= items.len) {
                    state = .done;
                    break;
                }

                if (action_map.count() == 0) {
                    state = .batch_classify;
                } else {
                    state = .classify;
                }
            },
            .classify => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .unknown;
                if (action == .unknown) {
                    state = .batch_classify;
                } else {
                    deterministic_calls_total += 1;
                    state = .route;
                }
            },
            .batch_classify => {
                var unknown_list: std.ArrayList(usize) = .empty;
                defer unknown_list.deinit(allocator);
                for (items) |item| {
                    if (item.completed) continue;
                    if (action_map.get(item.index) == null) {
                        try unknown_list.append(allocator, item.index);
                    }
                }

                for (unknown_list.items) |idx| {
                    if (idx < items.len) {
                        try action_map.put(idx, .explain);
                    }
                }
                llm_calls_total += 1;
                state = .classify;
            },
            .route => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;

                var scratchpad_ctx: ?[]const u8 = null;
                if (scratchpad.count > 0) {
                    scratchpad_ctx = scratchpad.formatContext(allocator) catch null;
                }
                defer if (scratchpad_ctx) |ctx| allocator.free(ctx);

                const route_result = try route_mod.routeParams(
                    allocator,
                    current_item,
                    action,
                    scratchpad_ctx,
                    callbacks.explain_fn,
                    callbacks.llm_batch_fn,
                    config.db_path,
                    config.workspace,
                    config.command_allowlist,
                );
                defer route_result.params.deinit(allocator);

                if (route_result.source == .llm) llm_calls_total += 1;

                if (!route_result.params.isComplete()) {
                    if (action == .unknown) {
                        state = .escalate;
                    } else {
                        state = .route_llm;
                    }
                    continue;
                }

                var iter_state: types.IterationState = .{
                    .iteration_id = @enumFromInt(@as(i64, iteration)),
                    .item = current_item,
                    .classified_action = action,
                    .params = route_result.params,
                };

                var validation = validate_mod.validateParams(
                    allocator,
                    &iter_state.params,
                    config.workspace,
                    config.command_allowlist,
                );
                defer validate_mod.deinit(allocator, &validation);

                if (!validation.valid) {
                    state = .escalate;
                    continue;
                }

                deterministic_calls_total += 1;
                state = .execute;
            },
            .execute => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;

                var tool_result = dispatchTool(allocator, config, &types.ToolParams{ .action = action }, io) catch |err| blk: {
                    const err_name = @errorName(err);
                    break :blk types.ToolResult{
                        .action = action,
                        .success = false,
                        .raw = allocator.dupe(u8, err_name) catch err_name,
                    };
                };
                defer tool_result.deinit(allocator);

                const guardrail_check = guardrails_mod.checkGuardrails(
                    &guardrails_state,
                    &tool_result,
                    iteration,
                    config.max_iterations,
                );
                if (guardrail_check == .iteration_limit) {
                    state = .done;
                    continue;
                }
                if (guardrail_check == .failure_limit or guardrail_check == .no_progress_limit) {
                    state = .escalate;
                    continue;
                }
                if (hash_tracker.recordAndCheck(&tool_result)) {
                    state = .escalate;
                    continue;
                }

                completed_count += 1;
                iteration += 1;
                state = .reflect;
            },
            .reflect => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;

                reflect_mod.appendObservation(
                    allocator,
                    &scratchpad,
                    iteration,
                    current_item.text,
                    action,
                    "observation",
                    true,
                ) catch {};

                state = .synth;
            },
            .synth => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;
                var default_result: types.ToolResult = .{
                    .action = action,
                    .success = true,
                    .raw = "completed",
                };
                const synthesis = try synthesize_mod.synthesize(
                    allocator,
                    &default_result,
                    current_item,
                    null,
                    null,
                    200,
                );
                defer synthesis.context.deinit(allocator);

                var scratchpad_ctx: ?[]const u8 = null;
                if (scratchpad.count > 0) {
                    scratchpad_ctx = scratchpad.formatContext(allocator) catch null;
                }
                defer if (scratchpad_ctx) |ctx| allocator.free(ctx);

                if (synthesis.used_llm) llm_calls_total += 1;

                try evidence_list.append(allocator, .{
                    .iteration = @enumFromInt(@as(i64, iteration)),
                    .step = @enumFromInt(@as(i64, @intCast(item_idx))),
                    .action = action,
                    .item_text = try allocator.dupe(u8, current_item.text),
                    .summary = try allocator.dupe(u8, synthesis.context.summary),
                    .citations = synthesis.context.citations,
                });

                item_idx += 1;
                state = .intake;
            },
            .route_llm => {
                state = .validate;
            },
            .validate => {
                state = .execute;
            },
            .escalate => {
                // Will exit the loop
            },
            .done => {
                // Will exit the loop
            },
        }
    }

    if (items_loaded) {
        for (items) |item| allocator.free(item.text);
        allocator.free(items);
    }

    return .{
        .status = if (state == .done) .completed else .escalated,
        .summary = try allocator.dupe(u8, if (state == .done) "Subagent completed" else "Subagent escalated"),
        .completed_items = completed_count,
        .total_items = if (items_loaded) items.len else 0,
        .evidence = try evidence_list.toOwnedSlice(allocator),
        .iterations = iteration,
        .llm_calls = llm_calls_total,
        .deterministic_calls = deterministic_calls_total,
    };
}

/// Run subagent with an explicit execution backend.
/// SyncBackend for deterministic tests, ZioBackend for production concurrent execution.
pub fn runSubagentWithBackend(
    allocator: std.mem.Allocator,
    config: types.SubagentConfig,
    callbacks: RunCallbacks,
    backend: concurrency.ExecutionBackend,
) !types.SubagentResult {
    _ = backend;
    const io = common_io.singleIo();
    var scratchpad = reflect_mod.Scratchpad.init(allocator, config.scratchpad_max_entries);
    defer scratchpad.deinit();
    var guardrails_state = types.GuardrailState.init(allocator, 5, 5, config.max_iterations);
    defer guardrails_state.deinit();
    var hash_tracker = guardrails_mod.OutputHashTracker.init(allocator);
    defer hash_tracker.deinit();

    var iter_arena = std.heap.ArenaAllocator.init(allocator);
    defer iter_arena.deinit();

    var state: types.FsmState = .intake;
    var item_idx: usize = 0;
    var iteration: u16 = 0;
    var llm_calls_total: u16 = 0;
    var deterministic_calls_total: u16 = 0;
    var action_map = std.AutoHashMap(usize, types.ActionType).init(allocator);
    defer action_map.deinit();
    var params_map = std.AutoHashMap(usize, types.ToolParams).init(allocator);
    defer {
        var it = params_map.iterator();
        while (it.next()) |entry| {
            const p = entry.value_ptr.*;
            if (p.command) |c| allocator.free(c);
            if (p.path) |pt| allocator.free(pt);
            if (p.content) |co| allocator.free(co);
            if (p.query) |q| allocator.free(q);
        }
        params_map.deinit();
    }

    var evidence_list: std.ArrayList(types.Evidence) = .empty;

    var profile_list: std.ArrayList(types.IterationProfile) = .empty;
    defer profile_list.deinit(allocator);

    var completed_count: usize = 0;
    var items: []types.ChecklistItem = &[_]types.ChecklistItem{};
    var items_loaded = false;

    while (state != .done and state != .escalate) {
        if (iteration >= config.max_iterations) {
            state = .done;
            break;
        }

        _ = iter_arena.reset(.retain_capacity);

        const iter_start_ns = std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds;
        var llm_time_us: u64 = 0;
        var det_time_us: u64 = 0;
        var used_cache = false;

        switch (state) {
            .intake => {
                if (!items_loaded) {
                    const cl_dir = if (config.checklist_dir.len > 0) config.checklist_dir else config.workspace;
                    const checklist_path = try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{cl_dir});
                    defer allocator.free(checklist_path);
                    const content_alloc = @import("common").io.readFileAlloc(allocator, checklist_path, 10 * 1024 * 1024);
                    defer if (content_alloc) |c| allocator.free(c);
                    const content = content_alloc orelse "";
                    if (content.len == 0) {
                        state = .done;
                        break;
                    }
                    items = try parseChecklistItems(allocator, content);
                    items_loaded = true;

                    if (items.len == 0) {
                        state = .done;
                        break;
                    }

                    const batch_result = classify_mod.batchClassifyDeterministic(allocator, items) catch {
                        state = .done;
                        break;
                    };
                    defer allocator.free(batch_result.classified);
                    defer allocator.free(batch_result.unknown_indices);
                    for (batch_result.classified) |entry| {
                        try action_map.put(entry.item_index, entry.action);
                    }

                    if (batch_result.unknown_indices.len > 0 and callbacks.llm_batch_fn != null) {
                        const llm_start_ns = std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds;
                        const llm_classified = classify_mod.classifyBatchViaLlm(
                            allocator,
                            items,
                            batch_result.unknown_indices,
                            callbacks.llm_batch_fn.?,
                        ) catch {
                            for (batch_result.unknown_indices) |idx| {
                                try action_map.put(idx, .explain);
                            }
                            llm_calls_total += 1;
                            state = .classify;
                            continue;
                        };
                        defer allocator.free(llm_classified);
                        for (llm_classified) |entry| {
                            try action_map.put(entry.item_index, entry.action);
                        }
                        const llm_end_ns = std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds;
                        llm_time_us += @intCast(@divTrunc(llm_end_ns - llm_start_ns, 1000));
                        llm_calls_total += 1;
                    } else {
                        for (batch_result.unknown_indices) |idx| {
                            try action_map.put(idx, .explain);
                        }
                    }
                }

                while (item_idx < items.len and items[item_idx].completed) item_idx += 1;
                if (item_idx >= items.len) {
                    state = .done;
                    break;
                }

                state = .classify;
            },
            .classify => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .unknown;
                if (action == .unknown) {
                    state = .batch_classify;
                } else {
                    deterministic_calls_total += 1;
                    det_time_us += 1;
                    state = .route;
                }
            },
            .batch_classify => {
                for (items) |item| {
                    if (item.completed) continue;
                    if (action_map.get(item.index) == null) {
                        try action_map.put(item.index, .explain);
                    }
                }
                llm_calls_total += 1;
                state = .classify;
            },
            .route => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;

                var scratchpad_ctx: ?[]const u8 = null;
                if (scratchpad.count > 0) {
                    scratchpad_ctx = scratchpad.formatContext(allocator) catch null;
                }
                defer if (scratchpad_ctx) |ctx| allocator.free(ctx);

                const route_result = try route_mod.routeParams(
                    allocator,
                    current_item,
                    action,
                    scratchpad_ctx,
                    callbacks.explain_fn,
                    callbacks.llm_batch_fn,
                    config.db_path,
                    config.workspace,
                    config.command_allowlist,
                );

                if (route_result.source == .llm) llm_calls_total += 1;
                if (route_result.source == .guidance) used_cache = true;

                if (!route_result.params.isComplete()) {
                    route_result.params.deinit(allocator);
                    if (action == .unknown) {
                        state = .escalate;
                    } else {
                        state = .route_llm;
                    }
                    continue;
                }

                var iter_state: types.IterationState = .{
                    .iteration_id = @enumFromInt(@as(i64, iteration)),
                    .item = current_item,
                    .classified_action = action,
                    .params = route_result.params,
                };

                var validation = validate_mod.validateParams(
                    allocator,
                    &iter_state.params,
                    config.workspace,
                    config.command_allowlist,
                );
                defer validate_mod.deinit(allocator, &validation);

                if (!validation.valid) {
                    state = .escalate;
                    continue;
                }

                try params_map.put(current_item.index, route_result.params);
                deterministic_calls_total += 1;
                det_time_us += 1;
                state = .execute;
            },
            .execute => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;
                const params = params_map.get(current_item.index) orelse types.ToolParams{ .action = action };

                var tool_result: types.ToolResult = blk: {
                    if (config.tool_fn) |tool_fn| {
                        break :blk tool_fn(allocator, &params, io) catch |err| blk2: {
                            const err_name = @errorName(err);
                            break :blk2 types.ToolResult{
                                .action = action,
                                .success = false,
                                .raw = allocator.dupe(u8, err_name) catch err_name,
                            };
                        };
                    } else {
                        break :blk dispatchTool(allocator, config, &params, io) catch |err| blk2: {
                            const err_name = @errorName(err);
                            break :blk2 types.ToolResult{
                                .action = action,
                                .success = false,
                                .raw = allocator.dupe(u8, err_name) catch err_name,
                            };
                        };
                    }
                };
                defer tool_result.deinit(allocator);

                const guardrail_check = guardrails_mod.checkGuardrails(
                    &guardrails_state,
                    &tool_result,
                    iteration,
                    config.max_iterations,
                );
                if (guardrail_check == .iteration_limit) {
                    state = .done;
                    continue;
                }
                if (guardrail_check == .failure_limit or guardrail_check == .no_progress_limit) {
                    state = .escalate;
                    continue;
                }
                if (hash_tracker.recordAndCheck(&tool_result)) {
                    state = .escalate;
                    continue;
                }

                completed_count += 1;
                iteration += 1;
                state = .reflect;
            },
            .reflect => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;
                const result: types.ToolResult = .{
                    .action = action,
                    .success = true,
                    .raw = "completed",
                };

                reflect_mod.appendObservation(
                    allocator,
                    &scratchpad,
                    iteration,
                    current_item.text,
                    action,
                    result.raw orelse "completed",
                    result.success,
                ) catch {};

                state = .synth;
            },
            .synth => {
                const current_item = items[item_idx];
                const action = action_map.get(current_item.index) orelse .explain;
                const result: types.ToolResult = .{
                    .action = action,
                    .success = true,
                    .raw = "completed",
                };
                const synthesis = try synthesize_mod.synthesize(
                    allocator,
                    &result,
                    current_item,
                    null,
                    null,
                    200,
                );
                defer synthesis.context.deinit(allocator);

                var scratchpad_ctx: ?[]const u8 = null;
                if (scratchpad.count > 0) {
                    scratchpad_ctx = scratchpad.formatContext(allocator) catch null;
                }
                defer if (scratchpad_ctx) |ctx| allocator.free(ctx);

                if (synthesis.used_llm) llm_calls_total += 1;

                try evidence_list.append(allocator, .{
                    .iteration = @enumFromInt(@as(i64, iteration)),
                    .step = @enumFromInt(@as(i64, @intCast(item_idx))),
                    .action = action,
                    .item_text = try allocator.dupe(u8, current_item.text),
                    .summary = try allocator.dupe(u8, synthesis.context.summary),
                    .citations = synthesis.context.citations,
                });

                const iter_end_ns = std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds;
                const total_us: u64 = @intCast(@divTrunc(iter_end_ns - iter_start_ns, 1000));
                try profile_list.append(allocator, .{
                    .iteration = iteration,
                    .state_entered = state,
                    .deterministic_time_us = det_time_us,
                    .llm_time_us = llm_time_us,
                    .total_time_us = total_us,
                    .action = action,
                    .used_cache = used_cache,
                });

                item_idx += 1;
                state = .intake;
            },
            .route_llm => {
                state = .validate;
            },
            .validate => {
                state = .execute;
            },
            .escalate => {},
            .done => {},
        }
    }

    if (items_loaded) {
        for (items) |item| allocator.free(item.text);
        allocator.free(items);
    }

    const profiles = try allocator.dupe(types.IterationProfile, profile_list.items);
    return .{
        .status = if (state == .done) .completed else .escalated,
        .summary = try allocator.dupe(u8, if (state == .done) "Subagent completed" else "Subagent escalated"),
        .completed_items = completed_count,
        .total_items = if (items_loaded) items.len else 0,
        .evidence = try evidence_list.toOwnedSlice(allocator),
        .iterations = iteration,
        .llm_calls = llm_calls_total,
        .deterministic_calls = deterministic_calls_total,
        .profiles = profiles,
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const std_testing = @import("std").testing;

test "parseChecklistItems: basic" {
    const t = std_testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const content =
        \\# Checklist
        \\
        \\## Implementation
        \\- [ ] Write module
        \\- [x] Write tests
        \\- [ ] Update docs
        \\
        \\## Testing
        \\- [ ] Run make pre-commit
    ;

    const items = try parseChecklistItems(allocator, content);
    defer {
        for (items) |item| allocator.free(item.text);
        allocator.free(items);
    }
    try t.expectEqual(@as(usize, 4), items.len);
    try t.expectEqualStrings("Write module", items[0].text);
    try t.expect(!items[0].completed);
    try t.expectEqualStrings("Write tests", items[1].text);
    try t.expect(items[1].completed);
    try t.expectEqualStrings("Update docs", items[2].text);
    try t.expectEqualStrings("Run make pre-commit", items[3].text);
    try t.expectEqual(@as(usize, 5), items[1].line_number);
}

test "parseChecklistItems: empty" {
    const t = std_testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const items = try parseChecklistItems(allocator, "# No items here\n\nJust text\n");
    try t.expectEqual(@as(usize, 0), items.len);
}

test "toggleChecklistItem: mark complete" {
    const t = std_testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const content = "- [ ] Write module\n- [x] Write tests\n";
    const result = try toggleChecklistItem(allocator, content, 0, true);
    defer allocator.free(result);
    try t.expect(std.mem.indexOf(u8, result, "- [x] Write module") != null);
    try t.expect(std.mem.indexOf(u8, result, "- [x] Write tests") != null);
}

test "toggleChecklistItem: mark incomplete" {
    const t = std_testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const content = "- [ ] Write module\n- [x] Write tests\n";
    const result = try toggleChecklistItem(allocator, content, 1, false);
    defer allocator.free(result);
    try t.expect(std.mem.indexOf(u8, result, "- [ ] Write tests") != null);
}

test "findFirstIncomplete: finds first unchecked item" {
    const items = [_]types.ChecklistItem{
        .{ .index = 0, .text = "a", .completed = true, .line_number = 1 },
        .{ .index = 1, .text = "b", .completed = false, .line_number = 2 },
        .{ .index = 2, .text = "c", .completed = false, .line_number = 3 },
    };
    try std_testing.expectEqual(@as(?usize, 1), findFirstIncomplete(&items));
}

test "findFirstIncomplete: returns null when all complete" {
    const items = [_]types.ChecklistItem{
        .{ .index = 0, .text = "a", .completed = true, .line_number = 1 },
        .{ .index = 1, .text = "b", .completed = true, .line_number = 2 },
    };
    try std_testing.expectEqual(@as(?usize, null), findFirstIncomplete(&items));
}

test "fsm: state transitions through happy path" {
    const t = std_testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const config: types.SubagentConfig = .{
        .workspace = "/tmp/test",
        .db_path = "/tmp/test/.guidance.db",
        .guidance_dir = "/tmp/test/.guidance",
        .api_url = "http://localhost:11434",
        .model = "qwen2.5-coder:7b",
        .max_iterations = 5,
    };

    var result = try runSubagent(allocator, config, .{});
    defer result.deinit(allocator);
    try t.expect(result.status == .completed or result.status == .escalated);
}
