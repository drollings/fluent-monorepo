# Capabilities Inbox

Major new features implemented. Run `make document` to promote into `guidance/capabilities/`.

<!-- Add new capabilities as bullets below:
- CozoDB backend: `src/cozo.zig` wraps the CozoDB C API (`cozo_c.h`) providing open/close/query/exec with RAII result handling. `src/db.zig` implements the unified Library (context_nodes, targets, depends_on, provides_capability, neighbor_of, wasm_tools), HydrationPipeline (in-Zig cosine KNN + edge persistence), and ContextPacker (LOD selection by graph distance). Schema DDL lives in `src/schema.zig` as CozoScript `:create` statements. CozoDB replaces both pgvector and LadybugDB/Cypher.
- Hybrid resolution: TraitSet bitmasks (u128) stored in CozoDB as Int hi/lo pairs; Datalog recursive rules traverse depends_on edges; final @popCount validation runs in Zig.

## 2026-03-18: Module Detail Comments with Keyword-Linked Embeddings

### Summary
Add a `detail` comment enrichment stage that generates comprehensive module documentation (<800 words) using the `thinking` model, extracts keywords using the `fast` model, and stores these in a new database schema that links detail comments to related embeddings via keyword matching.

### Model Selection Strategy

| Operation | Model Slot | Rationale |
|-----------|------------|-----------|
| Member comment infill | `fast` | Current behavior, fast iteration |
| Struct/container detail | `thinking` | Deep reasoning for comprehensive docs |
| Keyword extraction | `fast` | Quick extraction task |
| Module comment summary | `fast` | Summarization from detail |
| Explain synthesis | `default` | Balanced quality for user-facing output |
| LLM filter (stages) | `default` | Quality filtering for long queries |

### Database Schema Changes

```sql
-- New table: module detail comments (semantic text, not embeddings)
CREATE TABLE module_details (
  id            INTEGER PRIMARY KEY,
  file_path     TEXT    NOT NULL UNIQUE,
  module        TEXT    NOT NULL,
  detail        TEXT    NOT NULL,  -- <800 word detailed summary
  keywords      TEXT,               -- comma-separated keywords for discovery
  comment       TEXT,               -- concise module description (≤200 chars)
  last_modified INTEGER NOT NULL
);

CREATE INDEX idx_md_file ON module_details(file_path);
CREATE INDEX idx_md_module ON module_details(module);

-- New join table: detail comments ↔ related embeddings via keywords
CREATE TABLE detail_embedding_links (
  detail_id       INTEGER NOT NULL,
  ast_node_id     INTEGER NOT NULL,
  keyword_match   TEXT,    -- which keyword created this link
  relevance_score REAL,    -- match quality (1.0 for exact, 0.5 for fuzzy)
  PRIMARY KEY (detail_id, ast_node_id),
  FOREIGN KEY (detail_id) REFERENCES module_details(id),
  FOREIGN KEY (ast_node_id) REFERENCES ast_nodes(id)
);

CREATE INDEX idx_del_detail ON detail_embedding_links(detail_id);
CREATE INDEX idx_del_node ON detail_embedding_links(ast_node_id);
```

### Enhancement Pipeline (guidance gen)

1. **Parse source file** → extract members (current)
2. **Generate member comments** → `fast` model (current)
3. **NEW: For struct/container types**:
   - Use `thinking` model to generate `detail` comment
   - Input: source code, existing comments, related capabilities, skills
   - Output: <800 word comprehensive documentation
4. **NEW: Extract keywords** → `fast` model reviews detail
   - Output: 5-10 most valuable discovery keywords
5. **NEW: Summarize module comment** → `fast` model
   - Input: detail comment
   - Output: ≤200 char concise description
6. **NEW: Store in module_details table**
7. **NEW: Create keyword links**:
   - For each keyword, find matching ast_nodes
   - Insert into detail_embedding_links

### Explain Function Changes

1. **Search embeddings** (current behavior)
2. **NEW: Search module_details** for keyword matches
3. **NEW: Follow detail_embedding_links** to find related code
4. **Synthesize using `default` model**:
   - Input: module detail comments, code excerpts, prose stages
   - Output: accurate, concise reply to query

### Detail Comment Prompt Template

```
You are documenting a Zig module for an AI coding assistant.

SOURCE FILE: {file_path}
MODULE: {module_name}

SOURCE CODE:
{source_content}

EXISTING COMMENTS:
{existing_comments}

RELATED CAPABILITIES:
{capabilities}

RELATED SKILLS:
{skills}

Generate a comprehensive module documentation (<800 words) that:
1. Describes the module's purpose and architecture
2. Lists key abstractions and their relationships
3. Explains the public API and usage patterns
4. Notes important implementation details
5. Identifies design patterns used

Format as plain text (no markdown). Be technically precise.
```

### Keyword Extraction Prompt Template

```
You are extracting discovery keywords from module documentation.

DOCUMENTATION:
{detail_comment}

Extract 5-10 keywords that would help discover this module when searching.
Prioritize:
- Unique API names (structs, functions)
- Domain concepts
- Design patterns
- Technical terms

Output as comma-separated: keyword1, keyword2, keyword3
```

### Files to Modify

1. `src/guidance/config.zig` - Add `detailModel()` method
2. `src/guidance/enhancer.zig` - Add `enhanceDetail()` and `extractKeywords()`
3. `src/guidance/lance_db.zig` - Add schema migration, sync methods
4. `src/guidance/sync.zig` - Integrate detail generation
5. `src/guidance/staged.zig` - Include module_details in search
6. `src/guidance/main.zig` - Wire up new pipeline stages

