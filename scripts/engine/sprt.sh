#!/usr/bin/env bash
# Sequential probability ratio test: is the change better?
#
#   scripts/engine/sprt.sh --set se_depth=6             a parameter, against the same build
#   scripts/engine/sprt.sh --base main                  the working tree against a git ref
#   scripts/engine/sprt.sh --base main --simplify       bounds [-5, 0]: must not lose
#   scripts/engine/sprt.sh --base main --ltc            60+0.6 instead of 10+0.1
#   scripts/engine/sprt.sh --net new.nnue               a network, against no network
#   scripts/engine/sprt.sh --net new.nnue --base-net old.nnue
#
# Exactly one thing differs between the two sides. Everything else is
# fixed: hash 16 MB, one thread each, the UHO book, no pondering, the
# same seed. The test stops itself when it has an answer.

. "$(dirname "$0")/common.sh"
need "$FASTCHESS"

base_ref=""; sets=(); net=""; base_net=""; tc="10+0.1"; elo0=0; elo1=5; name="sprt"; rounds=20000
while [ $# -gt 0 ]; do
    case "$1" in
        --base) base_ref="$2"; shift 2 ;;
        --set) sets+=("$2"); name="$name-${2%%=*}"; shift 2 ;;
        --net) net="$2"; name="$name-net"; shift 2 ;;
        --base-net) base_net="$2"; shift 2 ;;
        --ltc) tc="60+0.6"; shift ;;
        --tc) tc="$2"; shift 2 ;;
        --simplify) elo0=-5; elo1=0; shift ;;
        --bounds) elo0="$2"; elo1="$3"; shift 3 ;;
        --name) name="$2"; shift 2 ;;
        --rounds) rounds="$2"; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
[ -n "$base_ref" ] || [ ${#sets[@]} -gt 0 ] || [ -n "$net" ] || { echo "nothing to test: --base, --set or --net" >&2; exit 2; }
[ -s "$UHO_BOOK" ] || { echo "no opening book; run scripts/engine/setup.sh" >&2; exit 2; }

ts="$(stamp)"
new="$RIG/$ts-$name-new"
build_engine "$new"
if [ -n "$base_ref" ]; then
    old="$RIG/$ts-$name-base"
    build_engine_at "$base_ref" "$old"
    name="$name-vs-$base_ref"
else
    old="$new"
fi

new_opts=(option.Hash=16 option.Threads=1)
old_opts=(option.Hash=16 option.Threads=1)
for s in "${sets[@]}"; do new_opts+=("option.$s"); done
[ -n "$net" ] && new_opts+=("option.EvalFile=$net")
[ -n "$base_net" ] && old_opts+=("option.EvalFile=$base_net")

log="$RIG/$ts-$name.log"
echo "SPRT [$elo0, $elo1] at $tc, $THREADS games at once → $log"
"$FASTCHESS" \
    -engine cmd="$new" name=new "${new_opts[@]}" \
    -engine cmd="$old" name=base "${old_opts[@]}" \
    -each tc="$tc" proto=uci \
    -openings file="$UHO_BOOK" format=epd order=random plies=16 \
    -sprt elo0="$elo0" elo1="$elo1" alpha=0.05 beta=0.05 model=normalized \
    -rounds "$rounds" -games 2 -repeat -concurrency "$THREADS" -ratinginterval 20 \
    -srand 7 -recover -pgnout file="$RIG/$ts-$name.pgn" \
    2>&1 | tee "$log"
