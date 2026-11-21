# guidance Makefile
# Context: Rust-native AST-guided SQLite FTS5 database generator for NullClaw
# Maintainer: AI & Human Co-Pilot
#
# RALPH LOOP (guidance only; coral-router is built by `build`/`router*`):
#   build → guidance sync (generate)
#     └─► test → lint → fmt (from config) → staleness check → structure → db check
#           └─► .guidance/src/**/*.json  (JSON mtime = universal marker)
#                 └─► .explain.db
#                       └─► STRUCTURE.md
#
# `guidance check` (validate: config test/lint/fmt commands, repo-wide per
# extension key — not per-component narrow gates) runs separately, never as
# part of this recipe. Known quirk (documented, not fixed): `.zig`/`.py`
# lint/fmt config entries may contain a literal `{file}` token that is
# passed through unsubstituted.
#
# Key invariant: $(TARGET_BIN) depends ONLY on source files — never on
# STRUCTURE.md or markers that themselves depend on the binary.
# Change detection: source mtime vs guidance JSON mtime (no separate marker files).

SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := pre-commit

TARGET_BIN  := guidance
# Recipes invoke $(CARGO_BIN), never bare `guidance`: an installed copy
# on PATH would shadow every fresh build.
CORAL_ROUTER_BIN := target/debug/coral-router
CORAL_ROUTER_CONFIG := env/coral-router.json
# Extract bind address from the config file (source of truth), with a sane
# fallback when the config is unreadable at parse time.
CORAL_ROUTER_BIND_ADDR := $(shell python3 -c "import json; print(json.load(open('$(CORAL_ROUTER_CONFIG)'))['server']['bind_addr'])" 2>/dev/null || echo "127.0.0.1:8079")
CORAL_ROUTER_HEALTH_URL := http://$(CORAL_ROUTER_BIND_ADDR)/health
# How long to poll /health after (re)starting the router. Real mode spawns
# managed llama-servers at boot, so the default is generous; override with
# ROUTER_START_TIMEOUT_S=<n>. Mock mode skips supervision and boots fast.
ROUTER_START_TIMEOUT_S ?= 300
ROUTER_WAIT_SCRIPT := bin/router-wait-health.sh
ROUTER_LOG := /tmp/coral-router.out
# Paper-trading now lives in the split repo at /opt/src/rust/protopyne
# (protopyne-server on :9060, trading terminal, chart shell, and all
# protopyne-* gates moved there with the crates).
CONFIG      := .guidance/guidance-config.json
INSTALLDIR  := $(HOME)/.local/bin

# The MIGraphX-enabled ONNX Runtime dylib (AMD ROCm GPU EP) the router loads via
# ort's `load-dynamic` for `execution_provider: "gpu"`. Only used by `onnx`
# builds (see ROUTER_FEATURES below); ort-free default builds ignore it.
# Override with `make router-start ORT_DYLIB_PATH=/path/to/libonnxruntime.so`.
ORT_DYLIB_PATH ?= /lib64/libonnxruntime.so.1.28.0

# ── ONNX / ort opt-in ────────────────────────────────────────────────────────
# The `fluent-onnx` workers (`ort` + `tokenizers`) are NOT built by default:
# `fluent-onnx`, `fluent-router`, and `coral-router` all ship `default = []`,
# and the `onnx` feature (`fluent-onnx/onnx` → `ort` + `tokenizers`) is strictly
# opt-in. A default build is ort-free (`cargo tree -p fluent-router -i ort`
# matches nothing); onnx-declared models fail open with a loud warning.
#
#   make router ROUTER_FEATURES=onnx        # one-off onnx build
#   make router-onnx / router-start-onnx    # convenience targets (same thing)
ROUTER_FEATURES ?=
ROUTER_FEATURE_ARGS := $(if $(ROUTER_FEATURES),--features $(ROUTER_FEATURES))

RUST_SRC_DIR := src
GUIDANCE_DIR  := .guidance
GUIDANCE_DB   := .guidance.db
ENV_DIR      := .env
HASH_DIR     := $(GUIDANCE_DIR)/src

# Verbosity control: V=1 enables shell echo. Gate logs live in GATE_LOG_DIR.
V ?= 0
Q := $(if $(filter 1,$V),,@)

# Directory holding the full captured output of every development-loop gate
# (`bin/gate-run.sh` writes "<target>.log" here, then prints one summary line on
# success or the diagnostic excerpt on failure). Override per run.
export GATE_LOG_DIR ?= /tmp/opencode/monorepo-gates

# ==============================================================================
# MISE INTEGRATION
# ==============================================================================

