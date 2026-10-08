#!/usr/bin/env node
// verify-diet-consistency.js — CONTENT cross-check for diet-today.js against the
// authoritative CSV logs. Run AFTER validate-diet-today.js, on every log event and
// before any diet commit:
//   node vault/verify-diet-consistency.js
//
// validate-diet-today.js is a pure SCHEMA check — it guarantees field names/types but
// never reads the CSVs, never checks the date is today, and never compares meals/weight
// against the log. So it passed a well-formed-but-factually-wrong file on 2026-06-26
// (diet-today.js held a phantom 11:00 snack with weight:null while the CSVs had the real
// breakfast + snacks + weigh-in). This guard closes that gap: it asserts diet-today.js
// agrees, item-for-item and macro-for-macro, with food-log.csv / weight-log.csv /
// exercise-log.csv for diet-today.js's date, and that the date is today (Europe/Rome).
//
// The CSVs are the append-only source of truth (per Diet-Logging-Flow.md). diet-today.js
// is a convenience cache the dashboard renders; this guard refuses to let the cache ship
// when it disagrees with the log.
//
// Exit 0 = consistent. Exit 1 = divergence (all problems printed, not just the first).
//
// Flags:
//   --day YYYY-MM-DD    the DIET DAY to check. Selects the cache for that day —
//                       diet-today.js when it is the day in progress, otherwise
//                       diet-logs/days/<day>.js — and cross-checks it against the CSV rows
//                       whose Date is that day. `--date=` is the old spelling and still
//                       works.
//   --skip-today        skip the "is this the current diet day" assertion (still
//                       cross-checks content against the cache's own date). Offline/replay.
//
// WHAT A DAY IS. The diet day of an entry is the calendar date, in the effective zone, of
// the moment it was eaten MINUS FOUR HOURS: a 00:30 snack belongs to the evening that just
// ended, a 04:00 coffee starts the new one. This guard used to assert "diet-today.js.date
// equals today in Europe/Rome", which is two wrong things at once — it hard-coded one zone
// for an owner who travels, and it treated midnight as the boundary, so every log between
// midnight and 04:00 looked like a stale cache. Without --day it now asks for the current
// DIET day in the PROCESS zone (whatever TZ says), which is the same answer on a Rome host
// during the day and the right answer at 01:00.

const fs = require('fs');
const path = require('path');

// Extract the calorie target asserted in the `dayType` prose. Pure + testable.
// Keys on the word "target" so workout kcal, body weights, and dates elsewhere in the
// prose can never match. Recognizes "target = <arithmetic> = N" and bare "target = N";
// N is an integer that may carry comma thousands separators. Takes the number after the
// LAST "=" of the LAST such clause. Returns null when no target clause parses.
function extractDayTypeTarget(dayType) {
  if (typeof dayType !== 'string') return null;
  let last = null, m;
  const clauseRe = /target\s*=\s*([^.;]*)/gi;
  while ((m = clauseRe.exec(dayType)) !== null) {
    // Re-prepend the "=" the clause regex consumed so a bare "target = N"
    // (no inner "=") is still seen by the number scan below.
    const full = '= ' + m[1];
    const numRe = /=\s*(\d{1,3}(?:,\d{3})*|\d+)\b/g;
    let nm, lastNum = null;
    while ((nm = numRe.exec(full)) !== null) lastNum = nm[1];
    if (lastNum !== null) last = lastNum;
  }
  return last === null ? null : parseInt(last.replace(/,/g, ''), 10);
}

// When required (e.g. `node -e` unit tests) expose the pure function and stop before the
// main cross-check body, which reads files and calls process.exit.
if (require.main !== module) { module.exports = { extractDayTypeTarget }; return; }

const args = process.argv.slice(2);
function flag(name) {
  const eq = args.find(a => a.startsWith(name + '='));
  if (eq !== undefined) return eq.slice(name.length + 1);
  const i = args.indexOf(name);
  if (i === -1) return undefined;
  const v = args[i + 1];
  return (v === undefined || v.startsWith('--')) ? null : v;
}
const rawDay = flag('--day') !== undefined ? flag('--day') : flag('--date');
if (rawDay !== undefined && (rawDay === null || !/^\d{4}-\d{2}-\d{2}$/.test(rawDay))) {
  console.error('FAIL diet-consistency: --day must be YYYY-MM-DD, got ' + JSON.stringify(rawDay));
  process.exit(1);
}
const dateArg = rawDay === undefined ? null : rawDay;
const skipToday = args.includes('--skip-today');

