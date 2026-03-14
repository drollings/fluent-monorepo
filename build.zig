const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // ---------------------------------------------------------------------------
    // Common module — LLM helpers shared across explain-gen source
    // ---------------------------------------------------------------------------
    const common_module = b.createModule(.{
        .root_source_file = b.path("src/common/llm.zig"),
        .target = target,
        .optimize = optimize,
    });

    // ---------------------------------------------------------------------------
    // explain-gen executable
    // ---------------------------------------------------------------------------
    const explain_exe = b.addExecutable(.{
        .name = "explain-gen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/explain-gen/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    explain_exe.linkLibC();
    explain_exe.linkSystemLibrary("sqlite3");
    b.installArtifact(explain_exe);

    const run_explain = b.addRunArtifact(explain_exe);
    if (b.args) |args| {
        run_explain.addArgs(args);
    }
    const run_step = b.step("run", "Run explain-gen");
    run_step.dependOn(&run_explain.step);

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------
    const explain_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/explain-gen/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    explain_tests.linkLibC();
    explain_tests.linkSystemLibrary("sqlite3");

    const run_tests = b.addRunArtifact(explain_tests);
    const test_step = b.step("test", "Run explain-gen unit tests");
    test_step.dependOn(&run_tests.step);
}
