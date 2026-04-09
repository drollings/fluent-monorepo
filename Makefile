# guidance Makefile
# Context: Zig-native AST-guided SQLite FTS5 database generator for NullClaw
# Maintainer: AI & Human Co-Pilot
#
# RALPH LOOP (delegated to guidance check):
#   build → guidance check
#     └─► test → lint → fmt → guidance (per stale file, all languages)
#           └─► .guidance/src/**/*.json  (JSON mtime = universal marker)
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

DOCUMENTOR := $(if $(wildcard zig-out/bin/guidance), \
	zig-out/bin/guidance, \
	$(shell which guidance 2>/dev/null || echo "guidance not found" && exit 1))

TARGET_BIN  := zig-out/bin/guidance
AST_PY      := bin/guidance-py
CONFIG      := .guidance/guidance-config.json
INSTALLDIR  := $(HOME)/.local/bin

SRC_DIR      := ./src
GUIDANCE_DIR  := .guidance
GUIDANCE_DB   := .guidance.db
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

# Database target: rebuilt by guidance check automatically.
# This rule is kept for direct `make .explain.db` invocations.
$(GUIDANCE_DB): | $(TARGET_BIN)
	$(Q)echo "Syncing database: $@"
	$(Q)$(TARGET_BIN) check --workspace . --json-dir $(GUIDANCE_DIR) --db $@ --timeout 1
	$(Q)touch $@

.PHONY: commit
commit: $(TARGET_BIN) | STRUCTURE.md ## Generate AI commit message from staged diff + guidance JSON context
	$(Q)$(TARGET_BIN) commit $(if $(DRY_RUN),--dry-run) $(if $(DEBUG),--debug)

##@ C++ Language Provider

CPPPARSER_SRC   := /opt/src/development/cppparser
BOOST_JSON_SRC  := /opt/src/development/json
CPP_BUILD_DIR   := .guidance-cpp-build
CPP_PARSER_BUILD := $(CPP_BUILD_DIR)/cppparser
AST_CPP         := bin/guidance-cpp
CPP_MAIN_SRC    := src/guidance-cpp/main.cpp

CPPPARSER_LIBS  := \
	$(CPP_PARSER_BUILD)/cppparser/libcppparser.a \
	$(CPP_PARSER_BUILD)/cppast/libcppast.a

CPPPARSER_INCLUDES := \
	-I$(CPPPARSER_SRC)/cppparser/include \
	-I$(CPPPARSER_SRC)/cppast/include

# Boost.JSON: use system installation (local copy lacks transitive Boost deps)
CPP_LDFLAGS := -lssl -lcrypto -lstdc++fs -lboost_json

# Build cppparser static libraries via CMake
# A fake clang-tidy shim is injected so the build succeeds without clang-tidy installed.
$(CPP_BUILD_DIR)/bin/clang-tidy:
	$(Q)mkdir -p $(CPP_BUILD_DIR)/bin
	$(Q)printf '#!/bin/sh\nexit 0\n' > $@
	$(Q)chmod +x $@

$(CPPPARSER_LIBS): $(CPPPARSER_SRC)/CMakeLists.txt $(CPP_BUILD_DIR)/bin/clang-tidy
	$(Q)echo "Building cppparser via CMake..."
	$(Q)PATH="$(CURDIR)/$(CPP_BUILD_DIR)/bin:$$PATH" \
		cmake -S $(CPPPARSER_SRC) -B $(CPP_PARSER_BUILD) \
		-DCMAKE_BUILD_TYPE=Release \
		-DCPPPARSER_BUILD_TESTS=OFF \
		-DCMAKE_CXX_FLAGS="-w" \
		-DCMAKE_C_FLAGS="-w" \
		-Wno-dev \
		2>&1 | tail -5
	$(Q)PATH="$(CURDIR)/$(CPP_BUILD_DIR)/bin:$$PATH" \
		cmake --build $(CPP_PARSER_BUILD) --parallel $$(nproc) 2>&1 | tail -10
	$(Q)echo "cppparser built."

# Build guidance-cpp binary
$(AST_CPP): $(CPP_MAIN_SRC) $(CPPPARSER_LIBS)
	$(Q)echo "Building $@"
	$(Q)g++ -std=c++17 -O2 -o $@ $(CPP_MAIN_SRC) \
		$(CPPPARSER_INCLUDES) \
		$(CPPPARSER_LIBS) \
		$(CPP_LDFLAGS)
	$(Q)echo "Built: $@"

.PHONY: cpp-build
cpp-build: $(AST_CPP) ## Build the guidance-cpp binary

.PHONY: cpp-sync
cpp-sync: $(AST_CPP) ## Sync guidance JSON for C/C++ source files
	$(Q)$(AST_CPP) sync --scan src/ --output $(GUIDANCE_DIR)

