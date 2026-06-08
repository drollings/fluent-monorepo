/// cli.zig — Ingestion CLI Command Implementation
///
/// Provides argument parsing and state management for the `coral ingest` subcommand.
///
/// Usage:
///   coral ingest --source <path> --ontology yago-4.5 --batch-size 10000
///   coral ingest --resume <checkpoint-file>
///   coral ingest --status
///
/// Checkpointing: stores progress in the SQLite ingestion_checkpoints table.
const std = @import("std");
const batch_mod = @import("batch.zig");
const BatchConfig = batch_mod.BatchConfig;
const IngestStats = batch_mod.IngestStats;

// ---------------------------------------------------------------------------
// Ingestion status
// ---------------------------------------------------------------------------

pub const IngestStatus = enum {
    not_started,
    in_progress,
    completed,
    failed,
};

// ---------------------------------------------------------------------------
// Checkpoint — persists ingestion progress
// ---------------------------------------------------------------------------

pub const Checkpoint = struct {
    checkpoint_id: []const u8, // owned
    source: []const u8, // owned
    /// Byte offset in the source file (0 = start).
    offset: u64,
    triples_processed: u64,
    status: IngestStatus,
    updated_at: f64, // Unix timestamp
};

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

pub const IngestArgs = struct {
    source: ?[]const u8 = null,
    ontology: []const u8 = "yago-4.5",
    batch_size: usize = 10_000,
    resume_checkpoint: ?[]const u8 = null,
    show_status: bool = false,
    preferred_lang: []const u8 = "en",

    pub fn parse(args: []const []const u8) !IngestArgs {
        var result = IngestArgs{};
        var i: usize = 0;
        while (i < args.len) : (i += 1) {
            const arg = args[i];
            if (std.mem.eql(u8, arg, "--source") or std.mem.eql(u8, arg, "-s")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                result.source = args[i];
            } else if (std.mem.eql(u8, arg, "--ontology")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                result.ontology = args[i];
            } else if (std.mem.eql(u8, arg, "--batch-size")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                result.batch_size = std.fmt.parseInt(usize, args[i], 10) catch return error.InvalidBatchSize;
            } else if (std.mem.eql(u8, arg, "--resume")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                result.resume_checkpoint = args[i];
            } else if (std.mem.eql(u8, arg, "--status")) {
                result.show_status = true;
            } else if (std.mem.eql(u8, arg, "--lang")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                result.preferred_lang = args[i];
            }
        }
        return result;
    }

    pub fn toBatchConfig(self: IngestArgs) BatchConfig {
        return BatchConfig{
            .batch_size = self.batch_size,
            .mapping = .{ .preferred_lang = self.preferred_lang },
        };
    }
};

pub const CliError = error{
    MissingValue,
    InvalidBatchSize,
    MissingSource,
};

// ---------------------------------------------------------------------------
// CheckpointStore — in-memory (later persisted to SQLite)
// ---------------------------------------------------------------------------

pub const CheckpointStore = struct {
    allocator: std.mem.Allocator,
    checkpoints: std.ArrayList(Checkpoint),

    pub fn init(allocator: std.mem.Allocator) CheckpointStore {
        return .{
            .allocator = allocator,
            .checkpoints = .{},
        };
    }

    pub fn deinit(self: *CheckpointStore) void {
        for (self.checkpoints.items) |cp| {
            self.allocator.free(cp.checkpoint_id);
            self.allocator.free(cp.source);
        }
        self.checkpoints.deinit(self.allocator);
    }

    pub fn save(self: *CheckpointStore, cp: Checkpoint) !void {
        const owned_id = try self.allocator.dupe(u8, cp.checkpoint_id);
        errdefer self.allocator.free(owned_id);
        const owned_src = try self.allocator.dupe(u8, cp.source);
        errdefer self.allocator.free(owned_src);
        try self.checkpoints.append(self.allocator, .{
            .checkpoint_id = owned_id,
            .source = owned_src,
            .offset = cp.offset,
            .triples_processed = cp.triples_processed,
            .status = cp.status,
            .updated_at = cp.updated_at,
        });
    }

    pub fn findLatest(self: *const CheckpointStore, source: []const u8) ?Checkpoint {
        var latest: ?Checkpoint = null;
        for (self.checkpoints.items) |cp| {
            if (std.mem.eql(u8, cp.source, source)) {
                if (latest == null or cp.updated_at > latest.?.updated_at) {
                    latest = cp;
                }
            }
        }
        return latest;
    }
};

// =============================================================================
// Tests — Milestone 3.3
// =============================================================================

const testing = std.testing;

test "parse ingest args source" {
    const args = [_][]const u8{ "--source", "data/yago-tiny.ttl" };
    const parsed = try IngestArgs.parse(&args);
    try testing.expectEqualStrings("data/yago-tiny.ttl", parsed.source.?);
}

test "parse ingest args batch size" {
    const args = [_][]const u8{ "--source", "foo.ttl", "--batch-size", "5000" };
    const parsed = try IngestArgs.parse(&args);
    try testing.expectEqual(@as(usize, 5000), parsed.batch_size);
}

test "parse ingest args status flag" {
    const args = [_][]const u8{"--status"};
    const parsed = try IngestArgs.parse(&args);
    try testing.expect(parsed.show_status);
}

test "parse ingest args resume" {
    const args = [_][]const u8{ "--resume", "checkpoint-abc" };
    const parsed = try IngestArgs.parse(&args);
    try testing.expectEqualStrings("checkpoint-abc", parsed.resume_checkpoint.?);
}

test "ingest args default batch size" {
    const args = [_][]const u8{};
    const parsed = try IngestArgs.parse(&args);
    try testing.expectEqual(@as(usize, 10_000), parsed.batch_size);
}

test "checkpoint store save and retrieve" {
    var store = CheckpointStore.init(testing.allocator);
    defer store.deinit();

    try store.save(.{
        .checkpoint_id = "cp1",
        .source = "data/yago-tiny.ttl",
        .offset = 12345,
        .triples_processed = 100,
        .status = .in_progress,
        .updated_at = 1_700_000_000.0,
    });

    const latest = store.findLatest("data/yago-tiny.ttl").?;
    try testing.expectEqual(@as(u64, 12345), latest.offset);
    try testing.expectEqual(IngestStatus.in_progress, latest.status);
}

test "checkpoint store returns null for unknown source" {
    var store = CheckpointStore.init(testing.allocator);
    defer store.deinit();
    try testing.expect(store.findLatest("nonexistent.ttl") == null);
}
