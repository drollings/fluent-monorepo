//! todo.zig — Work item lifecycle tracking for guidance.
//!
//! Directory layout:
//!   .guidance/todo/
//!   ├── YYYYMMDD_HHMMSS_<slug>/
//!   │   ├── TODO.md        — human-written goal + acceptance criteria
//!   │   ├── TRIAGE.md      — LLM-generated risk + steps + dependencies
//!   │   ├── CHECKLIST.md   — LLM-generated checkboxes from TODO.md
//!   │   ├── DIARY.md       — timestamped manual entries
//!   │   └── COMMITTED.md   — auto-generated on successful commit
//!   └── archive/           — abandoned work items

const std = @import("std");
const common = @import("common");
const llm = @import("llm");
const subagent_types = @import("types.zig");
const subagent_fsm = @import("fsm.zig");
const subagent_builder = @import("builder.zig");

// ---------------------------------------------------------------------------
// Directory helpers
// ---------------------------------------------------------------------------

/// Fetches the current work item from a directory, returning its slice of data.
pub fn findCurrentWorkItem(allocator: std.mem.Allocator, todo_dir: []const u8) !?[]const u8 {
    const io = common.io.singleIo();
    var dir = std.Io.Dir.openDirAbsolute(io, todo_dir, .{ .iterate = true }) catch return null;
    defer dir.close(io);

    // Collect candidate names (chronological order by name: YYYYMMDD_HHMMSS_...).
    var names: std.ArrayList([]const u8) = .empty;
    defer {
        for (names.items) |n| allocator.free(n);
        names.deinit(allocator);
    }

    var it = dir.iterate();
    while (try it.next(common.io.singleIo())) |entry| {
        if (entry.kind != .directory) continue;
        if (std.mem.eql(u8, entry.name, "archive")) continue;
        // Must start with digits (YYYYMMDD).
        if (entry.name.len < 9 or !std.ascii.isDigit(entry.name[0])) continue;
        try names.append(allocator, try allocator.dupe(u8, entry.name));
    }

    // Sort lexicographically descending to find newest first.
    std.sort.block([]const u8, names.items, {}, struct {
        fn gt(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.order(u8, a, b) == .gt;
        }
    }.gt);

    // Return the first (newest) without COMMITTED.md.
    for (names.items) |name| {
        const committed = try std.fmt.allocPrint(allocator, "{s}/{s}/COMMITTED.md", .{ todo_dir, name });
        defer allocator.free(committed);
        std.Io.Dir.accessAbsolute(common.io.singleIo(), committed, .{}) catch {
            // COMMITTED.md not found — this is the current item.
            const path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ todo_dir, name });
            return path;
        };
    }
    return null;
}

/// Converts a byte slice to a slugified string by trimming spaces and converting to lowercase.
fn slugify(allocator: std.mem.Allocator, s: []const u8) ![]const u8 {
    var buf: std.ArrayList(u8) = .empty;
    var prev_dash = true; // avoid leading dashes
    for (s) |c| {
        if (std.ascii.isAlphanumeric(c)) {
            try buf.append(allocator, std.ascii.toLower(c));
            prev_dash = false;
        } else if (!prev_dash and buf.items.len > 0) {
            try buf.append(allocator, '-');
            prev_dash = true;
        }
    }
    // Trim trailing dash.
    while (buf.items.len > 0 and buf.items[buf.items.len - 1] == '-') {
        buf.items.len -= 1;
    }
    if (buf.items.len == 0) try buf.appendSlice(allocator, "work-item");
    // Cap at 40 chars.
    if (buf.items.len > 40) buf.items.len = 40;
    return buf.toOwnedSlice(allocator);
}

/// Reads a file path into a Zig-safe slice, returning the contents or an error.
fn readFileOpt(allocator: std.mem.Allocator, path: []const u8) ?[]const u8 {
    const io = common.io.singleIo();
    const f = std.Io.Dir.openFileAbsolute(io, path, .{}) catch return null;
    defer f.close(io);
    return std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(1 * 1024 * 1024)) catch null;
}

