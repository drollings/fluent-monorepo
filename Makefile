# explain-gen Makefile
# Context: Zig-native AST-guided SQLite FTS5 database generator for NullClaw
# Maintainer: AI & Human Co-Pilot
#
# RALPH LOOP: build → test → lint → guidance (per-file) → STRUCTURE.md
#
# Key invariant: $(TARGET_BIN) depends ONLY on source files — never on
# STRUCTURE.md or markers that themselves depend on the binary.
#
# Dependency chain:
#   $(ZIG_SRC_FILES)
#     └─► $(TARGET_BIN)                    (zig build)
#     └─► zig-out/mark/.tests-passed     (zig build test — single pass)
#           └─► zig-out/mark/lint/*.zig     (zig fmt --check)
#                 └─► zig-out/mark/guidance/*.zig  (explain-gen gen --file)
#
#   All GUIDANCE_MARKERS → STRUCTURE.md

SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := pre-commit

# ==============================================================================
# CONFIGURATION
# ==============================================================================

PYTHON      := python3
VENV        := .venv
BIN         := $(VENV)/bin
PYTHON_VENV := $(BIN)/python
UV          := uv

DOCUMENTOR := $(if $(wildcard zig-out/bin/explain-gen), \
	zig-out/bin/explain-gen, \
	$(shell which explain-gen 2>/dev/null || echo "explain-gen not found" && exit 1))

TARGET_BIN  := zig-out/bin/explain-gen
AST_PY      := bin/explain-gen-py
CONFIG      := .explain-gen/explain-gen-config.json
INSTALLDIR  := $(HOME)/.local/bin

SRC_DIR      := ./src
EXPLAIN_DIR  := .explain-gen
EXPLAIN_DB   := .explain.db
ENV_DIR      := .env
HASH_DIR     := $(ENV_DIR)/.make_hashes

# Marker directories — store per-file success stamps for incremental builds
LINT_MARKER_DIR     := zig-out/mark/lint
GUIDANCE_MARKER_DIR := zig-out/mark/guidance
TEST_PASSED         := zig-out/mark/.tests-passed

# Verbosity control: V=1 enables shell echo
V ?= 0
Q := $(if $(filter 1,$V),,@)

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

##@ Intelligence Layer

# Database target: depends on guidance JSON files being present
# Only rebuilds when source files change (via guidance markers)
$(EXPLAIN_DB): $(GUIDANCE_MARKERS) | $(TARGET_BIN)
	$(Q)echo "Syncing database: $@"
	$(Q)$(TARGET_BIN) gen --workspace . --json-dir $(EXPLAIN_DIR) --db $(EXPLAIN_DB)
	$(Q)touch $@

.PHONY: gen
gen: $(EXPLAIN_DB) ## Generate .explain-gen/ JSON and .explain.db

.PHONY: gen-infill
gen-infill: $(TARGET_BIN) ## Generate with LLM comment infill
	$(Q)$(TARGET_BIN) gen --workspace . --json-dir $(EXPLAIN_DIR) --db $(EXPLAIN_DB) --infill

.PHONY: gen-status
gen-status: $(TARGET_BIN) ## Show generation status
	$(Q)$(TARGET_BIN) status --json-dir $(EXPLAIN_DIR) --db $(EXPLAIN_DB)

.PHONY: gen-clean
gen-clean: ## Remove .explain-gen/src and .explain.db
	$(Q)rm -rf $(EXPLAIN_DIR)/src $(EXPLAIN_DB)

.PHONY: learn
learn: ## (legacy) Drain inbox — kept for compatibility
	@echo "learn: use explain-gen for this operation"

.PHONY: commit
commit: ## (legacy) AI commit message — kept for compatibility
	@echo "commit: use explain-gen for this operation"

##@ Python Language Provider

.PHONY: py-sync
py-sync: $(VENV) ## Sync guidance JSON for Python source files
	$(Q)$(PYTHON_VENV) $(AST_PY) sync --scan src/ --output $(EXPLAIN_DIR)

.PHONY: py-sync-infill
py-sync-infill: $(VENV) ## Sync Python guidance with AI comment infill
	$(Q)$(PYTHON_VENV) $(AST_PY) sync --scan src/ --output $(EXPLAIN_DIR) --infill

.PHONY: py-lint
py-lint: $(VENV) ## Lint Python with ruff
	$(Q)$(BIN)/ruff check src/ bin/

.PHONY: py-fmt
py-fmt: $(VENV) ## Format Python with ruff
	$(Q)$(BIN)/ruff format src/ bin/ && $(BIN)/ruff check --fix src/ bin/

.PHONY: py-test
py-test: $(VENV) ## Run Python tests
	$(Q)$(PYTHON_VENV) -m pytest tests/ -v

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
check-prereqs: ## Verify prerequisites (zig, uv)
	@which zig > /dev/null || (echo "zig not found. Install via mise: mise install zig"; exit 1)
	@which uv  > /dev/null || (echo "uv not found.  Install: curl -LsSf https://astral.sh/uv/install.sh | sh"; exit 1)
	@echo "Prerequisites satisfied"

.PHONY: env-init
env-init: check-prereqs venv ## Initialize development environment
	@echo "Environment ready."

.PHONY: clean
clean: ## Remove build artifacts and markers (keeps venv and .explain-gen config)
	$(Q)rm -rf .zig-cache zig-out $(HASH_DIR)
	$(Q)find . -type d -name ".zig-cache" -exec rm -rf {} + 2>/dev/null || true

