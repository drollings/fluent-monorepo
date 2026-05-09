//! Forward+reverse @import dependency graph for the guidance sync pipeline.
//!
//! `DepGraph` records which source files import which other source files and
//! provides O(1) reverse lookups ("who imports this file?").  Building the
//! graph once per multi-file sync run replaces the O(N) directory walk that
//! `findReverseDeps` performs per file, reducing full-sync time from O(N²)
//! to O(N log N).
//!
//! ## Ownership model
//!
//!   - `forward` keys: owned strings (duped from the caller's `importer_rel`).
//!   - `forward` value elements: owned strings (duped from each import target).
//!   - `reverse` outer keys: owned strings (duped independently from targets).
//!   - `reverse` inner map keys: NOT owned — they are the SAME pointers stored
//!     as the forward outer keys (so no double-free in deinit/remove).
//!
//! This keeps ownership unambiguous: `forward` owns importer strings,
//! `reverse` outer map owns target strings, and inner map keys borrow from
//! `forward`.

const std = @import("std");

/// Forward+reverse @import dependency graph.
pub const DepGraph = struct {
    /// forward[importer_rel] = owned slice of resolved repo-relative import paths
    forward: std.StringHashMapUnmanaged([][]const u8),
    /// reverse[target_rel] = set of importers; inner keys borrow from forward keys
    reverse: std.StringHashMapUnmanaged(std.StringHashMapUnmanaged(void)),
    allocator: std.mem.Allocator,

    /// Initialise an empty graph.
    pub fn init(allocator: std.mem.Allocator) DepGraph {
        return .{
            .forward = .empty,
            .reverse = .empty,
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *DepGraph) void {
        // Forward: free keys + value slices + value elements.
        var fwd_it = self.forward.iterator();
        while (fwd_it.next()) |entry| {
            self.allocator.free(@constCast(entry.key_ptr.*));
            for (entry.value_ptr.*) |target| self.allocator.free(target);
            self.allocator.free(entry.value_ptr.*);
        }
        self.forward.deinit(self.allocator);

        // Reverse: free outer keys + deinit inner maps.
        // Inner map keys are borrowed from forward — do NOT free them here.
        var rev_it = self.reverse.iterator();
        while (rev_it.next()) |entry| {
            self.allocator.free(@constCast(entry.key_ptr.*));
            entry.value_ptr.deinit(self.allocator);
        }
        self.reverse.deinit(self.allocator);
    }

    /// Record that `importer_rel` imports each path in `import_targets`.
    ///
    /// Each string in `import_targets` must already be a normalized
    /// repo-relative path (e.g. `"src/common/log.zig"`).
    ///
    /// Takes ownership of `import_targets` (the slice and each element).
    /// Caller must NOT free them after this call.
    ///
    /// If `importer_rel` was previously registered, the old edges are removed
    /// and replaced with the new ones.
    pub fn setDeps(
        self: *DepGraph,
        importer_rel: []const u8,
        import_targets: [][]const u8,
    ) !void {
        const gop = try self.forward.getOrPut(self.allocator, importer_rel);
        if (gop.found_existing) {
            // Remove stale reverse inner-map edges for the old targets.
            for (gop.value_ptr.*) |old_target| {
                if (self.reverse.getPtr(old_target)) |inner| {
                    _ = inner.remove(gop.key_ptr.*);
                }
                self.allocator.free(old_target);
            }
            self.allocator.free(gop.value_ptr.*);
            // The forward key is re-used — no need to re-dupe it.
        } else {
            // New importer: dupe the key string.
            gop.key_ptr.* = try self.allocator.dupe(u8, importer_rel);
        }

        // Store the (now-owned) import_targets as the forward value.
        gop.value_ptr.* = import_targets;

        // Build reverse edges.
        // inner map keys borrow the SAME pointer as the forward key.
        const importer_ptr = gop.key_ptr.*;
        for (import_targets) |target| {
            const rev_gop = try self.reverse.getOrPut(self.allocator, target);
            if (!rev_gop.found_existing) {
                // Dupe the target string as the reverse outer key (independent ownership).
                rev_gop.key_ptr.* = try self.allocator.dupe(u8, target);
                rev_gop.value_ptr.* = .empty;
            }
            try rev_gop.value_ptr.put(self.allocator, importer_ptr, {});
        }
    }

    /// Return all repo-relative paths that @import `target_rel`.
    ///
    /// Also matches importers that recorded the basename only
    /// (e.g. `@import("log.zig")` when `target_rel` is `"src/common/log.zig"`).
    ///
    /// Caller owns the returned slice and each element.
    /// Returns an empty slice when no importers are found.
    pub fn getImportedBy(
        self: *const DepGraph,
        target_rel: []const u8,
        allocator: std.mem.Allocator,
    ) ![][]const u8 {
        var seen: std.StringHashMapUnmanaged(void) = .empty;
        defer seen.deinit(allocator);

        var results: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (results.items) |s| allocator.free(s);
            results.deinit(allocator);
        }

        const Helper = struct {
            fn addImporters(
                inner: *const std.StringHashMapUnmanaged(void),
                s: *std.StringHashMapUnmanaged(void),
                r: *std.ArrayList([]const u8),
                a: std.mem.Allocator,
            ) !void {
                var it = inner.keyIterator();
                while (it.next()) |key_ptr| {
                    const k = key_ptr.*;
                    if (s.contains(k)) continue;
                    try s.put(a, k, {});
                    try r.append(a, try a.dupe(u8, k));
                }
            }
        };

        // Full-path lookup.
        if (self.reverse.getPtr(target_rel)) |inner| {
            try Helper.addImporters(inner, &seen, &results, allocator);
        }

        // Basename lookup — handles `@import("log.zig")` style imports.
        const basename = std.fs.path.basename(target_rel);
        if (!std.mem.eql(u8, basename, target_rel)) {
            if (self.reverse.getPtr(basename)) |inner| {
                try Helper.addImporters(inner, &seen, &results, allocator);
            }
        }

        // Sort for determinism.
        std.mem.sort([]const u8, results.items, {}, struct {
            fn lessThan(_: void, a: []const u8, b: []const u8) bool {
                return std.mem.lessThan(u8, a, b);
            }
        }.lessThan);

        return results.toOwnedSlice(allocator);
    }

    /// Remove all edges for `path` (both as importer and as target).
    ///
    /// After this call `path` will not appear in any `getImportedBy` result.
    pub fn remove(self: *DepGraph, path: []const u8) void {
        // ── Remove as importer ────────────────────────────────────────────────
        if (self.forward.fetchRemove(path)) |kv| {
            for (kv.value) |target| {
                if (self.reverse.getPtr(target)) |inner| {
                    // Inner key is the forward key pointer — remove by value.
                    _ = inner.remove(kv.key);
                }
                // Safe: forward owns its own copy of the target string
                // (independent from the reverse outer key copy).
                self.allocator.free(target);
            }
            self.allocator.free(kv.value);
            self.allocator.free(kv.key);
        }

        // ── Remove as target ─────────────────────────────────────────────────
        if (self.reverse.fetchRemove(path)) |kv| {
            // Strip `path` from the forward value slice of every importer.
            var it = kv.value.keyIterator();
            while (it.next()) |importer_ptr| {
                if (self.forward.getPtr(importer_ptr.*)) |targets| {
                    var new_list: std.ArrayList([]const u8) = .empty;
                    for (targets.*) |t| {
                        if (std.mem.eql(u8, t, path)) {
                            self.allocator.free(t); // free forward's own copy
                        } else {
                            new_list.append(self.allocator, t) catch {};
                        }
                    }
                    self.allocator.free(targets.*);
                    targets.* = new_list.toOwnedSlice(self.allocator) catch &.{};
                }
            }
            var inner = kv.value;
            inner.deinit(self.allocator);
            // Free the reverse outer key (independently owned).
            self.allocator.free(@constCast(kv.key));
        }
    }
};