.PHONY: cpp-sync-regen
cpp-sync-regen: $(AST_CPP) ## Force-regenerate all C/C++ guidance JSON
	$(Q)$(AST_CPP) sync --scan src/ --output $(GUIDANCE_DIR) --regen

.PHONY: cpp-scrub
cpp-scrub: $(AST_CPP) ## Scrub synthetic comments from C/C++ guidance JSON
	$(Q)$(AST_CPP) scrub --scan $(GUIDANCE_DIR)/src

.PHONY: clean-cpp
clean-cpp: ## Remove guidance-cpp build artifacts
	$(Q)rm -rf $(CPP_BUILD_DIR) $(AST_CPP)

##@ Python Language Provider

.PHONY: py-sync
py-sync: $(VENV) ## Sync guidance JSON for Python source files
	$(Q)$(PYTHON_VENV) $(AST_PY) sync --scan src/ --output $(GUIDANCE_DIR)

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
clean: clean-db ## Remove build artifacts and markers (keeps venv and .guidance config)
	$(Q)rm -rf .zig-cache zig-out $(HASH_DIR)
	$(Q)find . -type d -name ".zig-cache" -exec rm -rf {} + 2>/dev/null || true

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
	$(Q)$(DOCUMENTOR) explain --guidance $(GUIDANCE_DIR) "$(QUERY)"

##@ Zig Build & RALPH Loop

ZIG_SRC_FILES := $(shell find $(SRC_DIR) -name '*.zig' 2>/dev/null)

# ── Binary ───────────────────────────────────────────────────────────────────

$(TARGET_BIN): $(ZIG_SRC_FILES)
	$(Q)echo "Building $@"
	$(Q)zig build guidance --summary failures
	$(Q)echo "Build complete: $@"

.PHONY: install
install: $(TARGET_BIN)
	$(Q)cp $(TARGET_BIN) $(shell dirname $(shell which guidance 2>/dev/null || echo $(INSTALLDIR)))/guidance
	$(Q)echo "Installed build in $(INSTALLDIR)/guidance"

# ── STRUCTURE.md ─────────────────────────────────────────────────────────────
# Delegated: guidance check always regenerates STRUCTURE.md after guidance.
# The target below is kept for direct `make STRUCTURE.md` invocations.

STRUCTURE.md: $(GUIDANCE_DB) | $(TARGET_BIN)
	$(Q)$(TARGET_BIN) structure --json-dir $(GUIDANCE_DIR) 2>&1 | grep -E "STRUCTURE|Generated|✓" || true
	$(Q)touch STRUCTURE.md

##@ Gate Targets

# Full RALPH loop: build → guidance check (test/lint/fmt/guidance/structure/db)
# guidance check handles incremental detection via JSON mtime comparison.
.PHONY: pre-commit
pre-commit: STRUCTURE.md ## Run full RALPH loop via guidance check
	@# $(Q)echo "Building coral"
	@# $(Q)zig build coral --summary failures
	$(Q)$(TARGET_BIN) check
	$(Q)echo "✓ All checks passed. Ready to commit."

# .PHONY: setup-guidance-php
# 
# setup-guidance-php:
# 	@echo "Setting up guidance-php..."
# 	@mkdir -p tools/guidance-php
# 	@mkdir -p bin
# 	@cp tools_src/guidance-php.php tools/guidance-php/guidance-php
# 	@cp tools_src/composer.json tools/guidance-php/composer.json
# 	@cd tools/guidance-php && composer install --no-dev --quiet
# 	@chmod +x tools/guidance-php/guidance-php
# 	@ln -sf ../tools/guidance-php/guidance-php bin/guidance-php
# 	@echo "✓ bin/guidance-php is ready."

# .PHONY: build-guidance-rs
# 
# build-guidance-rs:
# 	@echo "Building guidance-rs..."
# 	@cd guidance-rs && cargo build --release
# 	@mkdir -p bin
# 	@cp guidance-rs/target/release/guidance-rs bin/
# 	@echo "guidance-rs installed to bin/guidance-rs"

# # ============================================================================
# # Guidance TypeScript Provider (guidance-ts)
# # ============================================================================
# 
# .PHONY: build-guidance-ts
# 
# build-guidance-ts: bin/guidance-ts
# 
# bin/guidance-ts: src/guidance-ts.ts package.json
# 	@echo "=> Building guidance-ts..."
# 	@npm install typescript esbuild @types/node --no-save
# 	@mkdir -p bin
# 	@npx esbuild src/guidance-ts.ts \
# 		--bundle \
# 		--platform=node \
# 		--target=node18 \
# 		--outfile=bin/guidance-ts \
# 		--banner:js="#!/usr/bin/env node"
# 	@chmod +x bin/guidance-ts
# 	@echo "✓ Compiled to bin/guidance-ts"
# 
# package.json:
# 	@echo '{"private": true}' > package.json
# 