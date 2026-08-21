#!/usr/bin/env bash
# Build libneedle.so from the shipped static libneedle.a.
#
# Needle ships `linux-x86_64/libneedle.a` (compiled against LLVM libc++, so
# the symbols are `std::__1::`). This links those members into a shared
# library with the C ABI (`needle_init`/`needle_complete`/`needle_load`/
# `needle_reset`) that coral-router FFI-loads in-process.
#
# Requirements: g++ (or clang++), and the libc++ / libc++abi runtimes
# (Debian/Ubuntu: `libc++-dev libc++abi-dev`). The produced .so must be run
# with libc++ on the runtime path (or the runtime linked in statically).
#
# Usage: bin/build-libneedle.sh [path/to/libneedle.a]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCH_DIR="${1:-$REPO_ROOT/vendor/needle/linux-x86_64}"
ARCHIVE="$ARCH_DIR/libneedle.a"
OUT="$ARCH_DIR/libneedle.so"

if [[ ! -f "$ARCHIVE" ]]; then
    echo "error: needle static archive not found at $ARCHIVE" >&2
    exit 1
fi

command -v g++ >/dev/null 2>&1 || command -v clang++ >/dev/null 2>&1 || {
    echo "error: no C++ compiler (g++/clang++) on PATH" >&2
    exit 1
}
CXX="${CXX:-$(command -v g++ || command -v clang++)}"

# Verify libc++/libc++abi are linkable (the .a was compiled against LLVM libc++).
if ! printf 'int main(){return 0;}' | "$CXX" -x c++ - -lc++ -lc++abi -o /tmp/.needle-cxx-probe 2>/dev/null; then
    echo "error: libc++ / libc++abi not found (install libc++-dev libc++abi-dev)" >&2
    exit 1
fi
rm -f /tmp/.needle-cxx-probe

echo "building $OUT from $ARCHIVE"
"$CXX" -shared -o "$OUT" \
    -Wl,--whole-archive "$ARCHIVE" -Wl,--no-whole-archive \
    -fPIC -lc++ -lc++abi -pthread

echo "built $OUT"
nm -D "$OUT" 2>/dev/null | grep -E 'needle_(init|complete|load|reset)' || {
    echo "warning: exported needle symbols not found in $OUT" >&2
}
