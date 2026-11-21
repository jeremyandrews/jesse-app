# Testing the bridge

The bridge's tests gate every pull request in `ci.yml`. This page says how to run them, where a
new test goes, and the rules that keep the suite fast: a paused clock for timer logic, an
allowlist for the few tests whose time is the point, and a wall clock budget on the CI steps.

## Running the suite

From `bridge/`:

```sh
cargo nextest run --features containment-probe   # what CI runs (bridge-test)
cargo nextest run --lib                           # the default feature set (bridge-lint)
cargo test --doc --features containment-probe     # doc tests; nextest does not run them
```

Install nextest once with `cargo install cargo-nextest --locked` (or `brew install
cargo-nextest`). nextest runs every test in its own process, all at once, so the suite takes about
as long as its slowest tests.

Plain `cargo test` still works and runs the same tests:

```sh
cargo test --features containment-probe
cargo test --lib
```

Under plain `cargo test` the tests of one binary share a process, which is why the tests that set
environment variables still take `ENV_LOCK` (see "Serialization" below).

Live tests (`*_live.rs`, the containment battery) are `#[ignore]`d: they spend credits or need
real agents. Run one by name, for example
`cargo test --features containment-probe --test it containment:: -- --ignored --nocapture`.

Two environment notes for running locally:

* `JESSE_TEST_PG_URL` (a Postgres server the tests may create databases on) turns on the
  `healthdb_*` tests; without it they pass without touching a database. CI sets it.
* A shell inside a Studio run carries `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0` for the attribution
  hooks. `integration::argv_split_fixture` compares a child's environment against a fixture made
  without them, so run the suite with those unset (`env -u GIT_CONFIG_COUNT -u GIT_CONFIG_KEY_0
  -u GIT_CONFIG_VALUE_0 cargo nextest run ...`).

## Where a new test goes

* **A unit test** goes in the module it tests, in its `#[cfg(test)]` block, as today.
* **An integration test** goes in a module under `bridge/tests/it/`, declared in
  `bridge/tests/it/main.rs`. All of them are one test binary, `it`, so a test build links the
  crate once rather than once per file. Shared helpers are in `tests/common/` (`use
  crate::common;`), fixtures in `tests/fixtures/`. A feature gated module is gated in `main.rs`
  with `#[cfg(feature = "...")]`.
* **Never a new top level `bridge/tests/*.rs`.** `scripts/ci-guards.sh` fails on one. The three
  that exist each need a process of their own, and `tests/it/main.rs` says why:
  `gateway_fds.rs` (counts the whole process's descriptors), `hostrun_env_file.rs` (no test
  harness; redirects the process's own stderr) and `gateway.rs` (sets `PATH` and the probe's
  environment for every child it starts). A new file that genuinely needs its own process is
  added to that list in both places, with its reason.

Test names carry their module: `integration::chat_no_auth_is_401`. Filter with it:
`cargo nextest run --features containment-probe -E 'test(/^scheduler::/)'`.

## Timers: the paused clock is the default

A test that checks what scheduled work does (a tick, a backoff, a debounce, a queue timer) runs on
tokio's paused clock and moves time itself:

```rust
#[tokio::test(start_paused = true)]
async fn the_retry_waits_out_its_backoff() {
    // ...
    tokio::time::advance(Duration::from_secs(30)).await;
    // ...
}
```

With the clock paused, a `tokio::time::sleep` or `timeout` that the runtime is idle on completes
at once in virtual time. It does not move `std::time::Instant`: code that measures a deadline with
`std::time::Instant` (most of this crate) still waits in real time, so check that before
converting.

A test whose purpose is real time behavior (a real subprocess timeout, a socket deadline) keeps
real time. Shorten its tolerance only with a stated reason and 50 consecutive local passes.

A test that waits for a subprocess or a server waits for the event (the line, the file, the
message), with the old timeout as the ceiling, never a fixed sleep.

