#!/usr/bin/env node
// generate-diet-today.js — derive a DIET DAY's cache from the authoritative CSV logs.
// diet-today.js is a CONVENIENCE CACHE the dashboard renders; the source of truth is the
// diet-logs/*.csv trio. Hand-writing the cache is what produced the bug class this script
// kills (the 2026-06-26 phantom snack / dropped weigh-in). Run it after appending the CSV
// row(s):
//   node vault/generate-diet-today.js --day YYYY-MM-DD [--roll-to YYYY-MM-DD]
//
// --day IS REQUIRED, AND THAT IS THE POINT. This script used to default to "today in
// Europe/Rome", read from the host's clock. Three nights running, a food log after
// midnight therefore rolled diet-today.js to the new day and blanked the evening that had
// just been logged: the row went into the CSV for the right day and the cache was rebuilt
// for the wrong one. The caller — the bridge, or the logging skill — knows which DIET DAY
// it is writing (the calendar date, in the effective zone, of the entry minus four hours);
// this script must not guess. A caller on the old contract exits 2 rather than silently
// rebuilding the wrong day.
//
// WHO WRITES WHAT:
//   * --day equals the day diet-today.js already records → rebuild diet-today.js. The
//     ordinary log path.
//   * --day is some OTHER day → rebuild that day's ARCHIVE (diet-logs/days/<day>.js) and
//     leave diet-today.js BYTE-IDENTICAL. A post-midnight or late-arriving row lands on
//     the day it belongs to without disturbing the day in progress.
//   * --roll-to R → close --day (regenerating its archive from the CSVs) and START day R
//     in diet-today.js. THE ONLY WAY THE DAY ROLLS. Only the health-new-day skill's
//     audit-and-roll step passes it; a log never does.
//
// Contract (see Jesse-Guidelines/Diet-Logging-Flow.md):
//   - REBUILT from the CSVs (rows where Date == target date): meals[], weight, exercise[].
//   - DERIVED, not preserved, since 2026-09-15: dayStyle. It comes from
//     diet-logs/day-styles.csv for the day, else `long-run` when a Run over 20 km is logged
//     that day, else `normal`; a preserved scalar that disagrees is replaced and a WARNING
//     printed. Only while that table does not exist is the preserved scalar kept.
//   - PRESERVED from the existing diet-today.js: dayType, targets (incl carbsBase).
//     The generator NEVER computes targets — that would duplicate the calorie/day-style
//     formula; the morning routine / agent owns those. If the existing file is missing or
//     has no targets, a minimal skeleton is written (dayStyle "normal", targets null, empty
//     arrays) and a WARNING is printed — targets are not invented.
//   - IDEMPOTENT: same inputs → byte-identical output.
//   - The archive under diet-logs/days/ has the SAME shape as diet-today.js (it always
//     was an exact copy of that day's final file), so the bridge's history reader and the
//     Health tab parse it unchanged.
//
// After running, the guards re-run as the generator's self-test:
//   node vault/validate-diet-today.js && node vault/verify-diet-consistency.js
// A FAIL there means THIS generator has a bug (not the data) — fix the generator.

const fs = require('fs');
const path = require('path');

const args = process.argv.slice(2);

// Read a flag in either form: `--day 2026-09-03` or `--day=2026-09-03`. Returns undefined
// when the flag is absent and null when it is present but carries no value (which is a
// usage error, not a default).
function flag(name) {
  const eq = args.find(a => a.startsWith(name + '='));
  if (eq !== undefined) return eq.slice(name.length + 1);
  const i = args.indexOf(name);
  if (i === -1) return undefined;
  const v = args[i + 1];
  return (v === undefined || v.startsWith('--')) ? null : v;
}
const USAGE = 'usage: node vault/generate-diet-today.js --day YYYY-MM-DD [--roll-to YYYY-MM-DD]';

