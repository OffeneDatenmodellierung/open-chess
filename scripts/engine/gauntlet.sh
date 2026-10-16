#!/usr/bin/env bash
# The gauntlet: this build against engines with known ratings, for an Elo
# estimate and a place on the ladder.
#
#   scripts/engine/gauntlet.sh                  300 games per opponent at 10+0.1
#   scripts/engine/gauntlet.sh --games 100 --ltc
#   scripts/engine/gauntlet.sh --net new.nnue
#   scripts/engine/gauntlet.sh --only stockfish-2400,reckless-0.4
#
# Opponents (edit the ladder as the engine climbs; keep the retired ones
# so old results stay readable). Ratings are CCRL blitz for the named
# version, and Stockfish's UCI_Elo for the limited ones; the estimate is
# only as good as those numbers.

. "$(dirname "$0")/common.sh"
need "$FASTCHESS"

games=300; tc="10+0.1"; net=""; only=""
while [ $# -gt 0 ]; do
    case "$1" in
        --games) games="$2"; shift 2 ;;
        --ltc) tc="60+0.6"; shift ;;
        --tc) tc="$2"; shift 2 ;;
        --net) net="$2"; shift 2 ;;
        --only) only="$2"; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
[ -s "$UHO_BOOK" ] || { echo "no opening book; run scripts/engine/setup.sh" >&2; exit 2; }

# name | rating | command | extra options
LADDER=(
    "stockfish-2000|2000|$ENGINES/stockfish|option.UCI_LimitStrength=true option.UCI_Elo=2000"
    "stockfish-2400|2400|$ENGINES/stockfish|option.UCI_LimitStrength=true option.UCI_Elo=2400"
    "stockfish-2800|2800|$ENGINES/stockfish|option.UCI_LimitStrength=true option.UCI_Elo=2800"
    "reckless-0.4|2933|$ENGINES/reckless-0.4|"
    "reckless-0.5|3243|$ENGINES/reckless-0.5|"
    "reckless-0.6|3387|$ENGINES/reckless-0.6|"
)

ts="$(stamp)"
me="$RIG/$ts-gauntlet-engine"
build_engine "$me"
me_opts=(option.Hash=16 option.Threads=1)
[ -n "$net" ] && me_opts+=("option.EvalFile=$net")

args=(-engine cmd="$me" name=open-chess "${me_opts[@]}")
used=()
for row in "${LADDER[@]}"; do
    IFS='|' read -r name rating cmd extra <<<"$row"
    [ -z "$only" ] || [[ ",$only," == *",$name,"* ]] || continue
    [ -x "$cmd" ] || { echo "skipping $name: $cmd missing (scripts/engine/setup.sh)"; continue; }
    # shellcheck disable=SC2206
    args+=(-engine cmd="$cmd" name="$name" option.Hash=16 option.Threads=1 $extra)
    used+=("$name=$rating")
done
[ ${#used[@]} -gt 0 ] || { echo "no opponents available" >&2; exit 2; }

log="$RIG/$ts-gauntlet.log"
echo "gauntlet: $games games each against ${used[*]} at $tc → $log"
"$FASTCHESS" "${args[@]}" \
    -each tc="$tc" proto=uci \
    -openings file="$UHO_BOOK" format=epd order=random plies=16 \
    -tournament gauntlet -seeds 1 \
    -rounds "$((games / 2))" -games 2 -repeat -concurrency "$THREADS" -ratinginterval 50 \
    -srand 7 -recover -pgnout file="$RIG/$ts-gauntlet.pgn" \
    2>&1 | tee "$log"

# An Elo estimate: each opponent's rating plus our Elo difference against
# it (from fastchess's final table), averaged. Printed last.
awk -v used="${used[*]}" '
    BEGIN { n = split(used, pairs, " "); for (i = 1; i <= n; i++) { split(pairs[i], kv, "="); rating[kv[1]] = kv[2] } }
    /^Elo difference/ { diff = $3 }
    /^ *[0-9]+ +[a-z0-9.-]+ +[-+]?[0-9.]+ +/ { if ($2 in rating) { est[$2] = rating[$2] + $3; cnt++ } }
    END {
        if (cnt == 0) { print "no per-opponent Elo in the log (did the match finish?)"; exit }
        sum = 0; for (k in est) { printf "  vs %-16s ≈ %.0f\n", k, est[k]; sum += est[k] }
        printf "gauntlet estimate: %.0f Elo (mean over %d opponents, CCRL-blitz scale)\n", sum / cnt, cnt
    }' "$log" | tee -a "$log"