`scripts/ci-guards.sh` fails on a new `std::thread::sleep` or `tokio::time::sleep` of a literal
second or more in test code. The existing ones are on an allowlist in that script, each with its
reason; a new one needs the same.

## Slow tests

The nextest `ci` profile (`bridge/.config/nextest.toml`) marks any test over 5 s as SLOW in the
log. A CI step (`scripts/ci-guards.sh --slow-tests <junit.xml>`) fails the job when a test runs
over **10 s** and is not on `bridge/.config/slow-tests.txt`.

Each line there reads `<binary id> <test name> | <measured CI time> | <reason>`, the name exactly
as nextest prints it (`jesse-bridge hostrun::tests::...`, `jesse-bridge::it sentinel::...`).

A test earns a line only when its time is the point or is real work that cannot be faked: a real
`git` repository driven through many rounds, a real subprocess or database timeout, a deadline the
crate measures with `std::time::Instant`. "It waits for a timer" does not earn a line; that test
moves to the paused clock. Record the time from a CI run (the `junit-bridge-test` artifact), not
from a laptop.

When a test leaves the list (it got fast), delete its line; the check prints every allowlisted
test it saw over the limit, so a stale line is easy to spot.

## The wall clock budget

The test execution steps in `ci.yml` time themselves and fail with a "Test wall clock budget
exceeded" error when they run over their budget. The budgets are the `TEST_BUDGET_SECS` values in
`ci.yml`, set at the measured time plus 50 percent:

| CI step | Measured | Budget |
|---|---|---|
| `bridge-test` / Test (`cargo nextest run --features containment-probe`) | __TEST_MEASURED__ | __TEST_BUDGET__ s |
| `bridge-lint` / Test (default features) (`cargo nextest run --lib`) | __LIB_MEASURED__ | __LIB_BUDGET__ s |

The build is its own step and is not in the budget.

**When the budget trips:** download the run's `junit-bridge-test` artifact and sort by time
(`python3 -c` over the `testcase` elements, or the per test lines in the log). Usually one new
test is slow: make it fast. If the suite has simply grown, raise the budget in the same pull
request, with the new measured time, plus 50 percent, and say so in the description. Never retry
a test to get under it; nextest `retries` are off on purpose.

## Serialization

Under nextest each test is its own process, so process wide state (environment variables, the
working directory, statics) cannot collide between tests. Shared resources outside the process
still can:

* **`ENV_LOCK`** (`src/testutil.rs`, and `tests/gateway.rs`'s own) serializes tests that set
  environment variables. It stays, because plain `cargo test` runs a binary's tests as threads
  of one process, and there the variables collide. Under nextest it is never contended.
* **The `heic-temp-dir` test group** (`bridge/.config/nextest.toml`, one at a time): the HEIC
  transcode stages through `jesse-vision-*` directories in the system temp dir, and
  `vision::tests::heic_transcode_leaves_no_temp_dir_behind` asserts that none survive. Every test
  that transcodes a HEIC is in the group, so no other process creates one while it counts.

A new test that needs one of these takes the lock or joins the group; prefer removing the shared
resource (a temporary directory of its own, an ephemeral port) to either.

## Rerunning the measurements

These are the measurements behind the budgets and the allowlist (from `bridge/`):

```sh
# The test lists, ignored tests included, per feature set.
cargo nextest list --features containment-probe --run-ignored all > feat.txt
cargo nextest list --lib --run-ignored all > lib.txt
# Per test durations, as JUnit in target/nextest/ci/junit.xml.
cargo nextest run --features containment-probe --profile ci
# The slowest 40.
python3 -c 'import sys,xml.etree.ElementTree as E
r=sorted(((float(c.get("time")),c.get("classname"),c.get("name")) for c in E.parse(sys.argv[1]).iter("testcase")),reverse=True)
[print(f"{t:8.2f}  {b} {n}") for t,b,n in r[:40]]' target/nextest/ci/junit.xml
```

Local timings are noisy on a shared machine; the CI step times are the numbers that count.