// `--date=` is the OLD spelling of `--day` and still works: it always named an explicit
// day, which is exactly what the new contract requires. What no longer works is calling
// this script with NO day at all — that is the defect, and it exits 2.
const dayArg = flag('--day') !== undefined ? flag('--day') : flag('--date');
const rollArg = flag('--roll-to');

if (dayArg === undefined) {
  console.error('FAIL generate-diet-today: --day is required (the diet day to rebuild). ' + USAGE);
  console.error('  A bare invocation used to mean "today on this host", which is what filed ' +
                'after-midnight logs on the wrong day. The caller knows the diet day; pass it.');
  process.exit(2);
}
for (const [name, v] of [['--day', dayArg], ['--roll-to', rollArg]]) {
  if (v === undefined) continue;
  if (v === null || !/^\d{4}-\d{2}-\d{2}$/.test(v)) {
    console.error('FAIL generate-diet-today: ' + name + ' must be YYYY-MM-DD, got ' + JSON.stringify(v));
    process.exit(1);
  }
}
const dateArg = dayArg;

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
  if (field.length > 0 || row.length > 0) { row.push(field); rows.push(row); }
  return rows;
}

function readRecords(csvPath) {
  if (!fs.existsSync(csvPath)) { console.error('WARNING generate-diet-today: CSV not found: ' + csvPath); return []; }
  const rows = parseCSV(fs.readFileSync(csvPath, 'utf8'));
  if (rows.length === 0) return [];
  const header = rows[0];
  return rows.slice(1)
    .filter(r => r.length > 1 && r[0].trim() !== '')
    .map(r => Object.fromEntries(header.map((h, idx) => [h, r[idx] !== undefined ? r[idx] : ''])));
}

// ---- day styles ----
// diet-logs/day-styles.csv (`Start,End,Style,Note`, blank End = open) declares maintenance,
// refeed, sick, carb-load-training, carb-load-race and fasting days. A day no row covers is
// `normal`, or `long-run` when a Run over 20 km is logged on it: long-run is never stored.
// Returns null when the table does not exist, which keeps the preserved scalar instead.
function readDayStyles() {
  const p = path.join(path.resolve(__dirname, '..'), 'diet-logs', 'day-styles.csv');
  if (!fs.existsSync(p)) return null;
  return readRecords(p);
}
// Exercise Type as the bridge reads it: historical cells keep their own wording (Marathon,
// Swimming, Treadmill run) and map here, so a marathon row is the Run it was.
function exerciseType(raw) {
  const a = String(raw || '').trim().toLowerCase().replace(/[_-]/g, ' ').split(/\s+/).filter(Boolean).join(' ');
  const map = { 'treadmill run': 'Run', marathon: 'Run', swimming: 'Swim', gardening: 'Yard_Work',
    'garden work': 'Yard_Work', 'yard work': 'Yard_Work', 'manual labor': 'Yard_Work',
    'strength/weights': 'Strength', 'strength / weights': 'Strength', weights: 'Strength' };
  if (map[a]) return map[a];
  return ['Run', 'Walk', 'Swim', 'Bike', 'Strength', 'Yard_Work'].find(t => t.replace('_', ' ').toLowerCase() === a) || 'Other';
}
function styleFor(day, styles, exRows) {
  const hit = styles.find(s => !blank(s.Start) && s.Start <= day && (blank(s.End) || day <= s.End));
  if (hit) return String(hit.Style).trim();
  const longRun = exRows.some(r => exerciseType(r.Type) === 'Run' && (numOrNull(r.Distance_km) || 0) > 20);
  return longRun ? 'long-run' : 'normal';
}

// ---- helpers ----
const numOrNull = v => {
  if (v === undefined || v === null) return null;
  const s = String(v).trim();
  if (s === '') return null;
  const f = parseFloat(s);
  return Number.isNaN(f) ? null : f;
};
const blank = v => v === undefined || v === null || String(v).trim() === '';

