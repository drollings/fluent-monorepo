#!/usr/bin/env bash
# protopyne-build-matrix — lean-build matrix gate (roadmap M10).
#
# Asserts the producer-confidence / task-value split for build claims:
# the lean library (no features) must build and pass its unit suites with
# no `polars` in the ship closure, while `--all-features` must run the full
# suite including every feature-gated family. Suite *presence* is pinned by
# module substring (not counts, which churn with milestones):
#   - `ta_volume` stands for the research-gated Expr-oracle kernel suites,
#   - `native_apex` stands for the pine-gated engine suites,
#   - `signals_pine` stands for the strategy's pine-gated signal suite.
#   - `wq_series` / `wq_half_life` stand for the research-only `ts_*` vocabulary,
#     which is also asserted absent from the default build's symbol table.
# All must be present under `--all-features` and absent without features;
# anything else is either a leak into the lean closure or silent coverage
# loss under the full gate. The strategy ship closure is separately asserted
# to carry no `polars`: a decision crate reaches a scorer (a slice function),
# never the dataframe surface.
#
# Network: plain cargo (no `--offline`) so the same script runs on a dev
# machine and in CI. The roadmap's `--offline` gate commands are run by hand
# on a warm cache alongside this script.
#
# Usage:  bin/protopyne-build-matrix.sh   # exit 0 clean, non-zero on violation
#
# Wired into `make protopyne-matrix`.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

say() { echo "== $1"; }
fail() { echo "ERROR: $1" >&2; exit 1; }

say "lean library build carries no research/pine weight"
cargo build -p protopyne-kernels --no-default-features

say "lean unit suites pass"
cargo test -p protopyne-kernels --no-default-features --lib

say "ship closure without the research feature carries no polars"
LEAN_TREE="$(cargo tree -e no-dev -p protopyne-kernels --no-default-features)"
if [[ "$LEAN_TREE" == *"polars"* ]]; then
    fail "polars reachable without the research feature"
fi

say "strategy decision closure carries no polars"
STRATEGY_TREE="$(cargo tree -e no-dev -p protopyne-strategy)"
if [[ "$STRATEGY_TREE" == *"polars"* ]]; then
    fail "polars reachable from protopyne-strategy"
fi

say "default build carries no research-only wq_* vocabulary"
# `wq_series` (`ts_*`) and `wq_half_life` (`hl_*`/`ts_*_hl`) have no consumer in
# the shipping library. They are `pub` modules, so a leak raises no dead-code
# warning: the rlib's own symbol table is the observable proof that they are
# gone from a default build (symbol names embed the module path).
LEAN_JSON="$(cargo build -p protopyne-kernels --no-default-features --message-format=json 2>/dev/null)"
LEAN_RLIB="$(printf '%s\n' "$LEAN_JSON" | sed -n 's/.*"filenames":\["\([^"]*libprotopyne_kernels[^"]*\.rlib\)".*/\1/p' | head -n 1)"
[[ -n "$LEAN_RLIB" ]] || fail "could not locate the lean protopyne-kernels rlib"
if command -v nm >/dev/null 2>&1; then
    # Capture before grepping (see the FractionalKelly block below): `nm |
    # grep -q` on a large symbol table lets `grep` exit early and `nm` die of
    # SIGPIPE, which `pipefail` reports as a match failure — a leak would be
    # missed rather than caught.
    LEAN_K_SYMS="$(nm --defined-only "$LEAN_RLIB" 2>/dev/null)"
    for module in wq_series wq_half_life; do
        if grep -q "$module" <<< "$LEAN_K_SYMS"; then
            fail "module $module leaks into the default build"
        fi
    done
    say "lean rlib symbol table names neither wq_series nor wq_half_life"
else
    say "note: nm unavailable — symbol-level wq_* check skipped (suite check below still applies)"
fi

say "full suite under --all-features"
cargo test -p protopyne-kernels --all-features --lib