// ---------------------------------------------------------------------------
// guidance todo new
// ---------------------------------------------------------------------------

/// Creates a new todo entry with the given description and todo directory.
pub fn cmdTodoNew(allocator: std.mem.Allocator, description: []const u8, todo_dir: []const u8) !void {
    // Ensure todo directory exists.
    const io = common.io.singleIo();
    std.Io.Dir.createDirAbsolute(io, todo_dir, .default_dir) catch |e| switch (e) {
        error.PathAlreadyExists => {},
        else => return e,
    };

    // Build directory name: YYYYMMDD_HHMMSS_<slug>
    const ts: i64 = @intCast(@divTrunc(std.Io.Timestamp.now(io, .real).nanoseconds, std.time.ns_per_s));
    const epoch = std.time.epoch.EpochSeconds{ .secs = @intCast(ts) };
    const day = epoch.getDaySeconds();
    const year_day = epoch.getEpochDay().calculateYearDay();
    const month_day = year_day.calculateMonthDay();

    const slug = try slugify(allocator, if (description.len > 0) description else "work-item");
    defer allocator.free(slug);

    const dir_name = try std.fmt.allocPrint(allocator, "{d:0>4}{d:0>2}{d:0>2}_{d:0>2}{d:0>2}{d:0>2}_{s}", .{
        year_day.year,
        month_day.month.numeric(),
        month_day.day_index + 1,
        day.getHoursIntoDay(),
        day.getMinutesIntoHour(),
        day.getSecondsIntoMinute(),
        slug,
    });
    defer allocator.free(dir_name);

    const item_dir = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ todo_dir, dir_name });
    defer allocator.free(item_dir);

    try std.Io.Dir.createDirAbsolute(io, item_dir, .default_dir);

    // Write TODO.md template.
    const todo_path = try std.fmt.allocPrint(allocator, "{s}/TODO.md", .{item_dir});
    defer allocator.free(todo_path);

    const title = if (description.len > 0) description else "Describe the work item";
    const template = try std.fmt.allocPrint(allocator,
        \\# {s}
        \\
        \\## Goal
        \\<what needs to be done>
        \\
        \\## Files to change
        \\- <list files>
        \\
        \\## Acceptance criteria
        \\- [ ] <criterion 1>
        \\- [ ] <criterion 2>
        \\
    , .{title});
    defer allocator.free(template);

    const f = try std.Io.Dir.createFileAbsolute(io, todo_path, .{});
    {
        var wbuf: [4096]u8 = undefined;
        var writer = f.writer(io, &wbuf);
        try writer.interface.writeAll(template);
        try writer.interface.flush();
    }
    f.close(io);

    std.debug.print("todo: created {s}\n", .{item_dir});

    // Open $EDITOR.
    const cwd_val = try std.process.currentPathAlloc(common.io.singleIo(), allocator);
    defer allocator.free(cwd_val);

    const editor = blk: {
        if (std.c.getenv("EDITOR")) |e| {
            break :blk try allocator.dupe(u8, std.mem.span(e));
        }
        if (std.c.getenv("VISUAL")) |e| {
            break :blk try allocator.dupe(u8, std.mem.span(e));
        }
        break :blk try allocator.dupe(u8, "vi");
    };
    defer allocator.free(editor);

    const editor_result = std.process.run(allocator, io, .{ .argv = &.{ editor, todo_path }, .cwd = .{ .path = cwd_val } }) catch |err| {
        std.debug.print("todo: could not open editor ({s}): edit {s} manually\n", .{ @errorName(err), todo_path });
        return;
    };
    defer {
        allocator.free(editor_result.stdout);
        allocator.free(editor_result.stderr);
    }
}

// ---------------------------------------------------------------------------
// guidance todo triage
// ---------------------------------------------------------------------------

