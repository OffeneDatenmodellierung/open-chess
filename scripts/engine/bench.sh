#!/usr/bin/env bash
# The engine's node signature and speed.
#
#   scripts/engine/bench.sh              this build
#   scripts/engine/bench.sh main         this build against a git ref: same
#                                        signature = no functional change
#   scripts/engine/bench.sh --depth 12   a deeper (slower, steadier) run
#
# Speed is taken as the best of three runs, since the first warms the
# caches and any run can be disturbed.

. "$(dirname "$0")/common.sh"

depth=10
ref=""
while [ $# -gt 0 ]; do
    case "$1" in
        --depth) depth="$2"; shift 2 ;;
        *) ref="$1"; shift ;;
    esac
done

run_bench() { # binary
    local best_nps=0 nodes="" line
    for _ in 1 2 3; do
        line="$("$1" bench "$depth" 2>/dev/null | tail -1)"
        nodes="${line%% nodes*}"
        local nps="${line##*nodes }"; nps="${nps%% nps*}"
        [ "$nps" -gt "$best_nps" ] && best_nps="$nps"
    done
    echo "$nodes $best_nps"
}

new="$RIG/bench-new"
build_engine "$new"
read -r nodes nps < <(run_bench "$new")
echo "this build: $nodes nodes, $nps nps (depth $depth)"

if [ -n "$ref" ]; then
    old="$RIG/bench-$ref"
    build_engine_at "$ref" "$old"
    read -r onodes onps < <(run_bench "$old")
    echo "$ref: $onodes nodes, $onps nps"
    if [ "$nodes" = "$onodes" ]; then
        printf 'same signature: no functional change. speed %+.1f%%\n' \
            "$(echo "scale=3; ($nps - $onps) * 100 / $onps" | bc)"
    else
        echo "DIFFERENT signature: the search changed; this needs an SPRT"
        exit 1
    fi
fi
