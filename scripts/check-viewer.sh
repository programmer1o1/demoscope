#!/usr/bin/env bash
# Generates a viewer for each demo and loads it in headless Chromium, failing
# on any JavaScript error (uncaught exceptions or console.error).
#
#   ./scripts/check-viewer.sh                 # every demo in tests/fixtures/
#   ./scripts/check-viewer.sh a.dem b.dem     # specific demos
#
# Playwright and its Chromium are installed on first run into
# $DEMOSCOPE_PLAYWRIGHT_DIR (default ~/.cache/demoscope-playwright). They are
# dev-only tools, not project dependencies. Needs network access for the
# viewer's CDN scripts (three.js etc.).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PW_VERSION="1.64.1"
PW_DIR="${DEMOSCOPE_PLAYWRIGHT_DIR:-$HOME/.cache/demoscope-playwright}"
BIN="$ROOT/target/release/demoscope"

if [ "$#" -gt 0 ]; then
  demos=("$@")
else
  demos=("$ROOT"/tests/fixtures/*.dem)
fi

if [ ! -x "$BIN" ]; then
  (cd "$ROOT" && cargo build --release)
fi

if [ ! -f "$PW_DIR/node_modules/playwright/package.json" ] ||
   ! grep -q "\"version\": \"$PW_VERSION\"" "$PW_DIR/node_modules/playwright/package.json"; then
  echo "Installing Playwright $PW_VERSION into $PW_DIR ..."
  npm install --silent --no-save --prefix "$PW_DIR" "playwright@$PW_VERSION"
fi
if [ "${PLAYWRIGHT_WITH_DEPS:-0}" = "1" ]; then
  # CI: also install Chromium's system libraries (uses sudo on Linux).
  "$PW_DIR/node_modules/.bin/playwright" install --with-deps chromium >/dev/null
else
  "$PW_DIR/node_modules/.bin/playwright" install chromium >/dev/null
fi

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

pages=()
for d in "${demos[@]}"; do
  [ -e "$d" ] || continue
  page="$out/$(basename "$d").html"
  if ! "$BIN" "$d" --html "$page" >/dev/null 2>&1; then
    echo "FAIL  $d (demoscope --html failed)"
    exit 1
  fi
  pages+=("$page")
done

if [ "${#pages[@]}" -eq 0 ]; then
  echo "No demos to check."
  exit 1
fi

NODE_PATH="$PW_DIR/node_modules" node "$ROOT/scripts/check-viewer.cjs" "${pages[@]}"
