//! channel.zig — Bounded, mutex-backed MPMC channel (M13).
//!
//! ## Design choice: mutex-backed, not lock-free
//!
//! Lock-free queues are subtle (ABA problems, ordering requirements) and only
//! outperform mutex queues under sustained high-frequency contention with many
//! producers.  Channel operations here happen at work-unit granularity
//! (milliseconds per item).  A mutex-protected ring buffer with `Condition`
//! signals is correct by inspection, blocks properly (no spin), and is fast
//! enough by a large margin.
//!
//! ## Ownership
//!
//! `Channel(T).init()` heap-allocates the channel and its buffer.  Call
//! `deinit()` when done.  If `T` contains heap-allocated data, the caller is
//! responsible for freeing items — same contract as `ArrayList`.
//!
//! ## Close semantics (matching Go)
//!
//! - After `close()`: `send` returns `error.ChannelClosed`.
//! - After `close()`: `recv` returns remaining buffered items, then null.
//! - Closing an already-closed channel is safe (idempotent).

const std = @import("std");

/// Converts a generic type to a channel type, ensuring safe data flow between concurrent contexts.
pub fn Channel(comptime T: type) type {
    return struct {
        const Self = @This();

        buf: []T,
        head: usize, // index of next item to read
        tail: usize, // index of next slot to write
        count: usize, // number of items in the buffer
        closed: bool,
        mu: std.Thread.Mutex,
        not_full: std.Thread.Condition, // signalled when a slot is freed
        not_empty: std.Thread.Condition, // signalled when an item is added
        allocator: std.mem.Allocator,

        /// Allocate the channel and its ring buffer.  `capacity` must be > 0.
        pub fn init(allocator: std.mem.Allocator, capacity: usize) !*Self {
            std.debug.assert(capacity > 0);
            const self = try allocator.create(Self);
            self.* = .{
                .buf = try allocator.alloc(T, capacity),
                .head = 0,
                .tail = 0,
                .count = 0,
                .closed = false,
                .mu = .{},
                .not_full = .{},
                .not_empty = .{},
                .allocator = allocator,
            };
            return self;
        }

        /// Free the ring buffer and the channel struct.
        pub fn deinit(self: *Self) void {
            self.allocator.free(self.buf);
            self.allocator.destroy(self);
        }

        /// Blocking send.  Waits if the buffer is full.
        /// Returns `error.ChannelClosed` if the channel is closed.
        pub fn send(self: *Self, value: T) !void {
            self.mu.lock();
            defer self.mu.unlock();

            while (self.count == self.buf.len) {
                if (self.closed) return error.ChannelClosed;
                self.not_full.wait(&self.mu);
            }
            if (self.closed) return error.ChannelClosed;

            self.buf[self.tail] = value;
            self.tail = (self.tail + 1) % self.buf.len;
            self.count += 1;
            self.not_empty.signal();
        }

        /// Non-blocking send.  Returns immediately with an error if full or closed.
        pub fn trySend(self: *Self, value: T) !void {
            self.mu.lock();
            defer self.mu.unlock();

            if (self.closed) return error.ChannelClosed;
            if (self.count == self.buf.len) return error.ChannelFull;

            self.buf[self.tail] = value;
            self.tail = (self.tail + 1) % self.buf.len;
            self.count += 1;
            self.not_empty.signal();
        }

        /// Blocking receive.  Returns null only when the channel is closed AND empty.
        ///
        /// Drain-after-close pattern (equivalent to Go's `for v := range ch`):
        ///   while (ch.recv()) |value| { process(value); }
        pub fn recv(self: *Self) ?T {
            self.mu.lock();
            defer self.mu.unlock();

            while (self.count == 0) {
                if (self.closed) return null;
                self.not_empty.wait(&self.mu);
            }

            const value = self.buf[self.head];
            self.head = (self.head + 1) % self.buf.len;
            self.count -= 1;
            self.not_full.signal();
            return value;
        }

        /// Non-blocking receive.  Returns null if the buffer is empty.
        /// Check `isClosed()` separately if you need to distinguish empty-closed
        /// from empty-open.
        pub fn tryRecv(self: *Self) ?T {
            self.mu.lock();
            defer self.mu.unlock();

            if (self.count == 0) return null;

            const value = self.buf[self.head];
            self.head = (self.head + 1) % self.buf.len;
            self.count -= 1;
            self.not_full.signal();
            return value;
        }

        /// Signal that no more values will be sent.
        /// In-flight `recv` calls will drain remaining items and then return null.
        /// In-flight `send` calls after `close` return `error.ChannelClosed`.
        /// Idempotent: closing an already-closed channel is a no-op.
        pub fn close(self: *Self) void {
            self.mu.lock();
            defer self.mu.unlock();
            self.closed = true;
            self.not_empty.broadcast();
            self.not_full.broadcast();
        }

        /// Returns true if the channel has been closed.
        pub fn isClosed(self: *Self) bool {
            self.mu.lock();
            defer self.mu.unlock();
            return self.closed;
        }

        /// Returns the number of items currently in the buffer (snapshot).
        pub fn len(self: *Self) usize {
            self.mu.lock();
            defer self.mu.unlock();
            return self.count;
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "Channel: send then recv returns value" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    try ch.send(42);
    const v = ch.recv();
    try testing.expectEqual(@as(?i32, 42), v);
}

test "Channel: send then recv preserves FIFO order" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    try ch.send(1);
    try ch.send(2);
    try ch.send(3);
    try testing.expectEqual(@as(?i32, 1), ch.recv());
    try testing.expectEqual(@as(?i32, 2), ch.recv());
    try testing.expectEqual(@as(?i32, 3), ch.recv());
}

test "Channel: recv on empty closed channel returns null" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    ch.close();
    try testing.expectEqual(@as(?i32, null), ch.recv());
}