/// Processes a list of todo items using an allocator, returning a processed result.
pub fn cmdTodoTriage(allocator: std.mem.Allocator, todo_dir: []const u8, api_url: []const u8, model: []const u8) !void {
    const io = common.io.singleIo();
    const item_dir = (try findCurrentWorkItem(allocator, todo_dir)) orelse {
        std.debug.print("todo triage: no current work item found in {s}\n", .{todo_dir});
        return;
    };
    defer allocator.free(item_dir);

    const todo_path = try std.fmt.allocPrint(allocator, "{s}/TODO.md", .{item_dir});
    defer allocator.free(todo_path);

    const todo_content = readFileOpt(allocator, todo_path) orelse {
        std.debug.print("todo triage: cannot read {s}\n", .{todo_path});
        return;
    };
    defer allocator.free(todo_content);

    const triage_path = try std.fmt.allocPrint(allocator, "{s}/TRIAGE.md", .{item_dir});
    defer allocator.free(triage_path);

    const system_prompt =
        \\You are a senior software engineer reviewing a work item.
        \\Respond only with a TRIAGE.md document in Markdown.
        \\No preamble, no explanation.
    ;

    const prompt = try std.fmt.allocPrint(allocator,
        \\Produce a TRIAGE.md for this work item.  Format:
        \\
        \\# Triage: <title>
        \\
        \\**Risk**: Low | Medium | High
        \\**Estimated effort**: <range>
        \\
        \\## Implementation steps
        \\1. ...
        \\
        \\## Files affected
        \\- ...
        \\
        \\## Dependencies
        \\- None  (or list work items / external requirements)
        \\
        \\---
        \\
        \\Work item TODO.md:
        \\
        \\{s}
    , .{todo_content});
    defer allocator.free(prompt);

    const response = callLlm(allocator, api_url, model, prompt, system_prompt, 2000) orelse {
        std.debug.print("todo triage: LLM unavailable — writing stub TRIAGE.md\n", .{});
        const stub = try std.fmt.allocPrint(allocator,
            \\# Triage: (pending)
            \\
            \\**Risk**: Unknown
            \\
            \\## Implementation steps
            \\1. Review TODO.md and fill in manually.
            \\
            \\## Files affected
            \\- TBD
            \\
            \\## Dependencies
            \\- None
            \\
        , .{});
        defer allocator.free(stub);
        const wf = try std.Io.Dir.createFileAbsolute(io, triage_path, .{});
        defer wf.close(io);
        var wbuf: [4096]u8 = undefined;
        var writer = wf.writer(io, &wbuf);
        try writer.interface.writeAll(stub);
        try writer.interface.flush();
        return;
    };
    defer allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const wf = try std.Io.Dir.createFileAbsolute(io, triage_path, .{});
    defer wf.close(io);
    {
        var wbuf: [4096]u8 = undefined;
        var writer = wf.writer(io, &wbuf);
        try writer.interface.writeAll(stripped);
        if (!std.mem.endsWith(u8, stripped, "\n")) try writer.interface.writeAll("\n");
        try writer.interface.flush();
    }
    std.debug.print("todo triage: wrote {s}\n", .{triage_path});
}

// ---------------------------------------------------------------------------
// guidance todo checklist
// ---------------------------------------------------------------------------

