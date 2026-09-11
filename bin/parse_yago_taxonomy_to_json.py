#!/usr/bin/env python3
"""
parse_yago_taxonomy_to_json.py — convert pruned YaGO TTL to concise JSON.
DEPRECATED: use `cargo xtask yago-to-json` (Rust `common-core::normalize`); this shim warns and will be removed after v0.2.
"""
import warnings as _w; _w.warn("parse_yago_taxonomy_to_json.py deprecated — use cargo xtask yago-to-json", DeprecationWarning, stacklevel=2)

Parses a pruned YaGO taxonomy TTL (default: env/yago-taxonomy-n3.ttl) and
emits a JSON mapping each yago: class to its transitive ancestors.

Unlike a tree, multiple inheritance is preserved: ancestors is the full
transitive closure of rdfs:subClassOf (deduplicated, sorted).

This is the noun counterpart to ../../env/en_lemmatizer.json (which covers verbs):
  en_lemmatizer.json : surface form -> lemma  (verb morphology)
  yago-taxonomy.json : yago class -> ancestor classes (noun taxonomy)

Usage:
  python src/ontology/tools/parse_yago_taxonomy_to_json.py [input.ttl] [-o out.json]
  python src/ontology/tools/parse_yago_taxonomy_to_json.py --help
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

_PREFIX_RE = re.compile(r"^\s*@prefix\s+(\w+):\s+<([^>]+)>\s*\.\s*$")
_SUBCLASS_RE = re.compile(
    r"(?P<subj>(?:<[^>]+>|[A-Za-z_][\w\-]*:[^\s]+))\s+rdfs:subClassOf\s+(?P<obj>(?:<[^>]+>|[A-Za-z_][\w\-]*:[^\s,;.\]]+))"
)
# YaGO Wikidata suffix like _Q12345 — stripped and then lowercased to match
# lemmatized JSON style (env/en_lemmatizer.json keys are lowercased).
_Q_SUFFIX_RE = re.compile(r"_Q\d+$")
# Underscores in YaGO local names encode spaces (like lemmatized surface forms).
_UNDERSCORE_RE = re.compile(r"_")

DEFAULT_INPUT = Path(__file__).parents[3] / "env" / "yago-taxonomy-n3.ttl"


def parse_prefixes(line: str, prefixes: dict[str, str]) -> bool:
    m = _PREFIX_RE.match(line)
    if m:
        prefixes[m.group(1)] = m.group(2)
        return True
    return False


def expand_curie(curie: str, prefixes: dict[str, str]) -> str:
    curie = curie.strip().rstrip(".,;")
    if curie.startswith("<") and curie.endswith(">"):
        return curie[1:-1]
    if ":" in curie:
        pfx, local = curie.split(":", 1)
        local = local.rstrip(".,;")
        clean = f"{pfx}:{local}"
        pfx, local = clean.split(":", 1)
        if pfx in prefixes:
            return prefixes[pfx] + local
    return curie


def curie_for_iri(iri: str, prefixes: dict[str, str]) -> str:
    """Compress full IRI back to CURIE using longest prefix match for readability."""
    # Prefer yago:/schema: prefixes if available
    best = None
    best_len = -1
    for pfx, ns in prefixes.items():
        if iri.startswith(ns) and len(ns) > best_len:
            best = (pfx, ns)
            best_len = len(ns)
    if best:
        pfx, ns = best
        return f"{pfx}:{iri[len(ns):]}"
    return f"<{iri}>"


def normalize_curie(curie: str) -> str:
    """Strip Wikidata _Q123 suffix via regex, lowercase and turn underscores to spaces.

    Uses regex for both steps to mirror lemmatized JSON (env/en_lemmatizer.json
    keys are lowercased surface forms with spaces).

    e.g. yago:Adult_Video_Game_Q3362070 -> yago:adult video game
         yago:Remix__U0028_Work_U0029__Q113171270 -> yago:remix  u0028 work u0029  (underscores→spaces)
    Prefix is preserved lowercased; full IRIs (<...>) are lowercased as well.
    """
    if curie.startswith("<") and curie.endswith(">"):
        inner = curie[1:-1]
        inner = _Q_SUFFIX_RE.sub("", inner)
        inner = inner.lower()
        inner = _UNDERSCORE_RE.sub(" ", inner).strip()
        return f"<{inner}>"
    if ":" in curie:
        pfx, local = curie.split(":", 1)
        local = _Q_SUFFIX_RE.sub("", local)
        local = local.lower()
        local = _UNDERSCORE_RE.sub(" ", local).strip()
        return f"{pfx.lower()}:{local}"
    # bare
    bare = _Q_SUFFIX_RE.sub("", curie)
    bare = bare.lower()
    return _UNDERSCORE_RE.sub(" ", bare).strip()


