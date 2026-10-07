#!/usr/bin/env bash
# diet-regen.sh — PostToolUse hook for Edit|Write. When the edited file is one of the
# diet-logs CSVs, rebuild EVERY diet day the edit actually touched, validate each, and
# commit.
#
# Extracted from the inline one-liner in .claude/settings.json on 2026-08-30, at the same
# time as the off-day fix below. It had outgrown a single shell line.
#
# ---------------------------------------------------------------------------------
# THE DEFECT THIS FIXES (live 2026-08-30, hit by hand twice that morning)
# ---------------------------------------------------------------------------------
# The old hook read the open day out of vault/diet-today.js and regenerated THAT day,
# always, whatever the edit was. A backfill writes a row dated something else. So:
#
#   append a magnesium row dated 2026-08-29 while the open day is 2026-08-30
#     -> hook rebuilds 2026-08-30, finds nothing changed
#     -> diet-logs/days/2026-08-29.js keeps its pre-backfill state, silently
#
# Reproduced twice on 2026-08-29 (a magnesium row at 07:52, a walk at 09:49). Nothing
# reported it; verify-diet-consistency.js catches it, but only when a human points it at
# that date by hand.
#
# THE FIX: derive the affected dates FROM THE ROWS THAT CHANGED rather than from the
# clock or from the live file, and rebuild each one. The dates come from `git diff HEAD`
# on the CSV, taking column 1 of every added AND removed line:
#   * added lines cover appends and backfills;
#   * removed lines cover a row that was retracted, and the OLD date of a row whose date
#     was corrected (both old and new days need rebuilding, and one edit can touch many);
#   * a whole-file Write diffs the same way, so it needs no special case.
#
# The open day is ALWAYS rebuilt too, on top of whatever the diff found. That keeps the
# common path byte-identical to the old behaviour, and it refreshes the open day's
# rolling7 window, which a backfill into the last seven days legitimately changes.
#
# FALLBACK: if git cannot answer (untracked CSV, no HEAD, git missing), it rebuilds the
# open day alone — the old behaviour — and says so on stderr rather than failing the
# edit. Silence would put us back where we started.
#
# Exit codes: 0 = done (or not a diet CSV); 2 = something the caller must fix.

set -uo pipefail

NODE=/opt/homebrew/bin/node
[ -x "$NODE" ] || NODE=node

# ---- 1. which file did the tool touch ------------------------------------------------
payload="$(cat)"
fp="$(printf '%s' "$payload" | "$NODE" -e '
let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{
  try{process.stdout.write((JSON.parse(s).tool_input||{}).file_path||"")}catch(e){}
})' 2>/dev/null)"

case "$fp" in
  */diet-logs/*.csv) ;;
  *) exit 0 ;;
esac

cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0

# ---- 2. the open day, read from the live file (never from the clock) ------------------
open_day="$(sed -n 's/.*date: "\([0-9-]*\)".*/\1/p' vault/diet-today.js | head -1)"
case "$open_day" in
  [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
  *)
    echo "diet: could not read the current day from vault/diet-today.js — fix that file, then re-run the three node vault/*.js scripts with --day <the diet day>." >&2
    exit 2 ;;
esac

# ---- 3. every diet day the edit actually touched --------------------------------------
# Column 1 of each changed line is the Date, in all three CSVs (food, exercise, weight).
# -U0 keeps context lines out, so nothing unchanged is picked up.
days_file="$(mktemp -t dietdays)" || exit 0
trap 'rm -f "$days_file"' EXIT

git_ok=1
if command -v git >/dev/null 2>&1 && git rev-parse --verify HEAD >/dev/null 2>&1; then
  git diff -U0 HEAD -- "$fp" 2>/dev/null \
    | grep -E '^[+-][0-9]{4}-[0-9]{2}-[0-9]{2},' \
    | cut -c2- | cut -d, -f1 >> "$days_file"
else
  git_ok=0
fi

if [ "$git_ok" -eq 0 ]; then
  echo "diet: git could not report which rows changed in $fp — rebuilding only the open day ($open_day). If this was a backfill, run: node vault/generate-diet-today.js --day <that day>" >&2
fi

echo "$open_day" >> "$days_file"
days="$(sort -u "$days_file")"

# ---- 4. rebuild each affected day: generate, validate, verify -------------------------
# Archives first, the open day last, so the live file is the final thing written.
failed=""
for day in $(printf '%s\n' "$days" | grep -v "^${open_day}\$") "$open_day"; do
  [ -n "$day" ] || continue
  if "$NODE" vault/generate-diet-today.js --day "$day" >/dev/null \
  && "$NODE" vault/validate-diet-today.js  --day "$day" >/dev/null \
  && "$NODE" vault/verify-diet-consistency.js --day "$day" >/dev/null; then
    :
  else
    failed="$failed $day"
  fi
done

if [ -n "$failed" ]; then
  echo "diet-today regenerate/validate FAILED after editing $fp, for day(s):$failed — likely a CSV-quoting issue in the new row (a comma in an unquoted field); fix the row and re-run the three node vault/*.js scripts with --day for each day listed." >&2
  exit 2
fi

# Say so when a backfill was handled, so an off-day rebuild is visible rather than silent.
extra="$(printf '%s\n' "$days" | grep -v "^${open_day}\$" | tr '\n' ' ')"
[ -n "$extra" ] && echo "diet: rebuilt archive(s) for off-day row(s): $extra" >&2

# ---- 5. durability ---------------------------------------------------------------------
bash vault/diet-commit.sh >/dev/null 2>&1 \
  || echo "diet: regenerated+validated OK, but the durability commit failed — the 15-min autocommit will still persist it." >&2
exit 0