/// Processes a todo list file by allocating memory and validating its contents.
pub fn cmdTodoChecklist(allocator: std.mem.Allocator, todo_dir: []const u8, api_url: []const u8, model: []const u8) !void {
    const io = common.io.singleIo();
    const item_dir = (try findCurrentWorkItem(allocator, todo_dir)) orelse {
        std.debug.print("todo checklist: no current work item found\n", .{});
        return;
    };
    defer allocator.free(item_dir);

    const todo_path = try std.fmt.allocPrint(allocator, "{s}/TODO.md", .{item_dir});
    defer allocator.free(todo_path);

    const todo_content = readFileOpt(allocator, todo_path) orelse {
        std.debug.print("todo checklist: cannot read {s}\n", .{todo_path});
        return;
    };
    defer allocator.free(todo_content);

    const checklist_path = try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{item_dir});
    defer allocator.free(checklist_path);

    const system_prompt =
        \\You are a software engineer generating an implementation checklist.
        \\Respond only with a CHECKLIST.md document.  Use "- [ ]" for each item.
        \\No preamble.
    ;

    const prompt = try std.fmt.allocPrint(allocator,
        \\Produce a CHECKLIST.md for this work item.  Format:
        \\
        \\# Checklist: <title>
        \\
        \\## Implementation
        \\- [ ] ...
        \\
        \\## Testing
        \\- [ ] Run `make pre-commit`
        \\
        \\## Documentation
        \\- [ ] Update STRUCTURE.md if needed
        \\
        \\---
        \\
        \\Work item:
        \\
        \\{s}
    , .{todo_content});
    defer allocator.free(prompt);

    const response = callLlm(allocator, api_url, model, prompt, system_prompt, 1500) orelse {
        std.debug.print("todo checklist: LLM unavailable — writing stub CHECKLIST.md\n", .{});
        const stub =
            \\# Checklist: (pending)
            \\
            \\## Implementation
            \\- [ ] (fill in manually from TODO.md)
            \\
            \\## Testing
            \\- [ ] Run `make pre-commit`
            \\
        ;
        const wf = try std.Io.Dir.createFileAbsolute(io, checklist_path, .{});
        defer wf.close(io);
        {
            var wbuf: [4096]u8 = undefined;
            var writer = wf.writer(io, &wbuf);
            try writer.interface.writeAll(stub);
            try writer.interface.flush();
        }
        return;
    };
    defer allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const wf = try std.Io.Dir.createFileAbsolute(io, checklist_path, .{});
    defer wf.close(io);
    {
        var wbuf: [4096]u8 = undefined;
        var writer = wf.writer(io, &wbuf);
        try writer.interface.writeAll(stripped);
        if (!std.mem.endsWith(u8, stripped, "\n")) try writer.interface.writeAll("\n");
        try writer.interface.flush();
    }
    std.debug.print("todo checklist: wrote {s}\n", .{checklist_path});
}

// ---------------------------------------------------------------------------
// guidance todo status
// ---------------------------------------------------------------------------

/// Updates todo status based on allocation and directory data.
pub fn cmdTodoStatus(allocator: std.mem.Allocator, todo_dir: []const u8) !void {
    const io = common.io.singleIo();
    var dir = std.Io.Dir.openDirAbsolute(io, todo_dir, .{ .iterate = true }) catch {
        std.debug.print("todo status: no todo directory at {s}\n", .{todo_dir});
        return;
    };
    defer dir.close(io);

    var names: std.ArrayList([]const u8) = .empty;
    defer {
        for (names.items) |n| allocator.free(n);
        names.deinit(allocator);
    }

    var it = dir.iterate();
    while (try it.next(io)) |entry| {
        if (entry.kind != .directory) continue;
        if (std.mem.eql(u8, entry.name, "archive")) continue;
        if (entry.name.len < 9 or !std.ascii.isDigit(entry.name[0])) continue;
        try names.append(allocator, try allocator.dupe(u8, entry.name));
    }
    std.sort.block([]const u8, names.items, {}, struct {
        fn lt(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.order(u8, a, b) == .lt;
        }
    }.lt);

    const current = try findCurrentWorkItem(allocator, todo_dir);
    defer if (current) |c| allocator.free(c);

    std.debug.print("Work items in {s}:\n\n", .{todo_dir});
    for (names.items) |name| {
        const item_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ todo_dir, name });
        defer allocator.free(item_path);

        const committed_path = try std.fmt.allocPrint(allocator, "{s}/COMMITTED.md", .{item_path});
        defer allocator.free(committed_path);

        const is_committed = if (std.Io.Dir.accessAbsolute(common.io.singleIo(), committed_path, .{})) |_| true else |_| false;
        const is_current = if (current) |c| std.mem.eql(u8, c, item_path) else false;

        // Read CHECKLIST.md for completion %.
        const cl_path = try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{item_path});
        defer allocator.free(cl_path);

        var total_items: usize = 0;
        var done_items: usize = 0;
        if (readFileOpt(allocator, cl_path)) |cl| {
            defer allocator.free(cl);
            var lines = std.mem.splitScalar(u8, cl, '\n');
            while (lines.next()) |line| {
                if (std.mem.startsWith(u8, line, "- [x]") or std.mem.startsWith(u8, line, "- [X]")) {
                    total_items += 1;
                    done_items += 1;
                } else if (std.mem.startsWith(u8, line, "- [ ]")) {
                    total_items += 1;
                }
            }
        }

        // Read title from TODO.md first line.
        var title: []const u8 = name;
        const todo_path = try std.fmt.allocPrint(allocator, "{s}/TODO.md", .{item_path});
        defer allocator.free(todo_path);
        if (readFileOpt(allocator, todo_path)) |td| {
            defer allocator.free(td);
            var tlines = std.mem.splitScalar(u8, td, '\n');
            if (tlines.next()) |first| {
                const stripped = std.mem.trim(u8, first, "# \t");
                if (stripped.len > 0) title = stripped;
            }
        }

        const status_str: []const u8 = if (is_committed) "committed" else if (is_current) "► current" else "pending";
        const marker: []const u8 = if (is_current) "* " else "  ";

        if (total_items > 0) {
            const pct = done_items * 100 / total_items;
            std.debug.print("{s}[{s}] {s} ({d}%)\n", .{ marker, status_str, title, pct });
        } else {
            std.debug.print("{s}[{s}] {s}\n", .{ marker, status_str, title });
        }
    }

    if (names.items.len == 0) std.debug.print("  (no work items)\n", .{});
}