test "Channel: send on closed channel returns ChannelClosed" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    ch.close();
    try testing.expectError(error.ChannelClosed, ch.send(1));
}

test "Channel: trySend returns ChannelFull when buffer full" {
    const ch = try Channel(i32).init(testing.allocator, 2);
    defer ch.deinit();

    try ch.trySend(1);
    try ch.trySend(2);
    try testing.expectError(error.ChannelFull, ch.trySend(3));
}

test "Channel: trySend returns ChannelClosed when closed" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    ch.close();
    try testing.expectError(error.ChannelClosed, ch.trySend(1));
}

test "Channel: drain-after-close pattern" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    try ch.send(10);
    try ch.send(20);
    try ch.send(30);
    ch.close();

    var sum: i32 = 0;
    while (ch.recv()) |v| sum += v;
    try testing.expectEqual(@as(i32, 60), sum);
}

test "Channel: close is idempotent" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    ch.close();
    ch.close(); // must not panic or deadlock
    try testing.expect(ch.isClosed());
}

test "Channel: tryRecv returns null on empty channel" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    try testing.expectEqual(@as(?i32, null), ch.tryRecv());
}

test "Channel: len tracks item count" {
    const ch = try Channel(i32).init(testing.allocator, 4);
    defer ch.deinit();

    try testing.expectEqual(@as(usize, 0), ch.len());
    try ch.send(1);
    try testing.expectEqual(@as(usize, 1), ch.len());
    _ = ch.recv();
    try testing.expectEqual(@as(usize, 0), ch.len());
}

test "Channel: N senders, 1 receiver — all values received exactly once" {
    const N = 8;
    const M = 100; // items per sender
    const ch = try Channel(i32).init(testing.allocator, N * M);
    defer ch.deinit();

    const SenderCtx = struct {
        ch: *Channel(i32),
        start: i32,
        count: usize,

        fn run(self: @This()) void {
            for (0..self.count) |i| {
                self.ch.send(self.start + @as(i32, @intCast(i))) catch {};
            }
        }
    };

    var threads: [N]std.Thread = undefined;
    for (0..N) |i| {
        threads[i] = try std.Thread.spawn(.{}, SenderCtx.run, .{SenderCtx{
            .ch = ch,
            .start = @as(i32, @intCast(i)) * M,
            .count = M,
        }});
    }
    for (&threads) |*t| t.join();
    ch.close();

    var total: usize = 0;
    while (ch.recv()) |_| total += 1;
    try testing.expectEqual(@as(usize, N * M), total);
}

test "Channel: 1 sender, N receivers — each value received exactly once" {
    const N = 4;
    const M = 100;
    const ch = try Channel(i32).init(testing.allocator, 16);
    defer ch.deinit();

    var received = std.atomic.Value(usize).init(0);

    const RecvCtx = struct {
        ch: *Channel(i32),
        received: *std.atomic.Value(usize),

        fn run(self: @This()) void {
            while (self.ch.recv()) |_| {
                _ = self.received.fetchAdd(1, .monotonic);
            }
        }
    };

    var threads: [N]std.Thread = undefined;
    for (0..N) |i| {
        threads[i] = try std.Thread.spawn(.{}, RecvCtx.run, .{RecvCtx{
            .ch = ch,
            .received = &received,
        }});
    }

    for (0..M) |i| try ch.send(@intCast(i));
    ch.close();

    for (&threads) |*t| t.join();
    try testing.expectEqual(@as(usize, M), received.load(.monotonic));
}

test "Channel: blocking send unblocks when receiver consumes" {
    const ch = try Channel(i32).init(testing.allocator, 1);
    defer ch.deinit();

    const T = struct {
        fn recvAfterDelay(c: *Channel(i32)) void {
            std.Thread.sleep(1 * std.time.ns_per_ms);
            _ = c.recv();
        }
    };

    try ch.send(1); // fills the single-slot buffer

    const thread = try std.Thread.spawn(.{}, T.recvAfterDelay, .{ch});
    try ch.send(2); // blocks until receiver drains slot 1
    thread.join();
}

test "Channel: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    const ch = try Channel(i32).init(gpa.allocator(), 4);
    try ch.send(1);
    _ = ch.recv();
    ch.deinit();
}
