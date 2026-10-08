#!/usr/bin/env bash
# find-checked-archive-boxes.sh
# Prints the paths of every draft/research file whose archive footer has a
# checked box (Archive / Deep extract / Archive only). Zero or a small handful
# of hits is normal; hundreds means an exclude was dropped.
#
# The grep pattern is PINNED here — do not retype it from memory into a
# guideline or another script. Judgment rules live in
# vault/Knowledge/Jesse-Guidelines/Archive-Footer-Guidelines.md.
#
# Pattern notes (see history/Archive-Footer-Guidelines-History.md for the incidents):
#   ^\- \[[xX]\] *\**   tolerates [x]/[X], leading spaces, and **bold** labels
#   --exclude-dir=archive   without it, every archived file matches its own
#                           footer (373 false positives, 2026-04-22)
#   --exclude='.fuse_hidden*'   FUSE artifacts are not real vault files
#
# Run from the repo root (~/jesse), or pass the root as $1.

set -euo pipefail

ROOT="${1:-.}"

grep -rl \
  --exclude-dir=archive \
  --exclude='.fuse_hidden*' \
  '^\- \[[xX]\] *\**\(Archive\|Deep extract\|Archive only\)' \
  "$ROOT/vault/Projects/drafts/" \
  "$ROOT/vault/Projects/Research/"