// Coerce a CSV Time / Start_Time value to a bare sortable HH:MM, stripping a leading ~.
// Returns null if no clock time can be extracted (e.g. "Pre-run").
function coerceTime(raw) {
  if (raw === undefined || raw === null) return null;
  let s = String(raw).trim();
  if (s === '') return null;
  s = s.replace(/^~+\s*/, '');
  const m = s.match(/(\d{1,2}):(\d{2})/);
  if (!m) return null;
  const h = parseInt(m[1], 10);
  if (h < 0 || h > 23) return null;
  return String(h).padStart(2, '0') + ':' + m[2];
}

// Fallback meal time (display-only; consistency check never compares meal times) used only
// when a row carries no coercible clock time.
const MEAL_DEFAULT_TIME = { breakfast: '08:00', lunch: '12:30', dinner: '19:00', snack: '10:00' };
function mealFallbackTime(name) {
  return MEAL_DEFAULT_TIME[String(name || '').trim().toLowerCase()] || '12:00';
}

// Display amount comes from the log's Amount column verbatim (the writer makes it human-
// readable when appending the row). Append Unit ONLY when Amount is bare-numeric, so
// "0.75" + "cup" → "0.75 cup" but "1 large (~65g)" + "serving" stays "1 large (~65g)".
function displayAmount(amount, unit) {
  const a = String(amount === undefined || amount === null ? '' : amount).trim();
  const u = String(unit === undefined || unit === null ? '' : unit).trim();
  if (a === '') return u;
  if (/^~?\d+(\.\d+)?$/.test(a) && u !== '') return a + ' ' + u;
  return a;
}

// ---- JS-literal serializers (control quoting/order for byte-identical idempotency) ----
const jsStr = s => JSON.stringify(String(s));
const jsNum = n => String(n);
// For the unknown-aware micronutrients only: null serializes as the literal null. Unknown is
// null at every layer, never 0 (see the micronutrient block in the item builder).
const jsNumOrNull = n => (n === null || n === undefined) ? 'null' : String(n);
// Kill float-summation noise (0.1+0.2) without losing real precision. Applied only to
// rolling-window sums; per-item values pass through untouched.
const round3 = n => Math.round(n * 1000) / 1000;

function serializeWeight(w) {
  if (!w) return 'null';
  const parts = [`lbs: ${jsNum(w.lbs)}`, `kg: ${jsNum(w.kg)}`];
  if (w.bf !== undefined && w.bf !== null) parts.push(`bf: ${jsNum(w.bf)}`);
  if (w.mm !== undefined && w.mm !== null) parts.push(`mm: ${jsNum(w.mm)}`);
  if (w.notes !== undefined && w.notes !== null && String(w.notes) !== '') parts.push(`notes: ${jsStr(w.notes)}`);
  if (w.hydrationArtifact === true) parts.push('hydrationArtifact: true');
  return `{ ${parts.join(', ')} }`;
}

function serializeExercise(ex) {
  if (!ex.length) return '[]';
  const els = ex.map(e => {
    const parts = [`type: ${jsStr(e.type)}`];
    if (e.time) parts.push(`time: ${jsStr(e.time)}`);
    parts.push(`desc: ${jsStr(e.desc || '')}`);
    if (e.distance !== null && e.distance !== undefined) parts.push(`distance: ${jsNum(e.distance)}`);
    if (e.duration) parts.push(`duration: ${jsStr(e.duration)}`);
    parts.push(`calories: ${jsNum(e.calories)}`);
    if (typeof e.treadmill === 'boolean') parts.push(`treadmill: ${e.treadmill}`);
    if (e.src) parts.push(`src: ${jsStr(e.src)}`);
    return `    { ${parts.join(', ')} }`;
  });
  return `[\n${els.join(',\n')}\n  ]`;
}