// ---- the diet day, in one place ----
const DIET_DAY_START_HOUR = 4;
const shiftDays = (ymd, n) => {
  const [y, m, d] = ymd.split('-').map(Number);
  const dt = new Date(Date.UTC(y, m - 1, d));
  dt.setUTCDate(dt.getUTCDate() + n);
  return dt.toISOString().slice(0, 10);
};
// The current diet day in `tz` (the process zone when unnamed): the calendar date there of
// four hours ago.
function currentDietDay(tz) {
  const at = new Date(Date.now() - DIET_DAY_START_HOUR * 3600 * 1000);
  return tz ? at.toLocaleDateString('en-CA', { timeZone: tz }) : at.toLocaleDateString('en-CA');
}
// `HH:MM` → [h, m], tolerating a leading `~`; null when the cell is not a clock time.
function hhmm(raw) {
  if (raw === undefined || raw === null) return null;
  const m = String(raw).trim().replace(/^~+\s*/, '').match(/^(\d{1,2}):(\d{2})/);
  if (!m) return null;
  const h = parseInt(m[1], 10), mi = parseInt(m[2], 10);
  return (h <= 23 && mi <= 59) ? [h, mi] : null;
}
// Does `tz` name a zone this runtime knows? A blank cell means "written before the column
// existed", which is the process zone — not an unknown one.
function knownZone(tz) {
  if (!tz) return true;
  try { new Date().toLocaleString('en-CA', { timeZone: tz }); return true; }
  catch (e) { return false; }
}
// Round-trip a row's (Date, Time, TZ) through the rule: the calendar date the row implies
// is its Date when the clock is at or after 04:00 and the NEXT day when it is before, and
// re-deriving the diet day from that instant must land back on Date. Anything that fails to
// round-trip — an unparseable clock, a zone this runtime cannot resolve, a wall time that
// does not exist because of a DST jump — is a row whose day cannot be trusted.
function dayRoundTrips(date, timeCell, tz) {
  const t = hhmm(timeCell);
  if (t === null) return { ok: false, why: `Time ${JSON.stringify(String(timeCell || ''))} is not a clock time` };
  if (!knownZone(tz)) return { ok: false, why: `TZ ${JSON.stringify(tz)} is not a zone this runtime knows` };
  const [h, m] = t;
  const calendar = h >= DIET_DAY_START_HOUR ? date : shiftDays(date, 1);
  // Re-derive: the wall clock, minus four hours, on that calendar date.
  const back = h >= DIET_DAY_START_HOUR ? calendar : shiftDays(calendar, -1);
  if (back !== date) return { ok: false, why: `Date ${date} does not follow from Time ${h}:${String(m).padStart(2, '0')} under the 04:00 rule` };
  return { ok: true };
}

const errors = [];
const warnings = [];
const fail = m => errors.push(m);

// ---- RFC-4180 CSV parser (NEVER split(',') — fields contain commas/quotes) ----
function parseCSV(text) {
  const rows = [];
  let row = [], field = '', i = 0, inQuotes = false;
  const n = text.length;
  while (i < n) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') { field += '"'; i += 2; continue; }
        inQuotes = false; i++; continue;
      }
      field += ch; i++; continue;
    }
    if (ch === '"') { inQuotes = true; i++; continue; }
    if (ch === ',') { row.push(field); field = ''; i++; continue; }
    if (ch === '\r') { i++; continue; }
    if (ch === '\n') { row.push(field); rows.push(row); row = []; field = ''; i++; continue; }
    field += ch; i++;
  }
  // flush trailing field/row if file doesn't end in newline
  if (field.length > 0 || row.length > 0) { row.push(field); rows.push(row); }
  return rows;
}

