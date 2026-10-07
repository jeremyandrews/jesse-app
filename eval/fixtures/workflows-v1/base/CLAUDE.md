# Vault working rules

This is Alex Example's personal vault: a git repository whose notes live under `vault/`.
Alex reaches you from a phone app. Everything you know about Alex is in this vault.

## Hard rules

- **Never send outbound communication.** You draft; Alex sends.
- **Record durable facts to the right vault file immediately**; never ask permission to save.
- **No dash punctuation in anything you write into the vault:** no em dash, no en dash, no
  double hyphen. Hyphens inside compound words are fine.
- **Everything is Markdown.**

## Layout

- `vault/Today.md`: Alex's living task list. Read it before you change it. A done item is
  ticked in place (`- [ ]` becomes `- [x]`); never delete a line.
- `vault/Projects/`: project notes, the source of truth for what is going on.
- `vault/Projects/drafts/`: every draft you write for Alex (emails, messages, notes, prompts).
- `vault/Projects/Research/`: researched reports. A substantive question produces a report
  here, not only a chat reply.
- `vault/Knowledge/People/`: one note per person.
- `vault/Inbox/`: quick capture, and where unattended jobs write their reports.
- `diet-logs/`: the diet CSVs (food, exercise, weight). `vault/diet-today.js` is DERIVED
  from them; never hand-edit its meals, weight or exercise.
- `Code/<host>/<owner>/<repo>`: code checkouts. A review of a repository clones it here,
  for example `Code/github.com/acme/widget`, never anywhere else.

## Naming

- Drafts and research reports: `YYYY-MM-DD-HHMM-descriptive-name.md`, 24 hour local time.
- Every other note: `Hyphenated-Title-Case.md`.
- Archived files keep a name that already starts with a date; an undated name gains a
  `YYYY-MM-DD-` prefix when it is archived.

## Drafts and research reports

Every draft and every research report ends with this archive footer, as its last lines:

```
---

- [ ] Archive (extract key info, then archive)
- [ ] Deep extract (extract with a lower bar for what qualifies, then archive)
- [ ] Archive only (skip extraction, archive as-is)
```

A checked box is the instruction: process it with the `archive-processing` skill.

## Diet

Any mention of eating, drinking, a workout or a weight number IS the instruction to log it.
Follow the `diet-logging` skill (`.claude/skills/diet-logging/SKILL.md`).

## Currency

The running summaries are `vault/Projects/Research/Currency-Tracking/USD-EUR-Summary.md` and
`USD-BTC-Summary.md`, newest row first. After adding a row, rotate them by running exactly
`node vault/rotate-currency-summary.js` from the repository root.
