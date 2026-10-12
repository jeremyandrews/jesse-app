-- One row per diet day: the whole object of `diet-logs/days/YYYY-MM-DD.js` (or of the live
-- `vault/diet-today.js` until its day is archived), with that day's own `targets`. Upserted by
-- the mirror task (`bridge/src/healthdb.rs`) whenever the file's sha256 changes. Created by the
-- owner role like every table here, so the schema's default privileges give `health_ro`
-- SELECT on it.

CREATE TABLE raw.diet_day (
    date      date        NOT NULL PRIMARY KEY,
    sha256    text        NOT NULL,
    data      jsonb       NOT NULL,
    loaded_at timestamptz NOT NULL
);