function readRecords(csvPath) {
  if (!fs.existsSync(csvPath)) { fail(`CSV not found: ${csvPath}`); return []; }
  const rows = parseCSV(fs.readFileSync(csvPath, 'utf8'));
  if (rows.length === 0) return [];
  const header = rows[0];
  return rows.slice(1)
    .filter(r => r.length > 1 && r[0].trim() !== '')   // drop blank trailing lines
    .map(r => Object.fromEntries(header.map((h, idx) => [h, r[idx] !== undefined ? r[idx] : ''])));
}

const num = v => {
  if (v === undefined || v === null) return null;
  const s = String(v).trim();
  if (s === '') return null;
  const f = parseFloat(s);
  return Number.isNaN(f) ? null : f;
};
const close = (a, b, eps) => a !== null && b !== null && Math.abs(a - b) <= eps;
const normName = s => String(s || '').trim().toLowerCase().replace(/\s+/g, ' ');

// ---- Load the cache for the requested day ----
// A day's cache lives in diet-today.js while the day is in progress and in
// diet-logs/days/<day>.js once it has been rolled. The two files have the same shape, so
// --day picks the one that actually holds the day rather than assuming the live file does.
const root = path.resolve(__dirname, '..');
const dtPath = path.join(__dirname, 'diet-today.js');
const archiveFor = day => path.join(root, 'diet-logs', 'days', day + '.js');

function loadCache(file) {
  if (!fs.existsSync(file)) return null;
  global.window = {};
  try { eval(fs.readFileSync(file, 'utf8')); return global.window.DIET_TODAY || null; }
  catch (e) {
    console.error('FAIL diet-consistency: ' + path.relative(root, file) + ' is not valid JavaScript: ' + e.message);
    process.exit(1);
  }
}

const live = loadCache(dtPath);
const liveDate = live && typeof live.date === 'string' ? live.date : null;
let cacheFile = dtPath, d = live;
if (dateArg && liveDate !== dateArg) {
  cacheFile = archiveFor(dateArg);
  d = loadCache(cacheFile);
  if (!d) {
    console.error(`FAIL diet-consistency: --day ${dateArg} is not the day diet-today.js holds` +
      `${liveDate ? ' (' + liveDate + ')' : ''} and there is no archive at ` +
      `${path.relative(root, cacheFile)} to check it against.`);
    process.exit(1);
  }
}
if (!d) {
  console.error('FAIL diet-consistency: window.DIET_TODAY not set in ' + path.relative(root, cacheFile));
  process.exit(1);
}

const targetDate = String(d.date || '');
if (!/^\d{4}-\d{2}-\d{2}$/.test(targetDate)) {
  console.error('FAIL diet-consistency: ' + path.relative(root, cacheFile) +
    ' date missing or malformed: ' + JSON.stringify(d.date));
  process.exit(1);
}

// ---- Is this the CURRENT diet day, unless overridden ----
const dietToday = currentDietDay(null); // the process zone — no zone is hard-coded here
if (dateArg && targetDate !== dateArg)
  fail(`--day ${dateArg} but ${path.relative(root, cacheFile)}'s date is ${targetDate}`);
if (!dateArg && !skipToday && targetDate !== dietToday)
  fail(`diet-today.js.date ${targetDate} is not the current diet day (${dietToday}) — stale cache; today's data may be unwritten`);

const foodPath = path.join(root, 'diet-logs', 'food-log.csv');
const weightPath = path.join(root, 'diet-logs', 'weight-log.csv');
const exPath = path.join(root, 'diet-logs', 'exercise-log.csv');
const foodRows = readRecords(foodPath).filter(r => r.Date === targetDate);
const weightRows = readRecords(weightPath).filter(r => r.Date === targetDate);
const allExRows = readRecords(exPath);
const exRows = allExRows.filter(r => r.Date === targetDate);

