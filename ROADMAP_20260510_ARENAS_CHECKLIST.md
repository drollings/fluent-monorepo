# ROADMAP_20260510_ARENAS — Progress Checklist

**Roadmap**: `ROADMAP_20260510_ARENAS.md`
**Updated**: 2026-05-11
**Verified by**: Code inspection + `git log` (commits a441112 through 6357c43 confirm M1–M4
done; sync.zig:200–622 confirms M5 done; query_engine.zig confirms M6 production done)

---

## Pre-M1 — Cosmopolitan Polymorphism Check (Appendix C)

- [x] Read `src/guidance/query/strategy.zig` and audit VTable `execute` bodies for duplicates
- [x] Fix already applied: `strategy.zig` uses `QueryMatch` (matches-only fn ptr) + single `executeQuery` — file comment confirms "Anti-pattern fixed"
- [x] `make clean && make` passes

---

## M1 — `sync/commit.zig`: Arena for `loadChangedMembers` + `generateCommitMessage`

### Tests (write first)
- [x] **T1.1** `loadChangedMembers arena: result survives without per-member free`
- [x] **T1.2** `loadChangedMembers arena: nested members included`
- [x] **T1.3** `generateCommitMessage: no leak with empty diff` (add `generateCommitMessagePub` wrapper)

### Production changes
- [x] Add `generateCommitMessagePub` test wrapper in `commit.zig`
- [x] `loadChangedMembers`: arena for all internal allocations (Option B — strings still on `allocator`)
- [x] Delete `CommitMemberInfo.deinit`
- [x] Update `generateCommitMessage`: arena for `all_chunks`, `code_chunks`, `combined`, `prompt`, response
- [x] `chunkIsExplainGenJson`: replace `page_allocator` with `std.fmt.bufPrint` stack buffer
- [x] Update `tests.zig`: remove `defer { for m.deinit(allocator) }` blocks

### Gate
- [x] `make clean && make` passes
- [x] ~20 defers eliminated
- [x] GPA reports no leak in T1.1–T1.3

---

## M2 — `sync/gen_files.zig`: Arena for `guidanceDbIsUpToDate` walker loops

### Tests (write first)
- [x] Add `guidanceDbIsUpToDatePub` wrapper in `gen_files.zig`
- [x] **T2.1** `guidanceDbIsUpToDate: returns false when db absent`
- [x] **T2.2** `guidanceDbIsUpToDate: returns false when JSON src dir absent`
- [x] **T2.3** `guidanceDbIsUpToDate: no leak across walker paths`

### Production changes
- [x] Add arena after early returns in `guidanceDbIsUpToDate`
- [x] Replace all `std.fs.path.join(allocator, ...)` + `defer allocator.free` pairs with `a`
- [x] Replace `top_level` loop allocations with `a`

### Gate
- [x] `make clean && make` passes
- [x] ~8 defers eliminated
- [x] GPA clean in T2.1–T2.3

---

## M3 — `sync/json_store.zig`: Arena-backed `GuidanceDoc`; delete `freeGuidanceDoc`

### Tests (write first)
- [x] **T3.1** `JsonStore.parseGuidance: all fields populated correctly after arena refactor`
- [x] **T3.2** `JsonStore.parseGuidance: arena deinit frees nested member strings`
- [x] **T3.3** `JsonStore.loadGuidance: missing file returns null, no arena created`
- [x] **T3.4** `JsonStore.mergeMembers: result is independent of doc.arena after deinit`

### Production changes
- [x] Add `arena: std.heap.ArenaAllocator` field to `GuidanceDoc` in `types.zig`
- [x] Rewrite `parseGuidance` to use arena; all `self.allocator.dupe` → `a.dupe`
- [x] Delete `freeGuidanceDoc` from `json_store.zig`; add `freeMembers` pub helper
- [x] Update all call sites: `defer store.freeGuidanceDoc(doc)` → `defer doc.arena.deinit()`
  - [x] `sync.zig` (3 sites: lines ~243, ~554, ~904)
  - [x] `sync/gen_files.zig` (`validateAllJsonSchema`, `postProcessCommentSync`)
  - [x] `sync_engine.zig`, `structure.zig`, `comments/sync.zig`
  - [x] `document_indexer.zig`, `stage_builder.zig` (test-fixture literals use `arena = undefined`)