def collect_edges(path: Path, prefixes: dict[str, str]) -> list[tuple[str, str]]:
    edges: list[tuple[str, str]] = []
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if parse_prefixes(line, prefixes):
                continue
            if "rdfs:subClassOf" not in line and "subClassOf" not in line:
                continue
            for m in _SUBCLASS_RE.finditer(line):
                subj = expand_curie(m.group("subj"), prefixes)
                obj = expand_curie(m.group("obj"), prefixes)
                edges.append((subj, obj))
    # Second pass: re-expand any edges that were seen before prefix definition
    # (prefixes are at top in YaGO, so rare — we already expanded with
    # incremental prefixes, but to be safe re-parse if needed is omitted)
    return edges


def transitive_ancestors(
    direct: dict[str, list[str]]
) -> dict[str, list[str]]:
    """Compute transitive closure of subClassOf for every node.

    direct: subject -> [direct parents]
    Returns: subject -> sorted list of all ancestors (transitive, deduplicated).
    Cycle-safe via visited set and memoization.
    """
    memo: dict[str, set[str]] = {}

    def dfs(node: str, stack: set[str]) -> set[str]:
        if node in memo:
            return memo[node]
        if node in stack:
            # cycle — break
            return set()
        stack.add(node)
        result: set[str] = set()
        for parent in direct.get(node, []):
            result.add(parent)
            result.update(dfs(parent, stack))
        stack.remove(node)
        memo[node] = result
        return result

    all_nodes = set(direct.keys()) | {p for parents in direct.values() for p in parents}
    out: dict[str, list[str]] = {}
    for node in all_nodes:
        anc = dfs(node, set())
        # exclude self if cycle introduced it
        anc.discard(node)
        out[node] = sorted(anc)
    return out


