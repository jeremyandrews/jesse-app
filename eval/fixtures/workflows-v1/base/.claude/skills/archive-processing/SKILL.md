---
name: archive-processing
description: Process draft and research files whose archive footer has a checked box (Archive, Deep extract, Archive only): find them, extract what matters, and move them to archive. Discovery is the trigger; never ask whether to process.
---

# archive-processing

## 1. Find checked files

From the repository root run the pinned script:

    ./.claude/skills/archive-processing/find-checked-archive-boxes.sh

No hits means nothing to do.

## 2. Process each file

- **Archive**: extract the facts that clearly matter into the right project or People note.
- **Deep extract**: the same, with a lower bar.
- **Archive only**: no extraction.

## 3. Move it to archive, with a single `mv`

- Drafts: `vault/Projects/drafts/FILE` to `vault/Projects/drafts/archive/FILE`.
- Research: `vault/Projects/Research/FILE` to `vault/Projects/Research/archive/FILE`.
- **Idempotency guard, check first:** a name that already starts with `YYYY-MM-DD-` keeps its
  name unchanged. Only an undated name gains a `YYYY-MM-DD-` prefix (today's date). A name like
  `2026-08-06-2026-08-05-1937-foo.md` is the bug, not the convention.
- `mv` is the delete: never copy and leave the original, never `rm`.

## 4. Verify

The original is gone from its folder and present in `archive/`. Report one line per file.
