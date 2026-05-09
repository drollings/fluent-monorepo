const std = @import("std");

pub fn build(b: *std.Build) void {
    // Use the local zig-crt/ directory (crt_dir in libc.conf) to avoid the
    // R_X86_64_PC64 relocation in gcc-15's crt1.o .sframe section, which
    // Zig 0.14–0.16's linker cannot handle.  Setting b.libc_file propagates
    // --libc <path> to every compile step automatically.
    b.libc_file = b.pathFromRoot("libc.conf");

    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // -------------------------------------------------------------------------
    // External dependencies
    // -------------------------------------------------------------------------
    // vaxis: reserved for future TUI work. Disabled until its uucode transitive
    // dependency is compatible with Zig 0.15.
    // const vaxis = b.dependency("vaxis", .{
    //     .target = target,
    //     .optimize = optimize,
    // });

    const zigsharedstring = b.dependency("zigsharedstring", .{
        .target = target,
        .optimize = optimize,
    });

    const zigrc = b.dependency("zigrc", .{
        .target = target,
        .optimize = optimize,
    });

    const zio_dep = b.dependency("zio", .{
        .target = target,
        .optimize = optimize,
    });

    const clap_dep = b.dependency("clap", .{
        .target = target,
        .optimize = optimize,
    });
    const clap_module = clap_dep.module("clap");

    // -------------------------------------------------------------------------
    // Core named modules
    // -------------------------------------------------------------------------

    // `reflection` — standalone peer module (promoted from src/common/reflection.zig in P2.4).
    const reflection_module = b.createModule(.{
        .root_source_file = b.path("src/reflection/root.zig"),
        .target = target,
        .optimize = optimize,
    });

    // `coral_csr` — CSR graph module (csr_graph.zig). Standalone, no deps.
    // Declared before common_module because common's root.zig imports it as a named module.
    const coral_csr_module = b.createModule(.{
        .root_source_file = b.path("src/common/csr_graph.zig"),
        .target = target,
        .optimize = optimize,
    });

    // `common` — full umbrella: reflection, interner, registry, target, hash,
    // context, repl, json_parser, embeddings, etc.
    // All sub-modules are within src/common/ so relative imports are valid.
    // Note: common does NOTimport dag to avoid circular dependency.
    // DAG types are defined in src/dag/ and consumers should import from "dag" module directly.
    // SharedString is imported from the external zigsharedstring package.
    // Arc, Rc, and reference-counting primitives from the external zigrc package.
    // csr_graph is imported as a named module to avoid Zig 0.16 file-ownership conflict
    // (the same .zig file cannot belong to two modules).
    const common_module = b.createModule(.{
        .root_source_file = b.path("src/common/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "reflection", .module = reflection_module },
            .{ .name = "zigsharedstring", .module = zigsharedstring.module("zigsharedstring") },
            .{ .name = "zigrc", .module = zigrc.module("zigrc") },
            .{ .name = "csr_graph", .module = coral_csr_module },
        },
    });

    // LLM inference client.
    const llm_module = b.createModule(.{
        .root_source_file = b.path("src/llm/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
        },
    });

    // `concurrency` — zio-backed work queue with Fluent WVR interface.
    const concurrency_module = b.createModule(.{
        .root_source_file = b.path("src/concurrency/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "zio", .module = zio_dep.module("zio") },
        },
    });

    // `dag` — DAG execution engine (Target, Registry, Resolver, Executor).
    // Uses named module imports for common (interner, builder_error).
    const dag_module = b.createModule(.{
        .root_source_file = b.path("src/dag/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
            .{ .name = "reflection", .module = reflection_module },
            .{ .name = "concurrency", .module = concurrency_module },
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

    const simhash_module = b.createModule(.{
        .root_source_file = b.path("src/vector/simhash.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "common", .module = common_module },
        },
    });
    _ = simhash_module;

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
            .{ .name = "dag", .module = dag_module },
            .{ .name = "options", .module = wasm_options.createModule() },
        },
    });

    // -------------------------------------------------------------------------
    // Tree-sitter libraries — AST parsing for non-Zig languages
    // -------------------------------------------------------------------------
    const ts_root = "/opt/src/development/tree-sitter";

    const treesitter_c = b.addLibrary(.{
        .name = "tree-sitter",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_c.root_module.addCSourceFile(.{
        .file = .{ .cwd_relative = ts_root ++ "/tree-sitter/lib/src/lib.c" },
        .flags = &.{ "-std=c11", "-O2", "-D_DEFAULT_SOURCE", "-D_POSIX_C_SOURCE=200809L" },
    });
    treesitter_c.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });
    treesitter_c.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/src" });

    const treesitter_python = b.addLibrary(.{
        .name = "tree-sitter-python",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_python.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-python/src" },
        .files = &.{ "parser.c", "scanner.c" },
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_python.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-python/src" });
    treesitter_python.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const treesitter_cpp = b.addLibrary(.{
        .name = "tree-sitter-cpp",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_cpp.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-cpp/src" },
        .files = &.{ "parser.c", "scanner.c" },
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_cpp.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-cpp/src" });
    treesitter_cpp.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const treesitter_rust = b.addLibrary(.{
        .name = "tree-sitter-rust",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_rust.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-rust/src" },
        .files = &.{ "parser.c", "scanner.c" },
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_rust.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-rust/src" });
    treesitter_rust.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const treesitter_go = b.addLibrary(.{
        .name = "tree-sitter-go",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_go.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-go/src" },
        .files = &.{"parser.c"},
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_go.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-go/src" });
    treesitter_go.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const treesitter_typescript = b.addLibrary(.{
        .name = "tree-sitter-typescript",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_typescript.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-typescript/typescript/src" },
        .files = &.{ "parser.c", "scanner.c" },
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_typescript.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-typescript/typescript/src" });
    treesitter_typescript.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const treesitter_tsx = b.addLibrary(.{
        .name = "tree-sitter-tsx",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_tsx.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-typescript/tsx/src" },
        .files = &.{ "parser.c", "scanner.c" },
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_tsx.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-typescript/tsx/src" });
    treesitter_tsx.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const treesitter_php = b.addLibrary(.{
        .name = "tree-sitter-php",
        .root_module = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true }),
    });
    treesitter_php.root_module.addCSourceFiles(.{
        .root = .{ .cwd_relative = ts_root ++ "/tree-sitter-php/php/src" },
        .files = &.{ "parser.c", "scanner.c" },
        .flags = &.{ "-std=c11", "-O2" },
    });
    treesitter_php.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter-php/php/src" });
    treesitter_php.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

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
                .{ .name = "llm", .module = llm_module },
                .{ .name = "vector", .module = vector_module },
                .{ .name = "clap", .module = clap_module },
            },
        }),
    });
    guidance_exe.root_module.link_libc = true;
    guidance_exe.root_module.linkSystemLibrary("sqlite3", .{});
    guidance_exe.root_module.linkLibrary(treesitter_c);
    guidance_exe.root_module.linkLibrary(treesitter_python);
    guidance_exe.root_module.linkLibrary(treesitter_cpp);
    guidance_exe.root_module.linkLibrary(treesitter_rust);
    guidance_exe.root_module.linkLibrary(treesitter_go);
    guidance_exe.root_module.linkLibrary(treesitter_typescript);
    guidance_exe.root_module.linkLibrary(treesitter_tsx);
    guidance_exe.root_module.linkLibrary(treesitter_php);
    guidance_exe.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });
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
                .{ .name = "dag", .module = dag_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_batch", .module = coral_batch_module },
                .{ .name = "ontology", .module = ontology_module },
                .{ .name = "rdf", .module = rdf_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "llm", .module = llm_module },
                .{ .name = "csr_graph", .module = coral_csr_module },
                .{ .name = "clap", .module = clap_module },
            },
        }),
    });
    coral_exe.root_module.link_libc = true;
    coral_exe.root_module.linkSystemLibrary("sqlite3", .{});
    if (have_extism) coral_exe.root_module.linkSystemLibrary("extism", .{});
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
                .{ .name = "llm", .module = llm_module },
                .{ .name = "vector", .module = vector_module },
                .{ .name = "clap", .module = clap_module },
            },
        }),
    });
    explain_tests.root_module.link_libc = true;
    explain_tests.root_module.linkSystemLibrary("sqlite3", .{});
    explain_tests.root_module.linkLibrary(treesitter_c);
    explain_tests.root_module.linkLibrary(treesitter_python);
    explain_tests.root_module.linkLibrary(treesitter_cpp);
    explain_tests.root_module.linkLibrary(treesitter_rust);
    explain_tests.root_module.linkLibrary(treesitter_go);
    explain_tests.root_module.linkLibrary(treesitter_typescript);
    explain_tests.root_module.linkLibrary(treesitter_tsx);
    explain_tests.root_module.linkLibrary(treesitter_php);
    explain_tests.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    const guidance_tests_module = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/tests.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "llm", .module = llm_module },
                .{ .name = "vector", .module = vector_module },
                .{ .name = "clap", .module = clap_module },
            },
        }),
    });
    guidance_tests_module.root_module.link_libc = true;
    guidance_tests_module.root_module.linkSystemLibrary("sqlite3", .{});
    guidance_tests_module.root_module.linkLibrary(treesitter_c);
    guidance_tests_module.root_module.linkLibrary(treesitter_python);
    guidance_tests_module.root_module.linkLibrary(treesitter_cpp);
    guidance_tests_module.root_module.linkLibrary(treesitter_rust);
    guidance_tests_module.root_module.linkLibrary(treesitter_go);
    guidance_tests_module.root_module.linkLibrary(treesitter_typescript);
    guidance_tests_module.root_module.linkLibrary(treesitter_tsx);
    guidance_tests_module.root_module.linkLibrary(treesitter_php);
    guidance_tests_module.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

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
            .root_source_file = b.path("src/vector/vector_db.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    lance_db_tests.root_module.link_libc = true;
    lance_db_tests.root_module.linkSystemLibrary("sqlite3", .{});

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
            .root_source_file = b.path("src/dag/registry.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
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
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
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
    coral_db_tests.root_module.link_libc = true;
    coral_db_tests.root_module.linkSystemLibrary("sqlite3", .{});

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
    context_node_schema_tests.root_module.link_libc = true;
    context_node_schema_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- Coral cache tier tests --
    const coral_cache_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/cache.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "dag", .module = dag_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_cache_tests.root_module.link_libc = true;
    coral_cache_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- Coral MCP server tests --
    // mcp.zig imports cache.zig (relative) which requires common, coral_db, dag, wasm, llm.
    const coral_mcp_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/mcp.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "dag", .module = dag_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_mcp_tests.root_module.link_libc = true;
    coral_mcp_tests.root_module.linkSystemLibrary("sqlite3", .{});

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
                .{ .name = "dag", .module = dag_module },
            },
        }),
    });
    wasm_tests.root_module.link_libc = true;
    wasm_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- Coral main integration test (pulls in schema, db, context_node_schema) --
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
    coral_batch_tests.root_module.link_libc = true;
    coral_batch_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // Note: main.zig imports coral_db, coral_batch, wasm, coral_schema, llm as named modules.
    // ontology + rdf added for yago_ingest.zig which imports both.
    const coral_main_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_batch", .module = coral_batch_module },
                .{ .name = "ontology", .module = ontology_module },
                .{ .name = "rdf", .module = rdf_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "llm", .module = llm_module },
                .{ .name = "clap", .module = clap_module },
                .{ .name = "csr_graph", .module = coral_csr_module },
            },
        }),
    });
    coral_main_tests.root_module.link_libc = true;
    coral_main_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- HNSW index tests (M5.1) — standalone, no external deps --
    const hnsw_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/vector/hnsw.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

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
    coral_executor_tests.root_module.link_libc = true;
    coral_executor_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- Coral frontier tests (M6 / M2.4 / M4.2) --
    const coral_frontier_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/frontier.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });
    coral_frontier_tests.root_module.link_libc = true;
    coral_frontier_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- Resolver tests (M3.1 getLevels) --
    const resolver_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/dag/resolver.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "reflection", .module = reflection_module },
            },
        }),
    });

    // -- Typed ID handle tests (M6.2) --
    const types_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/types.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // --SimHash tests (Task 3.2) --
    const simhash_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/vector/simhash.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral frontier_tool_compiler tests (Task 5.1) --
    const coral_frontier_tool_compiler_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/frontier_tool_compiler.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- LLM anonymize tests (moved from coral/ Task 8.2) --
    const coral_anonymize_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/llm/anonymize.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M1: BuilderError structured error context (builder_error.zig) --
    const builder_error_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/builder_error.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M6: Validation rules pipeline (validate.zig) --
    const validate_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/reflection/validate.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M4: Schema versioning primitives (schema_version.zig) --
    const schema_version_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/reflection/schema_version.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M8: Structured logging context and scope (logging.zig) --
    const logging_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/logging.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M7: Reference-counted VTable handles (refcount.zig) --
    const refcount_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/refcount.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M9: Conditional wrappers (wrapper.zig) --
    const wrapper_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/wrapper.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M3: Mock VTable testing infrastructure (mock_vtable.zig) --
    const mock_vtable_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/testing/mock_vtable.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- M11: WorkUnit(T) — type-erased work unit (new zio-based module) --
    const concurrency_work_unit_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/concurrency/work_unit.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "zio", .module = zio_dep.module("zio") },
            },
        }),
    });

    // -- M11: concurrency root — spawn convenience function --
    const concurrency_root_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/concurrency/root.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "zio", .module = zio_dep.module("zio") },
            },
        }),
    });

    // -- M11: ExecutionBackend / SyncBackend / ZioBackend --
    const concurrency_backend_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/concurrency/backend.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "zio", .module = zio_dep.module("zio") },
            },
        }),
    });


    // -- Coral BitSet DRIFT tests (P0.2) --
    const coral_drift_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/drift.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });

    // -- Guidance identifier match tests (P0.4) --
    const guidance_identifier_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/query/identifier.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Guidance batch LLM filter tests (P0.5) -- (run via guidance_tests_module)
    // -- Coral CSR graph tests (P1.1) --
    const coral_csr_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/csr_graph.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral Union-Find tests (P1.6) --
    const coral_union_find_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithms/union_find.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral Degree Centrality tests (P1.2) --
    const coral_degree_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithms/degree_centrality.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral PageRank tests (P1.3) --
    const coral_pagerank_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithms/pagerank.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "csr_graph", .module = coral_csr_module },
            },
        }),
    });

    // -- Coral Shortest Path (Dijkstra) tests (P1.4) --
    const coral_shortest_path_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithms/shortest_path.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "csr_graph", .module = coral_csr_module },
            },
        }),
    });

    // -- Coral Louvain Community Detection tests (P1.5) --
    const coral_louvain_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithms/louvain.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "csr_graph", .module = coral_csr_module },
            },
        }),
    });

    // -- Coral Edge Weights tests (P1.7) --
    const coral_edge_weights_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithms/edge_weights.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral Session persistence tests (P2.1) --
    const coral_session_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/session.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    coral_session_tests.root_module.link_libc = true;
    coral_session_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- Coral Frozen Snapshot tests (P2.2) --
    const coral_frozen_snapshot_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/frozen_snapshot.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // -- Coral Context Compressor tests (P2.3) --
    const coral_context_compressor_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/llm/context_compressor.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const coral_context_packer_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/llm/context_packer.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const coral_global_search_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/global_search.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const coral_algorithm_runner_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/algorithm_runner.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const coral_type_inference_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/type_inference.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    // =========================================================================
    // New *_tests.zig — individual focused test modules
    // =========================================================================

    // -- src/common: no named module imports --
    const common_builder_error_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/builder_error_tests.zig"), .target = target, .optimize = optimize }) });
    const common_hash_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/hash_tests.zig"), .target = target, .optimize = optimize }) });
    const common_io_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/io_tests.zig"), .target = target, .optimize = optimize }) });
    const common_json_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/json_tests.zig"), .target = target, .optimize = optimize }) });
    const common_logging_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/logging_tests.zig"), .target = target, .optimize = optimize }) });
    const common_pattern_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/pattern_tests.zig"), .target = target, .optimize = optimize }) });
    const common_shell_parser_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/shell_parser_tests.zig"), .target = target, .optimize = optimize }) });
    const common_shell_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/shell_tests.zig"), .target = target, .optimize = optimize }) });
    const common_source_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/source_tests.zig"), .target = target, .optimize = optimize }) });
    const common_string_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/string_tests.zig"), .target = target, .optimize = optimize }) });
    const common_url_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/url_tests.zig"), .target = target, .optimize = optimize }) });
    const common_wrapper_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/common/wrapper_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/guidance: no named module imports --
    const guidance_doc_parser_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/guidance/doc_parser_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/guidance/health: no named module imports --
    const guidance_health_build_validation_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/guidance/health/build_validation_tests.zig"), .target = target, .optimize = optimize }) });
    const guidance_health_orphan_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/guidance/health/orphan_tests.zig"), .target = target, .optimize = optimize }) });
    const guidance_health_test_audit_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/guidance/health/test_audit_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/guidance/sync: no named module imports --
    const guidance_sync_marker_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/guidance/sync/marker_tests.zig"), .target = target, .optimize = optimize, .imports = &.{ .{ .name = "common", .module = common_module } } }) });

    // -- src/llm: no named module imports --
    const llm_token_budget_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/llm/token_budget_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/rdf: no named module imports --
    const rdf_lexer_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/rdf/lexer_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/reflection: no named module imports --
    const reflection_schema_version_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/reflection/schema_version_tests.zig"), .target = target, .optimize = optimize }) });
    const reflection_sql_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/reflection/sql_tests.zig"), .target = target, .optimize = optimize }) });
    const reflection_validate_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/reflection/validate_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/testing: no named module imports --
    const testing_mock_vtable_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/testing/mock_vtable_tests.zig"), .target = target, .optimize = optimize }) });

    // -- src/vector: no named module imports --
    const vector_simhash_tests = b.addTest(.{ .root_module = b.createModule(.{ .root_source_file = b.path("src/vector/simhash_tests.zig"), .target = target, .optimize = optimize }) });

    // -- subdirectory tests (10 files) needing src/guidance/ as module root --
    // These tests import ../types.zig which is outside their own subdirectory module root.
    // A shim at src/guidance/ expands the module root to src/guidance/, making the relative
    // imports resolve correctly. Deps are the union of all 10 tests' requirements:
    // common + llm (sync→enhancer) + vector + sqlite3 (health/strategy) + treesitter (extractor).
    const guidance_subdirectory_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/subdirectory_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "llm", .module = llm_module },
                .{ .name = "vector", .module = vector_module },
            },
        }),
    });
    guidance_subdirectory_tests.root_module.link_libc = true;
    guidance_subdirectory_tests.root_module.linkSystemLibrary("sqlite3", .{});
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_c);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_python);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_cpp);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_rust);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_go);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_typescript);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_tsx);
    guidance_subdirectory_tests.root_module.linkLibrary(treesitter_php);
    guidance_subdirectory_tests.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    // -- guidance tests needing @import("common") --
    const guidance_document_indexer_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/document_indexer_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    const guidance_git_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/git_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    const guidance_hash_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/hash_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    const guidance_health_test_mover_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/health/test_mover_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    const guidance_plugin_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/plugin_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    // NOTE: guidance_markdown_plugin_tests, guidance_zig_plugin_tests compiled
    // via guidance_tests_module (imports ../types.zig outside standalone module path).
    const guidance_provider_discovery_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/provider_discovery_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    guidance_provider_discovery_tests.root_module.link_libc = true;
    const guidance_stage_builder_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/stage_builder_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    // NOTE: guidance_sync_line_verify_tests compiled via guidance_tests_module
    // (imports ../types.zig which is outside standalone module path).
    const guidance_triage_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/triage_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    const guidance_types_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/types_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });

    // -- guidance tests needing common + llm --
    // NOTE: guidance_comments_sync_tests compiled via guidance_tests_module
    // (imports ../types.zig which is outside standalone module path).
    const guidance_enhancer_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/enhancer_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "llm", .module = llm_module },
            },
        }),
    });

    // -- guidance tests needing llm only --
    const guidance_todo_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/todo_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "llm", .module = llm_module } },
        }),
    });

    // -- guidance tests needing vector + sqlite3 --
    const guidance_health_extractor_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/health/extractor_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "vector", .module = vector_module } },
        }),
    });
    guidance_health_extractor_tests.root_module.link_libc = true;
    guidance_health_extractor_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- guidance tests needing common + vector + sqlite3 --
    // NOTE: guidance_health_tests compiled via guidance_tests_module
    // (health.zig imports ../config.zig which is outside standalone module path).
    // NOTE: guidance_query_strategy_tests compiled via guidance_tests_module
    // (strategy.zig imports ../types.zig which is outside standalone module path).

    const guidance_ralph_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/ralph_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "vector", .module = vector_module },
            },
        }),
    });
    guidance_ralph_tests.root_module.link_libc = true;
    guidance_ralph_tests.root_module.linkSystemLibrary("sqlite3", .{});

    const guidance_staged_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/staged_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "llm", .module = llm_module },
                .{ .name = "vector", .module = vector_module },
            },
        }),
    });
    guidance_staged_tests.root_module.link_libc = true;
    guidance_staged_tests.root_module.linkSystemLibrary("sqlite3", .{});

    // -- guidance plugin tests needing treesitter C libraries --
    // NOTE: guidance_treesitter_extractor_tests compiled via guidance_tests_module
    // (treesitter_extractor.zig imports ../types.zig outside standalone module path).

    const guidance_treesitter_loader_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/plugins/treesitter_loader_tests.zig"),
            .target = target, .optimize = optimize,
        }),
    });
    guidance_treesitter_loader_tests.root_module.link_libc = true;
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_c);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_python);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_cpp);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_rust);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_go);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_typescript);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_tsx);
    guidance_treesitter_loader_tests.root_module.linkLibrary(treesitter_php);
    guidance_treesitter_loader_tests.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    // -- plugin registry tests needing common + treesitter (via treesitter_plugin.zig) --
    const guidance_plugin_registry_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/guidance/plugin_registry_tests.zig"),
            .target = target, .optimize = optimize,
            .imports = &.{ .{ .name = "common", .module = common_module } },
        }),
    });
    guidance_plugin_registry_tests.root_module.link_libc = true;
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_c);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_python);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_cpp);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_rust);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_go);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_typescript);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_tsx);
    guidance_plugin_registry_tests.root_module.linkLibrary(treesitter_php);
    guidance_plugin_registry_tests.root_module.addIncludePath(.{ .cwd_relative = ts_root ++ "/tree-sitter/lib/include" });

    // -------------------------------------------------------------------------
    // Wire all test runs
    // -------------------------------------------------------------------------

    const main_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/main_tests.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
                .{ .name = "coral_db", .module = coral_db_module },
                .{ .name = "coral_batch", .module = coral_batch_module },
                .{ .name = "ontology", .module = ontology_module },
                .{ .name = "rdf", .module = rdf_module },
                .{ .name = "wasm", .module = wasm_module },
                .{ .name = "coral_schema", .module = coral_schema_module },
                .{ .name = "llm", .module = llm_module },
                .{ .name = "clap", .module = clap_module },
                .{ .name = "csr_graph", .module = coral_csr_module },
            },
        }),
    });
    main_tests.root_module.link_libc = true;
    main_tests.root_module.linkSystemLibrary("sqlite3", .{});

    const math_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/vector/math_tests.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const vector_db_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/vector/vector_db_tests.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    vector_db_tests.root_module.link_libc = true;
    vector_db_tests.root_module.linkSystemLibrary("sqlite3", .{});

    const root_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/llm/root_tests.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });

    const embeddings_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/embeddings_tests.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });


    const work_unit_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/concurrency/work_unit_tests.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "zio", .module = zio_dep.module("zio") },
            },
        }),
    });
    test_step.dependOn(&b.addRunArtifact(explain_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_tests_module).step);
    test_step.dependOn(&b.addRunArtifact(vector_tests).step);
    test_step.dependOn(&b.addRunArtifact(vector_math_tests).step);
    test_step.dependOn(&b.addRunArtifact(hnsw_tests).step);
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
    test_step.dependOn(&b.addRunArtifact(simhash_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_frontier_tool_compiler_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_anonymize_tests).step);
    test_step.dependOn(&b.addRunArtifact(resolver_tests).step);
    test_step.dependOn(&b.addRunArtifact(types_tests).step);
    test_step.dependOn(&b.addRunArtifact(builder_error_tests).step);
    test_step.dependOn(&b.addRunArtifact(schema_version_tests).step);
    test_step.dependOn(&b.addRunArtifact(validate_tests).step);
    test_step.dependOn(&b.addRunArtifact(logging_tests).step);
    test_step.dependOn(&b.addRunArtifact(refcount_tests).step);
    test_step.dependOn(&b.addRunArtifact(wrapper_tests).step);
    test_step.dependOn(&b.addRunArtifact(mock_vtable_tests).step);
    test_step.dependOn(&b.addRunArtifact(concurrency_work_unit_tests).step);
    test_step.dependOn(&b.addRunArtifact(concurrency_root_tests).step);
    test_step.dependOn(&b.addRunArtifact(concurrency_backend_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_drift_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_identifier_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_csr_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_union_find_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_degree_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_pagerank_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_shortest_path_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_louvain_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_edge_weights_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_session_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_frozen_snapshot_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_context_compressor_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_context_packer_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_global_search_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_algorithm_runner_tests).step);
    test_step.dependOn(&b.addRunArtifact(coral_type_inference_tests).step);

    // -- New foundation module tests (from REVIEW_20260425) --
    const tokenizer_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/tokenizer.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    test_step.dependOn(&b.addRunArtifact(tokenizer_tests).step);

    const word_index_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/word_index.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    test_step.dependOn(&b.addRunArtifact(word_index_tests).step);

    const freq_table_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/freq_table.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    test_step.dependOn(&b.addRunArtifact(freq_table_tests).step);

    const trigram_index_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/trigram_index.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    test_step.dependOn(&b.addRunArtifact(trigram_index_tests).step);

    const entity_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/common/entity.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "common", .module = common_module },
            },
        }),
    });
    test_step.dependOn(&b.addRunArtifact(entity_tests).step);
    test_step.dependOn(&b.addRunArtifact(main_tests).step);
    test_step.dependOn(&b.addRunArtifact(math_tests).step);
    test_step.dependOn(&b.addRunArtifact(vector_db_tests).step);
    test_step.dependOn(&b.addRunArtifact(root_tests).step);
    test_step.dependOn(&b.addRunArtifact(embeddings_tests).step);

    // -- new *_tests.zig individual modules --
    test_step.dependOn(&b.addRunArtifact(common_builder_error_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_hash_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_io_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_json_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_logging_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_pattern_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_shell_parser_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_shell_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_source_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_string_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_url_tests).step);
    test_step.dependOn(&b.addRunArtifact(common_wrapper_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_doc_parser_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_health_build_validation_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_health_orphan_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_health_test_audit_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_sync_marker_tests).step);
    test_step.dependOn(&b.addRunArtifact(llm_token_budget_tests).step);
    test_step.dependOn(&b.addRunArtifact(rdf_lexer_tests).step);
    test_step.dependOn(&b.addRunArtifact(reflection_schema_version_tests).step);
    test_step.dependOn(&b.addRunArtifact(reflection_sql_tests).step);
    test_step.dependOn(&b.addRunArtifact(reflection_validate_tests).step);
    test_step.dependOn(&b.addRunArtifact(testing_mock_vtable_tests).step);
    test_step.dependOn(&b.addRunArtifact(vector_simhash_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_document_indexer_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_git_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_hash_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_health_test_mover_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_plugin_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_provider_discovery_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_stage_builder_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_triage_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_types_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_enhancer_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_todo_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_health_extractor_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_ralph_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_staged_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_treesitter_loader_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_plugin_registry_tests).step);
    test_step.dependOn(&b.addRunArtifact(guidance_subdirectory_tests).step);
    test_step.dependOn(&b.addRunArtifact(work_unit_tests).step);

    // -------------------------------------------------------------------------
    // 4. Benchmark step (G5)
    // -------------------------------------------------------------------------
    const benchmark_step = b.step("benchmark", "Run performance benchmarks");

    // Benchmark tests (unit tests for benchmark utilities)
    const benchmark_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/benchmark.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "vector", .module = vector_module },
                .{ .name = "reflection", .module = reflection_module },
            },
        }),
    });

    const benchmark_run = b.addRunArtifact(benchmark_tests);
    benchmark_step.dependOn(&benchmark_run.step);

    // Benchmark executable (runs main() for actual benchmarks)
    const benchmark_exe = b.addExecutable(.{
        .name = "benchmark",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/coral/benchmark.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{
                .{ .name = "vector", .module = vector_module },
                .{ .name = "reflection", .module = reflection_module },
            },
        }),
    });

    const benchmark_exe_run = b.addRunArtifact(benchmark_exe);
    benchmark_step.dependOn(&benchmark_exe_run.step);
}