include env/mk/common.mk
-include env/mk/targets/$(TARGET_LANG).mk

# ==============================================================================
# HELP
# ==============================================================================

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make <target>\n"} /^[a-zA-Z0-9_-]+:.*?##/ { printf "  %-22s%s\n", $$1, $$2 } /^##@/ { printf "\n%s\n", substr($$0, 5) }' $(MAKEFILE_LIST)
	@printf "\n%s\n" "Rigor gates (per component): make <component>-{format,lint,smoke,test,soak}"
	@printf "  components: %s\n" "$(COMPONENT_NAMES)"
	@printf "  repo-wide:  make {format,format-check,lint,smoke,test,soak}\n"
	@printf "  gate logs:  %s\n" "$(GATE_LOG_DIR)"
	@printf "  see AGENTS.md \"Build-Test Loop\" for when to run each level.\n"

##@ Intelligence Layer

# Database target: rebuilt by guidance sync automatically.
# This rule is kept for direct `make .guidance.db` invocations.
$(GUIDANCE_DB): | $(CARGO_BIN)
	$(Q)echo "Syncing database: $@"
	$(Q)$(CARGO_BIN) sync --workspace . --json-dir $(GUIDANCE_DIR) --db $@
	$(Q)touch $@

.PHONY: commit
commit: $(CARGO_BIN) | STRUCTURE.md ## Generate AI commit message from staged diff + guidance JSON context
	$(Q)$(CARGO_BIN) commit $(if $(DRY_RUN),--dry-run) $(if $(DEBUG),--debug)

##@ Environment

.PHONY: venv
venv: $(VENV) ## Install / verify Python dependencies

$(HASH_DIR):
	$(Q)mkdir -p $(HASH_DIR)

$(VENV): requirements.txt | $(HASH_DIR)
	$(Q)echo "Syncing Python environment..."
	$(Q)if [ ! -d $(VENV) ]; then $(UV) venv $(VENV); fi
	$(Q)$(UV) pip install --no-cache -q -r requirements.txt
	$(Q)$(UV) pip install --no-cache -q ruff pytest pytest-cov
	$(Q)touch $(VENV)
	$(Q)echo "Python environment ready."

