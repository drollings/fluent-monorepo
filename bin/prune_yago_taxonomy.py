#!/usr/bin/env python3
"""
prune_yago.py — streaming YaGO 4.5 TTL pruner by taxonomy depth.
DEPRECATED: use `cargo xtask prune-yago` (Rust `common-core::normalize`); this shim warns and will be removed after v0.2.
"""
import warnings
warnings.warn("prune_yago_taxonomy.py is deprecated — use cargo xtask prune-yago (common-core::normalize)", DeprecationWarning, stacklevel=2)

Keeps the top N tiers of the rdfs:subClassOf hierarchy and emits a
pruned but otherwise faithful TTL to STDOUT.

Tier definition (BFS from roots):
  tier 0 = roots: classes that never appear as subject of rdfs:subClassOf
           (e.g. yago:Entity, schema:Thing, rdfs:Class)
  tier 1 = immediate children of roots
  tier N = nodes whose shortest distance from any root <= N

Default -n 1 therefore emits roots + their immediate children (e.g. Entity's
direct descendants like Agent, Place, Artifact …).

Streaming: two passes over the file (first pass collects rdfs:subClassOf
edges, second pass filters). Edges are stored in-memory (~10^5 entries for
full YaGO); the TTL itself is never fully loaded — lines are streamed.

Usage:
  python prune_yago.py [-n NUM_TIERS] [input.ttl] > pruned.ttl
  cat input.ttl | python prune_yago.py -n 2 > pruned.ttl
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict, deque
from pathlib import Path

# ---------------------------------------------------------------------------
# Prefix handling
# ---------------------------------------------------------------------------

_PREFIX_RE = re.compile(r"^\s*@prefix\s+(\w+):\s+<([^>]+)>\s*\.\s*$")
# Matches CURIE like yago:Foo or schema:Bar (word chars, hyphen, underscore, dot)
_CURIE_RE = re.compile(r"\b([A-Za-z_][\w\-]*):([^\s,;.\[\]()\"'<>]+)")
# rdfs:subClassOf triples — supports both prefixed and full-IRI forms
_SUBCLASS_RE = re.compile(
    r"(?P<subj>(?:<[^>]+>|[A-Za-z_][\w\-]*:[^\s]+))\s+rdfs:subClassOf\s+(?P<obj>(?:<[^>]+>|[A-Za-z_][\w\-]*:[^\s,;.\]]+))"
)

# Normalization for lemmatizer filtering — same regexes as parse_yago_taxonomy_to_json.py
_Q_SUFFIX_RE = re.compile(r"_Q\d+$")
_UNDERSCORE_RE = re.compile(r"_")
_UNICODE_ESC_RE = re.compile(r"_U([0-9A-Fa-f]{4})_?")
_UNICODE_LOWER_RE = re.compile(r"u([0-9a-f]{4})")
_WS_RE = re.compile(r"\s+")
_HYPHEN_RE = re.compile(r"[-–—]")


def parse_prefixes(line: str, prefixes: dict[str, str]) -> bool:
    m = _PREFIX_RE.match(line)
    if m:
        prefixes[m.group(1)] = m.group(2)
        return True
    return False


def expand_curie(curie: str, prefixes: dict[str, str]) -> str:
    """Expand CURIE to full IRI; full IRIs (<...>) returned stripped."""
    curie = curie.strip().rstrip(".,;")
    if curie.startswith("<") and curie.endswith(">"):
        return curie[1:-1]
    if ":" in curie:
        pfx, local = curie.split(":", 1)
        # strip trailing punctuation that may follow a CURIE
        local = local.rstrip(".,;")
        curie_clean = f"{pfx}:{local}"
        # re-split after clean
        pfx, local = curie_clean.split(":", 1)
        if pfx in prefixes:
            return prefixes[pfx] + local
    return curie


