# Vault lint, nightly checklist

Read only, apart from the one report it writes. Run these checks in this order and report
each one, even when it finds nothing.

1. **Unprocessed archive boxes.** Any file in `vault/Projects/drafts/` or
   `vault/Projects/Research/` (not their `archive/` folders) with a checked archive box.
2. **Naming violations.** Drafts and research reports must be named
   `YYYY-MM-DD-HHMM-descriptive-name.md`; every other note `Hyphenated-Title-Case.md`.
   List every file that breaks its rule, with its path.
3. **Missing footers.** Drafts and research reports without the archive footer.
4. **Dash scan.** Any em dash or en dash in a note under `vault/Projects/`.
5. **Stale drafts.** Drafts older than 21 days by the date in their name.

## Report format

Write the findings to `vault/Inbox/YYYY-MM-DD-vault-lint.md` (today's date), one `##`
section per check, each finding a bullet naming the file's path. Change nothing else.
