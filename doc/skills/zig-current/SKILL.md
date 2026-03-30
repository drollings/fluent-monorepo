---
name: zig-current
description: Documentation of Zig 0.15.x APIs and coding patterns
---

# Zig 0.15.x Agent Skill

## ArrayList (Managed vs Unmanaged)

**Managed ArrayList** (recommended for most code):
```zig
// Initialize with .empty (Zig 0.15+)
var list: std.ArrayList(u8) = .empty;
defer list.deinit();
try list.append('a');                    // allocator NOT needed

// Or for types that need allocator in init:
var list = try std.ArrayList(u8).initCapacity(allocator, 100);
defer list.deinit();

// Access underlying slice
const slice = list.items;                 // NOT list.toSlice()
```

**Unmanaged ArrayList** (for ArenaAllocator patterns):
```zig
var list: std.ArrayListUnmanaged(u8) = .{};
defer list.deinit(allocator);
try list.append(allocator, 'a');         // allocator IS required

const slice = list.items;
```

**Common mistakes:**
- `list.init(allocator)` → Use `list = .{}` or `list = .empty`
- `list.append(item)` for unmanaged → Use `list.append(allocator, item)`
- `list.toSlice()` → Use `list.items`

## HashMap (Same Pattern as ArrayList)

```zig
// Managed
var map: std.StringHashMap(u32) = .{};
defer map.deinit();
try map.put(allocator, "key", 42);

// Unmanaged
var map: std.StringHashMapUnmanaged(u32) = .{};
defer map.deinit(allocator);
try map.put(allocator, "key", 42);
```

## I/O: Mandatory Buffer + Flush

Zig 0.15 uses a new I/O system with explicit buffers:

```zig
// Writer (stack buffer - REQUIRED)
var buf: [4096]u8 = undefined;
var f = try std.fs.cwd().createFile("out.txt", .{});
defer f.close();
var fw = f.writer(&buf);
const w = &fw.interface;
try w.print("data\n", .{});
try w.flush();                // REQUIRED before close/return

// Reader (stack buffer - REQUIRED)
var rb = f.reader(&buf);
const r = &rb.interface;
const data = try r.readAllAlloc(allocator, 1024);

// Stdout (stack buffer - use File.stdout(), NOT getStdOut())
var out_buf: [4096]u8 = undefined;
var out_fw = std.fs.File.stdout().writer(&out_buf);
try out_fw.interface.print("hello\n", .{});
try out_fw.interface.flush();
```

**Summary table:**
| Wrong (0.14) | Right (0.15) |
|--------------|--------------|
| `std.io.getStdOut()` | `std.fs.File.stdout()` |
| `file.writer()` | `file.writer(&buf)` |
| `file.reader()` | `file.reader(&buf)` |
| No flush | `try w.flush()` before return/close |
| Passing `fw` directly | Use `&fw.interface` for type-erased writer |

**Rule**: Always `flush()` before close/return. Never copy the interface—use `&fw.interface`.

## Format Strings: Explicit Specifiers Required

Zig 0.15 requires explicit format specifiers for slices and integers:

```zig
// WRONG - compile error:
try w.print("name: {}", .{name});           // slice without specifier
try w.print("count: {}", .{count});         // integer without specifier

// CORRECT:
try w.print("name: {s}\n", .{name});         // {s} for slices
try w.print("count: {d}\n", .{count});       // {d} for integers
try w.print("value: {any}\n", .{value});     // {any} for debug output
try w.print("ptr: {*}\n", .{ptr});           // {*} for pointers
```

**Format string rules:**
- `{s}` - slice (string)
- `{d}` - integer/float
- `{any}` - debug format (for unknown types)
- `{}` - only for types with custom `format` method
- `{x}` - lowercase hex
- `{X}` - uppercase hex

## JSON Serialization

```zig
var out = std.Io.Writer.Allocating.init(allocator);
defer out.deinit();
try std.json.Stringify.value(value, .{}, &out.writer);
const json = try out.toOwnedSlice();

// Parse
var parsed = try std.json.parseFromSlice(std.json.Value, allocator, content, .{});
defer parsed.deinit();
```

## HTTP Client (`std.http.Client`)

Use `client.fetch()` — `client.request()` no longer exists.

### POST with response body capture

```zig
var client = std.http.Client{ .allocator = allocator };
defer client.deinit();

// Capture response into a growable buffer.
var aw: std.Io.Writer.Allocating = .init(allocator);
defer aw.deinit();

const result = try client.fetch(.{
    .method = .POST,
    .location = .{ .url = "http://localhost:11434/api/chat" },
    .extra_headers = &[_]std.http.Header{
        .{ .name = "Content-Type", .value = "application/json" },
    },
    .payload = json_body_slice,
    .response_writer = &aw.writer,
});
// result.status is std.http.Status (.ok == 200)
const body: []const u8 = aw.writer.buffer[0..aw.writer.end];
```