.PHONY: clean-all
clean-all: clean ## Nuclear cleanup (includes venv)
	$(Q)rm -rf $(VENV) $(HOME)/.cache/uv/

##@ Guidance Management

.PHONY: guidance-prune
guidance-prune: ## Remove stale JSON files from .explain-gen/src
	$(Q)find $(EXPLAIN_DIR)/src -name '*.json' -type f -print0 2>/dev/null | \
		while IFS= read -r -d '' json; do \
			rel=$${json#$(EXPLAIN_DIR)/}; \
			src=$${rel%.json}; \
			if [ ! -f "$$src" ]; then \
				echo "Pruning: $$json"; \
				rm -f "$$json"; \
			fi; \
		done

.PHONY: explain
explain: $(EXPLAIN_DB) ## Explain a module, function, or concept  make explain QUERY="sma"
	@if [ -z "$(QUERY)" ]; then \
		echo "❌ Usage: make explain QUERY=\"<module or function or concept>\""; \
		echo "   Examples:"; \
		echo "     make explain QUERY=\"sma\""; \
		echo "     make explain QUERY=\"ring buffer\""; \
		echo "     make explain QUERY=\"ast_parser\""; \
		exit 1; \
	fi
	$(Q)$(DOCUMENTOR) explain --guidance $(EXPLAIN_DIR) "$(QUERY)"

##@ Zig Build & RALPH Loop

ZIG_SRC_FILES := $(shell find $(SRC_DIR) -name '*.zig' 2>/dev/null)

# Derive marker sets from source files
LINT_MARKERS     := $(patsubst $(SRC_DIR)/%.zig,$(LINT_MARKER_DIR)/%.zig,$(ZIG_SRC_FILES))
GUIDANCE_MARKERS := $(patsubst $(SRC_DIR)/%.zig,$(GUIDANCE_MARKER_DIR)/%.zig,$(ZIG_SRC_FILES))

# ── Binary ──────────────────────────────────────────────────────────────────

$(TARGET_BIN): $(ZIG_SRC_FILES)
	$(Q)mkdir -p $(LINT_MARKER_DIR) $(GUIDANCE_MARKER_DIR)
	$(Q)echo "Building explain-gen..."
	$(Q)zig build
	$(Q)echo "Build complete: $@"

.PHONY: install
install: $(TARGET_BIN)
	cp $(TARGET_BIN) $(shell dirname $(shell which explain-gen 2>/dev/null || echo $(INSTALLDIR)/explain-gen))/explain-gen

.PRECIOUS: $(GUIDANCE_MARKERS) $(LINT_MARKERS)

# ── Testpass ─────────────────────────────────────────────────────────────────
# Single test run - depends on source files so tests re-run when code changes.

$(TEST_PASSED): $(ZIG_SRC_FILES)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Testing:  zig build test"
	$(Q)zig build test --summary all 2>&1 || exit 1
	$(Q)touch $@

# ── Per-file lint marker ─────────────────────────────────────────────────────

$(LINT_MARKER_DIR)/%.zig: $(SRC_DIR)/%.zig $(TEST_PASSED) | $(LINT_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Linting:  $<"
	$(Q)zig fmt $< 2>&1
	$(Q)zig fmt --check $< 2>&1 || (echo "Format errors in $<. Run 'zig fmt $<' to fix."; exit 1)
	$(Q)touch $@

# ── Per-file guidance marker ─────────────────────────────────────────────────
# Uses --file for single-file incremental processing.
# Uses --no-db to skip full DB recompile on every file (gen-status/gen does full DB).

$(GUIDANCE_MARKER_DIR)/%.zig: $(SRC_DIR)/%.zig $(LINT_MARKER_DIR)/%.zig | $(TARGET_BIN) $(GUIDANCE_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Guidance: $<"
	$(Q)$(TARGET_BIN) gen --file $< --json-dir $(EXPLAIN_DIR) --no-db
	$(Q)[ -f $(EXPLAIN_DIR)/src/$*.zig.json ] || touch $(EXPLAIN_DIR)/src/$*.zig.json
	$(Q)touch $@

# ── Marker directory creation ────────────────────────────────────────────────

$(LINT_MARKER_DIR) $(GUIDANCE_MARKER_DIR):
	$(Q)mkdir -p $@

# ── STRUCTURE.md ─────────────────────────────────────────────────────────────
# Rebuilt when guidance markers are complete and database is synced.
# Depends on $(TARGET_BIN) being available but not through marker chain.

STRUCTURE.md: $(EXPLAIN_DB) | $(TARGET_BIN)
	$(Q)$(TARGET_BIN) structure --json-dir $(EXPLAIN_DIR) 2>&1 | grep -E "STRUCTURE|Generated|✓" || true
	$(Q)touch STRUCTURE.md

##@ Gate Targets

# Full RALPH loop: build → test → lint → guidance → STRUCTURE.md
# TARGET_BIN is built first as an order-only prerequisite, then the chain runs.
.PHONY: pre-commit
pre-commit: $(TARGET_BIN) STRUCTURE.md ## Run full RALPH loop (build/test/lint/guidance/structure)
	$(Q)echo "All checks passed."

.PHONY: fmt
fmt: ## Format all Zig source files
	$(Q)zig fmt $(SRC_DIR)/

.PHONY: build
build: $(TARGET_BIN) ## Build explain-gen binary

.PHONY: test
test: ## Run all Zig unit tests
	$(Q)zig build test --summary all

.PHONY: test-integration
test-integration: $(TARGET_BIN) ## Run integration smoke tests
	$(Q)bash tests/guidance_integration.sh
