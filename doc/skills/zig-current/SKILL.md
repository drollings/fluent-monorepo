---
name: zig-0.15
description: Accurate Zig 0.15.x API patterns — read this before writing any Zig 0.15 code
---

# Zig 0.15.x Reference

> **For agentic coders**: Models trained before late 2025 have stale Zig knowledge.
> Every section below documents a breaking change from 0.14. Read it all before writing code.

---

## 1. ArrayList — Now Unmanaged by Default

`std.ArrayList` **no longer stores the allocator**. Pass it to every mutating call.
`std.ArrayListUnmanaged` is now a deprecated alias for `std.ArrayList`.
The old managed behaviour (allocator stored in the list) lives at `std.array_list.Managed(T)`.

```zig
// ✅ CORRECT (0.15)
var list: std.ArrayList(u8) = .empty;
defer list.deinit(allocator);
try list.append(allocator, 'a');
try list.appendSlice(allocator, "bc");
const slice = list.items;                     // NOT list.toSlice()

// Pre-allocated capacity
var list2 = try std.ArrayList(u8).initCapacity(allocator, 64);
defer list2.deinit(allocator);
list2.appendAssumeCapacity('x');              // no allocator needed once capacity is reserved

// Managed variant (stores allocator) — use when you want the old 0.14 behaviour
var managed = std.array_list.Managed(u8).init(allocator);
defer managed.deinit();
try managed.append('a');                      // no allocator arg

// ❌ WRONG — compile errors in 0.15
// var list = std.ArrayList(u8).init(allocator); // .init() removed
// try list.append('a');                          // missing allocator arg
// list.deinit();                                 // missing allocator arg
// list.toSlice()                                 // removed; use .items
```

---

## 2. HashMap — Still Managed

**Unlike ArrayList, `std.StringHashMap` and `std.AutoHashMap` are still managed** —
they store the allocator and `put`/`deinit` do NOT take an allocator argument.

```zig
// ✅ CORRECT (0.15) — managed (unchanged from 0.14)
var map = std.StringHashMap(u32).init(allocator);
defer map.deinit();                            // no allocator arg
try map.put("key", 42);                       // no allocator arg
const val = map.get("key");                   // ?u32

var amap = std.AutoHashMap(u32, u32).init(allocator);
defer amap.deinit();
try amap.put(1, 100);

// Unmanaged variants exist and require allocator on every call
var umap: std.StringHashMapUnmanaged(u32) = .empty;
defer umap.deinit(allocator);
try umap.put(allocator, "key", 42);

// ❌ WRONG — HashMap did NOT change; these are compile errors
// map.put(allocator, "key", 42);   // managed put() takes NO allocator
// map.deinit(allocator);            // managed deinit() takes NO allocator
```

---

## 3. I/O — Mandatory Buffer + Flush ("Writergate")

All readers and writers now require an explicit stack buffer. The type-erased interface
is `std.Io.Writer` / `std.Io.Reader`. **Always flush before return or close.**
Do NOT copy the interface struct — always use `&fw.interface` (pointer). Copying it
causes undefined behaviour via broken `@fieldParentPtr` chains.

```zig
// Stdout
var out_buf: [4096]u8 = undefined;
var out_fw = std.fs.File.stdout().writer(&out_buf);
const stdout = &out_fw.interface;
try stdout.print("hello {s}\n", .{"world"});
try stdout.flush();                           // REQUIRED

// File write
var buf: [4096]u8 = undefined;
var f = try std.fs.cwd().createFile("out.txt", .{});
defer f.close();
var fw = f.writer(&buf);
const w = &fw.interface;
try w.print("data {d}\n", .{42});
try w.flush();                                // REQUIRED before close

// Read a whole file — simplest approach (no reader boilerplate)
const content = try std.fs.cwd().readFileAlloc(allocator, "data.txt", std.math.maxInt(usize));
defer allocator.free(content);

// Read via Reader (when streaming or already have a file handle)
try f.seekTo(0);
var rb: [4096]u8 = undefined;
var fr = f.reader(&rb);
// allocRemaining streams to EOF; .limited(n) caps at n bytes, .unlimited has no cap
const data = try fr.interface.allocRemaining(allocator, .limited(1024 * 1024));
defer allocator.free(data);

// Unbuffered I/O — pass empty slice; writes go directly to OS, no flush needed
var uw = std.fs.File.stdout().writer(&.{});
try uw.interface.writeAll("immediate\n");

// Accept a writer in a function — use *std.Io.Writer, not anytype
fn writeTo(w: *std.Io.Writer) !void {
    try w.print("value: {d}\n", .{123});
}
```