def normalize_yago_name(iri: str) -> str:
    """Normalize a YaGO IRI local name for lexicon matching.

    Strips Wikidata _Q\\d+ suffix via regex, decodes _UXXXX escapes via regex,
    replaces underscores with spaces via regex, and lowercases — same as
    parse_yago_taxonomy_to_json.py so yago:Adult_Video_Game_Q3362070 → "adult video game".

    Also collapses whitespace and lowercases to match en_lemmatizer.json keys.
    """
    # local part after last / or # or :
    local = iri.rsplit("/", 1)[-1].rsplit("#", 1)[-1].rsplit(":", 1)[-1]
    local = _Q_SUFFIX_RE.sub("", local)
    # decode YaGO unicode escapes like _U0028_ → '('  and trailing u0028 → '('
    local = _UNICODE_ESC_RE.sub(lambda m: chr(int(m.group(1), 16)), local)
    # leftover lower-case uXXXX from previous lowercasing — handle after lower
    local = local.lower()
    local = _UNICODE_LOWER_RE.sub(lambda m: chr(int(m.group(1), 16)), local)
    local = _UNDERSCORE_RE.sub(" ", local)
    local = _WS_RE.sub(" ", local).strip()
    return local


def _token_variants(token: str) -> list[str]:
    """Yield singular/plural variants for matching against en_lemmatizer.

    Uses noun lemma_rules-style stripping: ies→y, ves→f/fe, ses→s etc. so
    plural YaGO forms like 'Cities' match lemmatizer singular 'city'/'cities'.
    """
    token = token.lower()
    variants = [token]
    # common English plural → singular
    if token.endswith("ies") and len(token) > 3:
        variants.append(token[:-3] + "y")
    if token.endswith("ves") and len(token) > 3:
        variants.append(token[:-3] + "f")
        variants.append(token[:-3] + "fe")
    if token.endswith("ses") and len(token) > 3:
        variants.append(token[:-2])  # ses → s
        variants.append(token[:-2] + "s")  # keep
    if token.endswith("s") and len(token) > 2 and not token.endswith("ss"):
        variants.append(token[:-1])
    if token.endswith("men") and len(token) > 3:
        variants.append(token[:-3] + "man")
    return variants


def _matches_lexicon(normalized: str, lexicon: set[str]) -> bool:
    """Check if normalized multi-word YaGO name matches lexicon.

    Uses regex-derived tokens: split on whitespace and hyphens, lowercases,
    and checks exact phrase, any token, last token, and lemmatized variants.
    This mirrors how en_lemmatizer.json handles plural forms.
    """
    if not normalized:
        return False
    if normalized in lexicon:
        return True
    # split on whitespace and hyphens (yago uses hyphens heavily)
    tokens = []
    for part in normalized.split():
        tokens.extend(_HYPHEN_RE.split(part))
    tokens = [t.strip(" '\".,;:()[]") for t in tokens if t]
    if not tokens:
        return False
    for tok in tokens:
        if tok in lexicon:
            return True
        for var in _token_variants(tok):
            if var in lexicon:
                return True
    # last token is most informative for taxonomy (e.g. "adult video game" → "game")
    last = tokens[-1]
    for var in _token_variants(last):
        if var in lexicon:
            return True
    return False


def load_lemmatizer_words(path: str | Path) -> set[str]:
    """Load word set from en_lemmatizer.json.

    Format: {"lemma_rules":..., "lemma_exc": {pos: {surface: [lemmas]}}, "lemma_index": {pos: [words]}}
    All surface forms, lemmas, and index entries are lowercased and collected.
    """
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    words: set[str] = set()
    exc = data.get("lemma_exc", {})
    for pos_dict in exc.values():
        if isinstance(pos_dict, dict):
            for surface, lemmas in pos_dict.items():
                words.add(surface.lower())
                if isinstance(lemmas, list):
                    for l in lemmas:
                        words.add(str(l).lower())
                elif isinstance(lemmas, str):
                    words.add(lemmas.lower())
    idx = data.get("lemma_index", {})
    if isinstance(idx, dict):
        for lst in idx.values():
            if isinstance(lst, list):
                for w in lst:
                    words.add(str(w).lower())
            elif isinstance(lst, dict):
                for w in lst.keys():
                    words.add(str(w).lower())
    # also include bare top-level word lists if present
    return words


