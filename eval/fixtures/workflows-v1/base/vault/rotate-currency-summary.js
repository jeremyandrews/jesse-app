#!/usr/bin/env node
// rotate-currency-summary.js — keep the running currency summaries bounded by moving
// their oldest rows into a per-year archive beside them.
//
//   node vault/rotate-currency-summary.js [--keep=60] [--dry-run]
//
// WHY THIS EXISTS: the live summaries are read whole by the daily currency turn, and a
// table that grows without limit eventually costs more context than the analysis it feeds.
// The ceiling is a row COUNT, not bytes, because the rows are wildly uneven (one Key Driver
// cell runs past 11k characters) and a byte ceiling would cut mid-history at an arbitrary
// place.
//
// Contract (see Jesse-Guidelines/Daily-Currency-Tracking.md):
//   - The live file keeps the newest KEEP data rows. Rows are stored NEWEST FIRST, so the
//     kept rows are the first KEEP of the table.
//   - Everything older moves into `<name>-archive-<year>.md`, chosen by the YEAR OF THE ROW,
//     so a rotation spanning a new year splits correctly instead of dumping into one file.
//   - Moved rows are copied BYTE FOR BYTE. No reflow, no re-alignment, no reordering — the
//     line that leaves the live table is the line that lands in the archive.
//   - The archive is APPENDED INTO, never overwritten. Both 2026 archives already exist with
//     a heading, a provenance paragraph and a table header from the 2026-08-14 hand run;
//     clobbering those is the one failure this script must not have.
//   - IDEMPOTENT: with nothing to move, not a single byte changes — the script does not
//     rewrite the pointer, re-stamp a date, or touch mtime.
//
// Rows are merged into the archive in date-descending order (the archive's own convention).
// In the normal case every moved row is newer than everything already archived, so they land
// at the top of the existing table; the merge is stable, so existing archive rows keep their
// relative order regardless.

'use strict';

const fs = require('fs');
const path = require('path');

const VAULT_REL = 'vault/Projects/Research/Currency-Tracking';
const TARGETS = ['USD-EUR-Summary.md', 'USD-BTC-Summary.md'];
const WIKI_PREFIX = 'todo-list/Projects/Research/Currency-Tracking';

// The pointer line the live file carries, and the pattern that recognises a previous one so
// a re-run REPLACES it rather than stacking a second.
const POINTER_RE = /^Entries before \d{4}-\d{2}-\d{2} \(.*\) were rotated out on \d{4}-\d{2}-\d{2} under the size ceiling: see \[\[.*\]\]\.$/;

function arg(name, fallback) {
  const hit = process.argv.slice(2).find((a) => a.startsWith(`--${name}=`));
  return hit ? hit.slice(name.length + 3) : fallback;
}
const KEEP = Number(arg('keep', '60'));
const DRY_RUN = process.argv.includes('--dry-run');

/** Today in Europe/Rome, matching the other vault scripts' day boundary. */
function today() {
  return new Date().toLocaleDateString('en-CA', { timeZone: 'Europe/Rome' });
}

/** The date in a table row's first cell, or null if the line is not a dated data row. */
function rowDate(line) {
  const m = /^\|\s*(\d{4})-(\d{2})-(\d{2})\s*\|/.exec(line);
  return m ? { iso: `${m[1]}-${m[2]}-${m[3]}`, year: m[1] } : null;
}

/**
 * Split a summary file into its parts. The table is located by its separator row rather than
 * by a fixed line number, because the live files and the archives carry different preambles.
 */
function parse(text) {
  const lines = text.split('\n');
  const sep = lines.findIndex((l) => /^\|[-:\s|]+\|\s*$/.test(l));
  if (sep < 1) return null;
  let end = sep + 1;
  while (end < lines.length && rowDate(lines[end])) end++;
  return {
    head: lines.slice(0, sep + 1), // preamble + column header + separator
    rows: lines.slice(sep + 1, end),
    tail: lines.slice(end),
  };
}

/** Stable merge of two date-descending row lists. */
function mergeDesc(incoming, existing) {
  const out = [];
  let i = 0;
  let j = 0;
  while (i < incoming.length && j < existing.length) {
    const a = rowDate(incoming[i]);
    const b = rowDate(existing[j]);
    if (a && b && a.iso < b.iso) out.push(existing[j++]);
    else out.push(incoming[i++]);
  }
  return out.concat(incoming.slice(i), existing.slice(j));
}