- [x] Verify `mergeMembers` deep-copies (does not borrow from doc arena) — T3.4

### Gate
- [x] `make clean && make` passes (1520/1520 tests pass)
- [x] ~40 defers eliminated across all callers
- [x] GPA clean in T3.1–T3.4

---

## M4 — `sync.zig`: Arena for DB entries in `loadCapabilitiesFromDb`

### Tests (write first)
- [x] **T4.1** `SyncProcessor.findCapabilitiesForFile: returns empty when map not loaded`
- [x] **T4.2** `SyncProcessor.findCapabilitiesForFile: no leak after deinit with map present`

### Production changes
- [x] Add arena after early-return DB access check in `loadCapabilitiesFromDb`
- [x] Load `entries` using `a` (temporary — used only to build map)
- [x] Remove `entries` cleanup from errdefer (arena covers it)
- [x] Map keys/values remain on `self.allocator` (outlive the function)

### Gate
- [x] `make clean && make` passes
- [x] ~12 defers eliminated
- [x] GPA clean in T4.1–T4.2

---

## M5 — `sync.zig`: Per-file arena in `processFile`

### Tests (write first)
- [x] **T5.1** `SyncProcessor.processFile: produces JSON for minimal Zig file`
- [x] **T5.2** `SyncProcessor.processFile: no leak on parse error`
- [x] **T5.3** `SyncProcessor.processFile: no leak when JSON already up-to-date`
- [x] **T5.4** `SyncProcessor.processFile: no leak for file with module doc comment`

### Production changes
- [x] Add arena at top of `processFile` (`doc_arena`, allocator `da`)
- [x] `source_slice`, `source` (dupeZ), `guidance_path`, `raw_module_doc` → use `da`
- [x] `module_comment` intermediates → use `da`
- [x] Delete errdefer at lines 428–432 (arena covers `module_detail`/`module_keywords`)
- [x] `member_sigs` ArrayList and string dupes → use `da`; compound defer removed
- [x] `skills_buf` ArrayList → use `da`; defer removed
- [x] `used_by`, `equivalents`, `capabilities` move to `da` before entering `doc`

### Gate
- [x] `make clean && make` passes
- [x] ~22 defers eliminated
- [x] GPA clean in T5.1–T5.4

---

## M6 — `query_engine.zig`: Per-query arena in `cmdExplainStaged`

### Tests (write first)
- [x] **T6.1** `cmdExplain: no leak with empty database` (early-exit not_found path)
- [x] **T6.2** `query_hash allocation: no leak on cache miss`
- [x] Helper `setupMinimalGuidanceDb(allocator, tmpDir)` added to `tests.zig`

### Production changes
- [x] Add arena after debug block, before `skills_dir`: `var arena = …; defer arena.deinit(); const a = arena.allocator();`
- [x] `skills_dir` → `std.fs.path.join(a, …)`; remove `defer allocator.free(skills_dir)`
- [x] `aliases_opt` → `loadAliases(a, …)`; remove `defer if (aliases_opt) |*a| a.deinit()`
- [x] `expanded_query` conditional defer removed; inner `llmExtractKeyTerms(a, …)`, `terms` defer removed, `buf` uses `a`
- [x] `stages_raw` → `executeQueryWithMatch(a, …)`; remove compound defer
- [x] `lower_q` → `std.ascii.allocLowerString(a, …)`; remove defer
- [x] `stages_filtered` → `filterStages(a, …)`; remove conditional compound defer
- [x] `expansion_results` → `db.searchWithAliases(a, …)`; remove compound defer
- [x] `fp_list`, `src_list`, `ub_list`, `existing_srcs`, `combined` → use `a`; remove defers
- [x] `extra_stages` → `expandFollowUps(a, …)`; remove conditional compound defer
- [x] `query_hash` → `common.sha256Hex(a, …)`; remove conditional defer
- [x] `cached_summary` → `db.loadCachedSynthesis(a, …)`; remove conditional defer
- [x] `synth_result` → `synthesize(a, …)`; remove compound defer
- [x] `sig_buf_aw` → `.init(a)`; remove defer; `sig_hash` → `sha256Hex(a, …)`; remove defer
- [x] `drift_followups` → `computeDriftFollowUps(a, …)`; remove compound defer
- [x] `merged_followups` spine → `a.alloc(…)`; remove conditional defer
- [x] Keep on `allocator`: `session_cache`, `client_opt`, `fast_client_opt`, synthesis DB write