**Migration table:**

| 0.14 | 0.15 |
|------|------|
| `std.io.getStdOut()` | `std.fs.File.stdout()` |
| `file.writer()` | `file.writer(&buf)` — buffer required |
| `file.reader()` | `file.reader(&buf)` — buffer required |
| No flush | `try w.flush()` before close/return |
| Pass writer by value | Use `&fw.interface` — pointer only, never copy |
| `file.reader().readAllAlloc(a, n)` | `fr.interface.allocRemaining(a, .limited(n))` |
| `std.fs.cwd().readFileAlloc(a, p, n)` | Unchanged — simplest whole-file read |

---

## 4. Format Strings — Explicit Specifiers Required

```zig
// ✅ CORRECT
try w.print("{s}\n",   .{name});      // slice / string
try w.print("{d}\n",   .{count});     // integer or float (decimal)
try w.print("{x}\n",   .{byte});      // lowercase hex
try w.print("{X}\n",   .{byte});      // uppercase hex
try w.print("{*}\n",   .{ptr});       // pointer address
try w.print("{any}\n", .{value});     // debug format for any type
// {} only works for types that implement a custom format() method

// ❌ WRONG — compile errors in 0.15
// try w.print("{}\n", .{my_slice});  // slices require {s} or {any}
// try w.print("{}\n", .{my_int});    // integers require {d}
```

---

## 5. JSON Serialization

Use `std.io.Writer.Allocating` (lowercase `io`) to capture output dynamically.
Access the result with `.written()` — not raw internal field access.

```zig
// Serialize — result borrowed from the writer (valid until next write or deinit)
var aw: std.io.Writer.Allocating = .init(allocator);
defer aw.deinit();
try std.json.Stringify.value(my_value, .{}, &aw.writer);
const json: []const u8 = aw.written();

// Serialize — transfer ownership to caller
const owned = try aw.toOwnedSlice();   // caller must free
defer allocator.free(owned);

// Idiomatic one-liner using json.fmt
var aw2: std.io.Writer.Allocating = .init(allocator);
defer aw2.deinit();
try aw2.writer.print("{f}", .{std.json.fmt(my_value, .{})});
const json2 = aw2.written();

// Parse (unchanged from 0.14)
var parsed = try std.json.parseFromSlice(std.json.Value, allocator, json, .{});
defer parsed.deinit();

// ❌ WRONG — do not use raw field access
// const body = aw.writer.buffer[0..aw.writer.end]; // use aw.written() instead
```

---

## 6. HTTP Client (`std.http.Client`)

`client.request()` was removed. Use `client.fetch()`. Capture the response body
with `std.Io.Writer.Allocating` and read it via `.written()`.

```zig
// POST — capture response body
var client = std.http.Client{ .allocator = allocator };
defer client.deinit();

var aw: std.Io.Writer.Allocating = .init(allocator);
defer aw.deinit();

const result = try client.fetch(.{
    .method = .POST,
    .location = .{ .url = "http://localhost:11434/api/chat" },
    .extra_headers = &[_]std.http.Header{
        .{ .name = "Content-Type", .value = "application/json" },
    },
    .payload = json_body_slice,
    .response_writer = &aw.writer,     // *std.Io.Writer — pointer to inner writer field
});
// result.status is std.http.Status (.ok == 200)
const body: []const u8 = aw.written();

// GET — discard body, check status only
const result2 = try client.fetch(.{
    .method = .GET,
    .location = .{ .url = "http://example.com/health" },
    // omit response_writer — body is discarded automatically
});
const ok = result2.status == .ok;
```

---

## 7. Allocators

`GeneralPurposeAllocator` was **renamed** to `DebugAllocator`. The old name is a
deprecated alias that still compiles but should not appear in new code.

```zig
// ✅ CORRECT (0.15)
var gpa: std.heap.DebugAllocator(.{}) = .init;
defer if (gpa.deinit() == .leak) @panic("leak");
const allocator = gpa.allocator();

// Arena (unchanged)
var arena = std.heap.ArenaAllocator.init(allocator);
defer arena.deinit();                  // frees all arena allocations at once
const aa = arena.allocator();

// High-performance production allocator (no safety checks)
const alloc = std.heap.smp_allocator; // thread-safe, no init/deinit needed

// ❌ DEPRECATED — compiles via alias but do not use in new code
// var gpa = std.heap.GeneralPurposeAllocator(.{}){}; // old name + old init style
```

