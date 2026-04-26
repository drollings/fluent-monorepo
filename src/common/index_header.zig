/// index_header.zig — Binary file header shared by word_index.bin and trigram_index.bin.
///
/// Header envelope layout (little-endian):
///   u32  magic
///   u32  version
///   u16  git_head_len
///   [git_head_len]u8  git_head   (absent when git_head_len == 0)
///
/// Callers write the per-index magic and version constants; this module
/// encapsulates all read/write logic so neither word_index nor trigram_index
/// needs to duplicate the envelope handling.
const std = @import("std");

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub const Header = struct {
    magic: u32,
    version: u32,
    /// Borrowed slice; null if no git HEAD is stored.  Caller owns the bytes.
    git_head: ?[]const u8,
};

pub const ReadResult = struct {
    /// Byte offset immediately after the header (where index payload begins).
    offset: usize,
    /// Length of the git_head field stored in the header (may be 0).
    git_head_len: u16,
};

// ---------------------------------------------------------------------------
// write — append the header envelope to an open writer
// ---------------------------------------------------------------------------

/// Writes magic / version / git_head envelope.  Caller keeps `w` open and
/// continues writing index payload after this call.
pub fn write(w: anytype, h: Header) !void {
    try w.writeInt(u32, h.magic, .little);
    try w.writeInt(u32, h.version, .little);
    const gh = h.git_head orelse "";
    try w.writeInt(u16, @intCast(gh.len), .little);
    if (gh.len > 0) try w.writeAll(gh);
}

// ---------------------------------------------------------------------------
// read — parse header from an in-memory content slice
// ---------------------------------------------------------------------------

/// Reads the header from `content`.  Returns null when:
///   - the buffer is too short for the fixed fields,
///   - magic does not match `expected_magic`, or
///   - version does not match `expected_version`.
///
/// On success `result.offset` points to the first byte after the header.
pub fn read(
    content: []const u8,
    expected_magic: u32,
    expected_version: u32,
) ?ReadResult {
    // Minimum: magic(4) + version(4) + git_head_len(2) = 10 bytes
    if (content.len < 10) return null;

    var offset: usize = 0;

    const magic = std.mem.readInt(u32, content[offset..][0..4], .little);
    offset += 4;
    if (magic != expected_magic) return null;

    const version = std.mem.readInt(u32, content[offset..][0..4], .little);
    offset += 4;
    if (version != expected_version) return null;

    const gh_len = std.mem.readInt(u16, content[offset..][0..2], .little);
    offset += 2;

    if (gh_len > 0) {
        if (offset + gh_len > content.len) return null;
        offset += gh_len;
    }

    return .{ .offset = offset, .git_head_len = gh_len };
}

// ---------------------------------------------------------------------------
// readGitHeadFromFile — extract git HEAD from a binary index file
// ---------------------------------------------------------------------------

/// Opens `path`, parses the header using `expected_magic` / `expected_version`,
/// and returns the stored git HEAD SHA as a 40-byte array (zeroed-padded).
/// Returns null on any error (file missing, wrong magic/version, no git head).
pub fn readGitHeadFromFile(
    path: []const u8,
    expected_magic: u32,
    expected_version: u32,
) ?[40]u8 {
    const content = std.fs.cwd().readFileAlloc(
        std.heap.page_allocator,
        path,
        std.math.maxInt(usize),
    ) catch return null;
    defer std.heap.page_allocator.free(content);

    const hdr = read(content, expected_magic, expected_version) orelse return null;
    if (hdr.git_head_len == 0 or hdr.git_head_len > 40) return null;

    // git_head bytes live just before hdr.offset
    const gh_start = hdr.offset - hdr.git_head_len;
    var result: [40]u8 = [_]u8{0} ** 40;
    @memcpy(result[0..hdr.git_head_len], content[gh_start..][0..hdr.git_head_len]);
    return result;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "write/read round-trip: no git head" {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(testing.allocator);
    const bw = buf.writer(testing.allocator);

    try write(&bw, .{ .magic = 0xDEAD, .version = 1, .git_head = null });
    // Append some payload
    try bw.writeAll("payload");

    const result = read(buf.items, 0xDEAD, 1).?;
    try testing.expectEqual(@as(usize, 10), result.offset);
    try testing.expectEqual(@as(u16, 0), result.git_head_len);
    try testing.expectEqualStrings("payload", buf.items[result.offset..]);
}

test "write/read round-trip: with git head" {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(testing.allocator);
    const bw = buf.writer(testing.allocator);

    const sha = "aabbccddeeff00112233445566778899aabbccdd";
    try write(&bw, .{ .magic = 0xBEEF, .version = 2, .git_head = sha });
    try bw.writeAll("after");

    const result = read(buf.items, 0xBEEF, 2).?;
    try testing.expectEqual(@as(u16, 40), result.git_head_len);
    try testing.expectEqual(@as(usize, 10 + 40), result.offset);
    try testing.expectEqualStrings("after", buf.items[result.offset..]);
}

test "read: wrong magic returns null" {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(testing.allocator);
    const bw = buf.writer(testing.allocator);
    try write(&bw, .{ .magic = 0x1234, .version = 1, .git_head = null });

    try testing.expect(read(buf.items, 0xFFFF, 1) == null);
}

test "read: wrong version returns null" {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(testing.allocator);
    const bw = buf.writer(testing.allocator);
    try write(&bw, .{ .magic = 0x1234, .version = 1, .git_head = null });

    try testing.expect(read(buf.items, 0x1234, 99) == null);
}
