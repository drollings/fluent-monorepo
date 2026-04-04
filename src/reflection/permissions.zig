/// permissions.zig — Role-based permission system for Coral Context reflection.
const std = @import("std");

// ============================================================
// § 1  Role-based permission system
// ============================================================

/// Defines a permission role with fixed-size buffers, managed via ownership and invariants.
pub const Role = enum(u3) {
    coder,
    creator,
    staff,
    world,
    script,
    player,
};

/// Defines permission roles with ownership and access rules; manages role metadata and permissions.
pub const RolePermissions = packed struct(u18) {
    coder_read: bool = false,
    creator_read: bool = false,
    staff_read: bool = false,
    world_read: bool = false,
    script_read: bool = false,
    player_read: bool = false,

    coder_write: bool = false,
    creator_write: bool = false,
    staff_write: bool = false,
    world_write: bool = false,
    script_write: bool = false,
    player_write: bool = false,

    coder_derive: bool = false,
    creator_derive: bool = false,
    staff_derive: bool = false,
    world_derive: bool = false,
    script_derive: bool = false,
    player_derive: bool = false,

    pub fn canRead(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_read,
            .creator => self.creator_read,
            .staff => self.staff_read,
            .world => self.world_read,
            .script => self.script_read,
            .player => self.player_read,
        };
    }

    pub fn canWrite(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_write,
            .creator => self.creator_write,
            .staff => self.staff_write,
            .world => self.world_write,
            .script => self.script_write,
            .player => self.player_write,
        };
    }

    pub fn canDerive(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_derive,
            .creator => self.creator_derive,
            .staff => self.staff_derive,
            .world => self.world_derive,
            .script => self.script_derive,
            .player => self.player_derive,
        };
    }
};

/// All roles may read and write.  Suitable as a default for open/config data.
pub const perm_all: RolePermissions = .{
    .coder_read = true,
    .creator_read = true,
    .staff_read = true,
    .world_read = true,
    .script_read = true,
    .player_read = true,
    .coder_write = true,
    .creator_write = true,
    .staff_write = true,
    .world_write = true,
    .script_write = true,
    .player_write = true,
    .coder_derive = true,
    .creator_derive = true,
    .staff_derive = true,
    .world_derive = true,
    .script_derive = true,
    .player_derive = true,
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
    .script_read = true,
    .player_read = true,
    .coder_write = true,
    .creator_write = true,
};