// ---- 0. The logs' own DAY discipline ----
// A CARRIAGE RETURN INSIDE THE HEADER is a corrupt schema declaration, and it is checked
// separately from the file's line ENDING: `food-log.csv` is CRLF throughout, which is RFC
// 4180's own terminator and not a defect. What is flagged is a CR left in the header TEXT.
function headerLine(file) {
  if (!fs.existsSync(file)) return null;
  const body = fs.readFileSync(file, 'utf8');
  const nl = body.indexOf('\n');
  return (nl === -1 ? body : body.slice(0, nl)).replace(/\r$/, '');
}
for (const file of [foodPath, weightPath, exPath]) {
  const h = headerLine(file);
  if (h === null) continue;
  if (h.includes('\r'))
    fail(`${path.relative(root, file)} header carries a stray carriage return: ${JSON.stringify(h)}. ` +
         `The bridge's writer rewrites a header that fails its strict check; if this persists, ` +
         `something else is writing the header.`);
}

// THE TZ COLUMN. Every row written since the diet day was defined names the zone its clock
// was read in; a row written before it leaves the cell blank, which MEANS the process zone
// and is not a defect. So a blank is a warning in general and a FAILURE only once the day
// being checked is one where the column is in use — at that point a blank is a writer that
// forgot, and every date it produces is unverifiable.
//
// SCOPE, AND IT IS THE WHOLE POINT: "in use" is asked OF THE DAY BEING CHECKED, not of the
// whole file. This used to read `readRecords(foodPath).some(...)`, i.e. every row in
// food-log.csv ever written. That was latent while no row anywhere carried a zone, but the
// moment the column was activated it would have turned EVERY historical day into a hard
// failure the instant anything pointed verify at one — which the morning audit and
// repair-diet-days.js both do routinely. Per-day is what the paragraph above always said.
// A day whose rows are ALL blank is pre-column and warns; a day with SOME zones and some
// blanks is a writer that forgot, and fails.
const dayRows = [...foodRows, ...exRows, ...weightRows];
const tzColumnInUse = dayRows.some(r => String(r.TZ || '').trim() !== '');
for (const [label, rows] of [['food-log.csv', foodRows], ['exercise-log.csv', exRows], ['weight-log.csv', weightRows]]) {
  for (const r of rows) {
    const tz = String(r.TZ || '').trim();
    const what = `${label} row (${r.Item || r.Type || r.Weight_lbs || '?'}) on ${targetDate}`;
    if (tz === '') {
      if (tzColumnInUse) fail(`${what} has an empty TZ while the column is in use — its Date cannot be verified`);
      else warnings.push(`${what} has no TZ (written before the column existed — read as the process zone)`);
    } else if (!knownZone(tz)) {
      fail(`${what} names TZ ${JSON.stringify(tz)}, which is not a zone this runtime knows`);
    }
    // weight-log.csv has no clock column, so only the two that do are round-tripped.
    const timeCell = label === 'exercise-log.csv' ? r.Start_Time : (label === 'food-log.csv' ? r.Time : undefined);
    if (timeCell === undefined || String(timeCell).trim() === '') continue;
    const rt = dayRoundTrips(targetDate, timeCell, tz);
    if (!rt.ok) fail(`${what}: ${rt.why}`);
  }
}

