pub const Rc = struct {
    value: *anyopaque,

    pub fn new(value: anytype) Rc {
        return .{ .value = @ptrCast(value) };
    }
};

pub const RcAligned = Rc;
pub const RcUnmanaged = Rc;
pub const RcAlignedUnmanaged = Rc;
pub const Arc = Rc;
pub const ArcAligned = Rc;
pub const ArcUnmanaged = Rc;
pub const ArcAlignedUnmanaged = Rc;