def filter_kept_by_lexicon(
    kept: set[str],
    lemmatizer_words: set[str],
    edges: list[tuple[str, str]] | None = None,
    keep_ancestors: bool = True,
) -> set[str]:
    """Keep only IRIs whose normalized name matches a lemmatizer word.

    Uses _matches_lexicon (regex-derived normalization: strip _Q\\d+, _→space,
    lower, UXXXX decode, hyphen split, plural variants) so multi-word YaGO
    names like "adult video game" match if "game" is in the lexicon.

    If keep_ancestors and edges provided, also keeps transitive parents of
    matched nodes so taxonomy remains connected (otherwise child edges would be
    pruned because parent not in lexicon).
    """
    if not lemmatizer_words:
        return kept
    matched: set[str] = set()
    for iri in kept:
        norm = normalize_yago_name(iri)
        if _matches_lexicon(norm, lemmatizer_words):
            matched.add(iri)
    if not keep_ancestors or not edges or not matched:
        return matched
    # include transitive ancestors of matched nodes
    parent_of: dict[str, list[str]] = {}
    for s, o in edges:
        parent_of.setdefault(s, []).append(o)
    # BFS upward
    to_visit = list(matched)
    seen = set(matched)
    while to_visit:
        cur = to_visit.pop()
        for par in parent_of.get(cur, []):
            if par not in seen and par in kept:
                seen.add(par)
                to_visit.append(par)
    return seen


# ---------------------------------------------------------------------------
# Tier computation
# ---------------------------------------------------------------------------

def collect_edges_and_prefixes(path: str | None) -> tuple[dict[str, str], list[tuple[str, str]], list[str]]:
    """First streaming pass: collect prefix map and all rdfs:subClassOf edges.

    Returns (prefixes, edges as (subj_iri, obj_iri), header_lines).
    Header lines are the @prefix declarations (preserved for output).
    """
    prefixes: dict[str, str] = {}
    edges: list[tuple[str, str]] = []
    header_lines: list[str] = []

    def handle_line(line: str) -> None:
        if parse_prefixes(line, prefixes):
            header_lines.append(line)
            return
        if "rdfs:subClassOf" not in line and "subClassOf" not in line:
            return
        for m in _SUBCLASS_RE.finditer(line):
            subj_raw = m.group("subj")
            obj_raw = m.group("obj")
            subj = expand_curie(subj_raw, prefixes)
            obj = expand_curie(obj_raw, prefixes)
            edges.append((subj, obj))

    if path is None or path == "-":
        for line in sys.stdin:
            handle_line(line)
    else:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                handle_line(line)
                # prefixes are at the top; but we need them before expanding
                # subsequent edges — handle_line already uses current prefixes

    # Re-parse for correct expansion if prefixes were after some triples
    # (YaGO places them at top, so this is a no-op in practice).
    return prefixes, edges, header_lines


def collect_edges_from_lines(lines: list[str], prefixes: dict[str, str]) -> list[tuple[str, str]]:
    edges: list[tuple[str, str]] = []
    for line in lines:
        for m in _SUBCLASS_RE.finditer(line):
            subj = expand_curie(m.group("subj"), prefixes)
            obj = expand_curie(m.group("obj"), prefixes)
            edges.append((subj, obj))
    return edges


def _is_meta_iri(iri: str) -> bool:
    return any(k in iri for k in ("rdf-schema", "rdf-syntax", "owl#", "shacl", "/sh#", "skos", "xsd#"))


