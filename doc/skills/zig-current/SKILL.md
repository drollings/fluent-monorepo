---
name: zig-0.16
description: Accurate Zig 0.16.0 API patterns — read this before writing any Zig 0.16 code
---

# Zig 0.16.0 Reference

> **For agentic coders**: Models trained before 2026 have stale Zig knowledge.
> Zig 0.16 contains sweeping breaking changes from 0.15 — especially to I/O, process
> spawning, and filesystem APIs. Read every section before writing code.

---

## 1. `main()` Entry Point

`main()` now receives a `std.process.Init` or `Init.Minimal` parameter from the runtime.
`std.process.argsAlloc` / `argsFree` are **removed**. Iterate args via the Init.

```zig
// Minimal — just args and environ, no io/gpa:
pub fn main(init: std.process.Init.Minimal) !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var args_list: std.ArrayList([]const u8) = .empty;
    defer {
        for (args_list.items) |a| allocator.free(a);
        args_list.deinit(allocator);
    }
    {
        var iter = try init.args.iterateAllocator(allocator);
        defer iter.deinit();
        while (iter.next()) |arg|
            try args_list.append(allocator, try allocator.dupe(u8, arg));
    }
    const args = args_list.items;

    // Environment variables via init.environ:
    const home = init.environ.map.get("HOME") orelse "/unknown";
    _ = home; _ = args;
}

// Full Init — provides io, gpa, arena, environ_map:
pub fn main(init: std.process.Init) !void {
    const io = init.io;       // std.Io — use for all I/O
    const allocator = init.gpa;
    const home = init.environ_map.get("HOME") orelse "/unknown";
    _ = io; _ = allocator; _ = home;
}
```

**Migration:**

| 0.15 | 0.16 |
|------|------|
| `pub fn main() !void` | `pub fn main(init: std.process.Init.Minimal) !void` |
| `const args = try std.process.argsAlloc(allocator)` | `init.args.iterateAllocator(allocator)` |
| `defer std.process.argsFree(allocator, args)` | `defer iter.deinit()` (per above) |
| `std.process.getEnvVarOwned(allocator, "HOME")` | `init.environ_map.get("HOME")` (returns `?[]const u8`, not owned — dupe if needed) |

> **Note on `getEnvVarOwned`**: Completely removed. Use `init.environ_map.get("KEY")`
> from `Init` or `Init.Minimal`. The returned slice is NOT owned — call
> `allocator.dupe(u8, value)` if you need ownership.

---

## 2. Allocators

`GeneralPurposeAllocator` was **renamed** to `DebugAllocator`. Use `.init` not `{}`.

```zig
// ✅ CORRECT (0.16)
var gpa: std.heap.DebugAllocator(.{}) = .init;
defer if (gpa.deinit() == .leak) @panic("leak");
const allocator = gpa.allocator();

// Arena (unchanged)
var arena = std.heap.ArenaAllocator.init(allocator);
defer arena.deinit();
const aa = arena.allocator();

// Thread-safe production allocator (no init/deinit)
const alloc = std.heap.smp_allocator;

// ❌ WRONG
// var gpa = std.heap.GeneralPurposeAllocator(.{}){}; // old name, old init
// var gpa: std.heap.DebugAllocator(.{}) = .{};       // must be .init
```

---

## 3. ArrayList — Unmanaged by Default

`std.ArrayList` is the **unmanaged** type (allocator passed per call).
`std.ArrayListUnmanaged` is a deprecated alias — use `std.ArrayList` directly.
The old managed variant (stores allocator) lives at `std.array_list.Managed(T)`.

**`ArrayList.writer()` is removed.** Use `std.Io.Writer.Allocating` for string building
(see §6), or `Managed(T)` if you need stored-allocator convenience.

```zig
// ✅ CORRECT (0.16) — unmanaged, pass allocator to every call
var list: std.ArrayList(u8) = .empty;
defer list.deinit(allocator);
try list.append(allocator, 'a');
try list.appendSlice(allocator, "bc");
const owned = try list.toOwnedSlice(allocator);

// Pre-allocated capacity
var list2 = try std.ArrayList(u8).initCapacity(allocator, 64);
defer list2.deinit(allocator);
list2.appendAssumeCapacity('x');

// ❌ WRONG
// var list = std.ArrayList(u8).init(allocator);  // .init() is Managed-only
// var list: std.ArrayList(u8) = .{};             // missing required fields — use .empty
// list.append('a');                               // missing allocator arg
// list.deinit();                                  // missing allocator arg
// const w = list.writer(allocator);               // REMOVED — use Io.Writer.Allocating
```

