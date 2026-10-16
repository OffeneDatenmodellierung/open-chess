# Shared by the engine test scripts. Source, don't run.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RIG="$ROOT/target/engine-tests"
ENGINES="$RIG/engines"
BOOKS="$RIG/books"
mkdir -p "$RIG" "$ENGINES" "$BOOKS"

THREADS="${THREADS:-$(( $(nproc 2>/dev/null || sysctl -n hw.ncpu) - 1 ))}"
[ "$THREADS" -ge 1 ] || THREADS=1

FASTCHESS="${FASTCHESS:-$ENGINES/fastchess}"
UHO_BOOK="$BOOKS/UHO_4060_v2.epd"

need() {
    command -v "$1" >/dev/null 2>&1 || { echo "need $1 (scripts/engine/setup.sh installs it)" >&2; exit 2; }
}

# Build the engine from the working tree into $1.
build_engine() {
    local out="$1"
    (cd "$ROOT" && cargo build --release -p open-chess-uci >/dev/null)
    cp "$ROOT/target/release/open-chess-uci" "$out"
}

# Build the engine at git ref $1 into $2, in a throw-away worktree.
build_engine_at() {
    local ref="$1" out="$2"
    local wt
    wt="$(mktemp -d)"
    git -C "$ROOT" worktree add --detach "$wt" "$ref" >/dev/null
    (cd "$wt" && cargo build --release -p open-chess-uci >/dev/null)
    cp "$wt/target/release/open-chess-uci" "$out"
    git -C "$ROOT" worktree remove --force "$wt"
}

stamp() { date +%Y%m%d-%H%M%S; }