---

## 8. Child Process

```zig
var child = std.process.Child.init(&[_][]const u8{ "cmd", "arg" }, allocator);
child.stdin_behavior  = .Ignore;
child.stdout_behavior = .Pipe;
child.stderr_behavior = .Pipe;
try child.spawn();

var stdout: std.ArrayList(u8) = .empty;
var stderr: std.ArrayList(u8) = .empty;
defer stdout.deinit(allocator);
defer stderr.deinit(allocator);

try child.collectOutput(&stdout, &stderr, 10 * 1024 * 1024);
const term = try child.wait();
switch (term) {
    .Exited => |code| if (code != 0) return error.NonZeroExit,
    else => return error.ProcessFailed,
}
// stdout.items and stderr.items are []u8 slices of the output
```

---

## 9. File I/O — Sentinel Slices for AST Parsing

```zig
const source = try file.readToEndAllocOptions(
    allocator, max_size, null, .@"1", 0  // 0 = null terminator → [:0]const u8
);
defer allocator.free(source);
var ast = try std.zig.Ast.parse(allocator, source, .zig);
defer ast.deinit(allocator);
```

---

## 10. SHA-256

```zig
var hash_out: [32]u8 = undefined;
std.crypto.hash.sha2.Sha256.hash(data, &hash_out, .{});
const hex = std.fmt.bytesToHex(hash_out, .lower);  // [64]u8
```

---

## 11. Random Number Generation

```zig
var rng = std.Random.DefaultPrng.init(seed);   // seed: u64
const f = rng.random().float(f32);             // [0.0, 1.0)
const n = rng.random().int(u32);
```

---

## 12. Time

```zig
const ns: i128 = std.time.nanoTimestamp();     // returns i128 — must cast before arithmetic
const ms: u64  = @intCast(std.time.milliTimestamp());
const us: u64  = @intCast(std.time.microTimestamp());
```

---

## 13. Thread Synchronization

```zig
var ready = std.Thread.ResetEvent{};
// worker thread:
ready.set();
// main thread (blocks until set):
ready.wait();
```

---

## 14. mem.Alignment Enum

```zig
const a1: std.mem.Alignment = .@"1";  // NOT @alignOf(u8) as usize
const a8: std.mem.Alignment = .@"8";
```

---

## 15. String Case

```zig
const lower = try std.ascii.allocLowerString(allocator, str);
defer allocator.free(lower);
```

---

## Master Checklist

| ❌ 0.14 / Wrong | ✅ 0.15 / Correct |
|---|---|
| `ArrayList.init(alloc)` | `.empty` or `try .initCapacity(alloc, n)` |
| `list.append(item)` | `list.append(allocator, item)` |
| `list.deinit()` | `list.deinit(allocator)` |
| `list.toSlice()` | `list.items` |
| `StringHashMap.init()` without allocator | `.init(allocator)` — allocator required |
| `map.put(allocator, k, v)` (managed) | `map.put(k, v)` — managed maps take NO allocator |
| `map.deinit(allocator)` (managed) | `map.deinit()` — managed maps take NO allocator |
| `std.io.getStdOut()` | `std.fs.File.stdout()` |
| `file.writer()` | `file.writer(&buf)` |
| `file.reader()` | `file.reader(&buf)` |
| No `.flush()` | `try w.flush()` before close/return |
| Passing writer by value | Always `&fw.interface` — pointer only |
| `fr.interface.readAlloc(a, n)` (exact n bytes) | `fr.interface.allocRemaining(a, .limited(n))` to EOF |
| `aw.writer.buffer[0..aw.writer.end]` | `aw.written()` |
| `GeneralPurposeAllocator(.{}){}` | `DebugAllocator(.{}) = .init` |
| `print("{}", .{slice_or_int})` | `{s}` for slices, `{d}` for ints, `{any}` for unknown |
| `client.request()` | `client.fetch()` |
| Alignment as `usize` | `std.mem.Alignment` enum, e.g. `.@"1"` |
| `nanoTimestamp()` stored as `u64` | Cast: `@intCast(std.time.nanoTimestamp())` — returns `i128` |