function archiveFor(dir, base, year, sourceStem, header, sep, span) {
  const file = path.join(dir, `${base}-archive-${year}.md`);
  if (fs.existsSync(file)) return { file, existing: parse(fs.readFileSync(file, 'utf8')) };
  // First rotation into this year: build the same shape the hand run left behind.
  const title = /USD-EUR/.test(base) ? 'USD/EUR' : 'USD/BTC';
  return {
    file,
    existing: {
      head: [
        `# ${title} Running Summary, Archive ${year}`,
        '',
        `Older entries rotated out of [[${WIKI_PREFIX}/${sourceStem}]] on ${today()} under the ` +
          `output size ceiling in [[todo-list/Knowledge/Jesse-Guidelines/Daily-Currency-Tracking]] ` +
          `(live summary keeps the most recent ${KEEP} entries). Newest first. Covers ${span}.`,
        '',
        header,
        sep,
      ],
      rows: [],
      tail: [''],
    },
  };
}

function rotate(dir, name) {
  const live = path.join(dir, name);
  const stem = name.replace(/\.md$/, '');
  const parsed = parse(fs.readFileSync(live, 'utf8'));
  if (!parsed) {
    console.error(`rotate: ${name}: no table found — refusing to guess. Nothing written.`);
    return 1;
  }
  const { head, rows, tail } = parsed;
  if (rows.length <= KEEP) {
    console.log(`${name}: ${rows.length} rows, ceiling ${KEEP} — nothing to rotate.`);
    return 0;
  }

  const kept = rows.slice(0, KEEP);
  const moved = rows.slice(KEEP);

  // Group the moved rows by their own year, preserving order within each group.
  const byYear = new Map();
  for (const r of moved) {
    const d = rowDate(r);
    if (!byYear.has(d.year)) byYear.set(d.year, []);
    byYear.get(d.year).push(r);
  }

  const written = [];
  for (const [year, group] of byYear) {
    const dates = group.map((r) => rowDate(r).iso).sort();
    const span = `${dates[0]} through ${dates[dates.length - 1]}`;
    const { file, existing } = archiveFor(
      dir, stem, year, stem, head[head.length - 2], head[head.length - 1], span,
    );
    const merged = mergeDesc(group, existing.rows);
    const out = [...existing.head, ...merged, ...existing.tail].join('\n');
    if (!DRY_RUN) fs.writeFileSync(file, out);
    written.push({ file: path.basename(file), count: group.length, span });
  }

  // One pointer line in the live file, replacing any previous one.
  const movedDates = moved.map((r) => rowDate(r).iso).sort();
  const oldestKept = rowDate(kept[kept.length - 1]).iso;
  const pointer =
    `Entries before ${oldestKept} (${movedDates[0]} through ${movedDates[movedDates.length - 1]}) ` +
    `were rotated out on ${today()} under the size ceiling: see ` +
    written.map((w) => `[[${WIKI_PREFIX}/${w.file.replace(/\.md$/, '')}]]`).join(', ') + '.';

  const newTail = tail.slice();
  const at = newTail.findIndex((l) => POINTER_RE.test(l));
  if (at >= 0) newTail[at] = pointer;
  else newTail.splice(newTail[0] === '' ? 1 : 0, 0, pointer, '');

  if (!DRY_RUN) fs.writeFileSync(live, [...head, ...kept, ...newTail].join('\n'));
  console.log(
    `${name}: kept ${kept.length}, moved ${moved.length} → ` +
      written.map((w) => `${w.file} (+${w.count}, ${w.span})`).join(', ') +
      (DRY_RUN ? '   [dry run, nothing written]' : ''),
  );
  return 0;
}

function main() {
  const dir = path.resolve(process.cwd(), VAULT_REL);
  if (!fs.existsSync(dir)) {
    console.error(`rotate: ${dir} does not exist — run from the vault root.`);
    process.exit(1);
  }
  let rc = 0;
  for (const t of TARGETS) {
    if (!fs.existsSync(path.join(dir, t))) {
      console.error(`rotate: ${t} missing — skipped.`);
      rc = 1;
      continue;
    }
    rc = rotate(dir, t) || rc;
  }
  process.exit(rc);
}

main();
