const std = @import("std");
const string_mod = @import("string.zig");

pub const EntityFreq = struct {
    name: []const u8,
    frequency: usize,
    entity_type: EntityType,
};

pub const EntityType = enum {
    person,
    project,
    location,
    uncertain,
};

pub const ENTITY_STOPLIST = std.StaticStringMap(void).initComptime(.{
    .{ "The", {} },      .{ "This", {} },     .{ "That", {} },      .{ "When", {} },
    .{ "Where", {} },    .{ "What", {} },     .{ "Why", {} },       .{ "Who", {} },
    .{ "Which", {} },    .{ "How", {} },      .{ "Then", {} },      .{ "There", {} },
    .{ "Here", {} },     .{ "Now", {} },      .{ "Just", {} },      .{ "Also", {} },
    .{ "Some", {} },     .{ "Such", {} },     .{ "Each", {} },      .{ "Every", {} },
    .{ "Monday", {} },   .{ "Tuesday", {} },  .{ "Wednesday", {} }, .{ "Thursday", {} },
    .{ "Friday", {} },   .{ "Saturday", {} }, .{ "Sunday", {} },    .{ "January", {} },
    .{ "February", {} }, .{ "March", {} },    .{ "April", {} },     .{ "May", {} },
    .{ "June", {} },     .{ "July", {} },     .{ "August", {} },    .{ "September", {} },
    .{ "October", {} },  .{ "November", {} }, .{ "December", {} },
});

pub fn candidateEntityWords(text: []const u8, allocator: std.mem.Allocator) ![]const []const u8 {
    var candidates: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (candidates.items) |item| allocator.free(item);
        candidates.deinit(allocator);
    }

    var word_start: ?usize = null;
    var i: usize = 0;

    while (i <= text.len) : (i += 1) {
        const ch: u8 = if (i < text.len) text[i] else ' ';
        if (isEntityWordChar(ch)) {
            if (word_start == null) word_start = i;
        } else {
            if (word_start) |start| {
                const word = text[start..i];
                if (word.len >= 2 and isCapitalized(word)) {
                    if (!ENTITY_STOPLIST.has(word)) {
                        const duped = try allocator.dupe(u8, word);
                        try candidates.append(allocator, duped);
                    }
                }
            }
            word_start = null;
        }
    }

    return candidates.toOwnedSlice(allocator);
}

fn isEntityWordChar(c: u8) bool {
    return std.ascii.isAlphanumeric(c) or c == '_' or c == '-' or c == '\'' or c == '.';
}

fn isCapitalized(word: []const u8) bool {
    if (word.len == 0) return false;
    return std.ascii.isUpper(word[0]);
}

pub fn extractEntities(content: []const u8, allocator: std.mem.Allocator, min_frequency: usize) ![]EntityFreq {
    var freq_map = std.StringHashMap(usize).init(allocator);
    defer {
        var it = freq_map.iterator();
        while (it.next()) |entry| allocator.free(entry.key_ptr.*);
        freq_map.deinit();
    }

    const candidates = try candidateEntityWords(content, allocator);
    defer {
        for (candidates) |c| allocator.free(c);
        allocator.free(candidates);
    }

    for (candidates) |entity| {
        const key = try allocator.dupe(u8, entity);
        const result = try freq_map.getOrPut(key);
        if (result.found_existing) {
            allocator.free(key);
        }
        result.value_ptr.* += 1;
    }

    var results: std.ArrayList(EntityFreq) = .empty;
    errdefer results.deinit(allocator);

    var iter = freq_map.iterator();
    while (iter.next()) |entry| {
        if (entry.value_ptr.* >= min_frequency) {
            try results.append(allocator, .{
                .name = entry.key_ptr.*,
                .frequency = entry.value_ptr.*,
                .entity_type = .uncertain,
            });
        }
    }

    std.sort.insertion(EntityFreq, results.items, {}, struct {
        fn lessThan(ctx: void, a: EntityFreq, b: EntityFreq) bool {
            _ = ctx;
            return a.frequency > b.frequency;
        }
    }.lessThan);

    const slice = try results.toOwnedSlice(allocator);
    for (slice) |item| {
        _ = freq_map.remove(item.name);
    }
    return slice;
}

const testing = std.testing;

test "candidateEntityWords extracts capitalized words" {
    const text = "The GuidanceDb struct implements search functionality";
    const candidates = try candidateEntityWords(text, testing.allocator);
    defer {
        for (candidates) |c| testing.allocator.free(c);
        testing.allocator.free(candidates);
    }
    var found_guidance = false;
    for (candidates) |c| {
        if (std.mem.eql(u8, c, "GuidanceDb")) found_guidance = true;
    }
    try testing.expect(found_guidance);
}

test "candidateEntityWords filters stoplist" {
    const text = "The function returns This value";
    const candidates = try candidateEntityWords(text, testing.allocator);
    defer {
        for (candidates) |c| testing.allocator.free(c);
        testing.allocator.free(candidates);
    }
    var has_stop = false;
    for (candidates) |c| {
        if (std.mem.eql(u8, c, "The") or std.mem.eql(u8, c, "This")) has_stop = true;
    }
    try testing.expect(!has_stop);
}

test "extractEntities with min_frequency" {
    const text = "GuidanceDb handles search. GuidanceDb is fast. OtherClass is also here.";
    const entities = try extractEntities(text, testing.allocator, 2);
    defer {
        for (entities) |e| testing.allocator.free(e.name);
        testing.allocator.free(entities);
    }
    var found = false;
    for (entities) |e| {
        if (std.mem.eql(u8, e.name, "GuidanceDb")) found = true;
    }
    try testing.expect(found);
}