say "gated suites present under --all-features, absent without"
FULL_LIST="$(cargo test -p protopyne-kernels --all-features --lib -- --list)"
LEAN_LIST="$(cargo test -p protopyne-kernels --no-default-features --lib -- --list)"
for suite in ta_volume native_apex wq_series wq_half_life; do
    [[ "$FULL_LIST" == *"$suite"* ]] \
        || fail "suite family $suite missing under --all-features"
    [[ "$LEAN_LIST" == *"$suite"* ]] \
        && fail "suite family $suite leaks into the lean build"
done

say "server builds on lean kernels and runs its full suite"
cargo build -p protopyne-server
cargo test -p protopyne-server --all-features --lib

say "strategy pine suite present under --all-features, absent without"
STRATEGY_FULL_LIST="$(cargo test -p protopyne-strategy --all-features --lib -- --list)"
STRATEGY_LEAN_LIST="$(cargo test -p protopyne-strategy --lib -- --list)"
[[ "$STRATEGY_FULL_LIST" == *"signals_pine"* ]] \
    || fail "signals_pine missing under --all-features"
[[ "$STRATEGY_LEAN_LIST" == *"signals_pine"* ]] \
    && fail "signals_pine leaks into the lean build"

say "strategy research suite present under --all-features, absent without"
[[ "$STRATEGY_FULL_LIST" == *"kelly_sizing"* ]] \
    || fail "kelly_sizing missing under --all-features"
[[ "$STRATEGY_LEAN_LIST" == *"kelly_sizing"* ]] \
    && fail "kelly_sizing leaks into the lean build"

say "lean strategy rlib names no FractionalKelly; the research rlib does"
# The package artifact's reported filename is the shared top-level hardlink,
# which points at whichever feature set built last — resolving it once and
# reusing the path across two builds is order-dependent and vacuous. Instead
# each build is forced (a mtime touch; content unchanged), the stale link is
# removed so a fresh-no-rebuild cannot masquerade as the just-built artifact,
# and the artifact is checked immediately, while it still points at the
# feature set just built.
STRATEGY_LIB="$ROOT/src/protopyne-strategy/src/lib.rs"
STRATEGY_TOP_RLIB="$ROOT/target/debug/libprotopyne_strategy.rlib"
if command -v nm >/dev/null 2>&1; then
    # Capture before grepping: `nm` on a 14 MB rlib emits far more than the
    # 64 KB pipe buffer, so `nm | grep -q` lets `grep` exit early, `nm` die
    # of SIGPIPE, and `pipefail` fail the pipeline even on a match. A
    # herestring has no producer to kill, so the status is `grep`'s alone.
    touch "$STRATEGY_LIB"
    rm -f "$STRATEGY_TOP_RLIB"
    cargo build -p protopyne-strategy
    [[ -f "$STRATEGY_TOP_RLIB" ]] || fail "lean strategy rlib not materialized"
    LEAN_SYMS="$(nm --defined-only "$STRATEGY_TOP_RLIB" 2>/dev/null)"
    if grep -q "FractionalKelly" <<< "$LEAN_SYMS"; then
        fail "FractionalKelly leaks into the lean strategy build"
    fi
    say "lean strategy rlib names no FractionalKelly"
    touch "$STRATEGY_LIB"
    rm -f "$STRATEGY_TOP_RLIB"
    cargo build -p protopyne-strategy --features research
    [[ -f "$STRATEGY_TOP_RLIB" ]] || fail "research strategy rlib not materialized"
    RESEARCH_SYMS="$(nm --defined-only "$STRATEGY_TOP_RLIB" 2>/dev/null)"
    if ! grep -q "FractionalKelly" <<< "$RESEARCH_SYMS"; then
        fail "FractionalKelly missing from the research strategy build"
    fi
    say "research strategy rlib carries FractionalKelly"
else
    say "note: nm unavailable — FractionalKelly symbol check skipped (suite check above still applies)"
fi

say "strategy (lean kernels consumer) suite stays green"
cargo test -p protopyne-strategy --lib

echo "build matrix green"