def build_json(
    edges: list[tuple[str, str]],
    prefixes: dict[str, str],
    yago_only: bool = True,
    use_curie: bool = True,
) -> dict:
    direct: dict[str, list[str]] = defaultdict(list)
    for s, o in edges:
        if o not in direct[s]:
            direct[s].append(o)
    # sort direct parents for determinism
    for k in direct:
        direct[k] = sorted(set(direct[k]))

    trans = transitive_ancestors(direct)

    # Build output: only yago: classes as keys (noun taxonomy)
    yago_ns = prefixes.get("yago", "http://yago-knowledge.org/resource/")
    # Merge by normalized key (stripping _Q\d+ and lowercasing) — like lemmatized JSON
    merged: dict[str, dict[str, set[str]]] = {}
    for subj, ancestors in trans.items():
        is_yago = subj.startswith(yago_ns) or subj.startswith("http://yago-knowledge.org/resource/")
        if yago_only and not is_yago:
            continue
        if not ancestors and yago_only and not is_yago:
            continue
        raw_key = curie_for_iri(subj, prefixes) if use_curie else subj
        key = normalize_curie(raw_key)

        if use_curie:
            anc_list = [normalize_curie(curie_for_iri(a, prefixes)) for a in ancestors]
        else:
            anc_list = [normalize_curie(a) for a in ancestors]
        dparents_raw = direct.get(subj, [])
        if use_curie:
            dparents = [normalize_curie(curie_for_iri(a, prefixes)) for a in dparents_raw]
        else:
            dparents = [normalize_curie(a) for a in dparents_raw]

        entry = merged.setdefault(key, {"parents": set(), "ancestors": set()})
        entry["parents"].update(dparents)
        entry["ancestors"].update(anc_list)
        # remove self-reference after normalization (e.g. Q-suffix collapse)
        entry["parents"].discard(key)
        entry["ancestors"].discard(key)

    result: dict[str, dict] = {}
    for k in sorted(merged):
        result[k] = {
            "parents": sorted(merged[k]["parents"]),
            "ancestors": sorted(merged[k]["ancestors"]),
        }
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "input",
        nargs="?",
        default=str(DEFAULT_INPUT),
        help="Input TTL file (default: env/yago-taxonomy-n3.ttl)",
    )
    parser.add_argument("-o", "--output", default=None, help="Output JSON file (default: stdout)")
    parser.add_argument(
        "--include-all",
        action="store_true",
        help="Include all classes (not just yago:). Default: yago: only.",
    )
    parser.add_argument(
        "--full-iri",
        action="store_true",
        help="Emit full IRIs instead of CURIEs.",
    )
    parser.add_argument(
        "--flat",
        action="store_true",
        help="Emit flat mapping class -> ancestors (list) instead of {parents, ancestors} objects.",
    )
    args = parser.parse_args()

    inp = Path(args.input)
    if not inp.exists():
        parser.error(f"input not found: {inp}")

    prefixes: dict[str, str] = {}
    edges = collect_edges(inp, prefixes)
    data = build_json(
        edges,
        prefixes,
        yago_only=not args.include_all,
        use_curie=not args.full_iri,
    )

    if args.flat:
        flat = {k: v["ancestors"] for k, v in data.items()}
        output = flat
    else:
        output = data

    out_text = json.dumps(output, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    if args.output:
        Path(args.output).write_text(out_text, encoding="utf-8")
    else:
        sys.stdout.write(out_text)


if __name__ == "__main__":
    main()


# ---------------------------------------------------------------------------
# pytest suite
# ---------------------------------------------------------------------------

def _write_tmp_ttl(tmp_path, content: str) -> Path:
    p = tmp_path / "in.ttl"
    p.write_text(content, encoding="utf-8")
    return p


def test_parses_simple_hierarchy(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix schema: <http://schema.org/> .
yago:City rdfs:subClassOf yago:Place .
yago:Place rdfs:subClassOf yago:Entity .
yago:Entity rdfs:subClassOf rdfs:Class .
"""
    p = _write_tmp_ttl(tmp_path, ttl)
    prefixes: dict[str, str] = {}
    edges = collect_edges(p, prefixes)
    data = build_json(edges, prefixes, yago_only=True, use_curie=True)
    assert "yago:city" in data
    assert data["yago:city"]["parents"] == ["yago:place"]
    assert "yago:place" in data["yago:city"]["ancestors"]
    assert "yago:entity" in data["yago:city"]["ancestors"]


def test_multiple_inheritance(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:C rdfs:subClassOf yago:A .
yago:C rdfs:subClassOf yago:B .
yago:A rdfs:subClassOf yago:Root .
yago:B rdfs:subClassOf yago:Root .
"""
    p = _write_tmp_ttl(tmp_path, ttl)
    prefixes: dict[str, str] = {}
    edges = collect_edges(p, prefixes)
    data = build_json(edges, prefixes, yago_only=True, use_curie=True)
    assert set(data["yago:c"]["parents"]) == {"yago:a", "yago:b"}
    assert set(data["yago:c"]["ancestors"]) == {"yago:a", "yago:b", "yago:root"}


def test_yago_only_filter(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix schema: <http://schema.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
schema:City rdfs:subClassOf schema:Place .
yago:City rdfs:subClassOf yago:Place .
"""
    p = _write_tmp_ttl(tmp_path, ttl)
    prefixes: dict[str, str] = {}
    edges = collect_edges(p, prefixes)
    yago_only = build_json(edges, prefixes, yago_only=True, use_curie=True)
    assert "yago:city" in yago_only
    assert "schema:city" not in yago_only
    all_data = build_json(edges, prefixes, yago_only=False, use_curie=True)
    assert "schema:city" in all_data


def test_q_suffix_stripped_and_lowercased(tmp_path):
    ttl = """@prefix yago: <http://yago-knowledge.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
yago:Adult_Video_Game_Q3362070 rdfs:subClassOf yago:Video_game .
yago:Video_game rdfs:subClassOf yago:Software .
yago:Remix__U0028_Work_U0029__Q113171270 rdfs:subClassOf yago:CreativeWork .
"""
    p = _write_tmp_ttl(tmp_path, ttl)
    prefixes: dict[str, str] = {}
    edges = collect_edges(p, prefixes)
    data = build_json(edges, prefixes, yago_only=True, use_curie=True)
    # Q suffix stripped via regex, lowercased and underscores→spaces like en_lemmatizer.json
    assert "yago:adult video game" in data
    assert "yago:adult_video_game_q3362070" not in data
    assert "yago:adult video game_q3362070" not in data
    assert data["yago:adult video game"]["parents"] == ["yago:video game"]
    assert "yago:remix  u0028 work u0029" in data
    # ancestors also lowercased / stripped / underscores→spaces
    assert "yago:software" in data["yago:adult video game"]["ancestors"]


def test_normalize_curie_regex():
    assert normalize_curie("yago:City_Q515") == "yago:city"
    assert normalize_curie("yago:City") == "yago:city"
    assert normalize_curie("yago:Adult_Video_Game_Q3362070") == "yago:adult video game"
    assert normalize_curie("<http://yago-knowledge.org/resource/City_Q515>") == "<http://yago-knowledge.org/resource/city>"
    # underscores → spaces via regex
    assert normalize_curie("yago:Hello_World") == "yago:hello world"
    assert normalize_curie("yago:Video_game") == "yago:video game"
    # must use regex _Q\\d+$ — no strip without underscore
    assert normalize_curie("yago:AQ123") == "yago:aq123"