function serializeMeals(meals) {
  if (!meals.length) return '[]';
  const els = meals.map(m => {
    const items = m.items.map(it => {
      // The structured columns ride along only when the cell is filled, so a historical
      // item with blank cells serializes exactly as it always did.
      const extra = [];
      if (it.alc !== undefined) extra.push(`alc: ${jsNum(it.alc)}`);
      for (const k of ['cat', 'src', 'basis', 'tsrc']) if (it[k] !== undefined) extra.push(`${k}: ${jsStr(it[k])}`);
      return `      { item: ${jsStr(it.item)}, amount: ${jsStr(it.amount)}, cal: ${jsNum(it.cal)}, p: ${jsNum(it.p)}, f: ${jsNum(it.f)}, c: ${jsNum(it.c)}, fiber: ${jsNum(it.fiber)}, na: ${jsNumOrNull(it.na)}, satf: ${jsNumOrNull(it.satf)}, sug: ${jsNumOrNull(it.sug)}, k: ${jsNumOrNull(it.k)}, ca: ${jsNumOrNull(it.ca)}, o3: ${jsNumOrNull(it.o3)}, mg: ${jsNumOrNull(it.mg)}, chol: ${jsNumOrNull(it.chol)}, tfat: ${jsNumOrNull(it.tfat)}, asug: ${jsNumOrNull(it.asug)}, pur: ${jsNumOrNull(it.pur)}, hg: ${jsNumOrNull(it.hg)}, se: ${jsNumOrNull(it.se)}, vd: ${jsNumOrNull(it.vd)}, caf: ${jsNumOrNull(it.caf)}, iod: ${jsNumOrNull(it.iod)}, fe: ${jsNumOrNull(it.fe)}, ret: ${jsNumOrNull(it.ret)}, ox: ${jsNumOrNull(it.ox)}, o6: ${jsNumOrNull(it.o6)}${extra.length ? ', ' + extra.join(', ') : ''} }`;
    }).join(',\n');
    return `    { name: ${jsStr(m.name)}, time: ${jsStr(m.time)}, items: [\n${items}\n    ]}`;
  });
  return `[\n${els.join(',\n')}\n  ]`;
}

function serializeTargets(t) {
  if (t === null || t === undefined) return 'null';
  // Added 2026-08-13: transfat/addedsugar/purines/selenium/vitamind/mercury_weekly. There is
  // deliberately NO cholesterol key — cholesterol is informational, like total sugars, and a
  // target would invite a red/green judgment the dashboard must never render for it.
  // `selenium` is an OBJECT ({floor, ceiling}), and since 2026-09-16 so is `iodine`;
  // `mercury_weekly` is a 7-day ceiling read against `rolling7`, never against a single day.
  // Added 2026-09-16: caffeine (mg ceiling), caffeine_late_hour (the hour after which a
  // caffeinated row is worth noting), iodine (band) and retinol (µg ceiling, PREFORMED
  // vitamin A). Iron, oxalate and omega-6 are informational and deliberately have no target.
  const order = ['calories', 'protein', 'fat', 'carbs', 'carbsBase', 'fiber', 'sodium', 'satFat', 'potassium', 'sugar', 'calcium', 'omega3', 'magnesium', 'transfat', 'addedsugar', 'purines', 'selenium', 'vitamind', 'mercury_weekly', 'caffeine', 'caffeine_late_hour', 'iodine', 'retinol'];
  const seen = new Set();
  const parts = [];
  for (const k of order) {
    if (k in t && t[k] !== undefined) { parts.push(`${k}: ${typeof t[k] === 'number' ? jsNum(t[k]) : JSON.stringify(t[k])}`); seen.add(k); }
  }
  for (const k of Object.keys(t)) {
    if (seen.has(k) || t[k] === undefined) continue;
    parts.push(`${k}: ${typeof t[k] === 'number' ? jsNum(t[k]) : JSON.stringify(t[k])}`);
  }
  return `{ ${parts.join(', ')} }`;
}

