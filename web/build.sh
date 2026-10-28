#!/usr/bin/env bash
# Build the website into web/dist: the WebAssembly module and its JavaScript glue under pkg/,
# the static files, and the ready-made exports from data/ with their index.
#
# Needs the wasm32-unknown-unknown target (rustup target add wasm32-unknown-unknown) and a
# wasm-bindgen-cli of the same version as the wasm-bindgen crate in the workspace Cargo.lock.
set -euo pipefail
cd "$(dirname "$0")"

# One target directory for the whole workspace, also when this script is run from web/.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$(pwd)/../target}"

# 1. Compile for the browser. The "wasm" profile optimises for size.
cargo build -p atm-finder-web --profile wasm --target wasm32-unknown-unknown

# 2. Generate the JavaScript glue that loads the module (dist/pkg/atm_finder_web.js).
rm -rf dist && mkdir -p dist/pkg
wasm-bindgen --target web --no-typescript --out-dir dist/pkg --out-name atm_finder_web \
  "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm/atm_finder_web.wasm"

# 3. Shrink the module further when binaryen's wasm-opt is installed (optional). wasm-bindgen
#    drops the section that lists the WebAssembly features the module uses, so name the ones
#    Rust enables by default for wasm32-unknown-unknown.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz \
    --enable-bulk-memory --enable-multivalue --enable-mutable-globals \
    --enable-nontrapping-float-to-int --enable-reference-types --enable-sign-ext \
    -o dist/pkg/atm_finder_web_bg.wasm dist/pkg/atm_finder_web_bg.wasm
fi

# 4. Page, styles, map glue and vendored Leaflet.
cp -r static/. dist/

# 5. Ready-made exports and their index for the "ready-made files" section.
cargo run -q -p atm-finder --release -- data-index ../data dist/data

ls -la dist/pkg