### GET (health-check / discard body)

```zig
const result = try client.fetch(.{
    .method = .GET,
    .location = .{ .url = check_url },
    // Omit response_writer — body is discarded automatically.
});
const ok = result.status == .ok;
```

**Rules**:
- `std.Io.Writer.Allocating` is the standard sink for dynamic response bodies.
- `response_writer` is `?*std.Io.Writer`; pass `&aw.writer` (not `&aw`).
- Omit `response_writer` when you only need the status code.
- Never use shell subprocesses (curl) when Zig native HTTP is available.

## Child Process

```zig
var child = std.process.Child.init(&[_]const u8{"cmd", "arg"}, allocator);
child.stdin_behavior = .Ignore;
child.stdout_behavior = .Pipe;
child.stderr_behavior = .Pipe;
try child.spawn();

var stdout = std.ArrayList(u8).init(allocator);
try child.collectOutput(&stdout, &stderr, 10 * 1024 * 1024);
_ = try child.wait();
```

## File I/O: Sentinel Slices

```zig
const source = try file.readToEndAllocOptions(
    allocator, max_size, null, .@"1", 0  // 0 = null terminator
);
try std.zig.Ast.parse(allocator, source, .zig);  // source is [:0]const u8
```

## mem.Alignment Enum

```zig
const align_val: std.mem.Alignment = .@"1";  // not @alignOf(u8) as usize
const align_8: std.mem.Alignment = .@"8";    // for 8-byte alignment
```

## SHA-256

```zig
var hash_out: [32]u8 = undefined;
std.crypto.hash.sha2.Sha256.hash(data, &hash_out, .{});
const hex = std.fmt.bytesToHex(hash_out, .lower);
```

## String Case

```zig
const lower = try std.ascii.allocLowerString(allocator, str);
```

## Random Number Generation

```zig
var rng = std.Random.DefaultPrng.init(seed);
const float_val = rng.random().float(f32);
const int_val = rng.random().int(u32);
```

## Time

```zig
const ns: i128 = std.time.nanoTimestamp();
const ms: u64 = @intCast(std.time.milliTimestamp());
const us: u64 = @intCast(std.time.microTimestamp());
```

## Thread Synchronization

```zig
var ready = std.Thread.ResetEvent{};

// In worker thread:
ready.set();                           // Signal ready

// In main thread:
ready.wait();                          // Block until signaled
```

## Allocators

```zig
var gpa = std.heap.GeneralPurposeAllocator(.{}){};
defer if (gpa.deinit() == .leak) @panic("leak");

var arena = std.heap.ArenaAllocator.init(gpa.allocator());
defer arena.deinit();                   // Frees everything at once
const aa = arena.allocator();
```

## API Migration Table

| Zig 0.14 | Zig 0.15 |
|----------|----------|
| `std.io.getStdOut()` | `std.fs.File.stdout()` |
| `file.writer()` | `file.writer(&buf)` |
| `ArrayList.init(alloc)` | `ArrayList.empty` or `.{}` |
| `ArrayListUnmanaged.init(alloc)` | `.{}` |
| `list.toSlice()` | `list.items` |
| `print("{}", .{slice})` | `print("{s}", .{slice})` |
| `print("{}", .{int})` | `print("{d}", .{int})` |
| `client.request()` | `client.fetch()` |
| No flush needed | `try w.flush()` required |

## Quick Checklist

| Check | Fix |
|-------|-----|
| `.init(allocator)` on ArrayList | Use `.{}` or `.empty` |
| `.append(item)` on Unmanaged | Use `.append(allocator, item)` |
| `file.writer()` missing buffer | Add `&buf` argument |
| No `.flush()` | Add before close/return |
| Passing interface by value | Use `&fw.interface` |
| Struct format `{}` | Use `{f}` or `{s}` or `{d}` |
| Shell sed/awk braces | Escape as `{{` `}}` |
| `readToEndAlloc` for AST | Use `readToEndAllocOptions` with sentinel 0 |
| Alignment as usize | Use `.@"1"` enum |
| `client.request()` | Use `client.fetch()` (request API removed) |
| HTTP response body | Use `std.Io.Writer.Allocating`; pass `&aw.writer` |
| Curl subprocess for HTTP | Use `std.http.Client.fetch()` instead |
| `std.io.getStdOut()` | Use `std.fs.File.stdout()` |
| `ArrayList.init(a)` | Use `ArrayList.empty` or `try ArrayList.initCapacity(a, n)` |
| `ArrayListUnmanaged.init(a)` | Use `.{}` |
| Random without seed | Always init: `var rng = std.Random.DefaultPrng.init(seed);` |
| `nanoTimestamp()` as u64 | Cast: `@intCast(std.time.nanoTimestamp())` (returns i128) |