// Rolling window: emitted as a nutrient MAP, not a fixed set of keys, so a nutrient joins
// the window by adding one ROLLING_NUTRIENTS entry — no schema change, no consumer change
// beyond reading the new key. Each entry carries the same {known, knownCount, unknownCount}
// triple the daily gauges use, so a consumer renders a partial "≥" identically.
function serializeRolling7(r7) {
  const parts = Object.keys(r7.nutrients).map(k => {
    const n = r7.nutrients[k];
    return `      ${k}: { known: ${jsNum(n.known)}, knownCount: ${jsNum(n.knownCount)}, unknownCount: ${jsNum(n.unknownCount)} }`;
  });
  return `{\n    days: ${jsNum(r7.days)}, from: ${jsStr(r7.from)}, to: ${jsStr(r7.to)},\n    nutrients: {\n${parts.join(',\n')}\n    }\n  }`;
}

// ---- where the day's cache lives ----
// diet-today.js for the day in progress, diet-logs/days/<day>.js for every other day.
// The two files have the same shape (an archive always was an exact copy of that day's
// final diet-today.js), so ONE emitter writes both.
const root = path.resolve(__dirname, '..');
const dtPath = path.join(__dirname, 'diet-today.js');
const daysDir = path.join(root, 'diet-logs', 'days');
const archivePath = day => path.join(daysDir, day + '.js');

function loadCache(file) {
  if (!fs.existsSync(file)) return null;
  global.window = {};
  try { eval(fs.readFileSync(file, 'utf8')); return global.window.DIET_TODAY || null; }
  catch (e) {
    console.error('WARNING generate-diet-today: ' + path.basename(file) +
      ' is not valid JavaScript (' + e.message + ') — treating as missing');
    return null;
  }
}

const live = loadCache(dtPath);
const liveDate = live && typeof live.date === 'string' ? live.date : null;
const targetDate = dateArg;

// ---- the write decision, stated once ----
// Rolling is the ONLY way diet-today.js changes which day it holds, and only --roll-to
// asks for it. Everything else either rebuilds the day already in progress or rebuilds an
// archive; neither can move the day.
const rolling = rollArg !== undefined;
if (rolling && liveDate && liveDate !== targetDate) {
  console.error(`FAIL generate-diet-today: --roll-to ${rollArg} closes --day ${targetDate}, but ` +
    `diet-today.js currently holds ${liveDate}. Roll the day it is actually on, or omit ` +
    `--roll-to to rebuild ${targetDate}'s archive.`);
  process.exit(1);
}
if (rolling && rollArg === targetDate) {
  console.error(`FAIL generate-diet-today: --roll-to ${rollArg} is the same day as --day — a roll ` +
    `must start a DIFFERENT day.`);
  process.exit(1);
}
// Where this run's rebuild of `targetDate` is written. A roll always archives the day it
// closes, so it never writes the closed day back into diet-today.js.
const isLiveDay = liveDate === null || liveDate === targetDate;
const outPath = (isLiveDay && !rolling) ? dtPath : archivePath(targetDate);

// The cache to preserve scalars FROM, and it is always the one for the day being rebuilt:
// the live file when that day is the one in progress, that day's own archive otherwise.
// Reading the live file's targets into an older day would stamp today's plan onto a day it
// never applied to.
const prevForTarget = outPath === dtPath ? live : (loadCache(outPath) || (rolling ? live : null));