// ---------------------------------------------------------------------------
// guidance todo list
// ---------------------------------------------------------------------------

/// Processes a list of todo strings and returns a modified allocation.
pub fn cmdTodoList(allocator: std.mem.Allocator, todo_dir: []const u8) !void {
    const io = common.io.singleIo();
    var dir = std.Io.Dir.openDirAbsolute(io, todo_dir, .{ .iterate = true }) catch {
        std.debug.print("todo list: no todo directory at {s}\n", .{todo_dir});
        return;
    };
    defer dir.close(io);

    var names: std.ArrayList([]const u8) = .empty;
    defer {
        for (names.items) |n| allocator.free(n);
        names.deinit(allocator);
    }
    var it = dir.iterate();
    while (try it.next(io)) |entry| {
        if (entry.kind != .directory) continue;
        if (std.mem.eql(u8, entry.name, "archive")) continue;
        if (entry.name.len < 9 or !std.ascii.isDigit(entry.name[0])) continue;
        try names.append(allocator, try allocator.dupe(u8, entry.name));
    }
    std.sort.block([]const u8, names.items, {}, struct {
        fn lt(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.order(u8, a, b) == .lt;
        }
    }.lt);

    for (names.items) |name| {
        const item_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ todo_dir, name });
        defer allocator.free(item_path);

        const committed_path = try std.fmt.allocPrint(allocator, "{s}/COMMITTED.md", .{item_path});
        defer allocator.free(committed_path);
        const is_committed = if (std.Io.Dir.accessAbsolute(common.io.singleIo(), committed_path, .{})) |_| true else |_| false;

        var title: []const u8 = name;
        const todo_path = try std.fmt.allocPrint(allocator, "{s}/TODO.md", .{item_path});
        defer allocator.free(todo_path);
        if (readFileOpt(allocator, todo_path)) |td| {
            defer allocator.free(td);
            var lines = std.mem.splitScalar(u8, td, '\n');
            if (lines.next()) |first| {
                const stripped = std.mem.trim(u8, first, "# \t");
                if (stripped.len > 0) title = stripped;
            }
        }

        const flag: []const u8 = if (is_committed) " ✓" else "";
        std.debug.print("{s}{s}  {s}\n", .{ name, flag, title });
    }

    if (names.items.len == 0) std.debug.print("(no work items)\n", .{});
}

// ---------------------------------------------------------------------------
// guidance todo abandon
// ---------------------------------------------------------------------------