### String Building (replaces `ArrayList(u8).writer()`)

```zig
// ✅ CORRECT — use Io.Writer.Allocating for building strings
var aw: std.Io.Writer.Allocating = .init(allocator);
errdefer aw.deinit();
const w = &aw.writer;
try w.print("hello {s}\n", .{"world"});
const owned = try aw.toOwnedSlice();   // transfers ownership; caller must free
defer allocator.free(owned);

// Or just read the buffer without transferring ownership:
const view: []u8 = aw.written();       // valid until next write or deinit

// ❌ WRONG
// var buf: std.ArrayList(u8) = .empty;
// const w = buf.writer(allocator);  // REMOVED in 0.16
```

---

## 4. HashMap — Still Managed

`StringHashMap` and `AutoHashMap` store the allocator. No allocator arg to `put`/`deinit`.

```zig
// ✅ CORRECT (0.16) — managed (unchanged from 0.15)
var map = std.StringHashMap(u32).init(allocator);
defer map.deinit();
try map.put("key", 42);
const val = map.get("key"); // ?u32

// ❌ WRONG
// map.put(allocator, "key", 42); // managed put() takes NO allocator
// map.deinit(allocator);          // managed deinit() takes NO allocator
```

---

## 5. The `std.Io` System — Mandatory `io` Parameter

**The biggest 0.16 change.** All file and directory operations now require an
`io: std.Io` context. `std.fs.cwd()`, `std.fs.openFileAbsolute()`, and virtually
all of `std.fs` are **removed**. Use `std.Io.Dir.*` and `std.Io.File.*` instead.

For synchronous single-threaded code (CLI tools, generators, tests), obtain a
global `Io` from the singleton:

```zig
const io = std.Io.Threaded.global_single_threaded.io();
```

### 5a. Current Working Directory

```zig
const io = std.Io.Threaded.global_single_threaded.io();
const cwd: std.Io.Dir = std.Io.Dir.cwd(); // no io needed — just gets handle
const cwd_path = try std.process.currentPathAlloc(io, allocator);
defer allocator.free(cwd_path);

// ❌ WRONG
// std.fs.cwd()                      // removed
// std.process.getCwdAlloc(allocator) // removed
```

### 5b. Opening Files and Directories

```zig
const io = std.Io.Threaded.global_single_threaded.io();

const file = try std.Io.Dir.openFileAbsolute(io, "/abs/path/file.txt", .{});
defer file.close(io);

const rel = try std.Io.Dir.cwd().openDir(io, "src", .{ .iterate = true });
defer rel.close(io);

const f2 = try rel.openFile(io, "main.zig", .{});
defer f2.close(io);

const new = try std.Io.Dir.createFileAbsolute(io, "/abs/path/out.txt", .{});
defer new.close(io);

// ❌ WRONG
// std.fs.cwd().openDir("src", .{})   // openDir now requires io param
// file.close()                        // close() requires io param in 0.16
```

### 5c. Reading Files

```zig
const io = std.Io.Threaded.global_single_threaded.io();

// Whole file into heap buffer (relative or absolute path)
const content = try std.Io.Dir.cwd().readFileAlloc(
    io,
    "src/main.zig",
    allocator,
    .limited(10 * 1024 * 1024),
);
defer allocator.free(content);

// ❌ WRONG
// std.fs.cwd().readFileAlloc(allocator, path, n)  // signature changed
// file.readToEndAlloc(allocator, n)                 // removed
// file.readToEndAllocOptions(a, n, ...)             // removed
// file.reader(&buf)                                 // reader() now needs io param
```

### 5d. Writing Files

```zig
const io = std.Io.Threaded.global_single_threaded.io();

// Buffered writer pattern — REQUIRED in 0.16 (file.writeAll removed)
var wbuf: [4096]u8 = undefined;
var writer = file.writer(io, &wbuf);
const w: *std.Io.Writer = &writer.interface;
try w.print("hello {s}\n", .{"world"});
try w.flush();  // REQUIRED before close or data loss

// Unbuffered (empty slice): writes go directly to OS, no flush needed
var uwriter = file.writer(io, &.{});
try uwriter.interface.writeAll("immediate\n");
```

