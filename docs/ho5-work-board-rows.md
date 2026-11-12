# HO5: Work board rows that say what each project is about

HO2 cut each Work board row to a bold title and one status line: "Your turn", "N moving" or
"Next: <title>". On the live board, 20 of 32 strand notes have a `you:` Waiting on, so almost every
row said only "Your turn". It said that something was his, never what, and nothing said what the
project was about. This document records how the replacement was chosen. Every judgement was made
against a corpus taken from the live data, never invented rows.

## The corpus

The corpus is a **copy** of the vault's 32 `Strands/*.md` notes (not `archive/` or `history/`) and of
the run store. A local bridge from this branch ran against the copy, sandboxed: `launchctl`, `git`,
`gh`, `claude` and `codex` were denied and the disk gate held every queue. It served
`GET /jesse/strands` and `GET /jesse/runs`.

The scratch bridge cannot see the live launchd jobs, so it marks every running run interrupted. The
two runs really running at the time (GO256 and HO5) were kept running in the copy, so that "what is
moving" matched the live board. Nothing ran, and the copies were deleted afterwards.

## Use cases

- **A. Morning glance.** From the board alone, without tapping, say what each project is about now.
- **B. "What do I have to do?"** Find every row that needs him and read the action itself.
- **C. Something is running.** See which projects are moving and what they are doing.
- **D. Nothing needs him and nothing runs.** The row still says what comes next, never cut mid word.
- **E. A long or multi gate Waiting on** (K3s Cluster, Scolta, Jesse Pro, Jesse). The row stays at two
  lines and still names the first action.
- **F. Phone width** (iPhone 18 Pro, default and XL type) **and the Mac window.**
- **G. Scanning 20 or more rows.** The board stays a list he can scan, not a wall of text.

## Candidates

All three read the same derived phrases (below) and differ only in what goes where.

- **A, headline and action.** Line 1 is the bold title, with what is moving, else the note's headline,
  beside it in grey. Line 2 is what is his to do, in orange, else Next.
- **B, status word and headline.** Line 1 is the title. Line 2 is "Your turn" or "Running", then
  " · " and the headline.
- **C, action or progress.** Line 1 is the title. Line 2 is the first of: his action, what is moving,
  the headline, Next.

| Use case | A | B | C |
|---|---|---|---|
| A. Morning glance | passes | passes | fails: 20 rows show only the action |
| B. His action | passes | **fails**: "Your turn" never says what | passes |
| C. Running | passes | fails: "Your turn" hides a run (Goose) | fails: the action takes the only slot (Goose, Strands System) |
| D. Next | passes with a backstop: Next fills line 2 when nothing is his | passes with a backstop: Next only when there is no headline | passes |
| E. Long gates | passes: the first gate | **fails**: no action at all | passes |
| F. Phone and Mac | passes with a backstop: line 1 steps down at XL (see the layout below) | passes | passes |
| G. 20+ rows | passes | fails: about 20 orange "Your turn ·" lines | passes |

## Why B and C lost

- **B** says that something is his and never what. That is HO2's defect again, with a headline
  added. Its 20 orange "Your turn ·" rows are the same wall Jeremy complained about.
- **C** shows one thing per row. On the 20 rows that need him that thing is the action, so they lose
  what the project is about (A). Goose and Strands System also lose the run that is going (C).

Screenshots of both, taken on the same data, are in `docs/ho5-screenshots/ios/rejected-*.png`.

## The design (A)

**Line 1** is the bold title, with grey text beside it:

- what is moving: `Running: Ablation A2, 1 queued`;
- else the note's headline: `Two issues drafted, none released`.

**Line 2** is:

- what is his to do, in orange: `For you: generate the minisign keypair per Release Runbook section 1`;
- else what comes next, in grey: `Next: Issue 002 v3`.

**Layout.** The row uses the widest arrangement that fits, and never goes past two lines:

1. The grey text beside the title, cut on a word, and never shorter than four words.
2. Else only the word for what is moving (`Running`).
3. Else the title alone. If nothing is his to do, line 2 then shows what is moving, else Next, else
   the headline.

**Cuts.** Every cut falls on a word boundary, never after a small word such as "and" or "the", and a
quotation that is cut still closes. Lines run at most 560 points wide, so a Mac window does not
stretch them.

