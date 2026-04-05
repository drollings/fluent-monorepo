/// permissions.zig — Role-based permission system for Coral Context reflection.
const std = @import("std");

// ============================================================
// § 1  Role-based permission system
// ============================================================

/// Defines a permission role with fixed structure, manages access invariants, owned by the system.
pub const Role = enum(u3) {
    coder,
    creator,
    staff,
    world,
    tool,
    user,
};

/// Defines permission roles with ownership and access rules; central to access control logic.
pub const RolePermissions = packed struct(u18) {
    coder_read: bool = false,
    creator_read: bool = false,
    staff_read: bool = false,
    world_read: bool = false,
    tool_read: bool = false,
    user_read: bool = false,

    coder_write: bool = false,
    creator_write: bool = false,
    staff_write: bool = false,
    world_write: bool = false,
    tool_write: bool = false,
    user_write: bool = false,

    coder_derive: bool = false,
    creator_derive: bool = false,
    staff_derive: bool = false,
    world_derive: bool = false,
    tool_derive: bool = false,
    user_derive: bool = false,

    pub fn canRead(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_read,
            .creator => self.creator_read,
            .staff => self.staff_read,
            .world => self.world_read,
            .tool => self.tool_read,
            .user => self.user_read,
        };
    }

    pub fn canWrite(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_write,
            .creator => self.creator_write,
            .staff => self.staff_write,
            .world => self.world_write,
            .tool => self.tool_write,
            .user => self.user_write,
        };
    }

    pub fn canDerive(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_derive,
            .creator => self.creator_derive,
            .staff => self.staff_derive,
            .world => self.world_derive,
            .tool => self.tool_derive,
            .user => self.user_derive,
        };
    }
};

/// All roles may read and write.  Suitable as a default for open/config data.
pub const perm_all: RolePermissions = .{
    .coder_read = true,
    .creator_read = true,
    .staff_read = true,
    .world_read = true,
    .tool_read = true,
    .user_read = true,
    .coder_write = true,
    .creator_write = true,
    .staff_write = true,
    .world_write = true,
    .tool_write = true,
    .user_write = true,
    .coder_derive = true,
    .creator_derive = true,
    .staff_derive = true,
    .world_derive = true,
    .tool_derive = true,
    .user_derive = true,
};

/// Only the coder role has full access.  Use for engine-internal fields.
pub const perm_coder: RolePermissions = .{
    .coder_read = true,
    .coder_write = true,
    .coder_derive = true,
};

/// Coders and creators have full access; staff can read and write.
pub const perm_staff: RolePermissions = .{
    .coder_read = true,
    .coder_write = true,
    .coder_derive = true,
    .creator_read = true,
    .creator_write = true,
    .creator_derive = true,
    .staff_read = true,
    .staff_write = true,
};

/// All roles may read; only coders and creators may write.
pub const perm_public_read: RolePermissions = .{
    .coder_read = true,
    .creator_read = true,
    .staff_read = true,
    .world_read = true,
    .tool_read = true,
    .user_read = true,
    .coder_write = true,
    .creator_write = true,
};