> **Critical**: `file.writeAll()` is **removed** in 0.16. You MUST use the writer
> pattern shown above for all file writes.

### 5e. stdout / stderr

```zig
const io = std.Io.Threaded.global_single_threaded.io();
var out_buf: [4096]u8 = undefined;
var out_fw = std.Io.File.stdout().writer(io, &out_buf);
const stdout: *std.Io.Writer = &out_fw.interface;
try stdout.print("value: {d}\n", .{42});
try stdout.flush(); // REQUIRED
```

### 5f. Directory Operations

```zig
const io = std.Io.Threaded.global_single_threaded.io();

// Stat a file
const stat = try file.stat(io);
const size: u64 = stat.size;
const mtime_ns: i96 = stat.mtime.nanoseconds;

// Create directory (absolute)
std.Io.Dir.createDirAbsolute(io, "/abs/path/dir", .default_dir) catch |err| {
    if (err != error.PathAlreadyExists) return err;
};

// Create directory path relative to a Dir
try some_dir.createDirPath(io, "a/b/c");

// Delete file (absolute)
try std.Io.Dir.deleteFileAbsolute(io, "/abs/path/file.txt");

// Delete tree (relative to a Dir handle)
try std.Io.Dir.cwd().deleteTree(io, "some/subtree");

// Check access (absolute)
std.Io.Dir.accessAbsolute(io, path, .{}) catch |err| {
    if (err == error.FileNotFound) { /* does not exist */ }
};

// Dire stat (stat the directory itself)
const dstat = try cwd.stat(io);
const fstat = try cwd.statFile(io, "file.txt", .{}); // stat a file inside dir

// Walk directory entries
var dir = try std.Io.Dir.openDirAbsolute(io, "/abs/path", .{ .iterate = true });
defer dir.close(io);
var walker = try dir.walk(allocator);
defer walker.deinit();
while (try walker.next(io)) |entry| {  // walker.next() REQUIRES io param!
    _ = entry.path;
    _ = entry.kind; // .file, .directory, ...
}
```

### 5g. File Timestamps (`setTimestamps` replaces `updateTimes`)

```zig
const io = std.Io.Threaded.global_single_threaded.io();

// Set modify timestamp
const ns: i96 = @intCast(some_i128_ns);
try file.setTimestamps(io, .{
    .modify_timestamp = .{ .new = .{ .nanoseconds = ns } },
    .access_timestamp = .unchanged,
});

// Set both to now
try file.setTimestamps(io, .{
    .modify_timestamp = .now,
    .access_timestamp = .now,
});

// Current time as Io.Timestamp
const now: std.Io.Timestamp = std.Io.Timestamp.now(io, .real);
const later = now.addDuration(.{ .nanoseconds = std.time.ns_per_s });

// ❌ WRONG
// file.updateTimes(atime, mtime)     // removed; use setTimestamps
```

### Migration table: `std.fs` → `std.Io.Dir` / `std.Io.File`

| 0.15 (`std.fs.*`) | 0.16 (`std.Io.*`) |
|---|---|
| `std.fs.cwd()` | `std.Io.Dir.cwd()` |
| `std.fs.openFileAbsolute(path, opts)` | `std.Io.Dir.openFileAbsolute(io, path, opts)` |
| `std.fs.createFileAbsolute(path, opts)` | `std.Io.Dir.createFileAbsolute(io, path, opts)` |
| `std.fs.openDirAbsolute(path, opts)` | `std.Io.Dir.openDirAbsolute(io, path, opts)` |
| `std.fs.accessAbsolute(path, opts)` | `std.Io.Dir.accessAbsolute(io, path, opts)` |
| `std.fs.deleteFileAbsolute(path)` | `std.Io.Dir.deleteFileAbsolute(io, path)` |
| `std.fs.deleteTreeAbsolute(path)` | `std.Io.Dir.cwd().deleteTree(io, path)` |
| `std.fs.makeDirAbsolute(path)` | `std.Io.Dir.createDirAbsolute(io, path, .default_dir)` |
| `dir.openDir(path, opts)` | `dir.openDir(io, path, opts)` |
| `dir.close()` | `dir.close(io)` |
| `file.close()` | `file.close(io)` |
| `file.stat()` | `file.stat(io)` — returns `Io.File.Stat` with `.size`, `.mtime`, etc. |
| `file.updateTimes(atime, mtime)` | `file.setTimestamps(io, .{...})` |
| `file.readToEndAlloc(a, n)` | `dir.readFileAlloc(io, path, a, .limited(n))` |
| `file.readToEndAllocOptions(...)` | `dir.readFileAlloc(io, path, a, .limited(n))` |
| `file.writeAll(data)` | `writer.interface.writeAll(data)` — **REMOVED**; must use writer |
| `file.reader(&buf)` | `file.reader(io, &buf)` |
| `file.writer(&buf)` | `file.writer(io, &buf)` |
| `std.fs.File.stdout().writer(&buf)` | `std.Io.File.stdout().writer(io, &buf)` |
| `std.process.getCwdAlloc(allocator)` | `std.process.currentPathAlloc(io, allocator)` |
| `dir.readFileAlloc(a, p, n)` | `dir.readFileAlloc(io, p, a, .limited(n))` — note arg order change |
| `cwd.realpathAlloc(alloc, path)` | `std.fs.path.resolve(alloc, &.{path})` (no symlink res) |
| `dir.stat()` | `dir.stat(io)` — returns same `File.Stat` with kind, size, mtime |
| `dir.statFile(sub_path)` | `dir.statFile(io, sub_path, .{})` — `.follow_symlinks` option |
| `walker.next()` | `walker.next(io)` — **REQUIRES io parameter** |