**Everything else is kept from HO2 and HO4:** the group colour bar, the collapsible Dormant group,
the long press menu, the order and Edit toolbar items, the Runs button, Since your last look, and
the project view.

**The project view.** Its Your turn section leads with the row's exact words
(`For you: <action>`), with the whole Waiting on under them. For VoiceOver the row is spoken as its
title, then HO2's state ("Your turn", "2 moving"), then its two lines.

### The derivation

Everything here is deterministic and makes no model call. The bridge serves three new fields per
strand, from `bridge/src/hostrun/work/row.rs`. The app adds the runs.

**`headline`**, from `**Now:**`:

- Markdown, parentheticals and PR numbers go. A bare build number such as `(213)` stays.
- The sentence that names today's date wins, with its weekday and date dropped and its time kept.
  Otherwise the first sentence is used, unless it is one word (`trovato.rs.`), a `No …` remark, or
  a lone verb.
- The sentence is cut at its first `; `, ` — `, `, which `, `, so `, and at `: `, unless what comes
  before the colon is one word (`Next:`, `Ongoing:`).
- A clause that opens on step ids is reworded around the step's title. `GO123 merged` becomes
  `Merged: arm64 Chromium fallback chain`; dates leave the verb, `running` reads `Running` and
  `run is done` reads `Finished`. When the text after the colon opens on a quoted name, the name is
  used instead (`Finished: The Five-Minute Bargain`). When the rest of the clause is long, the ids
  are dropped instead.
- A step id in the lead parenthetical (`kc1 fixed for good (CL16c)`) names the clause, unless the
  clause already shares a word with that step's title.
- Branches, machine short names (`ks1`), paths, commit hashes and the remaining ids are dropped. A
  run of ids reads `N drafts`, or `6 Blog drafts` when they share a title. `Jeremy` reads `you`, and
  the verb after it agrees.

**`your_action`**, from a `you:` Waiting on:

- The first gate is kept (cut at `; `), and its first sentence.
- A lead-in (`for S23, Part 0:`) goes. So does a tail: `, which …`, `, and …`, `, then …`,
  `boarded …`.
- A possessive step id (`OT2's five questions`) becomes that step, named from its whole text.
- `-ing` verbs read as instructions (`installing` → `install`), and `whether` reads
  `decide whether`.
- A branch reads as its kind (`on the release branch`). A path reads as its file name, a URL as its
  host.
- A remark about the note gives way to the first queued line: `every open line here is … of yours`,
  `… only you can have`, `… is yours to run`. A verb in the remark is kept
  (`run the launch checklist`).

**`next_title`**: the first queued Drafts line, named from its whole text rather than its 57
character title. That is its first clause; a one or two word label keeps the first clause of what
follows it. It is scrubbed as above.

**The app adds the runs.** In order:

- **His action:** a waiting run's question, then a cut off run to resume, then a failed run, then
  the note's `your_action`. A bridge before 0.205.0 sends no `your_action`; the app then falls back
  on the gate's own words. A `you:` gate the bridge could not put in words (`you: PR 331`) reads
  `For you: see the note`, so it is never his without saying so.
- **What is moving:** the Studio runs (running before paused before queued), then a Drafts line
  under Running that no Studio run covers.
- **Next:** `next_title`, without parentheticals or a `, due …` tail.
- **Dedupe:** when the headline and Next say the same thing, line 2 keeps it and the grey text goes.

## Review rounds (pass 2)

Each round used three fresh reviewers who had not seen the reasoning, one per lane: information,
brevity and scanning, and derivation. Each was given the screenshots, the corpus and the use cases.
Must-fixes are listed below, one line each.

- **Round 1.** All three reviewers chose A. Fixed:
  - **Build number lost.** "install App 1.0" lost its `(213)`/`(203)`; a bare build number is now kept.
  - **Possessive id dropped.** "You: five questions" lost "OT2's"; the step is now substituted.
  - **Remarks as actions.** Family showed "every open line here is … of yours"; a remark now gives way
    to the first queued line.
  - **Bare domain headline.** Trovato Site read just "trovato.rs"; a one word sentence is now skipped.
  - **Today missed.** A bid project's feedback call is today; a sentence naming today now wins.
  - **Repeated Next.** A family errand and Editorial said the same thing twice; a looser dedupe now catches it.
  - **XL stubs.** The grey text shrank to stubs; it now needs at least three words.
  - **Mac width.** Lines ran the full 2000px; they are now capped at 560pt.
  - **Open quote.** A cut question left its quote open; it now closes.
  - **False "Running:".** Scolta showed "Running:" though no run existed; that line read
    "In progress:" from round 1 to round 5 (see round 5).