.PHONY: check-prereqs
check-prereqs: ## Verify prerequisites (cargo, uv)
	@which cargo > /dev/null || (echo "cargo not found. Install via rustup: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; exit 1)
	@which uv  > /dev/null || (echo "uv not found.  Install: curl -LsSf https://astral.sh/uv/install.sh | sh"; exit 1)
	@echo "Prerequisites satisfied"

.PHONY: env-init
env-init: check-prereqs venv ## Initialize development environment
	@echo "Environment ready."

.PHONY: clean
clean: clean-db ## Remove build artifacts and markers (keeps venv and .guidance config)
	$(Q)rm -rf target $(HASH_DIR)

##@ Guidance Management

.PHONY: clean-db
clean-db: ## Remove build artifacts and markers (keeps venv and .guidance config)
	$(Q)rm -f $(GUIDANCE_DB)

.PHONY: clean-all
clean-all: clean ## Remove stale JSON files from .guidance/src
	$(Q)find $(GUIDANCE_DIR)/src -name '*.json' -type f -exec rm -rf {} \; || true

.PHONY: explain
explain: $(GUIDANCE_DB) ## Explain a module, function, or concept  make explain QUERY="sma"
	@if [ -z "$(QUERY)" ]; then \
		echo "❌ Usage: make explain QUERY=\"<module or function or concept>\""; \
		echo "   Examples:"; \
		echo "     make explain QUERY=\"sma\""; \
		echo "     make explain QUERY=\"ring buffer\""; \
		echo "     make explain QUERY=\"ast_parser\""; \
		exit 1; \
	fi
	$(Q)$(CARGO_BIN) explain --guidance $(GUIDANCE_DIR) "$(QUERY)"

##@ Rust Build & RALPH Loop

RUST_SRC_FILES := $(shell find $(RUST_SRC_DIR) -type f -name '*.rs' -not -name '.#*' -not -name '#*#' -not -name '*~' 2>/dev/null)

# ── Binary ───────────────────────────────────────────────────────────────────

CARGO_BIN := target/debug/$(TARGET_BIN)

$(CARGO_BIN): $(RUST_SRC_FILES)
	$(Q)echo "Building guidance"
	$(Q)cargo build --bin guidance

$(CORAL_ROUTER_BIN): $(RUST_SRC_FILES)
	$(Q)echo "Building coral-router $(if $(ROUTER_FEATURES),with features: $(ROUTER_FEATURES), (ort-free default))"
	$(Q)cargo build --bin coral-router $(ROUTER_FEATURE_ARGS)

.PHONY: install
install: $(CARGO_BIN) $(CORAL_ROUTER_BIN)
	$(Q)mkdir -p $(INSTALLDIR)
	$(Q)cp $(CARGO_BIN) $(INSTALLDIR)/guidance
	$(Q)cp $(CORAL_ROUTER_BIN) $(INSTALLDIR)/coral-router
	$(Q)echo "Installed guidance and coral-router in $(INSTALLDIR)"

# ── Router Targets ────────────────────────────────────────────────────────────

.PHONY: router
router: $(CORAL_ROUTER_BIN) ## Build coral-router (fast, no run; ort-free unless ROUTER_FEATURES=onnx)

.PHONY: router-onnx
router-onnx: ## Build coral-router with the ONNX fleet (ort + tokenizers via --features onnx)
	$(Q)$(MAKE) router ROUTER_FEATURES=onnx

# Kill any running coral-router and wait for it to actually exit before the
# caller starts a fresh one. The router is the process owner of its spawned
# llama-servers and handles SIGTERM gracefully (stops the supervisor first),
# so a plain kill never orphans serving processes.
#
# Match by exact process name (`pkill -x`) AND by the router config file path
# (`pkill -f 'coral-router[.]json'`): the binary is often invoked through a
# symlink (e.g. the `gguf_tool` alias) whose comm(2) name is the symlink, so
# `killall coral-router` would silently miss it and leave a stale router on
# the port. The `[.]` character class keeps the second pattern from matching
# its own literal text (or a calling shell that merely mentions the word
# `coral-router`), so it only ever kills the router process.
define stop-router
	$(Q)echo "Stopping any running router"
	$(Q)pkill -x coral-router 2>/dev/null || true
	$(Q)pkill -f 'coral-router[.]json' 2>/dev/null || true
	$(Q)for i in $$(seq 1 75); do pgrep -f 'coral-router[.]json' >/dev/null 2>&1 || break; sleep 0.2; done
endef

.PHONY: router-stop
router-stop: ## Stop coral-router (SIGTERM, so it stops its managed llama-servers first)
	$(stop-router)

# ── Router Benchmark ──────────────────────────────────────────────────────────

CORAL_ROUTER_TEST_SCRIPT := bin/coral-router-test.py

.PHONY: router-benchmark
router-benchmark: router-start ## Score the live router (routing accuracy, TTFT, VRAM) via bin/coral-router-test.py; reads env/coral-router.json for routes + expectations
	$(Q)python3 $(CORAL_ROUTER_TEST_SCRIPT) --config $(CORAL_ROUTER_CONFIG)

.PHONY: bench
bench: ## Run the fluent-router criterion benches (pipeline dispatch, tree evaluation, filter before/after)
	$(Q)cargo bench -p fluent-router

# ── spaCy vs spacy-rs Parity Benchmark ───────────────────────────────────────
# Re-runs the tokenizer speed + memory comparison over the shared golden
# corpus fixture (`src/spacy-rs/tests/data/en_tokenization.json`, pinned to
# spaCy 3.8.15). Builds the in-repo Rust harness (`bin/spacy/benchmark/`), runs
# both tokenizers over the same dataset, and prints a comparison table with a
# parity gate. Hermetic to a python + spaCy install (no inference, no network).

SPACY_BENCH_DIR := bin/spacy/benchmark

.PHONY: spacy-benchmark
spacy-benchmark: ## Re-run the spaCy vs spacy-rs parity benchmark (speed + memory over the shared golden corpus); env: SPACY_PYTHON=<python-with-spacy>, SPACY_BENCH_PASSES=<n>, SPACY_BENCH_REPS=<n>
	$(Q)bash $(SPACY_BENCH_DIR)/run.sh

.PHONY: spacy-parse-benchmark
spacy-parse-benchmark: ## Score the deterministic ArcEager ladder (UPOS/UAS/LAS per category) against pinned refs; fully hermetic, no LLM (refs: tests/data/parse_bench.refs.json; new refs via `make spacy-test-live`)
	$(Q)cargo test -p spacy-rs --test parse_bench -- --nocapture

.PHONY: test-router
test-router: $(CORAL_ROUTER_BIN) ## Run fluent-router unit/golden/e2e tests + a --help dry-run of the built binary (ort-free unless ROUTER_FEATURES=onnx; stops a running router first)
	$(stop-router)
	$(Q)echo "Running fluent-router unit + golden + e2e mock tests $(if $(ROUTER_FEATURES),with features: $(ROUTER_FEATURES),(ort-free default))"
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS)
	$(Q)echo "Validating coral-router --help"
	$(Q)$(CORAL_ROUTER_BIN) --help > /dev/null && echo "All router tests passed." || echo "ERRROR: coral-router did NOT successfully run."