/// Processes a todo directory allocation, removing abandoned tasks.
pub fn cmdTodoAbandon(allocator: std.mem.Allocator, todo_dir: []const u8) !void {
    const item_dir = (try findCurrentWorkItem(allocator, todo_dir)) orelse {
        std.debug.print("todo abandon: no current work item to abandon\n", .{});
        return;
    };
    defer allocator.free(item_dir);

    const archive_dir = try std.fmt.allocPrint(allocator, "{s}/archive", .{todo_dir});
    defer allocator.free(archive_dir);

    const io = common.io.singleIo();
    std.Io.Dir.createDirAbsolute(io, archive_dir, .default_dir) catch |e| switch (e) {
        error.PathAlreadyExists => {},
        else => return e,
    };

    // item_dir ends in /YYYYMMDD_HHMMSS_slug; extract basename.
    const basename = std.fs.path.basename(item_dir);
    const dest = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ archive_dir, basename });
    defer allocator.free(dest);

    _ = std.process.run(allocator, io, .{ .argv = &.{ "mv", item_dir, dest } }) catch |err| {
        std.debug.print("todo abandon: could not move to archive ({s}), manual: mv {s} {s}\n", .{ @errorName(err), item_dir, dest });
        return;
    };
    std.debug.print("todo abandon: moved {s} to archive/\n", .{basename});
}

// ---------------------------------------------------------------------------
// Checklist query (used by cmdCommit)
// ---------------------------------------------------------------------------

/// Tracks checklist completion status with ownership model; ensures invariants are preserved.
pub const ChecklistStatus = struct {
    total: usize,
    incomplete: usize,
    item_dir: ?[]const u8,
};

/// Checks the status of a todo directory using an allocator and returns the status code.
pub fn queryChecklistStatus(allocator: std.mem.Allocator, todo_dir: []const u8) !ChecklistStatus {
    const item_dir = try findCurrentWorkItem(allocator, todo_dir);

    const cl_path = if (item_dir) |d|
        try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{d})
    else
        return .{ .total = 0, .incomplete = 0, .item_dir = null };
    defer allocator.free(cl_path);

    const content = readFileOpt(allocator, cl_path) orelse
        return .{ .total = 0, .incomplete = 0, .item_dir = item_dir };
    defer allocator.free(content);

    var total: usize = 0;
    var incomplete: usize = 0;
    var lines = std.mem.splitScalar(u8, content, '\n');
    while (lines.next()) |line| {
        if (std.mem.startsWith(u8, line, "- [ ]")) {
            total += 1;
            incomplete += 1;
        } else if (std.mem.startsWith(u8, line, "- [x]") or std.mem.startsWith(u8, line, "- [X]")) {
            total += 1;
        }
    }
    return .{ .total = total, .incomplete = incomplete, .item_dir = item_dir };
}