---

## 6. `std.io` is Removed — Use `std.Io`

`std.io` (lowercase) no longer exists. `std.io.fixedBufferStream`,
`std.io.getStdOut()`, `std.io.Writer.Allocating` are all gone.
Everything is in `std.Io` (capital I). Use `std.Io.Writer.Allocating` for
string building (the replacement for `ArrayList.writer()`).

```zig
// Capture output into a heap buffer (replaces fixedBufferStream + write-to-buffer)
var aw: std.Io.Writer.Allocating = .init(allocator);
errdefer aw.deinit();
try aw.writer.print("captured: {d}\n", .{42});
const captured: []u8 = aw.written();    // valid until next write or deinit
const owned = try aw.toOwnedSlice();    // transfers ownership; caller must free

// ❌ WRONG
// std.io.fixedBufferStream(&buf)         // removed
// std.io.getStdOut()                     // removed
// var aw: std.io.Writer.Allocating = ... // removed (note: lowercase io)
// buf.writer(allocator)                   // removed from ArrayList
```

---

## 7. Child Processes — Completely Rewritten

`std.process.Child.init()`, `child.spawn()`, `child.collectOutput()` are **removed**.
Use `std.process.run()` for simple cases or `std.process.spawn()` for manual control.
Term variant names are now **lowercase** (`.exited` not `.Exited`).

```zig
const io = std.Io.Threaded.global_single_threaded.io();

// Simple: run and capture stdout+stderr
const result = try std.process.run(allocator, io, .{
    .argv = &.{ "git", "log", "--oneline", "-5" },
});
defer {
    allocator.free(result.stdout);
    allocator.free(result.stderr);
}
switch (result.term) {
    .exited  => |code| if (code != 0) return error.NonZeroExit,
    .signal  => return error.KilledBySignal,
    else     => return error.ProcessFailed,
}

// With cwd override
const r2 = try std.process.run(allocator, io, .{
    .argv = &.{ "git", "config", "user.name" },
    .cwd = .{ .path = "/some/absolute/dir" },
});
defer { allocator.free(r2.stdout); allocator.free(r2.stderr); }

// Manual spawn (when you need stdin pipe or progress)
var child = try std.process.spawn(io, .{
    .argv = &.{ "cat" },
    .stdin = .pipe,
    .stdout = .pipe,
    .stderr = .ignore,
});
defer child.kill(io);
const term = try child.wait(io);

// RunOptions fields
// .argv            []const []const u8   — required
// .cwd             Child.Cwd = .inherit — .inherit / .path / .dir
// .stdout_limit    Io.Limit = .unlimited
// .stderr_limit    Io.Limit = .unlimited
// .environ_map     ?*const Environ.Map = null
// .timeout         Io.Timeout = .none

// ❌ WRONG (0.15 style)
// var child = std.process.Child.init(argv, allocator);
// child.stdout_behavior = .Pipe;
// try child.spawn();
// .Exited => ...                             // capital E — removed
```

---

## 8. `@EnumLiteral()` Builtin

The `@Type(.enum_literal)` syntax for enum literal types was removed.
Use `@EnumLiteral()` instead.