.PHONY: test-router-onnx
test-router-onnx: ## test-router with the ONNX fleet (ort + tokenizers via --features onnx)
	$(Q)$(MAKE) test-router ROUTER_FEATURES=onnx

.PHONY: test-router-all
test-router-all: $(CORAL_ROUTER_BIN) ## test-router + coral-context HNSW benchmarks (large, slow; ort-free unless ROUTER_FEATURES=onnx; stops a running router first)
	$(stop-router)
	$(Q)echo "Running fluent-router unit + golden + e2e mock tests"
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS)
	$(Q)echo "Running coral-context tests with HNSW benchmarks"
	$(Q)cargo test -p coral-context --features hnsw-bench -- --ignored --nocapture
	$(Q)echo "Validating coral-router --help"
	$(Q)$(CORAL_ROUTER_BIN) --help > /dev/null && echo "All router tests passed." || echo "ERRROR: coral-router did NOT successfully run."

.PHONY: test-heavy
test-heavy: ## Slow hermetic suites (#[ignore]d heavy tests): spacy-rs corpus/contention + router supervisor/HNSW calibration + search-vector scale-corpus calibration + db hnsw races / s2s readiness calibration (protopyne soaks moved with the crates to /opt/src/rust/protopyne)
	$(Q)cargo test -p spacy-rs --lib -- --ignored
	$(Q)cargo test -p spacy-rs --test arceager_golden -- --ignored
	$(Q)cargo test -p fluent-router --lib -- --ignored
	$(Q)cargo test -p fluent-router --test threshold_calibration -- --ignored
	$(Q)cargo test -p search-vector --lib -- --ignored
	$(Q)cargo test -p fluent-db --test hnsw_concurrent -- --ignored
	$(Q)cargo test -p orator-voice --test s2s_calibration -- --ignored

.PHONY: router-start
router-start: $(CORAL_ROUTER_BIN) ## Build (if needed), (re)start coral-router in real mode on :8079, and wait for /health (ort-free unless ROUTER_FEATURES=onnx; stops the old tree first)
	$(stop-router)
	$(Q)echo "Starting coral-router $(if $(ROUTER_FEATURES),with features: $(ROUTER_FEATURES),(ort-free default))"
	$(Q)nohup env ORT_DYLIB_PATH=$(ORT_DYLIB_PATH) $(CORAL_ROUTER_BIN) start -c $(CORAL_ROUTER_CONFIG) > $(ROUTER_LOG) 2>&1 &
	$(Q)bash $(ROUTER_WAIT_SCRIPT) $(CORAL_ROUTER_HEALTH_URL) $(ROUTER_START_TIMEOUT_S) $(ROUTER_LOG)

.PHONY: router-start-onnx
router-start-onnx: ## router-start with the ONNX fleet (ort + tokenizers via --features onnx)
	$(Q)$(MAKE) router-start ROUTER_FEATURES=onnx

.PHONY: router-mock
router-mock: ## Run the config-synced routing integration tests (intent -> model_group, derived from env/coral-router.json; ort-free unless ROUTER_FEATURES=onnx)
	$(stop-router)
	$(Q)echo "Running config-synced routing integration tests (intent -> model_group)"
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS) config_route_tests -- --nocapture

.PHONY: router-ledger
router-ledger: ## Boot with --ledger-default and assert LOD0+LOD5 eager + LOD4 from LOD0 only + cache hit (M4; honors ROUTER_FEATURES)
	$(stop-router)
	$(Q)echo "Running ledger LOD lifecycle tests (ledger-default)"
	$(Q)CORAL_LEDGER_DEFAULT=1 cargo test -p fluent-router $(ROUTER_FEATURE_ARGS) ledger -- --nocapture
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS) ledger_golden -- --nocapture

.PHONY: router-tree
router-tree: ## Run config-synced routing tests against the tree config (M7; honors ROUTER_FEATURES)
	$(stop-router)
	$(Q)echo "Running tree config routing tests (flat vs tree equivalence)"
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS) flat_tree -- --nocapture

# ── YaGO Taxonomy + Async Parse-Review ───────────────────────────────────────
# The interlingua spine (ROADMAP_20260826_INTERLINGUA_V2): the YaGO 4.5 class
# registry feeds the boot reconciliation (one loader, two homes — coral graph +
# router SqliteConceptStore), and the async review worker is the corrective
# write path (corrections atomic with the ledger).