/// Writes a commit hash to the specified directory with MD5 verification.
pub fn writeCommittedMd(
    allocator: std.mem.Allocator,
    item_dir: []const u8,
    commit_hash: []const u8,
    summary: []const u8,
    changed_files: []const []const u8,
) !void {
    const io = common.io.singleIo();
    const committed_path = try std.fmt.allocPrint(allocator, "{s}/COMMITTED.md", .{item_dir});
    defer allocator.free(committed_path);

    const ts: i64 = @intCast(@divTrunc(std.Io.Timestamp.now(io, .real).nanoseconds, std.time.ns_per_s));
    const epoch = std.time.epoch.EpochSeconds{ .secs = @intCast(ts) };
    const day = epoch.getDaySeconds();
    const year_day = epoch.getEpochDay().calculateYearDay();
    const month_day = year_day.calculateMonthDay();

    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(allocator);

    const basename = std.fs.path.basename(item_dir);
    try buf.appendSlice(allocator, "# Committed: ");
    try buf.appendSlice(allocator, basename);
    try buf.appendSlice(allocator, "\n\n");
    try buf.appendSlice(allocator, "**Commit**: `");
    try buf.appendSlice(allocator, commit_hash);
    try buf.appendSlice(allocator, "`\n");
    try buf.appendSlice(allocator, "**Date**: ");
    var time_buf: [32]u8 = undefined;
    const time_str = std.fmt.bufPrint(&time_buf, "{d:0>4}-{d:0>2}-{d:0>2}T{d:0>2}:{d:0>2}:{d:0>2}Z\n", .{
        year_day.year,
        month_day.month.numeric(),
        month_day.day_index + 1,
        day.getHoursIntoDay(),
        day.getMinutesIntoHour(),
        day.getSecondsIntoMinute(),
    }) catch unreachable;
    try buf.appendSlice(allocator, time_str);
    try buf.appendSlice(allocator, "\n## Summary\n\n");
    try buf.appendSlice(allocator, summary);
    if (!std.mem.endsWith(u8, summary, "\n")) try buf.append(allocator, '\n');
    try buf.appendSlice(allocator, "\n## Files changed\n\n");
    for (changed_files) |f| {
        try buf.appendSlice(allocator, "- ");
        try buf.appendSlice(allocator, f);
        try buf.append(allocator, '\n');
    }

    const cf = try std.Io.Dir.createFileAbsolute(io, committed_path, .{});
    defer cf.close(io);
    var wbuf: [4096]u8 = undefined;
    var writer = cf.writer(io, &wbuf);
    try writer.interface.writeAll(buf.items);
    try writer.interface.flush();
}

// ---------------------------------------------------------------------------
// LLM helper
// ---------------------------------------------------------------------------

/// Transforms an API response into a Zig array slice with specified token limits.
fn callLlm(
    allocator: std.mem.Allocator,
    api_url: []const u8,
    model: []const u8,
    prompt: []const u8,
    system: []const u8,
    max_tokens: usize,
) ?[]const u8 {
    if (model.len == 0) return null;
    const config: llm.LlmConfig = .{ .api_url = api_url, .model = model };
    var client = llm.LlmClient.init(allocator, config) catch return null;
    defer client.deinit();
    if (!client.available()) return null;
    return client.completeOrNull(prompt, max_tokens, 0.3, system);
}

// ---------------------------------------------------------------------------
// todo run — Subagent FSM execution
// ---------------------------------------------------------------------------

const subagent = @import("root.zig");

pub fn cmdTodoRun(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    db_path: []const u8,
    guidance_dir: []const u8,
    api_url: []const u8,
    model: []const u8,
    max_iterations: u16,
    allow_edit: bool,
) !subagent_types.SubagentResult {
    var b = subagent_builder.builder(allocator);
    _ = b.workspace(workspace);
    _ = b.dbPath(db_path);
    _ = b.guidanceDir(guidance_dir);
    _ = b.apiUrl(api_url);
    _ = b.model(model);
    _ = b.maxIterations(max_iterations);
    if (allow_edit) _ = b.allowEdit(true);
    const config = try b.build();
    defer {
        allocator.free(config.workspace);
        allocator.free(config.db_path);
        allocator.free(config.guidance_dir);
        allocator.free(config.api_url);
        allocator.free(config.model);
    }

    const callbacks: subagent_fsm.RunCallbacks = .{};
    return subagent_fsm.runSubagent(allocator, config, callbacks);
}

// =============================================================================
// Tests
// =============================================================================

test "slugify: basic" {
    const t = std.testing;
    const allocator = t.allocator;

    const s = try slugify(allocator, "Fix memory leak in cache");
    defer allocator.free(s);
    try t.expectEqualStrings("fix-memory-leak-in-cache", s);
}

test "slugify: special chars stripped" {
    const t = std.testing;
    const allocator = t.allocator;

    const s = try slugify(allocator, "Add --flag support!");
    defer allocator.free(s);
    try t.expectEqualStrings("add-flag-support", s);
}

test "slugify: empty becomes work-item" {
    const t = std.testing;
    const allocator = t.allocator;

    const s = try slugify(allocator, "");
    defer allocator.free(s);
    try t.expectEqualStrings("work-item", s);
}
