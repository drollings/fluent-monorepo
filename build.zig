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

    // `local_model` — LocalDecomposer for L4.5 task decomposition (P6.1).
    // Standalone module so cache.zig can import it without pulling it into common module.
    const local_model_module = b.createModule(.{
        .root_source_file = b.path("src/common/local_model.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "llm", .module = llm_module },
        },
    });

    // `reflection` — standalone peer module (promoted from src/common/reflection.zig in P2.4).
    const reflection_module = b.createModule(.{
        .root_source_file = b.path("src/reflection/root.zig"),
        .target = target,
        .optimize = optimize,
    });

    // `common` — full umbrella: reflection, interner, registry, target, hash,
    // context, repl, json_parser, embeddings, etc.
    // All sub-modules are within src/common/ so relative imports are valid.
    const common_module = b.createModule(.{
        .root_source_file = b.path("src/common/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "llm", .module = llm_module },
            .{ .name = "reflection", .module = reflection_module },
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

    // `rdf` — Turtle/N-Quads lexer, parser, and normalization helpers.
    const rdf_module = b.createModule(.{
        .root_source_file = b.path("src/rdf/root.zig"),
        .target = target,
        .optimize = optimize,
    });

    // `ontology` — Triple mapper, YAGO helpers, migration, inference.
    // Depends on rdf for parsing and coral_db for Library/ContextNode types.
    const ontology_module = b.createModule(.{
        .root_source_file = b.path("src/ontology/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "rdf", .module = rdf_module },
            .{ .name = "coral_db", .module = coral_db_module },
        },
    });

    // `coral_batch` — Streaming Turtle ingestion pipeline (batch.zig).
    // Uses named module deps only to avoid cross-directory relative import errors.
    const coral_batch_module = b.createModule(.{
        .root_source_file = b.path("src/coral/batch.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "rdf", .module = rdf_module },
            .{ .name = "ontology", .module = ontology_module },
            .{ .name = "coral_db", .module = coral_db_module },
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
    // Build with `-Dextism=true` when libextism is installed to enable real WASM execution.
    const have_extism = b.option(bool, "extism", "Enable Extism WASM runtime (requires libextism)") orelse false;
    const wasm_options = b.addOptions();
    wasm_options.addOption(bool, "have_extism", have_extism);
    const wasm_module = b.createModule(.{
        .root_source_file = b.path("src/wasm/wasm.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
            .{ .name = "coral_db", .module = coral_db_module },
            .{ .name = "coral_schema", .module = coral_schema_module },
            .{ .name = "options", .module = wasm_options.createModule() },
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

    // Named step used by the Makefile TARGET_BIN rule so that only the
    // guidance binary is (re)installed — coral is left untouched.
    const guidance_step = b.step("guidance", "Build and install the guidance binary");
    guidance_step.dependOn(&b.addInstallArtifact(guidance_exe, .{}).step);

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
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_batch", .module = coral_batch_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "local_model", .module = local_model_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_exe.linkLibC();
    coral_exe.linkSystemLibrary("sqlite3");
    if (have_extism) coral_exe.linkSystemLibrary("extism");
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

    // -- Local model decomposer tests (P6.1) --
    const local_model_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/local_model.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "llm", .module = llm_module },
            },
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
            .root_source_file = b.path("src/vector/vector_db.zig"),
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
    // Now points to the standalone src/reflection/root.zig module (P2.4).
    // All tests from the original reflection.zig and novelreflection/typed_reflection.zig
    // are included in root.zig.
    const reflection_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/reflection/root.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const interner_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/interner.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "reflection", .module = reflection_module },
            },
        }),
    });

    const registry_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/registry.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "reflection", .module = reflection_module },
            },
        }),
    });

    // -- Novel reflection tests (typed reflection extensions) --
    // Now uses the standalone reflection_module (P2.4).
    // typed_reflection.zig content has been merged into src/reflection/typed.zig,
    // binary.zig, and enum_registry.zig; tests are in src/reflection/root.zig.
    // This step remains for build compatibility but points to the new module root.
    const novelreflection_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/reflection/root.zig"),
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
                .{ .name = "local_model", .module = local_model_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_cache_tests.linkLibC();
    coral_cache_tests.linkSystemLibrary("sqlite3");

    // -- Coral MCP server tests --
    // mcp.zig imports cache.zig (relative) which requires common, coral_db, wasm, local_model.
    const coral_mcp_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/mcp.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "local_model", .module = local_model_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_mcp_tests.linkLibC();
    coral_mcp_tests.linkSystemLibrary("sqlite3");

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
    // -- Coral batch ingestion tests (batch.zig + rdf + ontology) --
    const coral_batch_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/batch.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "rdf", .module = rdf_module },
                .{ .name = "ontology", .module = ontology_module },
                .{ .name = "coral_db", .module = coral_db_module },
            },
        }),
    });
    coral_batch_tests.linkLibC();
    coral_batch_tests.linkSystemLibrary("sqlite3");

    // Note: main.zig imports coral_db, coral_batch, wasm, coral_schema, local_model as named modules.
    const coral_main_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_batch", .module = coral_batch_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "local_model", .module = local_model_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_main_tests.linkLibC();
    coral_main_tests.linkSystemLibrary("sqlite3");

    // -- Coral DAG executor tests (M3.3) --
    const coral_executor_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/executor.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_batch", .module = coral_batch_module },
            },
        }),
    });
    coral_executor_tests.linkLibC();
    coral_executor_tests.linkSystemLibrary("sqlite3");

    // -- Coral frontier tests (M6) --
    const coral_frontier_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/frontier.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "coral_db", .module = coral_db_module },
            },
        }),
    });
    coral_frontier_tests.linkLibC();
    coral_frontier_tests.linkSystemLibrary("sqlite3");

    // -------------------------------------------------------------------------
    // Wire all test runs
    // -------------------------------------------------------------------------
    test_step.dependOn(&b.addRunArtifact(explain_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_tests_module).step);
    test_step.dependOn(&b.addRunArtifact(local_model_tests).step);
    test_step.dependOn(&b.addRunArtifact(vector_tests).step);
    test_step.dependOn(&b.addRunArtifact(vector_math_tests).step);
    test_step.dependOn(&b.addRunArtifact(lance_db_tests).step);
    test_step.dependOn(&b.addRunArtifact(reflection_tests).step);
    test_step.dependOn(&b.addRunArtifact(interner_tests).step);
    test_step.dependOn(&b.addRunArtifact(registry_tests).step);
    test_step.dependOn(&b.addRunArtifact(novelreflection_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_targets_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_schema_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_db_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_batch_tests).step);
    test_step.dependOn(&b.addRunArtifact(context_node_schema_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_cache_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_mcp_tests).step);
    test_step.dependOn(&b.addRunArtifact(wasm_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_main_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_executor_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_frontier_tests).step);
}
