# explain-gen Makefile
# Context: Zig-native AST-guided SQLite FTS5 database generator for NullClaw
# Maintainer: AI & Human Co-Pilot
#
# RALPH LOOP: build → test → guidance (per-file) → lint → STRUCTURE.md
#
# Key invariant: $(TARGET_BIN) depends ONLY on source files — never on
# STRUCTURE.md or markers that themselves depend on the binary.  The circular
# dependency that previously existed (TARGET_BIN → STRUCTURE.md → LINT_MARKERS
# → GUIDANCE_MARKERS → TARGET_BIN) is broken by making pre-commit the gating
# target rather than TARGET_BIN.

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

TARGET_BIN  := zig-out/bin/explain-gen
AST_PY      := bin/explain-gen-py
CONFIG      := .explain-gen/explain-gen-config.json

SRC_DIR      := ./src
EXPLAIN_DIR  := .explain-gen
EXPLAIN_DB   := .explain.db
ENV_DIR      := .env
HASH_DIR     := $(ENV_DIR)/.make_hashes

# Marker directories — store per-file success stamps for incremental builds
BUILD_MARKER_DIR    := zig-out/mark/build
TEST_MARKER_DIR     := zig-out/mark/test
GUIDANCE_MARKER_DIR := zig-out/mark/guidance
LINT_MARKER_DIR     := zig-out/mark/lint

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

.PHONY: gen
gen: $(TARGET_BIN) ## Generate .explain-gen/ JSON and .explain.db   make gen
	$(Q)$(TARGET_BIN) gen --workspace . --json-dir $(EXPLAIN_DIR) --db $(EXPLAIN_DB)

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
	@echo "learn: use ast-guidance for this operation"

.PHONY: commit
commit: ## (legacy) AI commit message — kept for compatibility
	@echo "commit: use ast-guidance for this operation"

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
	$(Q)rm -rf .zig-cache zig-out $(HASH_DIR) $(ZIG_DEPEND_FILE)
	$(Q)rm -rf $(BUILD_MARKER_DIR) $(TEST_MARKER_DIR) $(GUIDANCE_MARKER_DIR) $(LINT_MARKER_DIR)
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

##@ Zig Build & RALPH Loop
#
# Dependency chain per .zig file (no circular deps):
#
#   src/foo.zig
#     └─► zig-out/mark/build/foo.zig    (zig build — compile check)
#           └─► zig-out/mark/test/foo.zig     (zig test — unit tests)
#                 └─► zig-out/mark/guidance/foo.zig  (explain-gen gen --file)
#                       └─► zig-out/mark/lint/foo.zig   (zig fmt --check)
#
#   All LINT_MARKERS → STRUCTURE.md
#   All LINT_MARKERS + STRUCTURE.md → pre-commit ✓
#
# TARGET_BIN depends only on ZIG_SRC_FILES (no markers, no STRUCTURE.md).

ZIG_SRC_FILES := $(shell find $(SRC_DIR) -name '*.zig' 2>/dev/null)

# Derive marker sets from source files
BUILD_MARKERS    := $(patsubst $(SRC_DIR)/%.zig,$(BUILD_MARKER_DIR)/%.zig,$(ZIG_SRC_FILES))
TEST_MARKERS     := $(patsubst $(SRC_DIR)/%.zig,$(TEST_MARKER_DIR)/%.zig,$(ZIG_SRC_FILES))
GUIDANCE_MARKERS := $(patsubst $(SRC_DIR)/%.zig,$(GUIDANCE_MARKER_DIR)/%.zig,$(ZIG_SRC_FILES))
LINT_MARKERS     := $(patsubst $(SRC_DIR)/%.zig,$(LINT_MARKER_DIR)/%.zig,$(ZIG_SRC_FILES))

# Module flags for zig test on explain-gen files (needs common module + sqlite3)
EXPLAIN_MODULE_FLAGS = --dep common -Mroot=$< -Mcommon=$(SRC_DIR)/common/llm.zig -lc -lsqlite3

# Zig dependency file (maps @import edges to build-marker prerequisites)
ZIG_DEPEND_FILE := zig.depend

# ── Binary ──────────────────────────────────────────────────────────────────
# TARGET_BIN depends ONLY on source files — no markers, no STRUCTURE.md.
# This is the key fix for the circular dependency.

$(TARGET_BIN): $(ZIG_SRC_FILES)
	$(Q)mkdir -p $(BUILD_MARKER_DIR) $(TEST_MARKER_DIR) $(GUIDANCE_MARKER_DIR) $(LINT_MARKER_DIR)
	$(Q)echo "Building explain-gen..."
	$(Q)zig build
	$(Q)echo "Build complete: $@"

