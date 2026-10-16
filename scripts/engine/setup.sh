#!/usr/bin/env bash
# Install what the engine tests need: fastchess, the UHO opening book, and
# the reference engines the gauntlet plays. Everything goes under
# target/engine-tests/; nothing touches the system. Re-run to update.
#
# The reference engines are built from source where a release binary
# wouldn't fit the machine. They are measuring tools: none of them is a
# dependency and none is linked or shipped (see deny.toml).

. "$(dirname "$0")/common.sh"

need git
need cargo

# fastchess (MIT) — the match runner.
if [ ! -x "$FASTCHESS" ]; then
    echo "building fastchess"
    rm -rf "$RIG/fastchess-src"
    git clone --depth 1 https://github.com/Disservin/fastchess "$RIG/fastchess-src"
    (cd "$RIG/fastchess-src" && make -j"$THREADS" >/dev/null)
    cp "$RIG/fastchess-src/fastchess" "$FASTCHESS"
fi

# UHO 4060 (unbalanced human openings, the standard book for engine tests).
if [ ! -f "$UHO_BOOK" ]; then
    echo "fetching the UHO opening book"
    curl -sSL -o "$UHO_BOOK" \
        https://raw.githubusercontent.com/official-stockfish/books/master/UHO_4060_v2.epd.zip
    (cd "$BOOKS" && unzip -o -q UHO_4060_v2.epd && rm -f UHO_4060_v2.epd.zip) || true
fi
[ -s "$UHO_BOOK" ] || { echo "the UHO book didn't unpack; put UHO_4060_v2.epd in $BOOKS" >&2; }

# Reference engines for the gauntlet, from source. Rust ones build with
# cargo; Stockfish needs make and a C++ compiler.
build_rust_engine() { # name repo tag
    local name="$1" repo="$2" tag="$3"
    [ -x "$ENGINES/$name" ] && return
    echo "building $name $tag"
    rm -rf "$RIG/$name-src"
    git clone --depth 1 --branch "$tag" "$repo" "$RIG/$name-src"
    (cd "$RIG/$name-src" && cargo build --release >/dev/null 2>&1)
    cp "$(find "$RIG/$name-src/target/release" -maxdepth 1 -type f -perm -u+x | head -1)" "$ENGINES/$name"
}

# Older Reckless versions span the ladder we climb (CCRL blitz ~2900 → 3400).
build_rust_engine reckless-0.4 https://github.com/codedeliveryservice/Reckless v0.4.0
build_rust_engine reckless-0.5 https://github.com/codedeliveryservice/Reckless v0.5.0
build_rust_engine reckless-0.6 https://github.com/codedeliveryservice/Reckless v0.6.0

# Stockfish, for the bottom rungs at a fixed Elo and for labelling data.
if [ ! -x "$ENGINES/stockfish" ]; then
    if command -v stockfish >/dev/null; then
        ln -sf "$(command -v stockfish)" "$ENGINES/stockfish"
    else
        echo "building stockfish"
        rm -rf "$RIG/stockfish-src"
        git clone --depth 1 --branch sf_17.1 https://github.com/official-stockfish/Stockfish "$RIG/stockfish-src"
        (cd "$RIG/stockfish-src/src" && make -j"$THREADS" build >/dev/null 2>&1)
        cp "$RIG/stockfish-src/src/stockfish" "$ENGINES/stockfish"
    fi
fi

echo "ready: $(ls "$ENGINES" | tr '\n' ' ')"
