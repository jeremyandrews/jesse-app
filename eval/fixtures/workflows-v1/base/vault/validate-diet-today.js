#!/usr/bin/env node
// validate-diet-today.js — schema check for a DIET DAY's cache. Run after EVERY rewrite:
//   node vault/validate-diet-today.js [--day YYYY-MM-DD]
// Catches the #1 subagent failure mode: wrong field names (calories/protein/fat/carbs
// instead of cal/p/f/c, dist instead of distance) which silently render zero totals
// in Dashboard-Fancy.html. Contract defined in Jesse-Guidelines/Diet-Logging-Flow.md.
//
// --day names the diet day to check and selects the file that HOLDS it: diet-today.js
// while the day is in progress, diet-logs/days/<day>.js once it has been rolled. The two
// have the same shape, so one schema check covers both — and a log that lands on an
// earlier day is validated where it was actually written instead of against the day in
// progress, which it never touched. Without --day this checks diet-today.js, exactly as
// before.
//
// THIS SCRIPT NEVER OPENS A CSV. It is a pure schema check by design; the row-level day,
// TZ and header checks live in verify-diet-consistency.js, which is the guard that reads
// the logs. They run back to back on every log event, so both fire on the same path.
const fs = require('fs');
const path = require('path');

function fail(msg) { console.error('FAIL diet-today.js: ' + msg); process.exit(1); }

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
if (rawDay !== undefined && (rawDay === null || !/^\d{4}-\d{2}-\d{2}$/.test(rawDay)))
  fail('--day must be YYYY-MM-DD, got ' + JSON.stringify(rawDay));
const day = rawDay === undefined ? null : rawDay;

const root = path.resolve(__dirname, '..');
const dtPath = path.join(__dirname, 'diet-today.js');

function load(f) {
  if (!fs.existsSync(f)) return null;
  global.window = {};
  try { eval(fs.readFileSync(f, 'utf8')); } catch (e) { fail(path.relative(root, f) + ' is not valid JavaScript: ' + e.message); }
  return global.window.DIET_TODAY || null;
}

let file = dtPath;
if (!fs.existsSync(file) && !day) fail('file not found at ' + file);
let d = load(file);
if (day && (!d || d.date !== day)) {
  file = path.join(root, 'diet-logs', 'days', day + '.js');
  if (!fs.existsSync(file))
    fail(`--day ${day} is not the day diet-today.js holds${d && d.date ? ' (' + d.date + ')' : ''} ` +
         `and there is no archive at ${path.relative(root, file)}`);
  d = load(file);
}
if (!d) fail('window.DIET_TODAY not set in ' + path.relative(root, file));

if (!/^\d{4}-\d{2}-\d{2}$/.test(d.date || '')) fail('date missing or not YYYY-MM-DD');
if (day && d.date !== day) fail(`--day ${day} but ${path.relative(root, file)} is dated ${d.date}`);
if (typeof d.dayType !== 'string') fail('dayType missing (free-text human label)');

// dayStyle is derived by the generator from diet-logs/day-styles.csv; `normal` and the
// derived `long-run` are never stored there, the other six are.
const DAY_STYLES = ['normal', 'long-run', 'maintenance', 'refeed', 'sick', 'carb-load-training', 'carb-load-race', 'fasting'];
if ('dayStyle' in d && !DAY_STYLES.includes(d.dayStyle))
  fail(`dayStyle ${JSON.stringify(d.dayStyle)} is not one of ${DAY_STYLES.join(', ')}`);

// The structured columns (2026-09-15), all optional because they are emitted only when the
// CSV cell is filled; when present each must hold a word of its vocabulary.
const VOCAB = {
  cat: ['food', 'alcoholic_drink', 'soft_drink', 'supplement', 'water'],
  src: ['home', 'restaurant', 'shop', 'other_home', 'unknown'],
  basis: ['label', 'weighed', 'reference', 'estimate', 'photo', 'unknown'],
  tsrc: ['actual', 'report', 'estimated', 'unknown'],
};
const EX_SOURCES = ['watch', 'self_reported', 'unknown'];