.PHONY: yago-load
yago-load: ## Operator action: download the full YaGO 4.5 taxonomy and regenerate the embedded class registry (tools/download_yago_taxonomy.sh → src/concept/data/yago_classes.json)
	$(Q)tools/download_yago_taxonomy.sh
	$(Q)if [ -f env/yago-taxonomy.ttl ]; then cargo run -q -p fluent-concept --bin ysm1_encode -- env/yago-taxonomy.ttl env/yago-taxonomy.ysm1; fi

.PHONY: review-test
review-test: ## Run the async parse-review + interlingua suites: spacy-rs review types, the router review worker + /v1/sessions/{id}/review-parse endpoints, and the concept loader/reconciliation
	$(Q)echo "Running parse-review + interlingua suites"
	$(Q)cargo test -p spacy-rs review
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS) server::review
	$(Q)cargo test -p fluent-router $(ROUTER_FEATURE_ARGS) review_parse
	$(Q)cargo test -p fluent-concept loader
	$(Q)cargo test -p coral-router boot::

# ── ONNX GPU Provider Check ────────────────────────────────────────────────
# Diagnostic: reports whether the *linked* ONNX Runtime exposes the AMD ROCm
# GPU (MIGraphX) execution provider — i.e. whether `execution_provider: "gpu"`
# in env/coral-router.json will actually engage the GPU. The ort prebuilt
# binaries ship no AMD GPU EP (the ROCm EP was removed upstream in ORT 1.23;
# MIGraphX is its successor), so a CPU-only report is expected unless a
# MIGraphX-enabled onnxruntime (an AMD ROCm build) is linked via ORT_LIB_PATH
# (static) or ORT_DYLIB_PATH (load-dynamic) and the crate rebuilt. See
# doc/fluent-onnx/ARCHITECTURE.md §Execution-provider selection.

.PHONY: onnx-gpu-check
onnx-gpu-check: ## Probe the linked onnxruntime for the AMD ROCm GPU (MIGraphX) EP; CPU-only means `execution_provider: "gpu"` fails open to CPU (requires --features onnx)
	$(Q)cargo test -p fluent-onnx --features onnx,live-ai --test live gpu_provider_available -- --ignored --nocapture

# ── Per-Crate Test Targets ──────────────────────────────────────────────────
# Run one crate's Tier-0/1/2 suite (unit + integration) WITHOUT enabling
# `live-ai`, so they stay hermetic. Package names are the Cargo package ids
# (`rg '^name' <crate>/Cargo.toml`). See doc/TESTING.md §5.

# Only the crates whose `test-<shorthand>` has no canonical `<component>-test`
# yet stay here. The named shared crates (`common-core`, `fluent-wvr`,
# `fluent-concurrency`, `dag`, `fluent-egui`) and the binary components carry
# the component gate matrix above; their historical `test-<shorthand>` names
# live on as thin aliases in the block just below.
TEST_CRATES := \
	db:fluent-db \
	llm:fluent-llm \
	macros:fluent-wvr-macros \
	spacy:spacy-rs \
	ort:fluent-onnx

# `test-ort` stays hermetic and ort-free (the default `fluent-onnx` build);
# `test-ort-onnx` runs the same suite with the ort workers compiled in.
define test-crate-rule
.PHONY: test-$(1)
test-$(1): ## Run the $(2) Tier-0/1/2 suite (hermetic; no live-AI)
	$(Q)cargo test -p $(2)
endef
$(foreach pair,$(TEST_CRATES),$(eval $(call test-crate-rule,$(firstword $(subst :, ,$(pair))),$(lastword $(subst :, ,$(pair))))))

.PHONY: router-test router-test-all
router-test: test-router
router-test-all: test-router-all

.PHONY: test-ort-onnx
test-ort-onnx: ## Run the fluent-onnx Tier-0/1/2 suite with the ort workers (onnx + tokenizers)
	$(Q)cargo test -p fluent-onnx --features onnx

