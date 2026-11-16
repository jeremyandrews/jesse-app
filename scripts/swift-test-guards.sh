#!/usr/bin/env bash
#
# swift-test-guards.sh: structural rules for the Swift TEST code, checked from
# source text alone, so it runs on Linux CI (ci.yml) and second thing in
# scripts/local-ci-macos.sh. Separate from scripts/ci-guards.sh, which holds the
# bridge's security guards.
#
# Usage: scripts/swift-test-guards.sh [REPOSITORY_ROOT]
#
# RULE 1, the window trap. A test that builds an AppKit window and closes it
# without `isReleasedWhenClosed = false` over-releases it under ARC, and the crash
# lands later, in some unrelated test's autorelease pool pop
# (VaultEditorStartTests did it in September 2026; ComposerTextViewTests before
# that). So a test builds a window ONLY through `makeTestWindow`, which sets the
# flag before returning:
#
#     Jesse/JesseMacTests/MacTestWindow.swift          (the Mac test target)
#     JesseKit/Tests/JesseVaultTests/TestWindow.swift  (JesseKit's tests)
#
# Every other test file fails on `NSWindow(`, `NSPanel(`, `NSWindow.init`,
# `NSPanel.init`, and any `(contentRect:` that is not `makeTestWindow(contentRect:`
# (which catches a subclass, such as an OffscreenWindow, constructed directly).
# There is no inline exception and no suppression comment: the helper is the only
# path.
#
# Bash 3.2 compatible.

set -uo pipefail

ROOT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
if [ ! -d "$ROOT" ]; then
  echo "swift-test-guards: no such directory: $ROOT" >&2
  exit 2
fi
ROOT="$(cd "$ROOT" && pwd)"

TEST_DIRS="Jesse/JesseMacTests Jesse/JesseTests Jesse/JesseUITests Jesse/JesseWatchTests JesseKit/Tests"
HELPERS="Jesse/JesseMacTests/MacTestWindow.swift JesseKit/Tests/JesseVaultTests/TestWindow.swift"

found=0
scanned=0
existing=""
for d in $TEST_DIRS; do
  [ -d "$ROOT/$d" ] && existing="$existing $d"
done
if [ -z "$existing" ]; then
  echo "swift-test-guards: none of the test directories exist under $ROOT" >&2
  exit 2
fi

files="$(cd "$ROOT" && find $existing -type f -name '*.swift' | LC_ALL=C sort)"
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  skip=no
  for h in $HELPERS; do [ "$rel" = "$h" ] && skip=yes; done
  [ "$skip" = yes ] && continue
  scanned=$((scanned + 1))
  hits="$(awk -v f="$rel" '
    {
      line = $0
      if (index(line, "NSWindow(") > 0)     { print f ":" NR ": NSWindow( constructs a window; use makeTestWindow"; next }
      if (index(line, "NSPanel(") > 0)      { print f ":" NR ": NSPanel( constructs a window; use makeTestWindow"; next }
      if (index(line, "NSWindow.init") > 0) { print f ":" NR ": NSWindow.init constructs a window; use makeTestWindow"; next }
      if (index(line, "NSPanel.init") > 0)  { print f ":" NR ": NSPanel.init constructs a window; use makeTestWindow"; next }
      rest = line
      while ((i = index(rest, "(contentRect:")) > 0) {
        before = substr(rest, 1, i - 1)
        if (before !~ /makeTestWindow$/) {
          print f ":" NR ": (contentRect: constructs a window directly; use makeTestWindow"
          next
        }
        rest = substr(rest, i + 13)
      }
    }' "$ROOT/$rel")"
  if [ -n "$hits" ]; then
    printf '%s\n' "$hits"
    found=$((found + $(printf '%s\n' "$hits" | wc -l)))
  fi
done <<EOF
$files
EOF

if [ "$found" -gt 0 ]; then
  echo "swift-test-guards: ${found} window construction(s) outside makeTestWindow." >&2
  echo "  A window a test closes without isReleasedWhenClosed = false is over-released" >&2
  echo "  under ARC and crashes a later, unrelated test. Build it with makeTestWindow" >&2
  echo "  (${HELPERS% *} for the Mac tests," >&2
  echo "  ${HELPERS#* } for JesseKit's)." >&2
  exit 1
fi
echo "swift-test-guards: ${scanned} test file(s) clean."
