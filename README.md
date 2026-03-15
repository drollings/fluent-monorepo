# explain-gen

A Zig-native, deterministic AST-guided SQLite FTS5 database generator for
AI-assisted codebase navigation. It analyzes source files (Zig, Python) via AST,
generates structured JSON metadata in `.explain-gen/src/`, and compiles them
into `.explain.db` for fast, token-efficient `make explain` queries — optimized
for subagent discovery workflows.

The `.explain.db` file is meant to be extensible for codebase and document
exploration.  This tool can be run on other Zig codebases and use local
inference to streamline iterative developent, and greatly reduce token burn
for agentic coding.

## Authorship and copyright

This code is authored by Daniel Rollings, February 2026, with a mixture of
elements from previous hand-written projects in Python and C++, rendered
into Zig with ease of extensibility into other languages.

It is released under a dual GPL/Commercial license.  See below.

## What it does

### Core Pipeline

- **AST-guided extraction**: Parses source files (Zig via `std.zig.Ast`, Python
  via `ast` module) extracting functions, structs, enums, signatures, line
  numbers, and comments with zero ambiguity.
- **Incremental sync**: SHA-256 `match_hash` comparison enables skip-if-unchanged
  behavior — only regenerate descriptions when the API contract changes.
- **SQLite FTS5 indexing**: Compiles all guidance JSON into a queryable
  `.explain.db` with BM25 ranking for sub-100ms lookups across 1000+ modules.
- **Semantic aliases**: Natural language queries like "database" expand to
  `ExplainDb`, `syncDatabase`, `searchWithAliases` — bridging terminology gaps.
- **Skill attachment**: Pattern detection (GoF, domain) auto-adds skill references
  from `.explain-gen/.skills/` to relevant code locations.

### Query Layer

- **`make explain QUERY="..."`**: Staged pipeline that:
  1. Expands query with semantic aliases
  2. Searches FTS5 database with stop-word filtering
  3. Extracts source excerpts with brace-aware capture
  4. For long queries (5+ words), uses local LLM to filter/synthesize
  5. Formats output with code verbatim, metadata follow-ups

- **Query modes**:
  - Short queries (1-4 words): Fast path, no LLM — direct FTS5 lookup
  - Long queries (5+ words): LLM filters irrelevant prose, synthesizes answer

- **Token efficiency**: 69% average token savings vs frontier model doing
  grep + whole-file loads. Designed for subagent workflows where context
  budget matters.

### LLM Enhancement

- **Comment infill**: `--infill` generates descriptions for members without
  comments, using local Ollama/AI-compatible endpoints.
- **Comment regeneration**: `--regen` compares AI-generated vs existing
  comments, keeps the better one (quality scoring).
- **Determinism-first**: AST parsing is ground truth; LLM is strictly additive.

### Structured Output

- **STRUCTURE.md synthesis**: Hierarchical tree view with inline comments from
  guidance JSON — human-legible codebase map without MCP or tool calls.
- **Guidance JSON schema**: Per-file metadata capturing module purpose,
  function signatures, design patterns, reverse dependencies (`used_by`).
- **Knowledge inbox**: `.explain-gen/.doc/inbox/` captures insights/capabilities
  during development forlater promotion into structured skills.

## Quick start

```bash
# Install toolchain (requires mise)
mise install          # installs Zig + Python + uv from mise.toml

# Set up Python provider venv
make env-init

# Build the Zig binary
make build            # → zig-out/bin/explain-gen

# Generate guidance JSON for source files
make guidance         # syncs src → .explain-gen/src/*.json

# Build the FTS5 database
make db               # → .explain.db

# Run the full RALPH loop gate
make pre-commit       # build → test → guidance → lint → STRUCTURE.md

# Query the guidance index
make explain QUERY="LLM integration"
make explain QUERY="How do I add a new language plugin?"
make explain QUERY="sqlite database schema"
```

## Query examples

```bash
# Short query (fast path, no LLM)
make explain QUERY="database"
# → Finds ExplainDb, syncDatabase, searchWithAliases

# Natural language question (LLM synthesis)
make explain QUERY="What design patterns are used in this codebase?"
# → Returns detectPatterns, skill references, synthesized summary

# Specific API lookup
make explain QUERY="Where is the LLM client defined?"
# → Returns LlmClient.complete, filterStages, synthesize

# Multi-token technical query
make explain QUERY="gitignore filtering"
# → Returns shouldIgnore, GitignoreFilter with full implementation excerpt
```

