---
name: diet-logging
description: >-
  Log Alex's food, exercise and weigh-ins. Fires whenever Alex mentions eating or drinking
  anything, doing any workout, or a weight reading. ANY such mention IS the instruction to
  log; never ask "want me to log this?". Appends the right diet-logs/*.csv row, then the
  derived vault/diet-today.js is regenerated, both validators run, and the change is
  committed. Confirm in chat with one totals line.
---

# Diet logging

## Date, Time and TZ

- **Diet day** = the calendar date, in the row's zone, of the moment eaten **minus four
  hours** (a 00:30 snack belongs to the day that just ended). It goes in `Date`
  (`YYYY-MM-DD`). `Time` is the `HH:MM` it happened.
- **`TZ` is mandatory on every new row:** the IANA zone Alex was in (`Europe/Rome` at home).
- **A leading `(eaten at <RFC3339 with offset>)` stamp is AUTHORITATIVE:** log at that moment
  in that offset's zone and derive `Date` from it, never from the clock.

## Every row

Header rows, verbatim:

- `food-log.csv`: `Date,Meal,Item,Amount,Unit,Cal_per_100g,Grams,Calories,Protein_g,Fat_g,Carbs_g,Notes,Time,Meal_Type,Fiber_g,Sodium_mg,SatFat_g,Sugar_g,Potassium_mg,Calcium_mg,Omega3_mg,Magnesium_mg,Cholesterol_mg,TransFat_g,AddedSugar_g,Purines_mg,Mercury_ug,Selenium_ug,VitaminD_ug,TZ,Alcohol_g,Category,Source,Basis,Time_Source,Caffeine_mg,Iodine_ug,Iron_mg,Retinol_ug,Oxalate_mg,Omega6_g`
- `exercise-log.csv`: `Date,Type,Description,Distance_km,Duration,Pace_min_per_km,Elevation_m,Avg_HR,Cadence,Calories,Plan_Source,Notes,Start_Time,TZ,Treadmill,Source`
- `weight-log.csv`: `Date,Weight_lbs,Weight_kg,Phase,BodyFat_pct,MuscleMass_lbs,Notes,TZ,Hydration_Artifact`

- **Fill the structured cells after `TZ` on every new row.** Food: `Alcohol_g` (`0` for
  anything without alcohol), `Category` (`food`, `alcoholic_drink`, `soft_drink`,
  `supplement`, `water`), `Source` (`home`, `restaurant`, `shop`, `other_home`, `unknown`),
  `Basis` (`label`, `weighed`, `reference`, `estimate`, `photo`, `unknown`), `Time_Source`
  (`actual`, `report`, `estimated`, `unknown`). Exercise: `Treadmill` (`true`/`false`),
  `Source` (`watch`, `self_reported`, `unknown`). Weight: `Hydration_Artifact`
  (`true` only when a Run over 20 km was logged on that date or the two before).
- **Exercise `Type`** is one of `Run`, `Walk`, `Swim`, `Bike`, `Strength`, `Yard_Work`,
  `Other`; the original wording goes in `Description`. `Start_Time` is the bare `HH:MM`.
- **RFC 4180 quoting:** a field with a comma, `"` or newline is double-quoted, inner `"`
  doubled. `Item` and `Notes` nearly always need it. A quoting slip shifts every later column.
- **Blank vs 0:** blank is unknown, `0` is a known zero. Never `0` as a guess.
- **Match earlier rows first:** search `food-log.csv` for the food's word stem and copy the
  nutrient columns of the closest match, scaled to the new amount; look up reference values
  only when nothing matches. The `Notes` cell names the source.

## Weigh-ins

- Append one row per date. **A second reading on a date that already has a row REPLACES that
  row in place** (edit the existing line); never two rows for one date.
- Leave `BodyFat_pct`/`MuscleMass_lbs` blank when not measured. A device health block dated
  the same day is the scale's own reading: use its body fat and lean mass (kg to lbs).
- Update the `Current:` line in `vault/Projects/Diet/Overview.md` to the new reading.

## Regenerate, validate, commit

On this harness an Edit or Write to a `diet-logs/*.csv` fires a hook
(`.claude/hooks/diet-regen.sh`) that regenerates `vault/diet-today.js` for every touched day,
runs both validators and commits. **If no hook ran** (another harness), do it yourself from
the repository root, for each diet day you touched:

    node vault/generate-diet-today.js --day <YYYY-MM-DD>
    node vault/validate-diet-today.js --day <YYYY-MM-DD>
    node vault/verify-diet-consistency.js --day <YYYY-MM-DD>

then commit the diet files with git: `git add diet-logs vault/diet-today.js` and
`git commit -m "diet: log <what>"`. A validator failure is yours to fix (usually quoting).

## Confirm

One totals line from the regenerated `vault/diet-today.js` (calories against the target and
what is left), then at most one observation about what was just logged.