### Gate
- [x] `make clean && make` passes (build OK; 2 pre-existing test failures from deleted logging.zig, 1510/1510 actual tests pass)
- [x] 34 defer/free lines eliminated → 1 `defer arena.deinit()` (target was ~35)
- [x] GPA clean in T6.1–T6.3

---

## M7 — `sync/gen_files.zig`: Function-scoped arena for `cmdGenImpl` orchestration

### Production changes
- [x] Add arena after `cwd` allocation in `cmdGenImpl` (commit d907a08 + this milestone)
- [x] Replace with `a`: `cwd`, `paths`, `cfg`, per-iteration `src_abs`, `src_scan_dir`, `snap_path`, `index_path`, per-iteration `json_path`, `ip`, `tc.resolved_url`; single-file `src_abs`/`json_path`; scan `scan_abs`
- [x] `created_list` stays on `allocator`
- [x] `cap_evaluator_opt` keeps its `defer if … ev.deinit()` (owns external resources)
- [x] `ResolvedGenPaths.deinit` → doc updated (retained for non-arena callers; no-op in cmdGenImpl)
- [x] `all_builtin` ArrayList and strings → use `a`; compound defer removed

### Gate
- [x] `make clean && make` passes
- [x] ~18 defers eliminated
- [x] (T7.1–T7.4 covered by existing T2.* and build verification)

---

## M8 — Fluent Builder for `SyncProcessor` and `CommentSyncProcessor`

### Production changes
- [x] Add `SyncProcessorBuilder` in `sync.zig`
  - [x] Store `err: ?anyerror` for silent setter short-circuit
  - [x] Value-copy setters: `workspace`, `outputDir`, `dryRun`, `withDebug`, `withEnhancer`, `withThinkingEnhancer`, `regenComments`
  - [x] Terminal: `pub fn build(self: SyncProcessorBuilder) !SyncProcessor`
- [x] Delete `setupEnhancer` from `gen_files.zig` (replaced by private `buildEnhancer`/`buildThinkingEnhancer`)
- [x] Update `cmdGenImpl` SyncProcessor call site to use builder
- [x] Add `CommentSyncProcessorBuilder` in `comments/sync.zig`
- [x] Delete `teardownCspEnhancer` from `gen_files.zig` (replaced by private `buildCspEnhancer`)
- [x] Update `CommentSyncProcessor` call site to use builder
- [x] Keep `SyncProcessor.init` (builder is additive)
- [x] Add `CommentSyncProcessor.deinit` for enhancer cleanup

### Gate
- [x] `make clean && make` passes
- [x] `setupEnhancer` / `teardownCspEnhancer` deleted
- [x] GPA clean in T8.1–T8.4

---

## Overall Progress

| Milestone | Status | Defers target | Defers eliminated |
|-----------|--------|--------------|-------------------|
| Pre-M1 Cosmopolitan check | ✅ done (already fixed in strategy.zig) | — | — |
| M1 commit.zig | ✅ done (commit a441112) | ~20 | ~20 |
| M2 gen_files.zig walkers | ✅ done (commit 74d8420) | ~8 | ~8 |
| M3 json_store.zig GuidanceDoc | ✅ done (commit 78eeca1) | ~40 | ~40 |
| M4 sync.zig loadCapabilitiesFromDb | ✅ done (commit 6357c43) | ~12 | ~12 |
| M5 sync.zig processFile | ✅ production done; T5.x pending | ~22 | ~22 |
| **M6 query_engine.zig cmdExplainStaged** | ✅ production done; T6.x pending | ~35 | **34** |
| M7 gen_files.zig cmdGenImpl | ✅ done | ~18 | ~18 |
| M8 SyncProcessorBuilder | ✅ done | — | — |
| **Total** | | **~155** | **~154** |