# ── Rigor Gates (per-component matrix) ───────────────────────────────────────
# Every major component — a product/binary target (`coral-router`, `guidance`,
# `coral`) or a shared library crate
# (`common-core`, `fluent-wvr`, `fluent-concurrency`, `dag`, `fluent-egui`) —
# exposes the SAME five gates, named `<component>-<level>`:
#
#   <c>-format  rustfmt the component's packages (WRITE, like `make format`)
#   <c>-lint    clippy --all-targets -D warnings on the component's packages
#   <c>-smoke   FAST    — type-check every target, then run unit tests (`--lib --bins`)
#   <c>-test    REGULAR — the component's full hermetic suite (unit + integration/e2e)
#   <c>-soak    SLOW    — the component's `#[ignore]`d heavy tests (`-- --ignored`)
#
# Only `format` mutates files; `lint`/`smoke`/`test`/`soak` verify. `smoke` runs
# the tests compiled into a lib/bin target — a crate that keeps every test in
# an auto-discovered integration target will report 0 smoke tests (its
# all-target type-check and its `-test` gate still cover it).
# Repo-wide equivalents are `make format` (write) / `format-check` / `lint` /
# `smoke` / `test` / `soak`. AGENTS.md "Build-Test Loop" says WHEN to run which
# level: smoke for a minor milestone on the component you are working in,
# regular for a concluded component, and the repo-wide levels only at the ends
# described there. The rich, composite targets keep their own names
# (`test-router`, `test-<shorthand>`, `test-live`, …); the matrix is the uniform
# format/lint/smoke/test/soak subset every component shares.

COMPONENT_NAMES := \
	common-core \
	fluent-wvr \
	fluent-concurrency \
	dag \
	fluent-egui \
	coral-router \
	guidance \
	coral \
	orator

# component -> the Cargo packages that make it up, plus any extra cargo args
# (features). `dag` is the workspace name of package `fluent-dag`; the binary
# components bundle their library (e.g. `coral-router` = `fluent-router` + the
# `coral-router` bin; `guidance` = `guidance-core` + `guidance` + concept).
common-core_PKGS        := common-core
fluent-wvr_PKGS         := fluent-wvr fluent-wvr-macros
fluent-concurrency_PKGS := fluent-concurrency
dag_PKGS                := fluent-dag
fluent-egui_PKGS        := fluent-egui
coral-router_PKGS       := fluent-router coral-router
guidance_PKGS           := guidance-core guidance fluent-concept
coral_PKGS              := coral-context coral
orator_PKGS            := orator orator-audio orator-voice orator-agent orator-models orator-memory orator-supervise orator-desktop orator-testkit orator-daemon orator-cli

# Emit the five gates for one component. `$(1)` is the component name; the
# PKGS/ARGS variables are looked up through it. Kept as a macro so the five
# levels can never drift between components.
define component-gates
.PHONY: $(1)-format $(1)-lint $(1)-smoke $(1)-test $(1)-soak
$(1)-format: ## Format the $(1) component (rustfmt, write)
	$(Q)bin/gate-run.sh $(1)-format fmt -- cargo fmt $(addprefix -p ,$($(1)_PKGS))
$(1)-lint: ## clippy -D warnings for the $(1) component
	$(Q)bin/gate-run.sh $(1)-lint lint -- cargo clippy $(addprefix -p ,$($(1)_PKGS)) --all-targets $($(1)_ARGS) -- -D warnings
$(1)-smoke: ## FAST gate for $(1): all-target type-check + unit tests
	$(Q)bin/gate-run.sh $(1)-smoke check build -- cargo check $(addprefix -p ,$($(1)_PKGS)) --all-targets $($(1)_ARGS)
	$(Q)bin/gate-run.sh $(1)-smoke test unit -- cargo test --quiet $(addprefix -p ,$($(1)_PKGS)) --lib --bins $($(1)_ARGS)
$(1)-test: ## REGULAR gate for $(1): full hermetic suite
	$(Q)bin/gate-run.sh $(1)-test test -- cargo test --quiet $(addprefix -p ,$($(1)_PKGS)) $($(1)_ARGS)
$(1)-soak: ## SLOW gate for $(1): ignored heavy tests
	$(Q)bin/gate-run.sh $(1)-soak test -- cargo test --quiet $(addprefix -p ,$($(1)_PKGS)) $($(1)_ARGS) -- --ignored
endef
$(foreach c,$(COMPONENT_NAMES),$(eval $(call component-gates,$(c))))

# (The trading-crate probe leg moved with its crates to /opt/src/rust/protopyne.)

# ── Live-AI Test Pathway ────────────────────────────────────────────────────
# THE ONLY pathway that runs real inference. Every live test is `#[ignore]` +
# `live-ai`-gated (in `tests/live/`); these targets enable the feature and run
# ONLY the ignored tests in the `live` test target. They NEVER run under
# `make test` / `make test-router` / `make router-mock` / CI. Live tests skip
# cleanly (early `return`) when the env contract (LLM_BASE_URL/LLM_MODEL) is
# absent, per the skip-not-fail policy in doc/TESTING.md §4.