- **Round 2.** Fixed:
  - **Stubs at XL:** the minimum went from three words to four.
  - **Next titles:** parentheticals and `, due …` tails are dropped.
  - **Goose's queued run:** it was counted as running; it now reads `, 1 queued`.
  - **Leaks:** paths, URLs, a branch, commit hashes and a leading machine name are dropped.
  - **Id lists:** `N drafts`.

  Not a derivation fault (follow-on): Jesse's stale "App 1.0 (203)" gate, and a bid project's
  "due end of today" line.
- **Round 3.** Fixed:
  - **Editorial blank at XL:** the row showed nothing but Next; the layout became a set of choices
    tried widest first.
  - **Strands System's run lost at XL:** the one-word `Running` marker now stays.
  - **Goose's branch:** an action lost the branch it is about; branches now stay in actions.
- **Round 4.** Fixed:
  - **Next lost:** the fallback layout dropped Next (D); it now keeps Next.
  - **Noun-phrase actions:** "You: the Apple account setup" read as a label; actions now read
    "For you: …".
  - **Cut Next titles:** Next came from the cut title ("…against the 87"); it now comes from the
    line's whole text.
- **Round 5.** Fixed:
  - **Unseen running work:** a Drafts line under Running with no Studio run showed nothing; it now
    reads `Running: <title>`, and "In progress" became "Running" everywhere.
- **Pass 3, natural and obvious** (between rounds 5 and 6; see the next section).
- **Round 6** (the cap, and pass 3's rerun of pass 2). Fixed:
  - **Editorial at XL:** the marker layout showed Next instead of what is running.
  - **Tag1 Writing headline:** "Finished: the rewrite prompt, one…" now reads the quoted name.
  - **Tag1 action:** it lost "and answer its five questions"; the step is now named from its whole
    text.

  These three fixes were made after the last design round, so no design reviewer saw them. Each has
  a test, and the code review loop read them.

The loop **ended at the cap of six rounds**. It never had two clean rounds in a row: rounds 1 to 6
each found at least one new must-fix.

## Pass 3: natural and obvious

The design was reread against one test: every word on a row is one Jeremy already uses, and nothing
is a label he has to learn. Changes:

- **The same state had two words**, "In progress" and "Running". It is now "Running" everywhere.
- **A branch name reached a row** in an action ("branch protection on release-0.19"). The branch now
  reads as its kind: "on the release branch".
- **"Jeremy" appeared on his own board.** It now reads "you": "the tool you call tree search",
  "Your health…".
- **Machine short names reached rows** after "on" or "from" ("Clean rerun on ks1"). They are dropped.
- **No new metadata.** Nothing on the row needs a note change or a new field in a note.

Not changed:

- **Tool and repository names stay** (`npm run flush:expand-cache`, `jesse-app`, a CA certificate file).
  They are the objects of the actions, written in his own notes.
- **`kc1` stays** in "kc1 fixed for good". Every clean alternative also contains it, because it is in
  the step's title.

## Follow-ons

These come from the notes, not the derivation. The rows quote the notes faithfully.

- **Definitions, not status.** Several Now lines say what the project is rather than how it stands:
  Homelab ("All home infrastructure"), Health, Trovato, Trovato Ritrovo, Vault Links. The headline
  inherits that. The `strand-update` turn could write Now as status. That is a writing convention,
  not a format change.
- **Stale notes.** Jesse's first gate asks for App 1.0 (203) while Strands System asks for (213).
  A bid project's Next is an October line "due end of today", and another project's Next is an October course. These
  need note upkeep.
- **Gates with no verb.** Some gates are noun phrases ("the read only user on the router"). The
  row reads "For you: …" so they still parse. A note convention of opening a gate on a verb would
  make every action an instruction.
- **Hidden gates.** Gates past the first are not counted on the row. The project view lists the whole
  Waiting on.
- **Unmarked gates.** A gate that is not written `you:` (one bid project's "your … date") is
  not recognised as his. No note format change is proposed; the convention is already `you:`.
