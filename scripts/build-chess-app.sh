#!/usr/bin/env bash
# Build the Chess lab app (web/chess) with its wasm engine into a directory
# ready to serve: the chess server's image does this, and so does CI.
#
#   scripts/build-chess-app.sh OUT_DIR
#
# Needs the wasm32-unknown-unknown target and wasm-bindgen-cli at the
# version in Cargo.lock (the script says which if it's missing).
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:?usage: scripts/build-chess-app.sh OUT_DIR}

want=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/version = |"/,""); print; exit}' Cargo.lock)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [[ "$have" != "$want" ]]; then
  echo "need wasm-bindgen-cli $want (have ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

# The board reader's weights ship to every browser that opens the app, so
# never an internal-only model (one trained on noncommercial data): its card
# in the safetensors header says so.
weights=web/chess/nets/board-reader.safetensors
if [[ -f $weights ]]; then
  n=$(od -An -t u8 -N 8 "$weights" | tr -d ' ')
  if head -c $((8 + n)) "$weights" | tail -c "$n" | grep -q 'internal_only\\":true'; then
    echo "$weights is an internal-only model (trained on noncommercial data): it can't ship in the app" >&2
    exit 1
  fi
fi

cargo build -p open-chess-wasm --target wasm32-unknown-unknown --release --locked
# The board reader for photos (chess-vision): candle's rand needs getrandom's
# JS backend in the browser.
RUSTFLAGS="${RUSTFLAGS:-} --cfg getrandom_backend=\"wasm_js\"" \
  cargo build -p open-chess-vision --features wasm --target wasm32-unknown-unknown --release --locked
rm -rf "$out"
mkdir -p "$out"
cp -r web/chess/. "$out/"
rm -rf "$out/pkg"
wasm-bindgen --target web --no-typescript --out-dir "$out/pkg" \
  target/wasm32-unknown-unknown/release/open_chess_wasm.wasm
wasm-bindgen --target web --no-typescript --out-dir "$out/pkg" \
  target/wasm32-unknown-unknown/release/open_chess_vision.wasm
# The guides, served with the app (/app/docs/…) for sites that show them.
mkdir -p "$out/docs"
cp docs/api.md docs/boards.md docs/photos.md docs/photo-licence.md docs/privacy.md docs/ios.md docs/lichess.md docs/engine.md docs/releases.md \
  docs/credits.md docs/dataset-permissions.md "$out/docs/"
# What's new (CHANGELOG.md), and the version the app shows beside it.
cp CHANGELOG.md "$out/docs/changelog.md"
version=$(awk '/^\[workspace.package\]/{p=1; next} /^\[/{p=0} p && $1 == "version" {gsub(/"/, "", $3); print $3; exit}' Cargo.toml)
printf '{"version":"%s"}\n' "$version" > "$out/version.json"

# A new cache for every release: the service worker's name changes when
# any file of the app does.
build=$(cd "$out" && find . -type f ! -name sw.js -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-12)
sed -i "s/__BUILD__/$build/" "$out/sw.js"
echo "built the chess app into $out (build $build)"