```zig
pub fn logFn(
    comptime level: std.log.Level,
    comptime scope: @EnumLiteral(),   // ✅ 0.16
    comptime format: []const u8,
    args: anytype,
) void { _ = scope; _ = level; _ = format; _ = args; }

// ❌ WRONG
// comptime scope: @Type(.enum_literal)  // removed in 0.16
```

---

## 9. `Io.Timestamp` — File Modification Times

`File.Stat.mtime` is now `Io.Timestamp` (not `i128`). Use `.nanoseconds` for raw ns.
`File.Stat` now has `.size` (u64), `.kind`, `.atime` (?Io.Timestamp), `.mtime`, `.ctime`.

```zig
const io = std.Io.Threaded.global_single_threaded.io();
const stat = try file.stat(io);

const size: u64 = stat.size;
const mtime_ns: i96 = stat.mtime.nanoseconds;  // i96 field
const mtime_i128: i128 = @as(i128, stat.mtime.nanoseconds);

// Current time
const now: std.Io.Timestamp = std.Io.Timestamp.now(io, .real);
const later = now.addDuration(.{ .nanoseconds = std.time.ns_per_s });

// Create a timestamp from nanoseconds
const ts = std.Io.Timestamp{ .nanoseconds = @as(i96, @intCast(some_i128)) };

// ❌ WRONG
// const mtime: i128 = stat.mtime;         // mtime is Io.Timestamp, not i128
// const now_i128 = std.time.nanoTimestamp(); // nanoTimestamp() is removed
```

---

## 10. JSON

```zig
// Serialize — capture into Io.Writer.Allocating
var aw: std.Io.Writer.Allocating = .init(allocator);
errdefer aw.deinit();
try std.json.Stringify.value(my_value, .{}, &aw.writer);
const json: []const u8 = aw.written();

// Transfer ownership
const owned_json = try aw.toOwnedSlice();
defer allocator.free(owned_json);

// Parse (unchanged)
var parsed = try std.json.parseFromSlice(std.json.Value, allocator, json_bytes, .{});
defer parsed.deinit();
```

---

## 11. HTTP Client

`client.fetch()` API is unchanged in 0.16. `response_writer` takes `?*std.Io.Writer`.

```zig
var client = std.http.Client{ .allocator = allocator };
defer client.deinit();

var aw: std.Io.Writer.Allocating = .init(allocator);
defer aw.deinit();

const result = try client.fetch(.{
    .method = .POST,
    .location = .{ .url = "http://localhost:11434/api/chat" },
    .extra_headers = &.{
        .{ .name = "Content-Type", .value = "application/json" },
    },
    .payload = json_body_slice,
    .response_writer = &aw.writer,
});
const body: []const u8 = aw.written();
```

---

## 12. Format Strings

```zig
try w.print("{s}\n",   .{name});      // slice / string
try w.print("{d}\n",   .{count});     // integer or float (decimal)
try w.print("{x}\n",   .{byte});      // lowercase hex
try w.print("{any}\n", .{value});     // debug format for any type

// ❌ WRONG
// try w.print("{}\n", .{my_slice}); // slices require {s} or {any}
// try w.print("{}\n", .{my_int});   // integers require {d}
```

---

## 13. Path Manipulation (Unchanged)

`std.fs.path` functions remain accessible and unchanged (deprecated alias for `std.Io.Dir.path`).

```zig
const joined  = try std.fs.path.join(allocator, &.{ base, "subdir", "file.txt" });
defer allocator.free(joined);
const dirname  = std.fs.path.dirname(path) orelse ".";
const basename = std.fs.path.basename(path);
const resolved = try std.fs.path.resolve(allocator, &.{ maybe_relative_path });
defer allocator.free(resolved);
const is_abs   = std.fs.path.isAbsolute(path);
```

---

## 14. SHA-256 / Crypto, Random, Alignment

```zig
// SHA-256
var hash_out: [32]u8 = undefined;
std.crypto.hash.sha2.Sha256.hash(data, &hash_out, .{});
const hex = std.fmt.bytesToHex(hash_out, .lower); // [64]u8

// Random
var rng = std.Random.DefaultPrng.init(seed); // seed: u64
const f = rng.random().float(f32);           // [0.0, 1.0)
const n = rng.random().int(u32);

// Alignment
const a1: std.mem.Alignment = .@"1";
const a8: std.mem.Alignment = .@"8";
```