## Source layout

```
src/
  explain-gen/      Zig core engine (AST parser, sync, db, staged query)
  common/            Shared LLM HTTP client
bin/
  explain-gen        Compiled binary (via zig build)
  explain-gen-py     Python AST provider
.explain-gen/
  explain-gen-config.json   Model / provider configuration
  semantic-aliases.json      Query expansion mappings
  .skills/                   Design-pattern skill documents
  .doc/                      Capabilities, diary, inbox
  src/                       Generated guidance JSON
.explain.db         SQLite FTS5 database for queries
env/
  mk/                Makefile helpers + per-language overrides
  mise/              Language-specific mise.toml fragments
doc/
  DESIGN.md          System design reference
```

## Staged Query Pipeline

The `make explain` target implements a staged pipeline optimized for subagent
discovery:

```
Query → Alias Expansion → FTS5 Search → Node Boosting → Stage Assembly
                                                              ↓
                          Prose stages ← [comments from guidance JSON]
                          Code stages  ← [source excerpts w/ brace capture]
                          Metadata      ← [keywords, see_also, skills]
                          Skill docs    ← [SKILL.md excerpts for patterns]
                                                              ↓
                    Long query? ──Yes──→ LLM Filter → LLM Synthesize → Answer
                              │
                              No
                              ↓
                         Format Output
```

**Key features:**
- Semantic aliases expand "database" → ExplainDb, syncDatabase, searchWithAliases
- Node boosting prioritizes structs/functions over tests
- Brace-aware source extraction captures complete function/struct bodies
- See-also traversal follows metadata breadcrumbs when results are sparse

## Adding a new language provider

Create `bin/explain-gen-<lang>` and ensure it accepts:

```
explain-gen-<lang> sync --file <path> --output <guidance_dir> [--infill]
explain-gen-<lang> sync --scan <dir>  --output <guidance_dir> [--infill]
```

Output JSON must follow the canonical schema:

```json
{
  "meta":     { "module": "…", "source": "…", "language": "…" },
  "comment":  "one-line module description",
  "skills":   [{ "name": "…", "type": "GoF|Domain", "ref": "…" }],
  "hashtags": [],
  "used_by":  ["src/other_file.zig"],
  "members":  [ { "type", "name", "is_pub", "line", "signature", "comment", "match_hash", … } ]
}
```

Register the provider in `.explain-gen/explain-gen-config.json` under
`providers`.

## Configuration

`.explain-gen/explain-gen-config.json` controls:
- `model`: Local LLM model name (e.g., "llama3.2")
- `api_url`: Ollama/OpenAI-compatible endpoint
- `providers`: Language-specific AST providers

`.explain-gen/semantic-aliases.json` defines query expansions:
```json
[
  {"key": "database", "values": ["ExplainDb", "syncDatabase", "searchWithAliases"]},
  {"key": "LLM", "values": ["LlmClient", "filterStages", "synthesize"]}
]
```

## Performance

| Metric | Value |
|--------|-------|
| Average accuracy | 9.0/10 |
| Average completion | 7.6/10 |
| Token savings vs grep+load | 69% |
| Queries scoring 9-10 | 71% |
| Complete failures | 0 |

Tested across 34 queries covering database, AST parsing, LLM integration,
design patterns, plugins, and more.

## License

### Licensing & Usage

This software is dual-licensed, meaning you must choose the appropriate
license for your use case.  This model ensures the software remains free and
open for the community, while ensuring sustainable development through
commercial support from large organizations.

### Option A: Community License (GNU GPLv3)

If you are building an open-source application, a hobby project, or are an
individual developer, you may use this software for free under the terms of
the GNU General Public License v3.0 (GPLv3).

* Obligations: If you distribute your software, you must open-source your
entire application under the GPLv3.

* Disclaimer: Provided "AS IS" with absolutely no warranty, no legal liability,
and no technical support.

### Option B: Commercial License

If you are developing proprietary, closed-source software, you cannot legally
use the GPLv3 license without open-sourcing your own codebase.  You must
purchase a Commercial License if you meet any of the following criteria:

* You wish to embed this software in a proprietary, closed-source product.

* Your Legal Entity (including parent companies and affiliates) generates gross
annual revenue exceeding $1,000,000 USD.

* You require usage for more than one (1) developer seat.

* You require technical support, indemnification, or liability waivers.