if (!Array.isArray(d.meals)) fail('meals must be an array');
let cal = 0, items = 0;
for (const m of d.meals) {
  if (typeof m.name !== 'string' || typeof m.time !== 'string' || !Array.isArray(m.items))
    fail(`meal "${m.name || '?'}" must be {name, time, items[]}`);
  if (!/^\d{2}:\d{2}$/.test(m.time)) fail(`meal "${m.name}" time "${m.time}" not HH:MM`);
  for (const it of m.items) {
    for (const k of ['calories', 'protein', 'fat', 'carbs', 'fiber_g'])
      if (k in it) fail(`item "${it.item || '?'}" uses forbidden key "${k}" — items use cal/p/f/c/fiber`);
    if (typeof it.item !== 'string' || typeof it.amount !== 'string')
      fail(`an item in meal "${m.name}" needs string item + amount`);
    for (const k of ['cal', 'p', 'f', 'c', 'fiber'])
      if (typeof it[k] !== 'number') fail(`item "${it.item}": "${k}" must be a number, got ${typeof it[k]}`);
    // Micronutrients (na/satf/sug/k): optional, and unknown is null, never 0. When the
    // key is present it must be a number or null; a string "12" would silently break the
    // app's unknown-aware totals the same way cal-vs-calories broke the macro totals.
    // chol/tfat/asug/pur/hg/se/vd added 2026-08-13 under the same rule, and
    // caf/iod/fe/ret/ox/o6 on 2026-09-16.
    for (const k of ['na', 'satf', 'sug', 'k', 'ca', 'o3', 'mg', 'chol', 'tfat', 'asug', 'pur', 'hg', 'se', 'vd',
                     'caf', 'iod', 'fe', 'ret', 'ox', 'o6'])
      if (k in it && it[k] !== null && typeof it[k] !== 'number')
        fail(`item "${it.item}": "${k}" must be a number or null, got ${typeof it[k]}`);
    if ('alc' in it && (typeof it.alc !== 'number' || !(it.alc >= 0)))
      fail(`item "${it.item}": "alc" (grams of ethanol) must be a non-negative number, got ${JSON.stringify(it.alc)}`);
    for (const [k, voc] of Object.entries(VOCAB))
      if (k in it && !voc.includes(it[k])) fail(`item "${it.item}": "${k}" ${JSON.stringify(it[k])} is not one of ${voc.join(', ')}`);
    cal += it.cal; items++;
  }
}

if (!Array.isArray(d.exercise)) fail('exercise must be an array');
for (const e of d.exercise) {
  for (const k of ['cal', 'dist'])
    if (k in e) fail(`exercise "${e.type || '?'}" uses forbidden key "${k}" — use calories/distance/duration`);
  if (typeof e.type !== 'string' || typeof e.calories !== 'number')
    fail(`exercise entries need string "type" + numeric "calories" (got ${JSON.stringify(e)})`);
  if (e.time && !/^\d{2}:\d{2}$/.test(e.time)) fail(`exercise "${e.type}" time not HH:MM`);
  if ('treadmill' in e && typeof e.treadmill !== 'boolean') fail(`exercise "${e.type}": "treadmill" must be a boolean`);
  if ('src' in e && !EX_SOURCES.includes(e.src)) fail(`exercise "${e.type}": "src" ${JSON.stringify(e.src)} is not one of ${EX_SOURCES.join(', ')}`);
}