// ---- one emitter, both files ----
// diet-today.js and diet-logs/days/<day>.js have the SAME shape, so a day is rebuilt by
// ONE function whichever file it is going to land in. `prev` is the cache for THAT day
// (never some other day's), because dayStyle/dayType/targets are properties of the day.
function buildDay(day, prev) {
  const warnings = [];
  let dayStyle, dayType, targets;
  if (prev && prev.targets) {
    dayStyle = typeof prev.dayStyle === 'string' ? prev.dayStyle : 'normal';
    dayType = typeof prev.dayType === 'string' ? prev.dayType : '';
    targets = prev.targets;
    if (prev.date && prev.date !== day)
      warnings.push(`the cache being rebuilt is dated ${prev.date} but generating for ${day}; dayType/dayStyle/targets carried over — review they fit the new day.`);
  } else {
    dayStyle = prev && typeof prev.dayStyle === 'string' ? prev.dayStyle : 'normal';
    dayType = prev && typeof prev.dayType === 'string' ? prev.dayType : '';
    targets = null;
    warnings.push(`no targets to preserve for ${day} (no cache for that day, or it has no targets) — wrote a skeleton with targets:null. The morning routine / agent must set targets; the generator does NOT invent them.`);
  }

  // ---- rebuild from CSVs ----
  const allFoodRows = readRecords(path.join(root, 'diet-logs', 'food-log.csv'));
  const foodRows = allFoodRows.filter(r => r.Date === day);
  const weightRows = readRecords(path.join(root, 'diet-logs', 'weight-log.csv')).filter(r => r.Date === day);
  const exRows = readRecords(path.join(root, 'diet-logs', 'exercise-log.csv')).filter(r => r.Date === day);

  // dayStyle is DERIVED from day-styles.csv (and a Run over 20 km), not preserved.
  const styles = readDayStyles();
  if (styles) {
    const derived = styleFor(day, styles, exRows);
    if (prev && typeof prev.dayStyle === 'string' && prev.dayStyle !== derived)
      warnings.push(`dayStyle for ${day} is "${derived}" from diet-logs/day-styles.csv, not the preserved "${prev.dayStyle}"; declare a style by adding a day-styles.csv row.`);
    dayStyle = derived;
  }

  // meals — one block per distinct (time, Meal); Meal is a label, never a key that reorders
  // out of time sequence. Two snacks at different times are two separate blocks; rows sharing
  // a time AND a meal name group together. Emitted in ascending time order (sorted HERE, not
  // by the dashboard). Items keep CSV (append) order within a block.
  const groups = new Map(); // "time|Meal" -> { name, time, items[], firstIdx }
  foodRows.forEach((r, idx) => {
    const time = coerceTime(r.Time) || mealFallbackTime(r.Meal);
    const key = time + '|' + r.Meal;
    if (!groups.has(key)) groups.set(key, { name: r.Meal, time, items: [], firstIdx: idx });
    const it = {
      item: r.Item,
      amount: displayAmount(r.Amount, r.Unit),
      cal: numOrNull(r.Calories) || 0,
      p: numOrNull(r.Protein_g) || 0,
      f: numOrNull(r.Fat_g) || 0,
      c: numOrNull(r.Carbs_g) || 0,
      // Fiber_g is the last CSV column and blank on every historical row (unmeasured);
      // blank → 0 in the cache, same convention as the other macros. A legacy row that
      // predates the column has r.Fiber_g === undefined, which also lands on 0.
      fiber: numOrNull(r.Fiber_g) || 0,
      // The micronutrients deliberately break the fiber convention: unknown is
      // NOT zero. A blank cell, or a legacy row that ends before these columns, stays
      // null so downstream layers can render a partial total instead of a false 0.
      na: numOrNull(r.Sodium_mg),
      satf: numOrNull(r.SatFat_g),
      sug: numOrNull(r.Sugar_g),
      k: numOrNull(r.Potassium_mg),
      ca: numOrNull(r.Calcium_mg),
      o3: numOrNull(r.Omega3_mg),
      mg: numOrNull(r.Magnesium_mg),
      // Added 2026-08-13, same unknown-is-null rule. Every row that predates the columns
      // has these fields undefined, which numOrNull maps to null (unknown), never 0.
      chol: numOrNull(r.Cholesterol_mg),
      tfat: numOrNull(r.TransFat_g),
      asug: numOrNull(r.AddedSugar_g),
      pur: numOrNull(r.Purines_mg),
      hg: numOrNull(r.Mercury_ug),
      se: numOrNull(r.Selenium_ug),
      vd: numOrNull(r.VitaminD_ug),
      // Added 2026-09-16, same unknown-is-null rule again. Caffeine, iodine, iron,
      // preformed retinol, oxalate and linoleic acid (omega-6); a row that predates the
      // columns reads undefined here, which numOrNull maps to null, never 0.
      caf: numOrNull(r.Caffeine_mg),
      iod: numOrNull(r.Iodine_ug),
      fe: numOrNull(r.Iron_mg),
      ret: numOrNull(r.Retinol_ug),
      ox: numOrNull(r.Oxalate_mg),
      o6: numOrNull(r.Omega6_g),
    };
    // The structured columns (2026-09-15), carried only when the cell is filled. A legacy
    // row that ends before them reads as blank and adds nothing.
    const alc = numOrNull(r.Alcohol_g);
    if (alc !== null) it.alc = alc;
    for (const [k, col] of [['cat', 'Category'], ['src', 'Source'], ['basis', 'Basis'], ['tsrc', 'Time_Source']])
      if (!blank(r[col])) it[k] = String(r[col]).trim();
    groups.get(key).items.push(it);
  });
  const meals = [...groups.values()];
  // chronological by time; ties (same time, different meal name) broken by CSV appearance
  meals.sort((a, b) => (a.time < b.time ? -1 : a.time > b.time ? 1 : a.firstIdx - b.firstIdx));
  meals.forEach(m => delete m.firstIdx);

  // weight — last weigh-in row for the date, or null
  let weight = null;
  if (weightRows.length) {
    const r = weightRows[weightRows.length - 1];
    weight = { lbs: numOrNull(r.Weight_lbs), kg: numOrNull(r.Weight_kg) };
    if (!blank(r.BodyFat_pct)) weight.bf = numOrNull(r.BodyFat_pct);
    if (!blank(r.MuscleMass_lbs)) weight.mm = numOrNull(r.MuscleMass_lbs);
    if (!blank(r.Notes)) weight.notes = r.Notes;
    if (String(r.Hydration_Artifact || '').trim().toLowerCase() === 'true') weight.hydrationArtifact = true;
  }

  // exercise — one entry per row
  const exercise = exRows.map(r => {
    const e = { type: r.Type };
    const t = coerceTime(r.Start_Time);
    if (t) e.time = t;
    e.desc = r.Description || '';
    const dist = numOrNull(r.Distance_km);
    if (dist !== null) e.distance = dist;
    if (!blank(r.Duration)) e.duration = r.Duration;
    e.calories = numOrNull(r.Calories) || 0;
    const tm = String(r.Treadmill || '').trim().toLowerCase();
    if (tm === 'true' || tm === 'false') e.treadmill = tm === 'true';
    if (!blank(r.Source)) e.src = String(r.Source).trim();
    return e;
  });

  // ---- rolling 7-day window (trailing, INCLUSIVE of the target date) ----
  // Some nutrients are meaningless as a daily pass/fail. Methylmercury is a body-burden with a
  // ~70-day half-life, so the FDA limit is a weekly one; marine omega-3 arrives in two oily-fish
  // meals a week, so a daily floor reads as a failure six days in seven. Both are judged HERE,
  // over the window, and the consumer renders the window rather than the day.
  //
  // Unknown is excluded from the sum, never counted as 0 — identical to the daily gauges. The
  // counts travel with the sum so the consumer can render "≥" for a partial window exactly as
  // it does for a partial day. A row that predates a column is simply unknown.
  // key in the emitted block -> CSV column it sums. Adding a nutrient to the window is this
  // one line; nothing else in the generator, the validator, or the schema changes.
  const ROLLING_NUTRIENTS = {
    mercury_ug: 'Mercury_ug',
    omega3_mg: 'Omega3_mg',
  };
  function shiftDate(ymd, days) {
    const p = ymd.split('-').map(Number);
    const dt = new Date(Date.UTC(p[0], p[1] - 1, p[2]));
    dt.setUTCDate(dt.getUTCDate() + days);
    return dt.toISOString().slice(0, 10);
  }
  const ROLLING_DAYS = 7;
  const rollFrom = shiftDate(day, -(ROLLING_DAYS - 1));
  // ISO dates compare correctly as strings; no Date parsing per row.
  const rollRows = allFoodRows.filter(r => r.Date >= rollFrom && r.Date <= day);
  const rolling7 = { days: ROLLING_DAYS, from: rollFrom, to: day, nutrients: {} };
  for (const key of Object.keys(ROLLING_NUTRIENTS)) {
    const col = ROLLING_NUTRIENTS[key];
    let known = 0, knownCount = 0, unknownCount = 0;
    for (const r of rollRows) {
      // Straight through the same numOrNull the item builder uses, so the window and the day
      // can never disagree about what "unknown" means.
      const v = numOrNull(r[col]);
      if (v !== null) { known += v; knownCount++; }
      else unknownCount++;
    }
    rolling7.nutrients[key] = { known: round3(known), knownCount, unknownCount };
  }

  const out =
`// Diet tracking data — rewritten on each food/exercise log
// Dashboard-Fancy.html loads this via <script src="diet-today.js">
// Reload the HTML page in browser to pick up changes
window.DIET_TODAY = {
  date: ${jsStr(day)},
  dayStyle: ${jsStr(dayStyle)},
  dayType: ${jsStr(dayType)},
  weight: ${serializeWeight(weight)},
  exercise: ${serializeExercise(exercise)},
  meals: ${serializeMeals(meals)},
  targets: ${serializeTargets(targets)},
  rolling7: ${serializeRolling7(rolling7)}
};
`;
  return { out, meals, exercise, weight, dayStyle, targets, warnings };
}

