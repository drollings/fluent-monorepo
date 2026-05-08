//! concurrency — lightweight zio-backed work queue with Fluent WVR interface.
//!
//! ## Public API
//!
//!   // Type-erased work unit
//!   AnyWorkUnit          — 3-pointer handle (ptr, runFn, deinitFn)
//!   WorkUnit(Handler)    — typed wrapper; init() → toAny()
//!
//!   // Execution backends
//!   ExecutionBackend     — VTable interface (submit / flush / deinit)
//!   SyncBackend          — inline, stack-allocatable; safe outside zio runtime
//!   ZioBackend           — async, zio.Group + zio.Semaphore; Fluent Builder
//!
//!   // zio primitives re-exported for caller convenience
//!   Channel              — zio.Channel(T)
//!   Semaphore            — zio.Semaphore
//!   Group                — zio.Group
//!   checkCancel          — zio.checkCancel
//!
//! ## Quick start
//!
//!   const concurrency = @import("concurrency");
//!
//!   // SyncBackend (tests)
//!   var sync = concurrency.SyncBackend{};
//!   const b  = sync.backend();
//!
//!   // ZioBackend (production — must be inside a zio runtime task)
//!   const zb = try concurrency.ZioBackend.builder()
//!       .withPermits(8)
//!       .build(allocator);
//!   defer zb.deinit();
//!   const b = zb.backend();
//!
//!   // Define a handler
//!   const MyHandler = struct {
//!       result_ch: *concurrency.Channel(MyResult),
//!       pub fn execute(self: *MyHandler, arena: std.mem.Allocator) !void {
//!           try concurrency.checkCancel();
//!           const r = try compute(arena);
//!           try self.result_ch.send(r);
//!       }
//!   };
//!
//!   // Submit work
//!   const unit = try concurrency.WorkUnit(MyHandler).init(allocator, handler);
//!   try b.submit(unit.toAny());
//!   try b.flush();

const zio = @import("zio");

// Own types
pub const AnyWorkUnit = @import("work_unit.zig").AnyWorkUnit;
pub const WorkUnit = @import("work_unit.zig").WorkUnit;
pub const ExecutionBackend = @import("backend.zig").ExecutionBackend;
pub const SyncBackend = @import("backend.zig").SyncBackend;
pub const ZioBackend = @import("backend.zig").ZioBackend;

// zio primitives — re-exported so callers need not import zio directly
pub const Channel = zio.Channel;
pub const Semaphore = zio.Semaphore;
pub const Group = zio.Group;
pub const checkCancel = zio.checkCancel;