---

## 15. Buffered I/O — Patterns Summary

Do NOT copy writer/reader structs — always use `&fw.interface` (pointer).
Copying causes broken `@fieldParentPtr` chains.

```zig
// Accept a writer in a function — use *std.Io.Writer
fn writeTo(w: *std.Io.Writer) !void {
    try w.print("value: {d}\n", .{123});
    try w.flush();
}

// The common "buffered stdout" pattern
pub fn main(init: std.process.Init.Minimal) !void {
    const io = std.Io.Threaded.global_single_threaded.io();
    var buf: [4096]u8 = undefined;
    var fw = std.Io.File.stdout().writer(io, &buf);
    const w = &fw.interface;
    try w.print("hello\n", .{});
    try w.flush();
    _ = init;
}
```

---

## 16. `std.mem.trim` — No Separate `trimRight` / `trimLeft`

`std.mem.trimRight` and `std.mem.trimLeft` are **removed**. Only `std.mem.trim(T, slice, pattern)`
exists, which trims from both sides. If you need one-sided trim, implement it inline:

```zig
// ✅ CORRECT (both sides)
const trimmed = std.mem.trim(u8, text, " \t\n\r");

// ✅ ONE-SIDED — implement inline
// trimRight: remove trailing chars from pattern
fn trimRight(comptime T: type, slice: []const T, pattern: []const T) []const T {
    var end: usize = slice.len;
    while (end > 0 and std.mem.indexOfScalar(T, pattern, slice[end - 1]) != null) end -= 1;
    return slice[0..end];
}

// trimLeft: remove leading chars from pattern
fn trimLeft(comptime T: type, slice: []const T, pattern: []const T) []const T {
    var start: usize = 0;
    while (start < slice.len and std.mem.indexOfScalar(T, pattern, slice[start]) != null) start += 1;
    return slice[start..];
}

// ❌ WRONG
// std.mem.trimRight(u8, text, " \t")  // removed
// std.mem.trimLeft(u8, text, " \t")   // removed
```

---

## 17. `std.io.fixedBufferStream` — Removed

Replaced by `std.Io.Writer.Allocating` for heap-buffered output, or direct
buffer slicing for fixed-size buffers.

```zig
// ✅ Heap-allocated growing buffer (most common replacement)
var aw: std.Io.Writer.Allocating = .init(allocator);
errdefer aw.deinit();
try aw.writer.print("value: {d}\n", .{42});
const result = try aw.toOwnedSlice();
defer allocator.free(result);

// ✅ Fixed buffer (if you know the max size)
var buf: [4096]u8 = undefined;
// Write to buf manually or use ArrayList
var list: std.ArrayList(u8) = .empty;
defer list.deinit(allocator);
try list.appendSlice(allocator, "data");

// ❌ WRONG
// std.io.fixedBufferStream(&buf)  // removed
```

---

## 18. `std.time.timestamp()` — Removed

Replaced by `std.Io.Timestamp.now(io, .real).nanoseconds` for nanosecond precision,
or compute seconds from that.

```zig
const io = std.Io.Threaded.global_single_threaded.io();

// Seconds-since-epoch (replaces std.time.timestamp())
const epoch_seconds: i64 = @divTrunc(std.Io.Timestamp.now(io, .real).nanoseconds, std.time.ns_per_s);

// Nanoseconds (replaces std.time.nanoTimestamp())
const now_ns: i96 = std.Io.Timestamp.now(io, .real).nanoseconds;
const now_i128: i128 = @as(i128, now_ns);

// ❌ WRONG
// const ts = std.time.timestamp();       // removed
// const ns = std.time.nanoTimestamp();   // removed
```

---

## Master Checklist

