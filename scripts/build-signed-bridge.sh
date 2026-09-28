#!/usr/bin/env bash
#
# build-signed-bridge.sh — build this instance's bridge and sign it with one stable identity.
#
# WHY. The linker signs every build ad hoc, so every build has a new code identity and macOS
# treats it as a new app: the Local Network and Full Disk Access grants of the last build do
# not carry over, and LAN access can be lost until someone clicks Allow at the screen.
# Signing with one identity and a fixed identifier keeps the grants across rebuilds.
#
# It builds and signs in place (bridge/target/release/jesse-bridge, which is what this
# instance's launchd job runs). It NEVER restarts anything: restart by label afterwards,
# `launchctl bootout` then `bootstrap`, as before.
#
# Usage:
#     scripts/build-signed-bridge.sh <identifier>      # e.g. the instance's launchd label
#
# The identity is JESSE_CODESIGN_IDENTITY, else the first line of
# ~/.jesse-sentinel/codesign-identity (the file the main instance's sentinel reads).
# A failed build, sign or verify exits non-zero.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/bridge/target/release/jesse-bridge"
IDENT="${1:?usage: $0 <identifier, e.g. the launchd label>}"
IDFILE="$HOME/.jesse-sentinel/codesign-identity"

IDENTITY="${JESSE_CODESIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ] && [ -f "$IDFILE" ]; then
  IDENTITY="$(grep -v '^[[:space:]]*#' "$IDFILE" | grep -m1 -v '^[[:space:]]*$' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' || true)"
fi
[ -n "$IDENTITY" ] || { echo "build-signed-bridge: no identity (set JESSE_CODESIGN_IDENTITY or $IDFILE)" >&2; exit 1; }

(cd "$ROOT/bridge" && cargo build --release --locked --bin jesse-bridge)
codesign -f -s "$IDENTITY" -i "$IDENT" "$BIN"
codesign -v "$BIN"
codesign -dv "$BIN" 2>&1 | grep -E '^(Identifier|TeamIdentifier|Signature)'
echo "build-signed-bridge: signed $BIN as $IDENT"
