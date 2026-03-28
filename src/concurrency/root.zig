//! concurrency/root.zig — Public API re-exports for the concurrency layer.
//!
//! Import this module to access all concurrency primitives:
//!
//!   const concurrency = @import("concurrency");
//!   const Context     = concurrency.Context;
//!   const WorkUnit    = concurrency.WorkUnit;
//!   const AnyWorkUnit = concurrency.AnyWorkUnit;
//!   const Channel     = concurrency.Channel;
//!   const ErrorGroup  = concurrency.ErrorGroup;
//!   const spawn       = concurrency.spawn;

pub const Context = @import("context.zig").Context;
pub const AnyWorkUnit = @import("any_work_unit.zig").AnyWorkUnit;
pub const WorkUnit = @import("any_work_unit.zig").WorkUnit;
pub const Channel = @import("channel.zig").Channel;
pub const ErrorGroup = @import("error_group.zig").ErrorGroup;
pub const spawn = @import("spawn.zig").spawn;