LIVE_CRATES := \
	router:fluent-router \
	guidance:guidance-core \
	llm:fluent-llm \
	spacy:spacy-rs \
	ort:fluent-onnx \
	orator_agent:orator-agent \
	orator_voice:orator-voice \
	orator_cli:orator-cli \
	orator_models:orator-models

# Live-AI feature sets per crate shorthand (the `ort` live tests drive real
# `ort` sessions, so they need `onnx` on top of `live-ai`; every other crate's
# live tests need `live-ai` only).
LIVE_FEATURES_ort := onnx,live-ai
LIVE_FEATURES_router := live-ai
LIVE_FEATURES_guidance := live-ai
LIVE_FEATURES_llm := live-ai
LIVE_FEATURES_spacy := live-ai
LIVE_FEATURES_orator_agent := live-ai
LIVE_FEATURES_orator_voice := live-ai
LIVE_FEATURES_orator_cli := live-ai
LIVE_FEATURES_orator_models := live-ai
LIVE_TESTS_orator_voice := stt_live tts_live s2s_live

define live-crate-rule
.PHONY: $(1)-test-live
$(1)-test-live: ## Run the live-AI (real model) tests for $(2); skip-not-fail without env
	$(Q)for t in $(or $(LIVE_TESTS_$(1)),live); do cargo test -p $(2) --features $(LIVE_FEATURES_$(1)) --test $$t -- --ignored; done
endef
$(foreach pair,$(LIVE_CRATES),$(eval $(call live-crate-rule,$(firstword $(subst :, ,$(pair))),$(lastword $(subst :, ,$(pair))))))

.PHONY: orator-test-live
orator-test-live: orator_agent-test-live orator_voice-test-live orator_cli-test-live orator_models-test-live ## Run the orator live-AI suites (LLM + voice pipeline + TUI barge + LFM3 reference); skip-not-fail without env

.PHONY: test-live
test-live: ## Run ALL live-AI tests (the ONLY real-inference pathway); skips cleanly without env
	$(Q)for pair in $(LIVE_CRATES); do \
		shorthand="$${pair%%:*}"; \
		pkg="$${pair##*:}"; \
		case "$$shorthand" in \
			ort) features="onnx,live-ai" ;; \
			*) features="live-ai" ;; \
		esac; \
		echo "==> live-AI tests for $${pkg} (--features $${features})"; \
		cargo test -p "$${pkg}" --features "$${features}" --test live -- --ignored || exit 1; \
	done

# ── Standard Targets ──────────────────────────────────────────────────────────
# Repo-wide rigor levels. They mirror the per-component matrix above but span
# the whole workspace; AGENTS.md "Build-Test Loop" governs when to run each:
# `smoke` at the end of a workflow, `test` on user command (never per
# milestone), `soak` only when a change touches concurrency/timing/calibration.
#
# Every gate runs through `bin/gate-run.sh`, which captures the command's full
# output to "$GATE_LOG_DIR/<target>.log" (default /tmp/opencode/monorepo-gates)
# and prints one summary line on success — a failure prints the diagnostic
# excerpt and the log path and exits non-zero. `cargo test` runs with
# `--quiet` (one character per test) so the captured log stays small.

.PHONY: smoke
smoke: ## Repo-wide FAST gate (workflow end): all-target type-check + unit tests
	$(Q)bin/gate-run.sh smoke check build -- cargo check --workspace --all-targets
	$(Q)bin/gate-run.sh smoke test unit -- cargo test --quiet --workspace --lib --bins

.PHONY: test
test: ## Repo-wide REGULAR gate (user command before commit): full hermetic suite
	$(Q)bin/gate-run.sh test test -- cargo test --quiet --workspace

.PHONY: soak
soak: ## Repo-wide SLOW gate: `#[ignore]`d heavy tests across the workspace
	$(Q)bin/gate-run.sh soak test -- cargo test --quiet --workspace -- --ignored

.PHONY: lint
lint: lint-vector ## Static gate: clippy across the workspace on all targets
	$(Q)bin/gate-run.sh lint lint -- cargo clippy --workspace --all-targets -- -D warnings

.PHONY: lint-vector
lint-vector: ## Consolidation guard: raw HNSW types contained; no re-export shims, no second threshold/error source, no fresh distance closures
	$(Q)bin/vector-check.sh

.PHONY: lint-llm-boundary
lint-llm-boundary: ## LLM-boundary guard (ROADMAP_20260903_LLM M0.1): no new common_core LLM-leak imports, no domain imports in llm/common-core (advisory until M1–M8 green)
	$(Q)bin/llm-boundary-check.sh