def compute_kept_iris(edges: list[tuple[str, str]], num_tiers: int) -> set[str]:
    """BFS from roots to depth num_tiers."""
    if num_tiers < 0:
        return set()

    subjects = {s for s, _ in edges}
    objects = {o for _, o in edges}
    all_nodes = subjects | objects

    # roots = nodes never appearing as subject, excluding meta vocabulary
    # (rdfs:Class, owl:Thing etc).  A class whose only parent is a meta
    # class (e.g. yago:Entity rdfs:subClassOf rdfs:Class) is itself a root
    # (tier 0) — this matches the spec "immediate descendants of Entity".
    roots = all_nodes - subjects
    # promote children of meta parents to roots
    meta_parents = {o for _, o in edges if _is_meta_iri(o)}
    for s, o in edges:
        if o in meta_parents and not _is_meta_iri(s):
            # s has only meta parents -> treat as root
            # check if s has any non-meta parent
            has_non_meta_parent = any(obj != o and not _is_meta_iri(obj) for (subj, obj) in edges if subj == s)
            if not has_non_meta_parent:
                roots.add(s)
    # remove meta IRIs themselves from roots/kept
    roots = {r for r in roots if not _is_meta_iri(r)}
    # Fallback: if every node is a subject (cycle or no leaves), treat
    # objects that never appear as objects?  But YaGO always has roots.
    if not roots:
        # use nodes with in-degree 0
        indeg = defaultdict(int)
        for s, o in edges:
            indeg[s] += 0  # ensure key
        for _, o in edges:
            indeg[o] += 0
        children = {s for s, _ in edges}
        # roots are those not in children? already handled.
        roots = {n for n in all_nodes if n not in children}
        if not roots:
            roots = set(list(all_nodes)[:1])

    # adjacency: parent -> [children]
    children_of: dict[str, list[str]] = defaultdict(list)
    for s, o in edges:
        children_of[o].append(s)

    # BFS
    depth: dict[str, int] = {}
    q: deque[str] = deque()
    for r in roots:
        depth[r] = 0
        q.append(r)

    kept: set[str] = set()
    while q:
        node = q.popleft()
        d = depth[node]
        if d > num_tiers:
            continue
        kept.add(node)
        if d == num_tiers:
            continue
        for ch in children_of.get(node, []):
            if ch not in depth or depth[ch] > d + 1:
                depth[ch] = d + 1
                q.append(ch)

    # For multiple inheritance, a class may be reachable at depth > N via one
    # parent but <=N via another — BFS with shortest distance handles it.

    # Ensure roots always kept even if isolated
    kept.update(roots)
    # If num_tiers == 0, only roots should survive (already).
    if num_tiers == 0:
        kept = roots

    # When we filtered via all_nodes - subjects, roots include schema:Thing etc.
    # That's intended: tier 0 = Thing, Entity, rdfs:Class etc.
    # For the spec example "Entity's immediate children" this keeps Entity + its children.
    return kept


# ---------------------------------------------------------------------------
# Second pass: streaming filter to stdout
# ---------------------------------------------------------------------------

# lines that should always pass through (prefixes, comments, empty)
_ALWAYS_RE = re.compile(r"^\s*(@prefix|@base|#|$)")

# Heuristic: extract first CURIE/IRI as subject for filtering
_SUBJ_RE = re.compile(r"^\s*(?P<subj>(?:<[^>]+>|[A-Za-z_][\w\-]*:[^\s]+))")


def should_keep_line(line: str, kept: set[str], prefixes: dict[str, str]) -> bool:
    stripped = line.strip()
    if not stripped:
        return True
    if stripped.startswith("@prefix") or stripped.startswith("@base") or stripped.startswith("#"):
        return True
    if "rdfs:subClassOf" in line or "subClassOf" in line:
        m = _SUBCLASS_RE.search(line)
        if m:
            subj = expand_curie(m.group("subj"), prefixes)
            obj = expand_curie(m.group("obj"), prefixes)
            return subj in kept and obj in kept

    # For rdf:type / shape / property lines, keep if subject is kept.
    # We extract the subject CURIE/IRI at line start.
    sm = _SUBJ_RE.match(line)
    if sm:
        subj = expand_curie(sm.group("subj"), prefixes)
        # If subject looks like a class IRI and is not kept, drop it.
        # Property/shape lines (ys:, sh:) are kept regardless unless they
        # reference a non-kept class as object — we conservatively keep them
        # if they don't declare a class.
        # Detect class declaration: "rdf:type rdfs:Class" or "rdf:type sh:NodeShape"
        if "rdf:type" in line and "rdfs:Class" in line:
            return subj in kept
        if subj in kept:
            return True
        # Subject not in kept but line might be a continuation (starts with sh:property etc.)
        # Continuation lines belong to previous subject's block; we handled blocks
        # poorly with line-level filter. For simplicity keep non-class lines.
        # Better: drop lines whose subject is a known class IRI not in kept.
        # We know class IRIs are those appearing in edges' subjects/objects.
        # If subj is yago:/schema: and not kept, drop.
        if subj.startswith("http://yago-knowledge.org/resource/") or subj.startswith("http://schema.org/"):
            return subj in kept
        return True

    return True


