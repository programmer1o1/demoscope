#!/usr/bin/env bash
# Build the WebAssembly bundle and place the JS bindings + .wasm + the
# drag-and-drop index.html in `web/`. Idempotent - re-run after any code
# change to the parser. Requires:
#   - rustup target add wasm32-unknown-unknown
#   - cargo install wasm-bindgen-cli  (matching version from Cargo.lock)
#
# To serve locally:
#   python3 -m http.server 8088 --directory web
# then open http://localhost:8088/

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "→ cargo build --release --lib --target wasm32-unknown-unknown"
cargo build --release --lib --target wasm32-unknown-unknown

echo "→ wasm-bindgen → web/"
mkdir -p web
wasm-bindgen --target web --out-dir web --no-typescript \
  target/wasm32-unknown-unknown/release/demoscope.wasm

# Preserve index.html across rebuilds. wasm-bindgen overwrites .wasm + .js
# but not unrelated files, so this just sanity-checks.
if [ ! -f web/index.html ]; then
  echo "warning: web/index.html missing - drag-and-drop UI won't work."
fi

echo "→ Built. Files in web/:"
ls -la web/
echo
echo "Serve with: python3 -m http.server 8088 --directory web"