.PHONY: lint-live-ai
lint-live-ai: ## Hermeticity guard: every #[ignore] has a reason; no hermetic test dials a non-loopback host
	$(Q)bin/live-ai-guard.sh

.PHONY: test-budget
test-budget: ## Per-test 60s budget audit: every non-ignored test runs once under timeout 60 (bin/test-budget-guard.sh --workspace for the full tree)
	$(Q)bin/test-budget-guard.sh --workspace

.PHONY: lint-no-dup
lint-no-dup: ## Guardrail: no bespoke ladder/retry/eviction duplicates outside canonical primitives (advisory)
	@echo "Checking for bespoke ladder duplicates..."
	@! grep -R "for.*ordered.*for.*backend.*matches" src/ --include="*.rs" | grep -v ladder | grep -v "first_accept" || (echo "FAIL: bespoke ordered ladder found"; exit 1)
	@! grep -R "if let Ok.*keyword_search" src/coral --include="*.rs" | grep -v ladder | grep -v "first_accept" || (echo "FAIL: bespoke keyword_search ladder found"; exit 1)
	@! grep -R "tokio::time::sleep.*retry" src/guidance src/coral --include="*.rs" | grep -v "retry_async" | grep -v "retry::" || (echo "FAIL: raw sleep retry loop found"; exit 1)
	@echo "ok: no bespoke ladder/retry duplicates"

.PHONY: lint-split-dup
lint-split-dup: ## SPLIT-roadmap duplication sweep: no second implementation of a unified shared helper, and every migrated call site still composes it
	$(Q)bin/split-dup-guard.sh

.PHONY: health
health: ## Run cargo tarpaulin and verify 85% coverage
	$(Q)cargo tarpaulin --workspace --fail-under 85

##@ Scorecard (guidance improvement instrument; not part of smoke/test)
.PHONY: guidance-scorecard
guidance-scorecard: ## Fast hermetic scorecard (<60s): ≤40 lines + JSON in the log; exit 2 on a ratchet violation
	$(Q)bin/gate-run.sh guidance-scorecard generic -- cargo run -q -p guidance -- scorecard
.PHONY: guidance-eval
guidance-eval: ## Eval ratchet over the shipping path (alias: same fast tier as scorecard, read.* metrics)
	$(Q)bin/gate-run.sh guidance-eval generic -- cargo run -q -p guidance -- scorecard
.PHONY: guidance-scorecard-deep
guidance-scorecard-deep: ## Deep tier: fast tier + per-query breakdown
	$(Q)bin/gate-run.sh guidance-scorecard-deep generic -- cargo run -q -p guidance -- scorecard --deep
.PHONY: guidance-scorecard-live
guidance-scorecard-live: ## Live tier: print the exact model-gated commands (never runs them)
	$(Q)bin/gate-run.sh guidance-scorecard-live generic -- cargo run -q -p guidance -- scorecard --live

.PHONY: format
format: ## rustfmt the whole workspace (write)
	$(Q)bin/gate-run.sh format fmt -- cargo fmt --all

.PHONY: format-check
format-check: ## rustfmt --check the whole workspace (verify, no write)
	$(Q)bin/gate-run.sh format-check fmt -- cargo fmt --all -- --check

.PHONY: fmt fmt-check
fmt: format ## Alias for `format` (cargo fmt muscle memory)
fmt-check: format-check ## Alias for `format-check`

# ── STRUCTURE.md ─────────────────────────────────────────────────────────────
# Generated by guidance structure after sync.
# The target below is kept for direct `make STRUCTURE.md` invocations.

STRUCTURE.md: $(GUIDANCE_DB) | $(CARGO_BIN)
	$(Q)$(CARGO_BIN) structure --json-dir $(GUIDANCE_DIR) 2>&1 | grep -E "STRUCTURE|Generated|✓" || true
	$(Q)touch STRUCTURE.md

##@ Gate Targets

# Guidance-only loop: build → sync (generate). `check` (validate) is a
# separate command: `guidance check` runs config test/lint/fmt commands
# repo-wide per extension key and validates staleness; `guidance sync`
# regenerates JSON + DB for stale files. The coral-router build stays
# available via `build` / `router*` targets — nothing below needs it.
.PHONY: pre-commit build
build: $(CARGO_BIN) $(CORAL_ROUTER_BIN) ## Build guidance and coral-router binaries

pre-commit: STRUCTURE.md $(CARGO_BIN) ## Regenerate the guidance mirror
	$(Q)$(CARGO_BIN) sync --workspace .
	$(Q)echo "✓ All checks passed. Ready to commit."

