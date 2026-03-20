const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{}); // [cite: 1, 19]
    const optimize = b.standardOptimizeOption(.{}); // [cite: 2, 20]

    // ---------------------------------------------------------------------------
    // Dependencies & Common Module
    // ---------------------------------------------------------------------------
    const vaxis = b.dependency("vaxis", .{
        .target = target, // [cite: 20]
        .optimize = optimize, // [cite: 20]
    });

    const common_module = b.createModule(.{
        .root_source_file = b.path("src/common/llm.zig"), // 
        .target = target, // 
        .optimize = optimize, // 
    });

    // ---------------------------------------------------------------------------
    // 1. Guidance Executable
    // ---------------------------------------------------------------------------
    const guidance_exe = b.addExecutable(.{
        .name = "guidance", // 
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/main.zig"), // 
            .target = target, // 
            .optimize = optimize, // 
            .imports = &.{
                .{ .name = "common", .module = common_module }, // [cite: 4]
            },
        }),
    });
    guidance_exe.linkLibC(); // 
    guidance_exe.linkSystemLibrary("sqlite3"); // 
    b.installArtifact(guidance_exe); // 

    const run_guidance = b.addRunArtifact(guidance_exe); // 
    if (b.args) |args| { // 
        run_guidance.addArgs(args); // 
    }
    const run_guidance_step = b.step("run-guidance", "Run guidance"); 
    run_guidance_step.dependOn(&run_guidance.step); // [cite: 6]

    // ---------------------------------------------------------------------------
    // 2. Coral Executable
    // ---------------------------------------------------------------------------
    const cozo_lib_path = b.path("cozo/target/debug"); // [cite: 23]
    const cozo_header_path = b.path("cozo/cozo-lib-c"); // [cite: 23]

    const coral_exe = b.addExecutable(.{
        .name = "coral", // 
        .root_module = b.createModule(.{
            // Note: Changed from src/main.zig to keep it clean in the monorepo
            .root_source_file = b.path("src/coral/main.zig"), 
            .target = target, // 
            .optimize = optimize, // 
            .imports = &.{
                .{ .name = "vaxis", .module = vaxis.module("vaxis") }, // [cite: 24, 25]
                .{ .name = "common", .module = common_module }, // [cite: 25]
            },
        }),
    });
    coral_exe.addLibraryPath(cozo_lib_path); // 
    coral_exe.addIncludePath(cozo_header_path); // 
    coral_exe.linkSystemLibrary("cozo_c"); // 
    coral_exe.linkSystemLibrary("pthread"); // 
    coral_exe.linkSystemLibrary("dl"); // 
    coral_exe.linkLibC(); // 
    b.installArtifact(coral_exe); // 

    const run_coral = b.addRunArtifact(coral_exe); // 
    if (b.args) |args| { // 
        run_coral.addArgs(args); // [cite: 27]
    }
    const run_coral_step = b.step("run-coral", "Run coral");
    run_coral_step.dependOn(&run_coral.step); // [cite: 27]

    // ---------------------------------------------------------------------------
    // 3. Unified Tests
    // ---------------------------------------------------------------------------
    const test_step = b.step("test", "Run all unit tests"); // [cite: 17, 30]

    // --- Guidance Tests ---
    const explain_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/main.zig"), // [cite: 7]
            .target = target, // [cite: 7]
            .optimize = optimize, // [cite: 7]
            .imports = &.{ .{ .name = "common", .module = common_module } }, // [cite: 8]
        }),
    });
    explain_tests.linkLibC(); // [cite: 9]
    explain_tests.linkSystemLibrary("sqlite3"); // [cite: 9]

    const guidance_tests_module = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/tests.zig"), // [cite: 9]
            .target = target, // [cite: 9]
            .optimize = optimize, // [cite: 9]
            .imports = &.{ .{ .name = "common", .module = common_module } }, // [cite: 9]
        }),
    });
    guidance_tests_module.linkLibC(); // [cite: 10]
    guidance_tests_module.linkSystemLibrary("sqlite3"); // [cite: 11]

    const lance_db_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/lance_db.zig"), // [cite: 11]
            .target = target, // [cite: 11]
            .optimize = optimize, // [cite: 11]
            .imports = &.{ .{ .name = "common", .module = common_module } }, // [cite: 11]
        }),
    });
    lance_db_tests.linkLibC(); // [cite: 12]
    lance_db_tests.linkSystemLibrary("sqlite3"); // [cite: 12]

    const vector_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/vector/embeddings.zig"), // [cite: 13]
            .target = target, // [cite: 13]
            .optimize = optimize, // [cite: 13]
            .imports = &.{ .{ .name = "common", .module = common_module } }, // [cite: 13]
        }),
    });

    const vector_math_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/vector/math.zig"), // [cite: 15]
            .target = target, // [cite: 15]
            .optimize = optimize, // [cite: 15]
        }),
    });

    // --- Coral Tests ---
    const coral_unit_tests = b.addTest(.{
        .root_module = b.createModule(.{
            // Adjusted to src/coral/main.zig
            .root_source_file = b.path("src/coral/main.zig"), 
            .target = target, // [cite: 28]
            .optimize = optimize, // [cite: 28]
            .imports = &.{
                .{ .name = "vaxis", .module = vaxis.module("vaxis") }, // [cite: 29]
                .{ .name = "common", .module = common_module }, // [cite: 29]
            },
        }),
    });
    coral_unit_tests.addLibraryPath(cozo_lib_path); // [cite: 30]
    coral_unit_tests.addIncludePath(cozo_header_path); // [cite: 30]
    coral_unit_tests.linkSystemLibrary("cozo_c"); // [cite: 30]
    coral_unit_tests.linkSystemLibrary("pthread"); // [cite: 30]
    coral_unit_tests.linkSystemLibrary("dl"); // [cite: 30]
    coral_unit_tests.linkLibC(); // [cite: 30]

    // --- Wire up all test runs ---
    test_step.dependOn(&b.addRunArtifact(explain_tests).step); // [cite: 16, 17]
    test_step.dependOn(&b.addRunArtifact(guidance_tests_module).step); // [cite: 16, 17]
    test_step.dependOn(&b.addRunArtifact(lance_db_tests).step); // [cite: 16, 17]
    test_step.dependOn(&b.addRunArtifact(vector_tests).step); // [cite: 16, 17]
    test_step.dependOn(&b.addRunArtifact(vector_math_tests).step); // [cite: 16, 17]
    test_step.dependOn(&b.addRunArtifact(coral_unit_tests).step); // [cite: 30]
}