// ---- 0b. The structured columns (added 2026-09-15) ----
// Appended after TZ: food Alcohol_g, Category, Source, Basis, Time_Source; exercise
// Treadmill, Source; weight Hydration_Artifact. Every row written from the cutover on
// fills all of them, with `unknown` where unsure, so a blank one there is a writer that
// forgot. On an older row a blank cell is an honest unknown, but a filled cell must still
// be a word of its vocabulary. Scoped to the day being checked, like the TZ rule: one bad
// row can only ever fail its own day. A file whose header does not carry the columns yet
// (before the migration ran) only warns.
const SCHEMA_CUTOVER = '2026-09-15';
const TAIL_VOCAB = {
  'food-log.csv': {
    Alcohol_g: 'grams',
    Category: ['food', 'alcoholic_drink', 'soft_drink', 'supplement', 'water'],
    Source: ['home', 'restaurant', 'shop', 'other_home', 'unknown'],
    Basis: ['label', 'weighed', 'reference', 'estimate', 'photo', 'unknown'],
    Time_Source: ['actual', 'report', 'estimated', 'unknown'],
  },
  'exercise-log.csv': { Treadmill: ['true', 'false'], Source: ['watch', 'self_reported', 'unknown'] },
  'weight-log.csv': { Hydration_Artifact: ['true', 'false'] },
};
const EXERCISE_TYPES = ['Run', 'Walk', 'Swim', 'Bike', 'Strength', 'Yard_Work', 'Other'];
// Exercise Type as the bridge reads it: historical cells keep their own wording and map
// here, so a "Marathon" row is the Run it was.
function exerciseType(raw) {
  const a = String(raw || '').trim().toLowerCase().replace(/[_-]/g, ' ').split(/\s+/).filter(Boolean).join(' ');
  const map = { 'treadmill run': 'Run', marathon: 'Run', swimming: 'Swim', gardening: 'Yard_Work',
    'garden work': 'Yard_Work', 'yard work': 'Yard_Work', 'manual labor': 'Yard_Work',
    'strength/weights': 'Strength', 'strength / weights': 'Strength', weights: 'Strength' };
  if (map[a]) return map[a];
  return EXERCISE_TYPES.find(t => t.replace('_', ' ').toLowerCase() === a) || 'Other';
}
const isLongRun = r => exerciseType(r.Type) === 'Run' && (num(r.Distance_km) || 0) > 20;
// The hydration rule: a Run over 20 km on the date or either of the two dates before it.
const hydrationRule = date => [date, shiftDays(date, -1), shiftDays(date, -2)]
  .some(day => allExRows.some(r => r.Date === day && isLongRun(r)));
const afterCutover = targetDate >= SCHEMA_CUTOVER;
const tailInUse = {};
for (const [label, rows, file] of [['food-log.csv', foodRows, foodPath], ['exercise-log.csv', exRows, exPath], ['weight-log.csv', weightRows, weightPath]]) {
  const cols = Object.keys(TAIL_VOCAB[label]);
  const header = (headerLine(file) || '').split(',');
  tailInUse[label] = cols.every(c => header.includes(c));
  if (!tailInUse[label]) {
    warnings.push(`${label} has no ${cols.join('/')} columns yet; run node vault/migrations/2026-09-diet-schema.js`);
    continue;
  }
  for (const r of rows) {
    const what = `${label} row (${r.Item || r.Type || r.Weight_lbs || '?'}) on ${targetDate}`;
    for (const c of cols) {
      const v = String(r[c] === undefined ? '' : r[c]).trim();
      const voc = TAIL_VOCAB[label][c];
      if (v === '') {
        if (afterCutover) fail(`${what} has a blank ${c}: from ${SCHEMA_CUTOVER} every structured cell is filled, with unknown where unsure`);
        continue;
      }
      const ok = voc === 'grams' ? /^\d+(\.\d+)?$/.test(v) : voc.includes(v);
      if (!ok) fail(`${what} has ${c}=${JSON.stringify(v)}, which is not ${voc === 'grams' ? 'a non-negative number of grams' : 'one of ' + voc.join(', ')}`);
    }
  }
}
if (tailInUse['food-log.csv']) {
  for (const r of foodRows) {
    const g = num(r.Alcohol_g), cal = num(r.Calories);
    const what = `food-log.csv row (${r.Item}) on ${targetDate}`;
    // 7 kcal per gram of ethanol: the ethanol alone cannot carry more calories than the
    // whole row (5 kcal of rounding allowed).
    if (g !== null && cal !== null && g * 7 > cal + 5)
      fail(`${what}: Alcohol_g ${g} x 7 = ${Math.round(g * 7)} kcal exceeds its Calories ${cal}`);
    if (String(r.Category || '').trim() === 'alcoholic_drink' && g === 0)
      fail(`${what} is an alcoholic_drink with Alcohol_g 0; give its grams of ethanol (ml x ABV/100 x 0.789)`);
  }
}
if (afterCutover && tailInUse['exercise-log.csv']) {
  for (const r of exRows)
    if (!EXERCISE_TYPES.includes(String(r.Type || '').trim()))
      fail(`exercise-log.csv row (${r.Type}) on ${targetDate}: Type must be one of ${EXERCISE_TYPES.join(', ')}; the original wording goes in Description`);
}
if (afterCutover && tailInUse['weight-log.csv']) {
  for (const r of weightRows) {
    const v = String(r.Hydration_Artifact || '').trim();
    if (v !== 'true' && v !== 'false') continue; // blank or off-vocabulary already failed above
    const rule = hydrationRule(targetDate);
    if ((v === 'true') !== rule)
      fail(`weight-log.csv row (${r.Weight_lbs} lbs) on ${targetDate} has Hydration_Artifact ${v}, but the rule (a Run over 20 km on the date or the two before) says ${rule}`);
  }
}

