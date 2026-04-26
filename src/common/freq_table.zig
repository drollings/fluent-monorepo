const std = @import("std");

pub const FrequencyTable = [256][256]u16;

var g_default_freq: ?FrequencyTable = null;

pub fn defaultFrequencyTable() FrequencyTable {
    var tbl: FrequencyTable = undefined;
    @memset(std.mem.asBytes(&tbl), 0xFE);
    for (&tbl, 0..) |*row, i| {
        for (row, 0..) |*cell, j| {
            const c: u8 = @intCast(i);
            const n: u8 = @intCast(j);
            if ((c >= 'a' and c <= 'z' or c >= 'A' and c <= 'Z') and (n >= 'a' and n <= 'z' or n >= 'A' and n <= 'Z')) {
                cell.* = 0x1000;
            }
        }
    }
    tbl[' '][' '] = 0x0800;
    tbl[' ']['a'] = 0x0800;
    tbl[' ']['t'] = 0x0800;
    tbl[' ']['s'] = 0x0800;
    tbl[' ']['i'] = 0x0800;
    tbl[' ']['o'] = 0x0800;
    tbl['e'][' '] = 0x0800;
    tbl['t']['h'] = 0x0800;
    tbl['t'][' '] = 0x0800;
    tbl['h']['e'] = 0x0800;
    tbl['i']['n'] = 0x0800;
    tbl['n'][' '] = 0x0800;
    tbl['s'][' '] = 0x0800;
    tbl['r'][' '] = 0x0800;
    tbl['.'][' '] = 0x0800;
    tbl['_']['a'] = 0x1000;
    tbl['_']['b'] = 0x1000;
    tbl['_']['c'] = 0x1000;
    tbl['_']['z'] = 0x1000;
    return tbl;
}

pub fn getDefaultPairFreq() *const FrequencyTable {
    if (g_default_freq == null) {
        g_default_freq = defaultFrequencyTable();
    }
    return &g_default_freq.?;
}

var active_pair_freq: ?*const FrequencyTable = null;

pub fn setFrequencyTable(table: *const FrequencyTable) void {
    active_pair_freq = table;
}

pub fn getFrequencyTable() *const FrequencyTable {
    return active_pair_freq orelse getDefaultPairFreq();
}

pub fn pairWeight(a: u8, b: u8) u16 {
    return getFrequencyTable()[a][b];
}

pub fn buildFrequencyTable(contents: []const u8) FrequencyTable {
    var tbl = defaultFrequencyTable();
    var i: usize = 0;
    while (i + 1 < contents.len) : (i += 1) {
        const a: u16 = contents[i];
        const b: u16 = contents[i + 1];
        if (a < 256 and b < 256) {
            tbl[@intCast(a)][@intCast(b)] +|= 1;
        }
    }
    return tbl;
}

pub fn buildFrequencyTableFromMap(contents_map: *const std.StringHashMap([]const u8)) FrequencyTable {
    var tbl = defaultFrequencyTable();
    var iter = contents_map.iterator();
    while (iter.next()) |entry| {
        const content = entry.value_ptr.*;
        var i: usize = 0;
        while (i + 1 < content.len) : (i += 1) {
            const a: u16 = content[i];
            const b: u16 = content[i + 1];
            if (a < 256 and b < 256) {
                tbl[@intCast(a)][@intCast(b)] +|= 1;
            }
        }
    }
    return tbl;
}

pub fn writeFrequencyTable(dir_path: []const u8, table: *const FrequencyTable) !void {
    var buf: [4096]u8 = undefined;
    const path = try std.fmt.bufPrint(&buf, "{s}/pair_freq.bin", .{dir_path});

    const f = try std.fs.cwd().createFile(path, .{});
    defer f.close();
    var fw_buf: [4096]u8 = undefined;
    var fw = f.writer(&fw_buf);
    const w = &fw.interface;

    const MAGIC: u32 = 0x46524551;
    const VERSION: u32 = 1;
    try w.writeInt(u32, MAGIC, .little);
    try w.writeInt(u32, VERSION, .little);

    const byte_ptr: [*]const u8 = @ptrCast(table);
    try w.writeAll(byte_ptr[0..@sizeOf(FrequencyTable)]);
    try w.flush();
}

pub fn readFrequencyTable(dir_path: []const u8) !?FrequencyTable {
    var buf: [4096]u8 = undefined;
    const path = try std.fmt.bufPrint(&buf, "{s}/pair_freq.bin", .{dir_path});

    const content = std.fs.cwd().readFileAlloc(std.heap.page_allocator, path, std.math.maxInt(usize)) catch return null;
    defer std.heap.page_allocator.free(content);

    if (content.len < 8) return null;

    const magic = std.mem.readInt(u32, content[0..4], .little);
    if (magic != 0x46524551) return null;
    const version = std.mem.readInt(u32, content[4..8], .little);
    if (version != 1) return null;

    const table_bytes = content[8 .. 8 + @sizeOf(FrequencyTable)];
    if (table_bytes.len < @sizeOf(FrequencyTable)) return null;

    var table: FrequencyTable = undefined;
    @memcpy(std.mem.asBytes(&table), table_bytes);
    return table;
}

const testing = std.testing;

test "buildFrequencyTable basic" {
    const content = "hello world test";
    const table = buildFrequencyTable(content);
    const default_tbl = defaultFrequencyTable();
    try testing.expect(table['h']['e'] > default_tbl['h']['e']);
    try testing.expect(table['l']['l'] > default_tbl['l']['l']);
}

test "buildFrequencyTableFromMap" {
    var map = std.StringHashMap([]const u8).init(testing.allocator);
    defer map.deinit();
    try map.put("file1", "foo bar baz");
    try map.put("file2", "hello world");

    const table = buildFrequencyTableFromMap(&map);
    const default_tbl = defaultFrequencyTable();
    try testing.expect(table['f']['o'] > default_tbl['f']['o']);
}

test "pairWeight default" {
    const w = pairWeight('t', 'h');
    try testing.expect(w > 0);
}

test "setFrequencyTable and getFrequencyTable" {
    var table = buildFrequencyTable("test data");
    setFrequencyTable(&table);
    defer setFrequencyTable(getDefaultPairFreq());
    const retrieved = getFrequencyTable();
    try testing.expect(retrieved == &table);
}

test "write and read frequency table" {
    const dir = ".test_tmp_freq";
    defer {
        std.fs.cwd().deleteTree(dir) catch {};
    }
    std.fs.cwd().makePath(dir) catch {};

    var table = buildFrequencyTable("hello world");
    try writeFrequencyTable(dir, &table);

    const loaded = try readFrequencyTable(dir);
    try testing.expect(loaded != null);
}
