#!/usr/bin/env bash
# doc-check — machine-checkable doc consistency lint.
#
# Verifies that every Rust type name referenced in the router/skill docs exists
# in the workspace source (no doc-only types like the old DynamicComponent).
# Test counts are deliberately NOT tracked here — they change so quickly that
# they don't belong in documentation.
#
# Usage:  bin/doc-check.sh            # run the type-existence check
#
# Exit 0 when clean; non-zero with a report when the check fails. Designed to
# be wired into `make router-test` / a `make doc-check` target.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

ROOT="$(pwd)"
FAIL=0

DOC_FILES=(
  doc/router/VISION.md
  doc/router/ARCHITECTURE.md
  doc/skills/fluent-wvr/SKILL.md
  doc/skills/fluent-concurrency/SKILL.md
  doc/skills/dag/SKILL.md
  doc/skills/common-core/SKILL.md
)

# Types the docs name that are legitimately doc-only vocabulary (not Rust
# identifiers in the workspace): skip them. Keep this list minimal and
# annotated — anything added here should be a deliberate, documented
# exception (e.g. a named future primitive like LoomRuntime).
DOC_ONLY_WHITELIST=(
  LoomRuntime  # explicitly documented as a future primitive (concurrency SKILL §2/Q3)
  DashMap      # external crate (dashmap), not a workspace type
  CreditFlow   # concept/section name — the real types are CreditSender/CreditReceiver (flow.rs)
  LogContext   # documented as deliberately NOT implemented (anti-pattern comparison)
)

# A backticked PascalCase identifier is a *false positive* when its line
# negates, disclaims, or historicizes the type ("no X", "no separate X",
# "the old X", "does not implement X", "the only variants are ..."). Only
# flag identifiers whose line asserts them as a live/current type.
is_negated_context() {
  local line="$1" name="$2"
  # Strip markdown bold (**not** → not) so it doesn't break the patterns.
  local masked="${line//\*\*/}"
  # Lowercase the whole line and the name first (so history notes like "The
  # old X" match), then replace the name so "no X" reads as "no __NAME__",
  # then drop backticks so surrounding code-span marks don't break patterns.
  masked="${masked,,}"
  local lname="${name,,}"
  masked="${masked//$lname/__NAME__}"
  masked="${masked//\`/}"
  [[ "$masked" =~ (^|[^A-Za-z0-9_])no[[:space:]]+[A-Za-z][A-Za-z0-9_-]*[[:space:]]+__NAME__ ]] && return 0
  [[ "$masked" =~ (^|[^A-Za-z0-9_])no[[:space:]]+__NAME__ ]] && return 0
  [[ "$masked" =~ no[[:space:]]+[A-Za-z][A-Za-z0-9_-]*[[:space:]]+/[[:space:]]+__NAME__ ]] && return 0
  # "no separate `ContextNode` / `SessionNode` split" — the second name in a
  # negated pairing separated by "/". The first sibling is not masked, so match
  # any word(s) between the negation and the "/ __NAME__".
  [[ "$masked" =~ no[[:space:]]+[A-Za-z][A-Za-z0-9_-]*[[:space:]]+[A-Za-z][A-Za-z0-9_-]*[[:space:]]+/[[:space:]]+__NAME__ ]] && return 0
  [[ "$masked" =~ there[[:space:]]+is[[:space:]]+no[[:space:]]+[A-Za-z][A-Za-z0-9_-]*[[:space:]]+__NAME__ ]] && return 0
  [[ "$masked" =~ does[[:space:]]+not[[:space:]]+(implement|build|have)[[:space:]]+(a[[:space:]]+standalone[[:space:]]+)?__NAME__ ]] && return 0
  [[ "$masked" =~ does[[:space:]]+not[[:space:]]+(implement|build|have)[[:space:]]+([A-Za-z][A-Za-z0-9_-]*[[:space:]]+or[[:space:]]+)?__NAME__ ]] && return 0
  [[ "$masked" =~ not[[:space:]]+[A-Za-z][A-Za-z0-9_-]*[[:space:]]+__NAME__[[:space:]]+(variants|struct|trait) ]] && return 0
  [[ "$masked" =~ the[[:space:]]+old[[:space:]]+__NAME__ ]] && return 0
  [[ "$masked" =~ only[[:space:]]+(variants|variants)[[:space:]]+are ]] && return 0
  [[ "$masked" =~ (deleted|was[[:space:]]+deleted|were[[:space:]]+deleted|removed) ]] && return 0
  [[ "$masked" =~ not[[:space:]]+in[[:space:]]+this[[:space:]]+(workspace|crate) ]] && return 0
  return 1
}

check_types() {
  local src="$(find "$ROOT/src" -name '*.rs' -not -path '*/target/*' -print0 \
    | xargs -0 cat 2>/dev/null)"
  local missing=0
  for doc in "${DOC_FILES[@]}"; do
    [[ -f "$doc" ]] || continue
    # Backtick code spans naming a PascalCase identifier (Rust type names),
    # with the line context kept so negated/deleted mentions are skipped.
    # Format per match line: "NAME|full source line".
    while IFS='|' read -r name line; do
      [[ -n "$name" ]] || continue
      for w in "${DOC_ONLY_WHITELIST[@]:-}"; do
        [[ "$name" == "$w" ]] && continue 2
      done
      is_negated_context "$line" "$name" && continue
      if rg -q "^[[:space:]]*pub (struct|trait|enum) $name\b" "$ROOT/src" 2>/dev/null \
        || rg -q "\b$name\b" <(echo "$src") >/dev/null 2>&1; then
        continue
      fi
      echo "  [type] '$name' referenced in $doc but not found in workspace source"
      echo "         line: $line"
      missing=1
    done < <(awk '
      /`[A-Z][A-Za-z0-9_]*`/ {
        full = $0
        rest = $0
        while (match(rest, /`([A-Z][A-Za-z0-9_]*)`/, m)) {
          print m[1] "|" full
          rest = substr(rest, RSTART + RLENGTH)
        }
      }
    ' "$doc")
  done
  return "$missing"
}

check_types
exit_code=$?
if [[ "$exit_code" -ne 0 ]]; then
  echo "doc-check: FAILED — see report above."
  exit 1
fi
echo "doc-check: clean."
