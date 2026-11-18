#!/usr/bin/env bash
# vector-check — HNSW/error-taxonomy consolidation lint (roadmap M8).
#
# Fails if any deleted re-export shim or second threshold/error source
# regrows anywhere under `src/`, or the raw HNSW graph types escape their
# canonical home:
#   0. `hnsw_rs::` / `DistCosine` outside `search-vector/src/hnsw.rs`.
#   1. `search_vector::math` / `search_vector::error` code paths
#      (prose uses the hyphenated "search-vector" form, which never matches).
#   2. `crate::hnsw::` paths (the deleted `fluent_router::hnsw` shim).
#   3. Fresh `1.0 - d` distance→similarity closures outside the canonical
#      kernel (`fluent_db::vector::{distance_to_similarity[_clamped],
#      scored_hits}` in `src/db/src/vector.rs`, the one allowed home).
#   4. The deleted shim files themselves.
#   5. Threshold/vocabulary definitions outside their two allowed homes
#      (canonical `search-vector/src/hnsw.rs` + deprecated
#      `common-core/src/constants.rs` shim; `AdaptiveHnsw` has no shim).
#   6. A second `From<rusqlite::Error>` impl (single error taxonomy in
#      `src/db/src/error.rs`).
#   7. New uses of the deprecated `common_core::constants::` HNSW path
#      outside the grandfathered files (new code imports
#      `search_vector::hnsw` directly).
#
# Usage:  bin/vector-check.sh [root]   # exit 0 clean, non-zero on violation
#   `root` overrides the tree to scan (default: repo root) so the checks can
#   be red-teamed against fixture trees without touching the repo.
#
# Wired into `make lint-vector` and CI (it is cheap, fast, and needs no
# compilation — same shape as `bin/live-ai-guard.sh`).

set -uo pipefail

ROOT="${1:-$(dirname "$0")/..}"
cd "$ROOT" || exit 1

FAIL=0
fail() { echo "FAIL: $1"; FAIL=1; }

# ── Check 0: raw HNSW graph types stay in their canonical home ──────────
while IFS= read -r line; do
  fail "${line}: raw HNSW type outside search-vector/src/hnsw.rs (compose HnswIndex / make_hnsw instead)"
done < <(grep -rn "hnsw_rs::\|DistCosine" src --include="*.rs" 2>/dev/null | grep -v "^src/search-vector/src/hnsw\.rs:" || true)

# ── Check 1: no search-vector error shim path ────────────────────────────
# (`search_vector::math` was a shim path and is now the canonical kernel
# home — only the deleted `search_vector::error` path is still forbidden.)
while IFS= read -r line; do
  fail "${line}: search-vector error shim path regrew (use search_vector::vector_store::VectorDbError / fluent_db::error::DbError)"
done < <(grep -rn "search_vector::error" src --include="*.rs" 2>/dev/null || true)

# ── Check 2: no router hnsw shim paths ──────────────────────────────────
while IFS= read -r line; do
  fail "${line}: crate::hnsw:: path regrew outside its home (use search_vector::hnsw::…)"
done < <(grep -rn "crate::hnsw::" src --include="*.rs" 2>/dev/null | grep -v "^src/db/" | grep -v "^src/search-vector/" || true)

# ── Check 3: no fresh distance→similarity closures ──────────────────────
# Forbidden shapes: `1.0 - distance`, `1.0 - dist`, `1.0 - d)`/`,`,
# `1.0 - f64::from(dist)`. The canonical kernel home is exempt — the mapping
# may exist in exactly one place.
while IFS= read -r line; do
  fail "${line}: distance→similarity closure outside the canonical kernel (use distance_to_similarity[_clamped] / scored_hits)"
done < <(grep -rn -E "1\.0 - (distance|dist\b|d\)|d,|f64::from\(dist\)|f32)" src --include="*.rs" 2>/dev/null | grep -v "^src/search-vector/src/math\.rs:" || true)

# ── Check 4: deleted shim files stay deleted ────────────────────────────
# (`search-vector/src/math.rs` was a deleted re-export shim and is now the
# canonical kernel home per the consolidation — it is protected by checks
# 0/3/5 instead of this list.)
for f in src/router/src/hnsw.rs src/search-vector/src/error.rs; do
  if [ -e "$f" ]; then
    fail "$f: deleted re-export shim regrew"
  fi
done

# ── Check 5: canonical threshold source plus one deprecated shim ──────
# `AdaptiveHnsw` was born in `search-vector` (no shim anywhere): exactly 1
# definition. The moved vocabulary lives canonically in
# `search-vector/src/hnsw.rs` with a value-identical
# `common-core/src/constants.rs` shim: exactly 2 definitions (one per
# home) until the shim is removed in an explicit later breaking change.
for def in "pub const DEFAULT_HNSW_THRESHOLD" "pub struct HnswParams"; do
  total=$(grep -rn -- "$def" src --include="*.rs" 2>/dev/null | wc -l)
  canonical=$(grep -c -- "$def" src/search-vector/src/hnsw.rs 2>/dev/null || true)
  shim=$(grep -c -- "$def" src/common-core/src/constants.rs 2>/dev/null || true)
  if [ "$total" -ne 2 ] || [ "$canonical" -ne 1 ] || [ "$shim" -ne 1 ]; then
    fail "expected \`$def\` in search-vector/src/hnsw.rs + common-core/src/constants.rs only, found $total"
  fi
done
count=$(grep -rn -- "pub struct AdaptiveHnsw" src --include="*.rs" 2>/dev/null | wc -l)
if [ "$count" -ne 1 ]; then
  fail "expected exactly 1 definition of \`pub struct AdaptiveHnsw\`, found $count"
fi

# ── Check 6: single rusqlite error taxonomy ─────────────────────────────
impls=$(grep -rn "impl From<rusqlite::Error>" src --include="*.rs" 2>/dev/null || true)
count=$(echo "$impls" | grep -c . || true)
if [ "$count" -ne 1 ]; then
  fail "expected exactly 1 From<rusqlite::Error> impl, found $count"
elif ! echo "$impls" | grep -q "^src/db/src/error\.rs:"; then
  fail "From<rusqlite::Error> impl lives outside src/db/src/error.rs: $impls"
fi

# ── Check 7: no new uses of the deprecated HNSW vocabulary path ─────────
# Grandfathered files pinning the shim (definitions, re-export, value
# tests, and the cross-crate identity test). Any other file naming the
# old path fails: new code imports `search_vector::hnsw` directly.
allowed_old_path="^(src/common-core/src/constants\.rs|src/common-core/src/lib\.rs|src/common-core/tests/constants\.rs|src/coral/src/db/mod\.rs|src/search-vector/tests/hnsw\.rs):"
while IFS= read -r line; do
  fail "${line}: deprecated common_core::constants HNSW path (import search_vector::hnsw directly)"
done < <(grep -rn -E "common_core::constants::(Hnsw|DEFAULT_HNSW_THRESHOLD|MAX_KNN_CANDIDATES)" src --include="*.rs" 2>/dev/null | grep -v -E "$allowed_old_path" || true)

if [ "$FAIL" -eq 0 ]; then
  echo "ok: raw HNSW types contained; no re-export shims, no second threshold/error source, no fresh distance closures"
fi
exit "$FAIL"
