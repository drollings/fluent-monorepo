# guidance

A Zig-native, deterministic AST-guided LanceDB vector search database generator for
AI-assisted codebase navigation. It analyzes source files (Zig, Python) via AST,
generates structured JSON metadata in `.guidance/src/`, and compiles them
into `.guidance.db` for fast, token-efficient `make explain` queries — optimized
for subagent discovery workflows.

The `.guidance.db` file uses vector embeddings (cosine similarity) combined with
keyword search for semantic code navigation. This tool can be run on other Zig
codebases and use local inference to streamline iterative development, greatly
reducing token burn for agentic coding.

## Authorship and copyright

This code is authored by Daniel Rollings, February 2026, with a mixture of
elements from previous hand-written projects in Python and C++, rendered
into Zig with ease of extensibility into other languages.

It is released under a dual GPL/Commercial license. See below.

## The starting point: deterministic-first AI-enhanced code navigation

This codebase is a monorepo that begins with `guidance`, its own lightweight
tool for human or AI code navigation and subagent workflows.

Typical agentic codebases outsource code navigation to external tools.  This
one builds it in, with the AST as the authority and the LLM as an optional
enhancement.  Starting with `guidance`, it differs from typical agentic
AI-native codebases in several fundamental ways:

1. Determinism-First Philosophy
- AST parsing produces ground truth; LLM enhancement is strictly additive, never authoritative
- The .guidance/src/*.json files are canonical—everything else (STRUCTURE.md, query results) is derived
- Typical AI-native codebases treat LLM output as primary; here it's cached metadata
2. Documentation as Cached Computation
- Uses match_hash (SHA-256 of signatures) for surgical incremental updates
- LLM-generated comments are stored, validated, and scrubbed for quality
- Typical: LLM re-generates context each session; here: idempotent, reusable artifacts
3. Human-in-the-Loop by Design
- RALPH loop: Discover → Understand → Decide → Implement → Verify
- Agent must read source files, validate skill applicability before acting
- Typical: agents act autonomously; here: structured handoffs with verification gates
4. Inbox Pattern for Knowledge Capture
- .guidance/.doc/inbox/ captures insights as they emerge during development
- Knowledge flows from unstructured bullets → structured skills
- Typical: knowledge lost between sessions; here: accumulated and upcycled
5. Skill-Based Context Linking
- Files reference skills (e.g., gof-patterns, zig-current) that must be read
- Pattern detection auto-attaches relevant skill references
- Typical: agent guesses patterns; here: explicit skill declarations
6. Multi-Modal Query System
- guidance explain combines AST data, cached comments, inbox bullets, and optional LLM synthesis
- Short queries (≤4 words) use fast path without LLM; longer queries invoke LLM filtering
- Typical: single-mode search; here: hybrid keyword + semantic + structured

## How This Codebase Differs from a Typical Agentic AI-Native Codebase

Most agentic codebases are Python glue layers over LLM APIs, with
determinism as an afterthought.  This codebase treats the LLM as a fallback
compiler for unstructured data, and builds deterministic, auditable,
edge-deployable intelligence in systems-level Zig — with the codebase
navigation infrastructure itself treated as a first-class component of the
AI development loop.

The extended codebase beyond `guidance` is built on these patterns:

1. Deterministic-First, LLM-Last Execution Model

A typical agentic stack (LangChain, AutoGPT, CrewAI) routes every query through an LLM for reasoning. Coral Context
inverts this completely:

Typical:  Query → LLM → Response         (always probabilistic, always expensive)

Coral:    Query → DAG bitmask resolution  (sub-100ms, zero cost)
               ↓ (miss) Local small model  (hybrid)
               ↓ (miss) Frontier LLM       (last resort, result cached as a new DAG node)

The expensive probabilistic step becomes a one-time cost. Each novel solution compiles into a permanent DAG
capability.

2. Written in Zig, Not Python

Most agentic frameworks are Python. Zig is a systems language with:
- No garbage collector, no runtime, explicit arena allocation
- Sub-50MB binary, <500MB RAM — targets Raspberry Pi
- Zero-cost abstractions, @popCount-accelerated bitmask matching

This is a fundamental architectural constraint that shapes every subsystem.

3. No External Vector DB

Inspired by NullClaw, guidance and Coral Context eschew Pinecone, Chroma, or
Weaviate.  They embed f32 vectors as SQLite BLOBs and computes cosine
similarity in-process in Zig.  The entire knowledge base is a single
vendored SQLite file — no separate server, no network hop.

4. Bitwise DAG Traversal Replaces Prompt Engineering

Capabilities are DynamicBitSet trait masks.  Dependency resolution is
hardware-accelerated @popCount over bitmask intersection — Kahn's algorithm,
not a chain-of-thought prompt.  The logic is encoded explicitly in the
graph, not inferred from an LLM every time.

5. WASM Sandboxing for LLM-Generated Tools

Dynamically generated tools (including those emitted by a frontier LLM)
compile to WebAssembly and run inside Extism sandboxes.  IPC across the
boundary is binary-only (extern struct align(1), FlatBuffers-style offsets)
— JSON parsing is explicitly forbidden inside the perimeter.  This prevents
injection attacks and memory leaks from untrusted code.

6. Reflection Layer as Single Schema Source of Truth

All access paths — TUI editor, WASM binary IPC, SQLite hydration, role-based
permission enforcement — derive from one comptime-generated Accessor table.
There is no schema drift possible between serialization formats because they
are all the same table.

7. LOD Context Packing Instead of Naive Context Stuffing

ContextNodes exist at six detail levels (full text → 800-char summary →
240-char brief → 80-char snippet → name → keywords).  Context windows are
packed by graph distance: closer nodes get higher LOD, distant nodes get
snippets.  This replaces the common approach of shoving raw chunks into a
context window.

8. Neurosymbolic Ontology (YAGO 4.5)

The type hierarchy enables duck-typing for capabilities: a tool built for
"Person" automatically works for "Scientist" through subsumption inference.
This is symbolic AI layered under the neural component — not just
retrieval-augmented generation.

9. Actor-Model Concurrency with Arena Isolation

The QueueReactor pattern: each task gets its own ArenaAllocator.  No shared
mutable state, no data races, no GC.  The arena is freed atomically on task
completion.  This achieves thread safety without a borrow checker.

---

### Core Pipeline

- **AST-guided extraction**: Parses source files (Zig via `std.zig.Ast`, Python
  via `ast` module) extracting functions, structs, enums, signatures, line
  numbers, and comments with zero ambiguity.
- **Incremental sync**: SHA-256 `match_hash` comparison enables skip-if-unchanged
  behavior — only regenerate descriptions when the API contract changes.
- **LanceDB vector search**: Compiles all guidance JSON into `.guidance.db` with
  hybrid vector + keyword search for sub-100ms lookups across 1000+ modules.
- **Semantic aliases**: Natural language queries like "database" expand to
  `GuidanceDb`, `syncDatabase`, `searchWithAliases` — bridging terminology gaps.
- **Skill attachment**: Pattern detection (GoF, domain) auto-adds skill references
  from `.guidance/.skills/` to relevant code locations.

### Query Layer

- **`make explain QUERY="..."`**: Staged pipeline that:
  1. Expands query with semantic aliases
  2. Searches vector database (hybrid: cosine similarity + LIKE)
  3. Extracts source excerpts with brace-aware capture
  4. For long queries (5+ words), uses local LLM to filter/synthesize
  5. Formats output with code verbatim, metadata follow-ups

- **Query modes**:
  - Short queries (1-4 words): Fast path, no LLM — direct vector search
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
- **Knowledge inbox**: `.guidance/.doc/inbox/` captures insights/capabilities
  during development forlater promotion into structured skills.

## Quick start

```bash
# Install toolchain (requires mise)
mise install          # installs Zig + Python + uv from mise.toml

# Set up Python provider venv
make env-init

# Build the Zig binary
make build            # → zig-out/bin/guidance

# Generate guidance JSON for source files
make guidance         # syncs src → .guidance/src/*.json

# Build the vector search database
make db               # → .guidance.db

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
# → Finds GuidanceDb, syncDatabase, searchWithAliases

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
  guidance/      Zig core engine (AST parser, sync, lance_db, staged query)
  common/            Shared LLM HTTP client
bin/
  guidance        Compiled binary (via zig build)
  guidance-py     Python AST provider
.guidance/
  guidance-config.json   Model / provider configuration
  semantic-aliases.json  Query expansion mappings
  .skills/               Design-pattern skill documents
  .doc/                  Capabilities, diary, inbox
  src/                   Generated guidance JSON
.guidance.db         SQLite vector search database for queries
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
Query → Alias Expansion → Hybrid Search → Node Boosting → Stage Assembly
                                                              ↓
                          Prose stages ← [comments from guidance JSON]
                          Code stages  ← [source excerpts w/ brace capture]
                          Metadata     ← [keywords, see_also, skills]
                          Skill docs   ← [SKILL.md excerpts for patterns]
                                                              ↓
                    Long query? ──Yes──→ LLM Filter → LLM Synthesize → Answer
                              │
                              No
                              ↓
                         Format Output
```

**Key features:**
- Semantic aliases expand "database" → GuidanceDb, syncDatabase, searchWithAliases
- Node boosting prioritizes structs/functions over tests
- Brace-aware source extraction captures complete function/struct bodies
- See-also traversal follows metadata breadcrumbs when results are sparse

## Adding a new language provider

Create `bin/guidance-<lang>` and ensure it accepts:

```
guidance-<lang> sync --file <path> --output <guidance_dir> [--infill]
guidance-<lang> sync --scan <dir>  --output <guidance_dir> [--infill]
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

Register the provider in `.guidance/guidance-config.json` under
`providers`.

## Configuration

`.guidance/guidance-config.json` controls:
- `model`: Local LLM model name (e.g., "llama3.2")
- `api_url`: Ollama/OpenAI-compatible endpoint
- `providers`: Language-specific AST providers
- `embedding_provider`: "ollama", "openai", or "none" (keyword-only)
- `embedding_model`: Model for vector embeddings (e.g., "nomic-embed-text")

`.guidance/semantic-aliases.json` defines query expansions:
```json
[
  {"key": "database", "values": ["GuidanceDb", "syncDatabase", "searchWithAliases"]},
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
license for your use case. This model ensures the software remains free and
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
use the GPLv3 license without open-sourcing your own codebase. You must
purchase a Commercial License if you meet any of the following criteria:

* You wish to embed this software in a proprietary, closed-source product.

* Your Legal Entity (including parent companies and affiliates) generates gross
annual revenue exceeding $1,000,000 USD.

* You require usage for more than one (1) developer seat.

* You require technical support, indemnification, or liability waivers.