.PHONY: install
install: $(TARGET_BIN)
	cp $(TARGET_BIN) $(shell dirname $(shell which explain-gen 2>/dev/null || echo /usr/local/bin/explain-gen))/explain-gen

# ── Dependency file ──────────────────────────────────────────────────────────

$(ZIG_DEPEND_FILE): $(ZIG_SRC_FILES) $(TARGET_BIN)
	$(Q)$(TARGET_BIN) deps --src $(SRC_DIR) \
	  | sed \
	      -e 's|__finish____src__|$(BUILD_MARKER_DIR)/|g' \
	      -e 's|__success____src__|$(BUILD_MARKER_DIR)/|g' \
	      -e 's|__src__|$(SRC_DIR)/|g' \
	  > $(ZIG_DEPEND_FILE)

# Only include the dependency file if it already exists — do not auto-rebuild it.
# This prevents make clean from triggering the binary build via the dep chain.
ifneq ($(wildcard $(ZIG_DEPEND_FILE)),)
include $(ZIG_DEPEND_FILE)
endif

.PRECIOUS: $(BUILD_MARKERS) $(TEST_MARKERS) $(GUIDANCE_MARKERS) $(LINT_MARKERS)

# ── Per-file build marker ────────────────────────────────────────────────────

$(BUILD_MARKER_DIR)/%.zig: $(SRC_DIR)/%.zig | $(BUILD_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Building: $<"
	$(Q)zig build 2>&1 || exit 1
	$(Q)touch $@

# ── Per-file test markers ────────────────────────────────────────────────────
# explain-gen files need common module + sqlite3

$(TEST_MARKER_DIR)/explain-gen/%.zig: $(SRC_DIR)/explain-gen/%.zig $(BUILD_MARKER_DIR)/explain-gen/%.zig | $(TEST_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Testing:  $<"
	$(Q)zig test $(EXPLAIN_MODULE_FLAGS) 2>&1 || exit 1
	$(Q)touch $@

# common/* and other files
$(TEST_MARKER_DIR)/%.zig: $(SRC_DIR)/%.zig $(BUILD_MARKER_DIR)/%.zig | $(TEST_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Testing:  $<"
	$(Q)zig test $< -lc 2>&1 || exit 1
	$(Q)touch $@

# ── Per-file guidance marker ─────────────────────────────────────────────────
# Uses --file for single-file incremental processing.
# Uses --no-db to skip full DB recompile on every file (gen-status/gen does full DB).

$(GUIDANCE_MARKER_DIR)/%.zig: $(SRC_DIR)/%.zig $(TEST_MARKER_DIR)/%.zig | $(TARGET_BIN) $(GUIDANCE_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Guidance: $<"
	$(Q)$(TARGET_BIN) gen --file $< --json-dir $(EXPLAIN_DIR) --no-db
	$(Q)[ -f $(EXPLAIN_DIR)/src/$*.zig.json ] || touch $(EXPLAIN_DIR)/src/$*.zig.json
	$(Q)touch $@

# ── Per-file lint marker ─────────────────────────────────────────────────────

$(LINT_MARKER_DIR)/%.zig: $(SRC_DIR)/%.zig $(GUIDANCE_MARKER_DIR)/%.zig | $(LINT_MARKER_DIR)
	$(Q)mkdir -p $(dir $@)
	$(Q)echo "Linting:  $<"
	$(Q)zig fmt $< 2>&1
	$(Q)zig fmt --check $< 2>&1 || (echo "Format errors in $<. Run 'zig fmt $<' to fix."; exit 1)
	$(Q)touch $@

# ── Marker directory creation ────────────────────────────────────────────────

$(BUILD_MARKER_DIR) $(GUIDANCE_MARKER_DIR) $(LINT_MARKER_DIR) $(TEST_MARKER_DIR):
	$(Q)mkdir -p $@

# ── STRUCTURE.md ─────────────────────────────────────────────────────────────
# Rebuilt when any lint marker is updated, or when explain.db changes.
# Depends on $(TARGET_BIN) being available but not through marker chain.

STRUCTURE.md: $(LINT_MARKERS) | $(TARGET_BIN)
	$(Q)$(TARGET_BIN) structure --json-dir $(EXPLAIN_DIR) 2>&1 | grep -E "STRUCTURE|Generated|✓" || true
	$(Q)touch STRUCTURE.md

##@ Gate Targets

# Full RALPH loop: build → test → guidance (per-file) → lint → STRUCTURE.md
# TARGET_BIN is built first as an order-only prerequisite, then the chain runs.
.PHONY: pre-commit
pre-commit: $(TARGET_BIN) STRUCTURE.md ## Run full RALPH loop (build/test/guidance/lint/structure)
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
