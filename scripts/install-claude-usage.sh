#!/usr/bin/env bash
#
# install-claude-usage.sh: build jesse-claude-usage into ~/.local/bin, render its LaunchAgent
# with this user's absolute paths, and (re)load it so it runs now and then hourly.
#
# Run it once on each Mac whose Claude Code login should appear in the vault. Every run writes
# <vault>/Knowledge/Reference/Claude-Usage/<host>.md and nothing else.
#
# It never touches the bridge: not its plist, not its binary, not its state. The binary it
# installs reads the Claude login read only and never stores or refreshes a token.
#
# Usage, from anywhere in the checkout:
#     scripts/install-claude-usage.sh

set -euo pipefail

LABEL="com.jesse.claude-usage"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$REPO/bridge/launchd/$LABEL.plist"
AGENTS="$HOME/Library/LaunchAgents"
TARGET="$AGENTS/$LABEL.plist"
DOMAIN="gui/$(id -u)"

case "$HOME" in
  *@@*|*'|'*) echo "install-claude-usage: HOME contains characters this script will not template: $HOME" >&2; exit 1 ;;
esac

echo "Building jesse-claude-usage into $HOME/.local/bin"
cargo install --path "$REPO/bridge" --bin jesse-claude-usage --root "$HOME/.local" --locked

mkdir -p "$AGENTS" "$HOME/Library/Logs"
sed "s|@@HOME@@|$HOME|g" "$TEMPLATE" > "$TARGET.tmp"
plutil -lint "$TARGET.tmp" >/dev/null
mv "$TARGET.tmp" "$TARGET"
echo "Wrote $TARGET"

# bootout returns before the job is fully gone; bootstrapping straight after can fail with
# "Input/output error", so give it a moment.
if launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null; then sleep 2; fi
launchctl bootstrap "$DOMAIN" "$TARGET"
echo "Loaded $LABEL; it runs now and every hour. Log: $HOME/Library/Logs/jesse-claude-usage.log"