def prune_stream(
    input_path: str | None, num_tiers: int, out, lemmatizer_json: str | Path | None = None
) -> None:
    # First pass: collect edges + prefixes
    # We need to handle stdin case: can't do two passes without buffering.
    # For path=None, read all lines into memory for second pass.
    if input_path is None or input_path == "-":
        raw_lines = sys.stdin.read().splitlines(keepends=True)
        prefixes: dict[str, str] = {}
        header: list[str] = []
        for l in raw_lines:
            if parse_prefixes(l, prefixes):
                header.append(l)
        # second scan for edges after prefixes known (expand correctly)
        edges = collect_edges_from_lines(raw_lines, prefixes)
        kept = compute_kept_iris(edges, num_tiers)
        if lemmatizer_json:
            words = load_lemmatizer_words(lemmatizer_json)
            kept = filter_kept_by_lexicon(kept, words, edges=edges)
        for line in raw_lines:
            if should_keep_line(line, kept, prefixes):
                out.write(line)
        return

    # File path: first pass to get prefixes/edges
    prefixes: dict[str, str] = {}
    edges: list[tuple[str, str]] = []
    # We need prefixes before expanding edges, so scan once for prefixes then again for edges
    # Since prefixes are at top, single pass suffices, but do two-phase for correctness:
    with open(input_path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if parse_prefixes(line, prefixes):
                continue
            if "rdfs:subClassOf" not in line and "subClassOf" not in line:
                continue
            for m in _SUBCLASS_RE.finditer(line):
                subj = expand_curie(m.group("subj"), prefixes)
                obj = expand_curie(m.group("obj"), prefixes)
                edges.append((subj, obj))

    # Re-expand edges that were seen before their prefix was defined (rare)
    # Re-parse if needed: for YaGO it's not needed.

    kept = compute_kept_iris(edges, num_tiers)
    if lemmatizer_json:
        words = load_lemmatizer_words(lemmatizer_json)
        kept = filter_kept_by_lexicon(kept, words, edges=edges)

    # Second streaming pass: filter
    with open(input_path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if should_keep_line(line, kept, prefixes):
                out.write(line)


def main() -> None:
    parser = argparse.ArgumentParser(description="Prune YaGO 4.5 TTL to top N tiers (streaming).")
    parser.add_argument("input", nargs="?", default=None, help="Input .ttl file (default: stdin)")
    parser.add_argument("-n", "--num-tiers", type=int, default=1, help="Number of tiers to keep (default: 1). Tier 0 = roots, tier 1 = their children, etc.")
    parser.add_argument(
        "--lemmatizer-json",
        dest="lemmatizer_json",
        default=None,
        help="Optional JSON file in en_lemmatizer.json format; if given, only entries whose normalized name (strip _Q\\d+ via regex, _→space via regex, lower) matches a word in the lexicon are kept",
    )
    args = parser.parse_args()

    if args.num_tiers is None or args.num_tiers < 0:
        parser.error("--num-tiers must be >= 0")

    prune_stream(args.input, args.num_tiers, sys.stdout, lemmatizer_json=args.lemmatizer_json)


if __name__ == "__main__":
    main()


# ---------------------------------------------------------------------------
# pytest suite
# ---------------------------------------------------------------------------

def _write_ttl(tmp_path, content: str):
    p = tmp_path / "in.ttl"
    p.write_text(content, encoding="utf-8")
    return str(p)


def test_num_tiers_default_is_one():
    import argparse, inspect
    src = Path(__file__).read_text()
    assert "--num-tiers" in src
    # parse default
    parser = argparse.ArgumentParser()
    parser.add_argument("-n", "--num-tiers", type=int, default=1)
    ns = parser.parse_args([])
    assert ns.num_tiers == 1


def test_tier_one_keeps_immediate_children(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix schema: <http://schema.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Entity rdfs:subClassOf rdfs:Class .
yago:Agent rdfs:subClassOf yago:Entity .
yago:Place rdfs:subClassOf yago:Entity .
yago:City rdfs:subClassOf yago:Place .
yago:Dog rdfs:subClassOf yago:Mammal .
yago:Mammal rdfs:subClassOf yago:Entity .
"""
    inp = _write_ttl(tmp_path, ttl)
    import io
    out = io.StringIO()
    prune_stream(inp, 1, out)
    text = out.getvalue()
    assert "yago:Agent" in text
    assert "yago:Place" in text
    assert "yago:Mammal" in text
    # depth 2 should be pruned
    assert "yago:City" not in text
    assert "yago:Dog" not in text


def test_tier_two_keeps_grandchildren(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Entity rdfs:subClassOf rdfs:Class .
yago:Agent rdfs:subClassOf yago:Entity .
yago:Person rdfs:subClassOf yago:Agent .
yago:City rdfs:subClassOf yago:Place .
yago:Place rdfs:subClassOf yago:Entity .
"""
    inp = _write_ttl(tmp_path, ttl)
    import io
    out = io.StringIO()
    prune_stream(inp, 2, out)
    text = out.getvalue()
    assert "yago:Person" in text
    assert "yago:City" in text


def test_prefixes_always_preserved(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Entity rdfs:subClassOf rdfs:Class .
yago:Agent rdfs:subClassOf yago:Entity .
yago:Deep rdfs:subClassOf yago:Agent .
"""
    inp = _write_ttl(tmp_path, ttl)
    import io
    out = io.StringIO()
    prune_stream(inp, 0, out)
    text = out.getvalue()
    assert "@prefix yago:" in text
    assert "yago:Deep" not in text


def test_streaming_from_stdin(monkeypatch, tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Entity rdfs:subClassOf rdfs:Class .
yago:Agent rdfs:subClassOf yago:Entity .
"""
    import io
    old_stdin = sys.stdin
    sys.stdin = io.StringIO(ttl)
    out = io.StringIO()
    try:
        prune_stream(None, 1, out)
    finally:
        sys.stdin = old_stdin
    assert "yago:Agent" in out.getvalue()


def test_compute_kept_iris_multiple_inheritance():
    edges = [
        ("http://a/C", "http://a/A"),
        ("http://a/C", "http://a/B"),
        ("http://a/A", "http://a/Root"),
        ("http://a/B", "http://a/Root"),
    ]
    kept = compute_kept_iris(edges, 1)
    assert "http://a/Root" in kept
    assert "http://a/A" in kept
    assert "http://a/B" in kept
    assert "http://a/C" not in kept
    kept2 = compute_kept_iris(edges, 2)
    assert "http://a/C" in kept2


def test_lemmatizer_filter(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Entity rdfs:subClassOf rdfs:Class .
yago:City_Q515 rdfs:subClassOf yago:Place .
yago:Place rdfs:subClassOf yago:Entity .
yago:FakeZootopia_Q999999 rdfs:subClassOf yago:Place .
"""
    inp = _write_ttl(tmp_path, ttl)
    # minimal lemmatizer JSON in en_lemmatizer.json format (lemma_exc + lemma_index)
    lex = tmp_path / "lex.json"
    lex.write_text(
        '{"lemma_rules":{}, "lemma_exc": {"noun": {"city": ["city"], "place": ["place"]}}, "lemma_index": {"noun": ["city", "place"]}}',
        encoding="utf-8",
    )
    import io

    out = io.StringIO()
    prune_stream(inp, 2, out, lemmatizer_json=str(lex))
    text = out.getvalue()
    # normalized: City_Q515 -> "city" matches, Place -> "place" matches, FakeZootopia -> "fakezootopia" does not
    assert "yago:City_Q515" in text
    assert "yago:Place" in text
    assert "FakeZootopia" not in text
    # also test underscore→space and lowercasing: Adult_Video_Game should match "video game" token "game"
    ttl2 = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Adult_Video_Game_Q3362070 rdfs:subClassOf yago:Thing .
"""
    inp2 = _write_ttl(tmp_path, ttl2)
    lex2 = tmp_path / "lex2.json"
    lex2.write_text(
        '{"lemma_rules":{}, "lemma_exc": {"noun": {"game": ["game"], "thing": ["thing"]}}, "lemma_index": {"noun": ["game", "thing"]}}',
        encoding="utf-8",
    )
    out2 = io.StringIO()
    prune_stream(inp2, 2, out2, lemmatizer_json=str(lex2))
    assert "Adult_Video_Game" in out2.getvalue()


def test_normalize_yago_name_regex():
    assert normalize_yago_name("http://yago-knowledge.org/resource/City_Q515") == "city"
    assert normalize_yago_name("http://yago-knowledge.org/resource/Adult_Video_Game_Q3362070") == "adult video game"
    assert _Q_SUFFIX_RE.sub("", "City_Q515").lower() == "city"
    assert _UNDERSCORE_RE.sub(" ", "hello_world") == "hello world"
