#!/usr/bin/env bash
# live-ai-guard — hermeticity / live-AI lint (roadmap M4.5).
#
# Enforces the AI-inference isolation rule from doc/TESTING.md §4:
#   1. Every `#[ignore]` test attribute must carry a `reason` string (rule 5),
#      so ignored tests are always self-describing about how to run them.
#
# NOTE (2026-11-09, owner decision): the former check 2 — "no dialable
# non-loopback host URL literal in hermetic test code" (rule 4) — was removed
# here after every one of its 5 hits was file-verified as a refusal/assertion
# fixture, never a dial (SSRF deny-list `expect_err`, config passthrough
# strings, PII `is_private_ip`/`anonymize` inputs). Rule 4 itself still stands
# in TESTING.md; only this string-match enforcement is gone, so review now
# carries it. Do not re-add a literal-match check without an exemption path
# for refusal fixtures.
#
# Usage:  bin/live-ai-guard.sh            # exit 0 clean, non-zero on violation
#
# Wired into `make lint-live-ai` and CI (it is cheap and fast).

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

ROOT="$(pwd)"
FAIL=0

# ── Check 1: every `#[ignore]` must carry a reason ─────────────────────────
# A reason is present if the attribute is `#[ignore = "..."]` or
# `#[ignore(reason = "...")]`. A bare `#[ignore]` is a violation.
#
# The match is anchored to the start of a line because an attribute always
# occupies its own line in valid Rust, while every false positive this guard
# has ever seen (doc prose, a scanner's own quoted sample) puts the token
# mid-line. Anchoring keeps the check exact instead of making it depend on
# whatever quoting style the surrounding text happens to use.
while IFS= read -r line; do
  file="${line%%:*}"; rest="${line#*:}"; lineno="${rest%%:*}"
  echo "FAIL: ${file}:${lineno}: bare #[ignore] has no reason (add '= \"...\"' or '(reason = \"...\")')"
  FAIL=1
done < <(grep -rnP '^[[:space:]]*#\[ignore\]' "$ROOT/src" 2>/dev/null || true)

if [ "$FAIL" -ne 0 ]; then
  echo ""
  echo "live-ai guard failed. Fix the violations above, or run them via"
  echo "'make test-live' only (live tests are #[ignore] + live-ai-gated)."
  exit 1
fi
echo "live-ai guard: clean."