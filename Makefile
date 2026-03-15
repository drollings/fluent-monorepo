# explain-gen Makefile
# Context: Zig-native AST-guided SQLite FTS5 database generator for NullClaw
# Maintainer: AI & Human Co-Pilot
#
# RALPH LOOP (delegated to explain-gen check):
#   build → explain-gen check
#     └─► test → lint → fmt → guidance (per stale file, all languages)
#           └─► .explain-gen/src/**/*.json  (JSON mtime = universal marker)
#                 └─► .explain.db
#                       └─► STRUCTURE.md
#
# Key invariant: $(TARGET_BIN) depends ONLY on source files — never on
# STRUCTURE.md or markers that themselves depend on the binary.
# Change detection: source mtime vs guidance JSON mtime (no separate marker files).

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

# Database target: rebuilt by explain-gen check automatically.
# This rule is kept for direct `make .explain.db` invocations.
$(EXPLAIN_DB): | $(TARGET_BIN)
	$(Q)echo "Syncing database: $@"
	$(Q)$(TARGET_BIN) gen --workspace . --json-dir $(EXPLAIN_DIR) --db $(EXPLAIN_DB)
	$(Q)touch $@

.PHONY: commit
commit: $(TARGET_BIN) ## Generate AI commit message from staged diff + guidance JSON context
	$(Q)$(TARGET_BIN) commit $(if $(DRY_RUN),--dry-run) $(if $(DEBUG),--debug)

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
clean: _clean-guidance ## Remove build artifacts and markers (keeps venv and .explain-gen config)
	$(Q)rm -rf .zig-cache zig-out $(HASH_DIR) $(EXPLAIN_DB)
	$(Q)find . -type d -name ".zig-cache" -exec rm -rf {} + 2>/dev/null || true

##@ Guidance Management

.PHONY: _clean-guidance
_clean-guidance: ## Remove stale JSON files from .explain-gen/src
	$(Q)find $(EXPLAIN_DIR)/src -name '*.json' -type f -exec rm -rf {} \;

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

# ── Binary ───────────────────────────────────────────────────────────────────

$(TARGET_BIN): $(ZIG_SRC_FILES)
	$(Q)echo "Building explain-gen..."
	$(Q)zig build
	$(Q)echo "Build complete: $@"

.PHONY: install
install: $(TARGET_BIN)
	cp $(TARGET_BIN) $(shell dirname $(shell which explain-gen 2>/dev/null || echo $(INSTALLDIR)/explain-gen))/explain-gen

# ── STRUCTURE.md ─────────────────────────────────────────────────────────────
# Delegated: explain-gen check always regenerates STRUCTURE.md after guidance.
# The target below is kept for direct `make STRUCTURE.md` invocations.

STRUCTURE.md: $(EXPLAIN_DB) | $(TARGET_BIN)
	$(Q)$(TARGET_BIN) structure --json-dir $(EXPLAIN_DIR) 2>&1 | grep -E "STRUCTURE|Generated|✓" || true
	$(Q)touch STRUCTURE.md

##@ Gate Targets

# Full RALPH loop: build → explain-gen check (test/lint/fmt/guidance/structure/db)
# explain-gen check handles incremental detection via JSON mtime comparison.
.PHONY: pre-commit
pre-commit: $(TARGET_BIN) ## Run full RALPH loop via explain-gen check
	$(Q)$(TARGET_BIN) check
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