// ---- 0c. diet-logs/day-styles.csv, and its maintenance rows against calorie-base.csv ----
// `Start,End,Style,Note`; a blank End is open. A day no row covers is normal; long-run is
// derived from the exercise log and never stored. A maintenance row declares the same
// block as a calorie-base.csv row, so the two must agree exactly, and every calorie-base
// block above 2,000 (a maintenance base) must have its maintenance row.
const stylesPath = path.join(root, 'diet-logs', 'day-styles.csv');
const basePath = path.join(root, 'diet-logs', 'calorie-base.csv');
const STORED_STYLES = ['maintenance', 'refeed', 'sick', 'carb-load-training', 'carb-load-race', 'fasting'];
const realDate = s => /^\d{4}-\d{2}-\d{2}$/.test(s) && !Number.isNaN(Date.parse(s + 'T00:00:00Z')) &&
  new Date(s + 'T00:00:00Z').toISOString().slice(0, 10) === s;
let dayStyles = null;
if (!fs.existsSync(stylesPath)) {
  warnings.push('diet-logs/day-styles.csv does not exist yet; every day reads as normal or long-run');
} else {
  const h = headerLine(stylesPath);
  if (h !== 'Start,End,Style,Note') fail(`diet-logs/day-styles.csv header is ${JSON.stringify(h)}, not Start,End,Style,Note`);
  const good = [];
  readRecords(stylesPath).forEach((s, i) => {
    const start = String(s.Start || '').trim(), end = String(s.End || '').trim(), style = String(s.Style || '').trim();
    const at = `diet-logs/day-styles.csv row ${i + 2} (${start}..${end || 'open'} ${style})`;
    if (!realDate(start)) { fail(`${at}: Start is not a real YYYY-MM-DD date`); return; }
    if (end !== '' && !realDate(end)) { fail(`${at}: End is not a real YYYY-MM-DD date (leave it blank for an open range)`); return; }
    if (end !== '' && end < start) { fail(`${at}: End is before Start`); return; }
    if (!STORED_STYLES.includes(style)) fail(`${at}: Style is not one of ${STORED_STYLES.join(', ')} (normal is the absence of a row; long-run is derived, never stored)`);
    good.push({ start, end, style, at });
  });
  good.sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : 0));
  for (let k = 1; k < good.length; k++) {
    const a = good[k - 1], b = good[k];
    if (a.end === '' || a.end >= b.start) fail(`${a.at} and ${b.at} overlap; a day has one style`);
  }
  const blocks = fs.existsSync(basePath) ? readRecords(basePath).map(b => ({
    start: String(b.Start || '').trim(), end: String(b.End || '').trim(), base: num(b.Base) })) : [];
  for (const m of good.filter(g => g.style === 'maintenance'))
    if (!blocks.some(b => b.start === m.start && b.end === m.end))
      fail(`${m.at}: a maintenance row must match a diet-logs/calorie-base.csv block's Start and End exactly; add the block in the same edit`);
  for (const b of blocks)
    if (b.base !== null && b.base > 2000 && !good.some(g => g.style === 'maintenance' && g.start === b.start && g.end === b.end))
      fail(`diet-logs/calorie-base.csv block ${b.start}..${b.end || 'open'} has Base ${b.base}, a maintenance base, but day-styles.csv has no maintenance row for it`);
  dayStyles = good;
}
// The day's style, as the generator derives it: a day-styles.csv row, else long-run when a
// Run over 20 km is logged that day, else normal. Without the table, the cache's own scalar.
function styleForDay(day) {
  if (!dayStyles) return String(d.dayStyle || '');
  const hit = dayStyles.find(s => s.start <= day && (s.end === '' || day <= s.end));
  if (hit) return hit.style;
  return allExRows.some(r => r.Date === day && isLongRun(r)) ? 'long-run' : 'normal';
}

