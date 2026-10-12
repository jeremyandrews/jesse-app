-- The health archive's raw tables, in schema `raw`, which the deployment creates and owns
-- (owner role `health`). This migration creates no schema, no table in `analytics` and no
-- grant: the schema's default privileges already give `health_ro` SELECT on every table the
-- owner creates here, routes included.
--
-- Every table is written by the bridge alone (`bridge/src/healthdb.rs`): sample tables by
-- `POST /jesse/health/ingest`, upserted by HealthKit UUID; the diet tables by the mirror task.

CREATE TABLE raw.quantity_sample (
    uuid        uuid             NOT NULL,
    seq         int              NOT NULL,
    type        text             NOT NULL,
    start_ts    timestamptz      NOT NULL,
    end_ts      timestamptz      NOT NULL,
    value       double precision NOT NULL,
    unit        text             NOT NULL,
    source      text             NOT NULL,
    source_name text             NOT NULL,
    device      text,
    metadata    jsonb,
    received_at timestamptz      NOT NULL DEFAULT now(),
    PRIMARY KEY (uuid, seq)
);
CREATE INDEX quantity_sample_type_start ON raw.quantity_sample (type, start_ts);

CREATE TABLE raw.category_sample (
    uuid        uuid        NOT NULL PRIMARY KEY,
    type        text        NOT NULL,
    start_ts    timestamptz NOT NULL,
    end_ts      timestamptz NOT NULL,
    value       int         NOT NULL,
    source      text        NOT NULL,
    source_name text        NOT NULL,
    device      text,
    metadata    jsonb,
    received_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX category_sample_type_start ON raw.category_sample (type, start_ts);

CREATE TABLE raw.workout (
    uuid          uuid             NOT NULL PRIMARY KEY,
    start_ts      timestamptz      NOT NULL,
    end_ts        timestamptz      NOT NULL,
    duration_s    double precision NOT NULL,
    activity_type int              NOT NULL,
    activity_name text             NOT NULL,
    distance_m    double precision,
    energy_kcal   double precision,
    avg_hr        double precision,
    max_hr        double precision,
    effort        double precision,
    indoor        boolean,
    source        text             NOT NULL,
    source_name   text             NOT NULL,
    device        text,
    metadata      jsonb,
    received_at   timestamptz      NOT NULL DEFAULT now()
);
CREATE INDEX workout_start ON raw.workout (start_ts);

CREATE TABLE raw.workout_event (
    workout_uuid uuid        NOT NULL REFERENCES raw.workout (uuid) ON DELETE CASCADE,
    idx          int         NOT NULL,
    type         text        NOT NULL,
    start_ts     timestamptz NOT NULL,
    end_ts       timestamptz NOT NULL,
    metadata     jsonb,
    PRIMARY KEY (workout_uuid, idx)
);

-- One row per UUID the phone reported deleted. The first report is kept; a resend of the
-- same deletion is a no-op.
CREATE TABLE raw.deleted_sample (
    uuid       uuid        NOT NULL PRIMARY KEY,
    type       text        NOT NULL,
    deleted_at timestamptz NOT NULL DEFAULT now()
);

-- No foreign key to raw.workout: a route can arrive before its workout.
CREATE TABLE raw.route (
    uuid         uuid        NOT NULL PRIMARY KEY,
    workout_uuid uuid        NOT NULL,
    source       text        NOT NULL,
    source_name  text        NOT NULL,
    n_points     int         NOT NULL,
    downsampled  boolean     NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX route_workout ON raw.route (workout_uuid);

CREATE TABLE raw.route_point (
    route_uuid uuid             NOT NULL REFERENCES raw.route (uuid) ON DELETE CASCADE,
    idx        int              NOT NULL,
    ts         timestamptz      NOT NULL,
    lat        double precision NOT NULL,
    lon        double precision NOT NULL,
    alt        double precision,
    h_acc      double precision,
    v_acc      double precision,
    speed      double precision,
    course     double precision,
    PRIMARY KEY (route_uuid, idx)
);

-- The diet CSV mirror: every row of each file as jsonb keyed by its header, replaced whole
-- whenever the file's sha256 changes.
CREATE TABLE raw.diet_row (
    file   text  NOT NULL,
    row_no int   NOT NULL,
    date   date,
    data   jsonb NOT NULL,
    PRIMARY KEY (file, row_no)
);

CREATE TABLE raw.diet_file (
    file      text        NOT NULL PRIMARY KEY,
    sha256    text        NOT NULL,
    loaded_at timestamptz NOT NULL DEFAULT now()
);
