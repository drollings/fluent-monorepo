const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // -------------------------------------------------------------------------
    // External dependencies
    // -------------------------------------------------------------------------
    const vaxis = b.dependency("vaxis", .{
        .target = target,
        .optimize = optimize,
    });
    _ = vaxis; // Reserved for future TUI work; remove _ when coral TUI lands.

    // -------------------------------------------------------------------------
    // Core named modules
    // -------------------------------------------------------------------------

    // LLM inference client (pure HTTP, no common deps).
    const llm_module = b.createModule(.{
        .root_source_file = b.path("src/llm/root.zig"),
        .target = target,
        .optimize = optimize,
    });

    // `common` — full umbrella: reflection, interner, registry, target, hash,
    // context, repl, json_parser, embeddings, etc.
    // All sub-modules are within src/common/ so relative imports are valid.
    const common_module = b.createModule(.{
        .root_source_file = b.path("src/common/llm.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "llm", .module = llm_module },
        },
    });

    // `coral_db` — Coral SQLite backend (ContextNode, Library, HydrationPipeline, ContextPacker).
    // Depends on common for reflection.
    const coral_db_module = b.createModule(.{
        .root_source_file = b.path("src/coral/db.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
        },
    });

    // `coral_schema` — Binary IPC schema (BinaryContextNode, BinaryExecutionRequest, …).
    // Uses named module deps only (no relative imports) so it does not conflict
    // with coral_db_module when both appear in the same compilation (e.g. wasm_tests).
    const coral_schema_module = b.createModule(.{
        .root_source_file = b.path("src/coral/context_node_schema.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
            .{ .name = "coral_db", .module = coral_db_module },
        },
    });

    // `vector` — cosine search, embeddings, hybrid merge.
    const vector_module = b.createModule(.{
        .root_source_file = b.path("src/vector/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
        },
    });

    // `wasm` — Extism WASM sandboxing + binary IPC.
    const wasm_module = b.createModule(.{
        .root_source_file = b.path("src/wasm/wasm.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
            .{ .name = "coral_db", .module = coral_db_module },
            .{ .name = "coral_schema", .module = coral_schema_module },
        },
    });

    // -------------------------------------------------------------------------
    // 1. Guidance executable
    // -------------------------------------------------------------------------
    const guidance_exe = b.addExecutable(.{
        .name = "guidance",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "vector", .module = vector_module },
            },
        }),
    });
    guidance_exe.linkLibC();
    guidance_exe.linkSystemLibrary("sqlite3");
    b.installArtifact(guidance_exe);

    const run_guidance = b.addRunArtifact(guidance_exe);
    if (b.args) |args| run_guidance.addArgs(args);
    const run_guidance_step = b.step("run-guidance", "Run guidance");
    run_guidance_step.dependOn(&run_guidance.step);

    // -------------------------------------------------------------------------
    // 2. Coral executable
    //    The coral binary is a DAG build-runner (like make).
    //    Coral Context database modules (db.zig, cache.zig, …) are compiled
    //    separately via their own test targets.
    // -------------------------------------------------------------------------
    const coral_exe = b.addExecutable(.{
        .name = "coral",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    coral_exe.linkLibC();
    coral_exe.linkSystemLibrary("sqlite3");
    b.installArtifact(coral_exe);
    coral_exe.step.dependOn(&guidance_exe.step);

    const run_coral = b.addRunArtifact(coral_exe);
    if (b.args) |args| run_coral.addArgs(args);
    const run_coral_step = b.step("run-coral", "Run coral");
    run_coral_step.dependOn(&run_coral.step);

    // -------------------------------------------------------------------------
    // 3. Unified test step
    // -------------------------------------------------------------------------
    const test_step = b.step("test", "Run all unit tests");

    // -- Guidance tests --
    const explain_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "vector", .module = vector_module },
            },
        }),
    });
    explain_tests.linkLibC();
    explain_tests.linkSystemLibrary("sqlite3");

    const guidance_tests_module = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/tests.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "vector", .module = vector_module },
            },
        }),
    });
    guidance_tests_module.linkLibC();
    guidance_tests_module.linkSystemLibrary("sqlite3");

    // -- Vector / embedding tests --
    const vector_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/embeddings.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const vector_math_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/vector/math.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const lance_db_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/vector/lance_db.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    lance_db_tests.linkLibC();
    lance_db_tests.linkSystemLibrary("sqlite3");

    // -- Common / reflection tests --
    const reflection_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/reflection.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const interner_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/interner.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const registry_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/registry.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral schema / target tests (need common for reflection) --
    const coral_targets_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/targets.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const coral_schema_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/schema.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral database + context node schema tests --
    const coral_db_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/db.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    coral_db_tests.linkLibC();
    coral_db_tests.linkSystemLibrary("sqlite3");

    // context_node_schema uses named modules only (coral_db for ContextNode + schema),
    // so it can be tested standalone without file-conflict issues.
    const context_node_schema_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/context_node_schema.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
            },
        }),
    });
    context_node_schema_tests.linkLibC();
    context_node_schema_tests.linkSystemLibrary("sqlite3");

    // -- Coral cache tier tests --
    const coral_cache_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/cache.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "wasm", .module = wasm_module },
            },
        }),
    });
    coral_cache_tests.linkLibC();
    coral_cache_tests.linkSystemLibrary("sqlite3");

    // -- WASM IPC tests (no Extism runtime — tests cover pure Zig parts) --
    const wasm_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/wasm/wasm.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
            },
        }),
    });
    wasm_tests.linkLibC();
    wasm_tests.linkSystemLibrary("sqlite3");

    // -- Coral main integration test (pulls in schema, db, context_node_schema, scrub) --
    // Note: coral/main.zig runtime does not use wasm directly; wasm excluded to avoid
    // module conflict with the relative db.zig/schema.zig imports in the test block.
    const coral_main_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    coral_main_tests.linkLibC();
    coral_main_tests.linkSystemLibrary("sqlite3");

    // -------------------------------------------------------------------------
    // Wire all test runs
    // -------------------------------------------------------------------------
    test_step.dependOn(&b.addRunArtifact(explain_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_tests_module).step);
    test_step.dependOn(&b.addRunArtifact(vector_tests).step);
    test_step.dependOn(&b.addRunArtifact(vector_math_tests).step);
    test_step.dependOn(&b.addRunArtifact(lance_db_tests).step);
    test_step.dependOn(&b.addRunArtifact(reflection_tests).step);
    test_step.dependOn(&b.addRunArtifact(interner_tests).step);
    test_step.dependOn(&b.addRunArtifact(registry_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_targets_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_schema_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_db_tests).step);
    test_step.dependOn(&b.addRunArtifact(context_node_schema_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_cache_tests).step);
    test_step.dependOn(&b.addRunArtifact(wasm_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_main_tests).step);
}