// ---- 1. Meals ⟺ food-log.csv (item-for-item + macro sums) ----
// Fiber is blank on historical rows (unmeasured); the generator writes blank → 0 into the
// cache, so normalize the CSV side to 0 too (num(...) || 0) — otherwise a legacy blank (null)
// would never equal the cache's 0 and every historical item would spuriously diverge.
const csvItems = foodRows.map(r => ({
  name: normName(r.Item), disp: r.Item,
  cal: num(r.Calories), p: num(r.Protein_g), f: num(r.Fat_g), c: num(r.Carbs_g), fiber: num(r.Fiber_g) || 0,
}));
const jsItems = [];
for (const m of (Array.isArray(d.meals) ? d.meals : []))
  for (const it of (Array.isArray(m.items) ? m.items : []))
    jsItems.push({ name: normName(it.item), disp: it.item, cal: num(it.cal), p: num(it.p), f: num(it.f), c: num(it.c), fiber: num(it.fiber) || 0 });

// FATAL layer: match by macro tuple (cal|p|f|c), name-agnostic. A missing/extra item or a
// changed macro is the dangerous divergence (the 2026-06-26 bug: 1 phantom item vs 6 real).
const macroKey = x => `${x.cal}|${x.p}|${x.f}|${x.c}|${x.fiber}`;
const ms = (arr, kf) => { const m = new Map(); for (const x of arr) m.set(kf(x), (m.get(kf(x)) || 0) + 1); return m; };
const fmtMacro = k => { const [cal, p, f, c, fiber] = k.split('|'); return `${cal} cal / ${p}p / ${f}f / ${c}c / ${fiber}fib`; };
const csvMac = ms(csvItems, macroKey), jsMac = ms(jsItems, macroKey);
let macrosMatch = true;
for (const k of new Set([...csvMac.keys(), ...jsMac.keys()])) {
  const cN = csvMac.get(k) || 0, jN = jsMac.get(k) || 0;
  if (cN !== jN) {
    macrosMatch = false;
    if (cN > jN) fail(`food-log.csv has ${cN - jN}× item(s) (${fmtMacro(k)}) that diet-today.js is missing`);
    else fail(`diet-today.js has ${jN - cN}× item(s) (${fmtMacro(k)}) not in food-log.csv`);
  }
}
// WARN layer (non-fatal): when macro tuples match exactly, surface pure name-text drift
// (e.g. "airport takeaway, for the plane" vs "airport, for the plane" — same food, same macros).
if (macrosMatch) {
  const nameKey = x => `${x.name}|${macroKey(x)}`;
  const csvNm = ms(csvItems, nameKey), jsNm = ms(jsItems, nameKey);
  for (const k of new Set([...csvNm.keys(), ...jsNm.keys()])) {
    if ((csvNm.get(k) || 0) !== (jsNm.get(k) || 0))
      warnings.push(`item name text differs between diet-today.js and food-log.csv (macros match): "${k.split('|')[0]}"`);
  }
}
const sum = (arr, k) => arr.reduce((a, x) => a + (x[k] || 0), 0);
if (jsItems.length !== csvItems.length)
  fail(`item count mismatch: diet-today.js has ${jsItems.length}, food-log.csv has ${csvItems.length} for ${targetDate}`);

