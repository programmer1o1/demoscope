#!/usr/bin/env bash
# Prints the CHANGELOG.md section for a release tag, for use as a GitHub Release
# body. Used by the release workflow; run it locally to preview the notes.
#
#   ./scripts/release-notes.sh v1.0.0            # the section body
#   ./scripts/release-notes.sh --title v1.0.0    # its heading, e.g. v1.0.0: "Baseline"
#
# The section is the one whose `## ` heading starts with the tag (`## v1.0.0:
# "Baseline"` matches v1.0.0, `## v1.0.01` does not), and runs to the next `## `
# heading. Fails if there is no such section or it is empty. Relative links such as
# docs/OUTPUT.md are rewritten to absolute GitHub URLs at the tag, since a release
# page can't resolve them. Set CHANGELOG to read another file.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
file="${CHANGELOG:-$ROOT/CHANGELOG.md}"

mode=body
if [ "${1:-}" = "--title" ]; then
  mode=title
  shift
fi
tag="${1:?usage: release-notes.sh [--title] <tag, e.g. v1.0.0>}"

# First line of the output is the heading text; the rest is the section body.
section="$(awk -v tag="$tag" '
  /^## / {
    if (inside) exit
    h = substr($0, 4)
    if (substr(h, 1, length(tag)) == tag) {
      c = substr(h, length(tag) + 1, 1)
      if (c == "" || c == ":" || c == " ") { inside = 1; print h; next }
    }
  }
  inside { print }
' "$file")"

if [ -z "$section" ]; then
  echo "release-notes: no '## $tag' section in $file" >&2
  exit 1
fi

if [ "$mode" = title ]; then
  printf '%s\n' "$section" | head -n 1
  exit 0
fi

body="$(printf '%s\n' "$section" | tail -n +2 | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')"
if [ -z "$(printf '%s' "$body" | tr -d '[:space:]')" ]; then
  echo "release-notes: the '## $tag' section in $file is empty" >&2
  exit 1
fi

base="https://github.com/${GITHUB_REPOSITORY:-programmer1o1/demoscope}/blob/$tag"
printf '%s\n' "$body" | BASE="$base" perl -pe 's{\]\((?!https?://|#)([^)]+)\)}{]($ENV{BASE}/$1)}g'
