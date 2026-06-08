//! reflect.zig — Scratchpad management for the subagent FSM.
//!
//! Implements a ring-buffer scratchpad that accumulates observations and
//! reasoning from prior iterations. Provides O(1) append and eviction,
//! sentence-boundary truncation for observations, and token estimation
//! for context budgeting.

const std = @import("std");
const types = @import("types.zig");
const string_mod = @import("common").string;

pub const Scratchpad = struct {
    ring: std.ArrayList(types.ScratchpadEntry),
    head: usize = 0,
    count: usize = 0,
    max_entries: u16,
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, max_entries: u16) Scratchpad {
        return .{
            .ring = .empty,
            .max_entries = max_entries,
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *Scratchpad) void {
        for (self.ring.items) |entry| {
            self.freeEntry(entry);
        }
        self.ring.deinit(self.allocator);
    }

    pub fn append(self: *Scratchpad, entry: types.ScratchpadEntry) !void {
        if (self.ring.items.len < self.max_entries) {
            const owned = try self.dupEntry(entry);
            try self.ring.append(self.allocator, owned);
        } else {
            self.freeEntry(self.ring.items[self.head]);
            self.ring.items[self.head] = try self.dupEntry(entry);
            self.head = (self.head + 1) % self.max_entries;
        }
        if (self.count < self.max_entries) self.count += 1;
    }

    pub fn entries(self: *const Scratchpad) []const types.ScratchpadEntry {
        return self.ring.items;
    }

    pub fn formatContext(self: *Scratchpad, allocator: std.mem.Allocator) ![]const u8 {
        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);
        for (self.ring.items) |entry| {
            try buf.appendSlice(allocator, "- [iter ");
            var aw: std.Io.Writer.Allocating = .init(allocator);
            errdefer aw.deinit();
            try aw.writer.print("{d}] ", .{entry.iteration});
            const num = try aw.toOwnedSlice();
            defer allocator.free(num);
            try buf.appendSlice(allocator, num);
            try buf.appendSlice(allocator, entry.item_text);
            try buf.appendSlice(allocator, " -> ");
            try buf.appendSlice(allocator, @tagName(entry.action));
            try buf.appendSlice(allocator, ": ");
            try buf.appendSlice(allocator, entry.observation);
            if (!entry.success) try buf.appendSlice(allocator, " (failed)");
            try buf.appendSlice(allocator, "\n");
        }
        return buf.toOwnedSlice(allocator);
    }

    fn dupEntry(self: *const Scratchpad, entry: types.ScratchpadEntry) !types.ScratchpadEntry {
        return .{
            .iteration = entry.iteration,
            .item_text = try self.allocator.dupe(u8, entry.item_text),
            .action = entry.action,
            .observation = try self.allocator.dupe(u8, entry.observation),
            .reasoning = try self.allocator.dupe(u8, entry.reasoning),
            .success = entry.success,
        };
    }

    fn freeEntry(self: *Scratchpad, entry: types.ScratchpadEntry) void {
        self.allocator.free(entry.item_text);
        self.allocator.free(entry.observation);
        self.allocator.free(entry.reasoning);
    }
};

pub fn appendObservation(
    allocator: std.mem.Allocator,
    scratchpad: *Scratchpad,
    iteration: u16,
    item_text: []const u8,
    action: types.ActionType,
    raw_output: []const u8,
    success: bool,
) !void {
    const truncated = string_mod.truncateAtSentence(allocator, raw_output, 300) catch raw_output;
    defer if (truncated.ptr != raw_output.ptr) allocator.free(truncated);
    try scratchpad.append(.{
        .iteration = iteration,
        .item_text = item_text,
        .action = action,
        .observation = truncated,
        .reasoning = "",
        .success = success,
    });
}