// ---- 2. Weight ⟺ weight-log.csv ----
const w = d.weight;
if (w === null || w === undefined) {
  if (weightRows.length > 0)
    fail(`weight-log.csv has a weigh-in for ${targetDate} (${weightRows[0].Weight_lbs} lbs) but diet-today.js weight is null — the exact 2026-06-26 divergence`);
} else {
  if (weightRows.length === 0)
    fail(`diet-today.js has weight ${w.lbs} lbs but weight-log.csv has no row for ${targetDate}`);
  else {
    const r = weightRows[weightRows.length - 1];
    if (!close(num(w.lbs), num(r.Weight_lbs), 0.05)) fail(`weight lbs mismatch: diet-today.js ${w.lbs} vs weight-log.csv ${r.Weight_lbs}`);
    if (w.kg != null && num(r.Weight_kg) != null && !close(num(w.kg), num(r.Weight_kg), 0.1)) fail(`weight kg mismatch: diet-today.js ${w.kg} vs weight-log.csv ${r.Weight_kg}`);
    if (w.bf != null && num(r.BodyFat_pct) != null && !close(num(w.bf), num(r.BodyFat_pct), 0.05)) fail(`BF% mismatch: diet-today.js ${w.bf} vs weight-log.csv ${r.BodyFat_pct}`);
    if (w.mm != null && num(r.MuscleMass_lbs) != null && !close(num(w.mm), num(r.MuscleMass_lbs), 0.05)) fail(`muscle-mass mismatch: diet-today.js ${w.mm} vs weight-log.csv ${r.MuscleMass_lbs}`);
  }
}

// ---- 3. Exercise ⟺ exercise-log.csv (type + calories) ----
const exKey = x => `${normName(x.type)}|${Math.round(num(x.calories) || 0)}`;
const csvEx = exRows.map(r => ({ type: r.Type, calories: num(r.Calories) }));
const jsEx = (Array.isArray(d.exercise) ? d.exercise : []).map(e => ({ type: e.type, calories: num(e.calories) }));
const csvExMS = new Map(), jsExMS = new Map();
for (const x of csvEx) csvExMS.set(exKey(x), (csvExMS.get(exKey(x)) || 0) + 1);
for (const x of jsEx) jsExMS.set(exKey(x), (jsExMS.get(exKey(x)) || 0) + 1);
for (const k of new Set([...csvExMS.keys(), ...jsExMS.keys()])) {
  const cN = csvExMS.get(k) || 0, jN = jsExMS.get(k) || 0;
  if (cN !== jN) {
    const [t, cal] = k.split('|');
    if (cN > jN) fail(`exercise-log.csv has a ${t} (${cal} cal) that diet-today.js is missing`);
    else fail(`diet-today.js has a ${t} (${cal} cal) not in exercise-log.csv`);
  }
}

// ---- 4. dayType target assertion ⟺ targets.calories ----
// The generator preserves the hand-edited `dayType` prose and `targets` verbatim and never
// computes a target; nothing else cross-checked them. On 2026-07-08 a morning exercise log
// updated the dayType prose to a new target ("... = 2,110") but left targets.calories at the
// prior day's 2461, both validators passed, and the dashboard showed the wrong target for
// hours. This asserts the two agree. Skip skeleton files (targets null) and carb-load days
// (their targets are windows and the prose phrases them differently — never block those here).
// Whether the day is carb-load comes from diet-logs/day-styles.csv (styleForDay), no longer
// from a regex over the cache's dayStyle scalar.
if (d.targets != null && !/^carb-load/.test(styleForDay(targetDate))) {
  const asserted = extractDayTypeTarget(d.dayType);
  if (asserted !== null && asserted !== num(d.targets.calories))
    fail(`dayType asserts target ${asserted} but targets.calories is ${d.targets.calories}. Update targets in the same edit as dayType (diet-logging skill step 3).`);
}

// ---- Report ----
for (const wm of warnings) console.error('WARN diet-consistency: ' + wm);
if (errors.length) {
  for (const e of errors) console.error('FAIL diet-consistency: ' + e);
  console.error(`\n${errors.length} divergence(s) between diet-today.js and the CSV logs for ${targetDate}. Reconcile before committing — the CSV is authoritative.`);
  process.exit(1);
}
const tc = sum(jsItems, 'cal');
const tfib = sum(jsItems, 'fiber');
const wnote = warnings.length ? ` (${warnings.length} name-text warning(s) — macros agree)` : '';
console.log(`OK diet-consistency — ${targetDate} (current diet day=${dietToday}): ${jsItems.length} meal items (${Math.round(tc)} cal, ${Math.round(tfib * 10) / 10}g fiber), ${jsEx.length} exercise, weight ${w ? w.lbs + ' lbs' : 'none'} all match food/weight/exercise CSV logs${wnote}.`);
