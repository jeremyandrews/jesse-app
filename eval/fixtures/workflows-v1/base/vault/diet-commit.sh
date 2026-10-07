#!/usr/bin/env bash
# diet-commit.sh — durably persist a diet log the INSTANT it's written, instead of
# waiting for the 15-min autocommit. Called by the per-log-event flow (the diet-logging
# skill / phone bridge) AFTER append + regenerate + validate + verify have all passed.
#
# WHY THIS EXISTS (root cause, 2026-06-28):
#   The per-log flow appended the CSV + regenerated diet-today.js and relied on the
#   every-15-min Studio autocommit to persist to git. On 2026-06-28 that autocommit
#   stalled (no run between 18:25 and 22:10). The 20:30 dinner — logged inside that gap —
#   was never committed and was lost from food-log.csv (it survived only because the next
#   morning's journal snapshotted diet-today.js). Committing on EACH log closes that
#   window: the autocommit becomes a backstop, not the primary persistence path.
#
# DURABILITY CONTRACT:
#   The local COMMIT is the guarantee. The pull-merge + push that follow are best-effort —
#   if the LAN remote is unreachable (off-LAN) or a merge needs a human, the commit still
#   stands on disk and the autocommit / next log will propagate it. Never lose a log;
#   never block logging on a network or merge problem.
#
# SCOPE: stages ONLY the git-only diet files (the .js cache + the diet-logs CSVs). It NEVER
#   stages vault/ markdown — Obsidian Sync + the Studio autocommit own that, and staging
#   it here re-creates the dual-ledger merge conflicts documented in
#   vault/Knowledge/Jesse-Guidelines/Vault-Git-Sync.md.
#
# Usage:  bash vault/diet-commit.sh ["commit message"]
#
# NOTE: written 2026-06-29 from the laptop; the git path could not be exercised there
#   (sandbox cannot write .git). MUST be smoke-tested on the Studio before it's trusted —
#   see vault/Projects/drafts/<dated>-diet-logging-durability-studio-prompt.md.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO" || { echo "diet-commit: cannot cd to repo $REPO"; exit 1; }

MSG="${1:-diet: log $(date '+%Y-%m-%d %H:%M')}"

# 1. Don't touch a repo that's mid-merge/rebase — the log is on disk; let the human /
#    backstop resolve, then it gets committed on the next cycle.
if [ -e .git/MERGE_HEAD ] || [ -d .git/rebase-merge ] || [ -d .git/rebase-apply ]; then
  echo "diet-commit: repo is mid-merge/rebase — skipping commit-on-log (log is safe on disk; autocommit will pick it up)."
  exit 0
fi

# 2. Clear a stale index.lock only if no git process actually holds it.
if [ -e .git/index.lock ] && ! pgrep -x git >/dev/null 2>&1; then
  rm -f .git/index.lock 2>/dev/null || true
fi

# 3. Stage ONLY the git-only diet propagation set (never markdown).
#    diet-logs/days/ joined the set on 2026-08-30: once the PostToolUse hook started
#    rebuilding the archive for a BACKFILLED day, that rebuild needed the same
#    commit-on-log durability as the live file. Without it an off-day fix sat
#    uncommitted until the 15-min autocommit, which is the window this script exists
#    to close.
#    diet-logs/day-styles.csv and diet-logs/calorie-base.csv joined on 2026-09-15: a day's
#    style and a maintenance block are declared there, in the same edit as the
#    diet-today.js scalars, so they need the same commit-on-log durability.
#    Only the paths that exist are passed: `git add` aborts the WHOLE command on a pathspec
#    that matches nothing, so one absent file would otherwise stage none of the others.
DIET_SET=(
  vault/diet-today.js
  vault/diet-progress.js
  vault/diet-coach-notes.js
  diet-logs/food-log.csv
  diet-logs/weight-log.csv
  diet-logs/exercise-log.csv
  diet-logs/day-styles.csv
  diet-logs/calorie-base.csv
  diet-logs/days
)
PRESENT=()
for p in "${DIET_SET[@]}"; do [ -e "$p" ] && PRESENT+=("$p"); done
[ "${#PRESENT[@]}" -gt 0 ] && git add -- "${PRESENT[@]}" 2>/dev/null

# 4. Nothing staged → nothing to do (e.g. re-run with no new log).
if git diff --cached --quiet; then
  echo "diet-commit: no diet changes staged — nothing to commit."
  exit 0
fi

# 5. Commit — the durability guarantee. --no-verify on purpose: the caller already ran
#    validate + verify (they must pass before this script is invoked), and a verified log
#    must never be blocked by an unrelated pre-commit hook.
if ! git commit -m "$MSG" --no-verify >/dev/null; then
  echo "diet-commit: commit FAILED — investigate; the CSV row is still on disk."
  exit 1
fi
echo "diet-commit: committed — $MSG"

# 6. Best-effort propagate. Failure is non-fatal: the commit is already safe locally.
if git pull --no-rebase --no-edit >/dev/null 2>&1; then
  if git push >/dev/null 2>&1; then
    echo "diet-commit: pushed to origin."
  else
    echo "diet-commit: push failed (off-LAN or remote down) — commit safe locally; backstop will push."
  fi
else
  echo "diet-commit: pull/merge needs attention — commit safe locally; NOT forcing. Resolve via Vault-Git-Sync runbook."
  git merge --abort 2>/dev/null || true
fi
exit 0
