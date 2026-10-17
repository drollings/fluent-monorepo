#!/usr/bin/env bash
# protopyne-build-matrix — lean-build matrix gate (roadmap M10).
#
# Asserts the producer-confidence / task-value split for build claims:
# the lean library (no features) must build and pass its unit suites with
# no `polars` in the ship closure, while `--all-features` must run the full
# suite including every feature-gated family. Suite *presence* is pinned by
# module substring (not counts, which churn with milestones):
#   - `ta_volume` stands for the research-gated Expr-oracle kernel suites,
#   - `native_apex` stands for the pine-gated engine suites.
# Both must be present under `--all-features` and absent without features;
# anything else is either a leak into the lean closure or silent coverage
# loss under the full gate.
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

say "full suite under --all-features"
cargo test -p protopyne-kernels --all-features --lib

say "gated suites present under --all-features, absent without"
FULL_LIST="$(cargo test -p protopyne-kernels --all-features --lib -- --list)"
LEAN_LIST="$(cargo test -p protopyne-kernels --no-default-features --lib -- --list)"
for suite in ta_volume native_apex; do
    [[ "$FULL_LIST" == *"$suite"* ]] \
        || fail "suite family $suite missing under --all-features"
    [[ "$LEAN_LIST" == *"$suite"* ]] \
        && fail "suite family $suite leaks into the lean build"
done

say "server builds on lean kernels and runs its full suite"
cargo build -p protopyne-server
cargo test -p protopyne-server --all-features --lib

say "strategy (research consumer) suite stays green"
cargo test -p protopyne-strategy --lib

echo "build matrix green"