// TARGETS ARE REQUIRED FOR THE DAY IN PROGRESS AND OPTIONAL FOR AN ARCHIVED ONE.
//
// Nothing should be logged against a day with no plan, so a null on diet-today.js is a
// failure exactly as it always was. An ARCHIVE is different: the generator writes one for
// any day a row lands on, and for a day whose plan was never recorded `targets: null` is
// the honest state rather than a defect — the bridge's history endpoint already renders a
// day with no recoverable target as having none. Failing here instead would fail the whole
// hook chain and roll a good log back, which trades a missing number for a lost meal.
const isLiveFile = file === dtPath;
if (!d.targets) {
  if (isLiveFile) fail('targets missing');
  console.error(`WARN ${path.relative(root, file)}: targets are null — this day's plan was ` +
    `never recorded. The rows are correct; the dashboard will show no goals for it.`);
} else {
for (const k of ['calories', 'protein', 'fat', 'carbs'])
  if (typeof d.targets[k] !== 'number') fail(`targets.${k} must be a number`);
// targets.fiber is optional (like carbsBase): a fiber floor is normally present (38g) and
// preserved by the generator, but the carb-load day-style may omit it. Type-check if present.
if ('fiber' in d.targets && typeof d.targets.fiber !== 'number') fail('targets.fiber must be a number when present');
// Micronutrient targets are all optional: sodium (mg ceiling), satFat (g ceiling),
// potassium (mg floor), sugar (g, informational), calcium (mg floor), omega3 (mg floor,
// EPA+DHA), magnesium (mg floor). Type-check when present.
// transfat (g ceiling), addedsugar (g ceiling), purines (mg soft flag), vitamind (ug floor),
// mercury_weekly (ug ceiling over 7 days, read against rolling7) added 2026-08-13. There is
// deliberately no `cholesterol` target — it is informational, so a target key would be a bug.
// caffeine (mg ceiling), caffeine_late_hour (the hour a caffeinated row starts being worth
// noting) and retinol (µg ceiling, PREFORMED vitamin A only) added 2026-09-16. Iron, oxalate
// and omega-6 are informational: a target key for them would be a bug, like cholesterol's.
for (const k of ['sodium', 'satFat', 'potassium', 'sugar', 'calcium', 'omega3', 'magnesium',
                 'transfat', 'addedsugar', 'purines', 'vitamind', 'mercury_weekly',
                 'caffeine', 'caffeine_late_hour', 'retinol'])
  if (k in d.targets && typeof d.targets[k] !== 'number') fail(`targets.${k} must be a number when present`);
if ('cholesterol' in d.targets)
  fail('targets.cholesterol must not be set — cholesterol is informational (no target, no red/green)');
// selenium and (since 2026-09-16) iodine are the BAND targets: an object with a numeric
// floor and ceiling, floor < ceiling. Every other target is a scalar.
for (const band of ['selenium', 'iodine']) {
  if (!(band in d.targets)) continue;
  const b = d.targets[band];
  if (!b || typeof b !== 'object' || Array.isArray(b))
    fail(`targets.${band} must be an object { floor, ceiling } when present`);
  for (const k of ['floor', 'ceiling'])
    if (typeof b[k] !== 'number') fail(`targets.${band}.${k} must be a number`);
  if (!(b.floor < b.ceiling)) fail(`targets.${band}.floor must be below targets.${band}.ceiling`);
}
}

// rolling7 — trailing 7-day sums, unknown-aware like the daily gauges. Optional so an
// older hand-written cache still validates, but strictly typed when present.
if ('rolling7' in d && d.rolling7 !== null) {
  const r7 = d.rolling7;
  if (typeof r7 !== 'object' || Array.isArray(r7)) fail('rolling7 must be an object when present');
  if (typeof r7.days !== 'number' || r7.days <= 0) fail('rolling7.days must be a positive number');
  for (const k of ['from', 'to'])
    if (!/^\d{4}-\d{2}-\d{2}$/.test(r7[k] || '')) fail(`rolling7.${k} must be YYYY-MM-DD`);
  if (r7.from > r7.to) fail('rolling7.from must not be after rolling7.to');
  if (!r7.nutrients || typeof r7.nutrients !== 'object' || Array.isArray(r7.nutrients))
    fail('rolling7.nutrients must be an object keyed by nutrient');
  for (const k of Object.keys(r7.nutrients)) {
    const n = r7.nutrients[k];
    if (!n || typeof n !== 'object') fail(`rolling7.nutrients.${k} must be an object`);
    // known is a SUM of known values only — never null. Unknown is expressed by the counts,
    // so a window with nothing known is known:0 with knownCount:0, which the consumer must
    // render as "not tracked yet", NOT as a real zero.
    for (const f of ['known', 'knownCount', 'unknownCount'])
      if (typeof n[f] !== 'number') fail(`rolling7.nutrients.${k}.${f} must be a number`);
    if (n.knownCount < 0 || n.unknownCount < 0) fail(`rolling7.nutrients.${k} counts must not be negative`);
  }
}
if ('weight' in d && d.weight !== null && typeof d.weight.lbs !== 'number')
  fail('weight must be null or an object with numeric lbs');
// Emitted only when the weigh-in is a hydration artifact, so present means true.
if (d.weight && 'hydrationArtifact' in d.weight && d.weight.hydrationArtifact !== true)
  fail('weight.hydrationArtifact is emitted only when true; got ' + JSON.stringify(d.weight.hydrationArtifact));

console.log(`OK ${path.relative(root, file)} — ${d.date} (${d.dayType}); ${d.meals.length} meals / ${items} items (${Math.round(cal)} cal); ${d.exercise.length} exercise entries; target ${d.targets ? d.targets.calories + ' cal' : 'none recorded'}`);
