#!/usr/bin/env bash
#
# gate-run.sh — run one development-loop gate with its output captured.
#
# The Makefile's per-component `*-format|*-lint|*-smoke|*-test|*-soak` gates and
# their repo-wide mirrors (format/format-check/lint/smoke/test/soak) run through
# this wrapper. A passing gate prints one line instead of thousands of
# test/progress lines; a failing gate still surfaces the relevant diagnostics and
# leaves the full output on disk for a follow-up `grep`.
#
# Usage:
#   gate-run.sh <label> <kind> [<log-suffix>] -- <command> [args...]
#
#   <label>      the make target name; used in the message and the log file name
#                (e.g. `dag-test`).
#   <kind>       how to summarize the captured output:
#                  test    aggregate the `test result:` lines
#                  lint    clippy diagnostics (`^error`/`^warning`)
#                  check   compiler diagnostics
#                  fmt     rustfmt diff (`^Diff in`)
#                  generic plain tail fallback
#   <log-suffix> optional; appended to the log file name so a target that runs
#                two commands (e.g. `smoke`: check then test) keeps two logs.
#
# Full output goes to "$GATE_LOG_DIR/<label>[.<log-suffix>].log" (overwritten
# each run); the path is printed on every run, so it is stable enough to `grep`
# later. The command's exit status becomes this script's exit status.
#
# Env overrides:
#   GATE_LOG_DIR   log directory        (default /tmp/opencode/monorepo-gates)
#   GATE_HEAD      failure excerpt cap  (default 50)
#   GATE_TAIL      fallback tail lines  (default 50, as originally proposed)
#
set -uo pipefail

label="${1:-}"
kind="${2:-}"
if [[ -z "$label" || -z "$kind" ]]; then
  echo "gate-run.sh: usage: gate-run.sh <label> <kind> [<log-suffix>] -- <command...>" >&2
  exit 2
fi
shift 2
step=""
if [[ "${1:-}" != "--" ]]; then
  step="$1"
  shift
fi
if [[ "${1:-}" != "--" ]]; then
  echo "gate-run.sh: expected '--' before the command" >&2
  exit 2
fi
shift
if [[ $# -eq 0 ]]; then
  echo "gate-run.sh: no command given" >&2
  exit 2
fi

log_dir="${GATE_LOG_DIR:-/tmp/opencode/monorepo-gates}"
head_lines="${GATE_HEAD:-50}"
tail_lines="${GATE_TAIL:-50}"

# Fall back to /tmp if the preferred directory cannot be created/written.
if ! mkdir -p "$log_dir" 2>/dev/null || [[ ! -w "$log_dir" ]]; then
  log_dir="/tmp"
fi
log="$log_dir/$label${step:+.${step}}.log"

# A self-describing header, then the gate's full stdout+stderr.
{
  printf '# gate-run.sh » %s (%s)\n' "$label" "$kind"
  printf '# command:'
  printf ' %q' "$@"
  printf '\n# started: %s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$log"

SECONDS=0
"$@" >>"$log" 2>&1
status=$?
elapsed=$SECONDS

warn_count=$(grep -cE '^warning(:|\[)' "$log" || true)

# ── summary line ────────────────────────────────────────────────────────────
summary=""
case "$kind" in
  test)
    summary=$(awk '
      /^test result:/ {
        bins++
        for (i = 1; i <= NF; i++) {
          if      ($i == "passed;")  passed  += $(i-1)
          else if ($i == "failed;")  failed  += $(i-1)
          else if ($i == "ignored;") ignored += $(i-1)
        }
      }
      END {
        if (bins == 0) { print "no tests ran"; exit }
        printf "%d passed, %d failed, %d ignored (%d %s)",
               passed, failed, ignored, bins, (bins == 1 ? "binary" : "binaries")
      }
    ' "$log")
    ;;
  lint)  summary="clippy clean" ;;
  check) summary="compiled" ;;
  fmt)   summary="rustfmt ok" ;;
  *)     summary="ok" ;;
esac

if [[ "$status" -eq 0 ]]; then
  warn_note=""
  if [[ "${warn_count:-0}" -gt 0 ]]; then
    w="warnings"; [[ "$warn_count" -eq 1 ]] && w="warning"
    warn_note=" · ${warn_count} ${w}"
  fi
  printf 'make %s SUCCEEDED — %s%s · %ss  [log: %s]\n' \
    "$label" "$summary" "$warn_note" "$elapsed" "$log"
  exit 0
fi

# ── failure excerpt ─────────────────────────────────────────────────────────
fail_summary="$summary"
case "$kind" in
  lint)   fail_summary="clippy errors" ;;
  check)  fail_summary="build failed" ;;
  fmt)    fail_summary="needs formatting" ;;
  generic) fail_summary="failed" ;;
  test)
    # A test-kind command that never reached the harness failed to build/run.
    grep -qE '^test result:' "$log" || fail_summary="build failed"
    ;;
esac
printf 'make %s FAILED (exit %s) — %s  [log: %s]\n' \
  "$label" "$status" "$fail_summary" "$log"

diag() {
  # rustc/clippy diagnostics: the `error:`/`warning:` header plus a few lines
  # of context (the `-->` location and the offending source line).
  grep -EA4 -E '^(error|warning)(\[[0-9A-Za-z]+\])?:' "$log" || true
}

excerpt() {
  case "$kind" in
    test)
      if grep -qE '^test result:' "$log"; then
        # Each libtest failure block runs from its `failures:` header to the
        # `test result: FAILED` line that closes it (the block plus the list of
        # failing test names). awk keeps the blocks, skips passing binaries.
        awk '/^failures:/ { inblk = 1 } inblk { print } /^test result:/ { inblk = 0 }' "$log"
        grep -E '^error: test failed' "$log" || true
      else
        # cargo exited non-zero before running any test → build/CLI failure.
        diag
      fi
      ;;
    lint | check) diag ;;
    fmt) grep -E '^Diff in ' "$log" || diag ;;
    *) diag ;;
  esac
}

out="$(excerpt)"
if [[ -z "$out" ]]; then
  out="$(tail -n "$tail_lines" "$log")"
fi
printf '%s\n' "$out" | head -n "$head_lines"

exit "$status"