// ---- write ----
function writeDay(day, prev, file) {
  const built = buildDay(day, prev);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const before = fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
  if (before !== built.out) fs.writeFileSync(file, built.out);
  for (const w of built.warnings) console.error('WARNING generate-diet-today: ' + w);
  const itemCount = built.meals.reduce((a, m) => a + m.items.length, 0);
  const cal = built.meals.reduce((a, m) => a + m.items.reduce((s, it) => s + it.cal, 0), 0);
  const fiber = built.meals.reduce((a, m) => a + m.items.reduce((s, it) => s + it.fiber, 0), 0);
  console.log(`OK generate-diet-today — wrote ${day} to ${path.relative(root, file)}: ` +
    `${built.meals.length} meals / ${itemCount} items (${Math.round(cal)} cal, ` +
    `${Math.round(fiber * 10) / 10}g fiber), ${built.exercise.length} exercise, ` +
    `weight ${built.weight ? built.weight.lbs + ' lbs' : 'none'}; ` +
    `dayStyle="${built.dayStyle}", preserved targets ` +
    `${built.targets ? built.targets.calories + ' cal' : 'null (skeleton)'}.`);
}

// Rebuild the day named by --day, into whichever file holds it.
writeDay(targetDate, prevForTarget, outPath);

// THE ROLL, and it is the only thing here that can change which day diet-today.js holds.
// The day just closed has already been rebuilt into its archive above; this starts the
// new one, carrying the closed day's scalars forward exactly as the morning routine has
// always done (it then edits them for the new day).
if (rolling) {
  writeDay(rollArg, live, dtPath);
  console.log(`OK generate-diet-today — rolled: ${targetDate} archived to ` +
    `${path.relative(root, archivePath(targetDate))}, diet-today.js now holds ${rollArg}.`);
}