| ❌ 0.15 | ✅ 0.16 |
|---|---|
| `pub fn main() !void` | `pub fn main(init: std.process.Init.Minimal) !void` |
| `std.process.argsAlloc(allocator)` | `init.args.iterateAllocator(allocator)` |
| `std.process.getEnvVarOwned(allocator, "KEY")` | `init.environ_map.get("KEY")` (returns `?[]const u8`, not owned) |
| `std.process.getCwdAlloc(allocator)` | `std.process.currentPathAlloc(io, allocator)` |
| `std.heap.GeneralPurposeAllocator(.{}){}` | `std.heap.DebugAllocator(.{}) = .init` |
| `var list: ArrayList(T) = .{}` | `var list: ArrayList(T) = .empty` |
| `list.writer(allocator)` | **REMOVED** — use `std.Io.Writer.Allocating` |
| `std.io.*` (lowercase) | `std.Io.*` (capital I) — `std.io` is removed |
| `std.io.fixedBufferStream(&buf)` | `var aw: std.Io.Writer.Allocating = .init(alloc)` |
| `std.fs.cwd()` | `std.Io.Dir.cwd()` |
| `std.fs.openFileAbsolute(path, opts)` | `std.Io.Dir.openFileAbsolute(io, path, opts)` |
| `std.fs.createFileAbsolute(path, opts)` | `std.Io.Dir.createFileAbsolute(io, path, opts)` |
| `std.fs.openDirAbsolute(path, opts)` | `std.Io.Dir.openDirAbsolute(io, path, opts)` |
| `std.fs.accessAbsolute(path, opts)` | `std.Io.Dir.accessAbsolute(io, path, opts)` |
| `std.fs.deleteFileAbsolute(path)` | `std.Io.Dir.deleteFileAbsolute(io, path)` |
| `std.fs.deleteTreeAbsolute(path)` | `std.Io.Dir.cwd().deleteTree(io, path)` |
| `std.fs.makeDirAbsolute(path)` | `std.Io.Dir.createDirAbsolute(io, path, .default_dir)` |
| `dir.openDir(path, opts)` | `dir.openDir(io, path, opts)` |
| `dir.close()` | `dir.close(io)` |
| `file.close()` | `file.close(io)` |
| `file.stat()` | `file.stat(io)` — returns `Io.File.Stat` with `.size`, `.mtime`, `.kind`, etc. |
| `stat.mtime` as `i128` | `stat.mtime.nanoseconds` (`i96`) — mtime is `Io.Timestamp` |
| `file.updateTimes(atime_ns, mtime_ns)` | `file.setTimestamps(io, .{ .modify_timestamp = .{ .new = ts } })` |
| `file.readToEndAlloc(a, n)` | `dir.readFileAlloc(io, path, a, .limited(n))` |
| `file.readToEndAllocOptions(a, n, ...)` | `dir.readFileAlloc(io, path, a, .limited(n))` |
| `file.writeAll(data)` | **REMOVED** — use `writer.interface.writeAll(data)` with buffered writer |
| `file.getEndPos()` | `file.stat(io).size` or `file.stat(io).size` |
| `dir.readFileAlloc(a, p, n)` | `dir.readFileAlloc(io, p, a, .limited(n))` — note arg order change |
| `cwd.realpathAlloc(alloc, path)` | `std.fs.path.resolve(alloc, &.{path})` (no symlink res) |
| `file.reader(&buf)` | `file.reader(io, &buf)` |
| `file.writer(&buf)` | `file.writer(io, &buf)` |
| `std.fs.File.stdout().writer(&buf)` | `std.Io.File.stdout().writer(io, &buf)` |
| `std.time.timestamp()` | `@divTrunc(std.Io.Timestamp.now(io, .real).nanoseconds, std.time.ns_per_s)` |
| `std.time.nanoTimestamp()` | `std.Io.Timestamp.now(io, .real).nanoseconds` (i96) |
| `@Type(.enum_literal)` | `@EnumLiteral()` |
| `std.process.Child.init(argv, alloc)` | `std.process.spawn(io, .{ .argv = ... })` |
| `child.collectOutput(a, &out, &err, n)` | `std.process.run(alloc, io, .{ .argv = ... })` |
| `child.wait()` | `child.wait(io)` |
| `term.Exited` | `term.exited` (lowercase) |
| `std.process.Child.run(.{ .allocator, .argv })` | `std.process.run(alloc, io, .{ .argv })` |
| `std.fs.File.Writer` (type) | `std.Io.File.Writer` |
| `std.fs.File.Reader` (type) | `std.Io.File.Reader` |
| `std.mem.trimRight(u8, s, pat)` | **REMOVED** — use inline `trimRight` or `std.mem.trim(u8, s, pat)` |
| `std.mem.trimLeft(u8, s, pat)` | **REMOVED** — use inline `trimLeft` or `std.mem.trim(u8, s, pat)` |
| `walker.next()` | `walker.next(io)` |
| `dir.stat()` | `dir.stat(io)` |
| `dir.statFile(sub_path)` | `dir.statFile(io, sub_path, .{})` |