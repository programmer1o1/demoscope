#!/usr/bin/env bash
# Runs the release binary over every demo in a directory: --summary, --check (no
# FAIL; a junk file would otherwise still produce a page, because any `.dem` that
# isn't Source or GoldSrc is read as NetQuake), then --html, then checks that the
# page has the data tables and that its script is valid JS.
# Fails if the directory has no demos, so an empty or missing corpus can't pass
# silently.
#
#   ./scripts/smoke-run.sh tests/fixtures
#   ./scripts/smoke-run.sh "DEMOS TESTING"
#
# Needs target/release/demoscope and Node.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/demoscope"
dir="${1:?usage: smoke-run.sh <demo-dir>}"

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

demos=()
while IFS= read -r f; do
  demos+=("$f")
done < <(find "$dir" -maxdepth 1 -type f \
  \( -iname '*.dem' -o -iname '*.dm2' -o -iname '*.mvd' -o -iname '*.qwd' \
     -o -iname '*.dm_[0-9]*' \) | LC_ALL=C sort)

if [ "${#demos[@]}" -eq 0 ]; then
  echo "FAIL: no demos found in '$dir'"
  exit 1
fi

pass=0
fail=0
for d in "${demos[@]}"; do
  echo "::group::$d"
  ok=1
  if ! "$BIN" "$d" --summary >/dev/null; then
    echo "FAIL: --summary failed"
    ok=0
  elif ! "$BIN" --check "$d" >/dev/null 2>&1; then
    echo "FAIL: --check reported FAIL"
    ok=0
  elif ! "$BIN" "$d" --html "$out/page.html" >/dev/null 2>&1; then
    echo "FAIL: --html failed"
    ok=0
  elif ! grep -q 'const META' "$out/page.html"; then
    echo "FAIL: page has no META table"
    ok=0
  elif ! node -e "
    const fs = require('fs');
    const h = fs.readFileSync(process.argv[1], 'utf8');
    const m = h.match(/<script>([\s\S]*?)<\/script>\s*<\/body>/);
    if (!m) { console.error('no script tag'); process.exit(1); }
    try { new Function(m[1]); } catch (e) { console.error('JS:', e.message); process.exit(1); }
  " "$out/page.html"; then
    echo "FAIL: page script is not valid JavaScript"
    ok=0
  fi
  [ "$ok" -eq 1 ] && echo "PASS" && pass=$((pass + 1)) || fail=$((fail + 1))
  echo "::endgroup::"
  [ "$ok" -eq 1 ] || echo "FAIL  $d"
done
echo "$dir: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
