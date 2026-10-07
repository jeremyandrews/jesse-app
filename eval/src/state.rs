//! **State assertions**: the ones that grade what a turn LEFT BEHIND rather than what it
//! said. Files that exist or do not, the last row of a CSV, the repository's HEAD, a value
//! inside a generated `.js` data file, and the vault's own diet validators.
//!
//! Everything here is Rust with no shell. Two kinds reach a child process at all, and both
//! through a CONSTANT argv:
//!
//! * the `git_*` kinds run `git -C <dir> <fixed subcommand and flags>`, with suite data only
//!   ever in a pathspec position after `--` or as a ref this module names itself;
//! * `process_exit_zero` runs `node <script>` where the script comes from the closed
//!   [`Validator`] table and the only argument is a day already checked to be `YYYY-MM-DD`.
//!
//! Each function returns `(passed, detail)`, the pair `crate::assertions` wraps.

use crate::suite::{is_iso_date, Validator};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The ref the runner points at the seed commit of a `git_init` workspace.
pub const SEED_REF: &str = "refs/eval/seed";

type Verdict = (bool, String);

// ---- git, through a constant argv ------------------------------------------------------

/// Run `git -C <dir> <args>` and return stdout, or a readable error. The environment is
/// pinned so a user's global config (a pager, a signing hook, a template) cannot change
/// what the harness reads or writes.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `git_head_message_matches`: HEAD's full message (subject and body) against `pattern`.
pub fn git_head_message_matches(workspace: &Path, repo: Option<&str>, pattern: &str) -> Verdict {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return (false, format!("invalid regex /{pattern}/: {e}")),
    };
    let dir = match repo {
        Some(r) => workspace.join(r),
        None => workspace.to_path_buf(),
    };
    match git(&dir, &["log", "-1", "--format=%B", "HEAD"]) {
        Err(e) => (false, e),
        Ok(msg) => {
            let hit = re.is_match(&msg);
            let subject = msg.lines().next().unwrap_or("").to_string();
            (
                hit,
                if hit {
                    format!("HEAD message {subject:?} matches /{pattern}/")
                } else {
                    format!("HEAD message {subject:?} does not match /{pattern}/")
                },
            )
        }
    }
}

/// `git_path_changed_since`: `path` differs between the seed and HEAD (or the working tree).
pub fn git_path_changed_since(workspace: &Path, path: &str, committed: bool) -> Verdict {
    // `diff --quiet` exits 1 on a difference, which `git` above reports as a failure, so
    // the name list is read instead: empty means unchanged.
    let args: Vec<&str> = if committed {
        vec!["diff", "--name-only", SEED_REF, "HEAD", "--", path]
    } else {
        vec!["diff", "--name-only", SEED_REF, "--", path]
    };
    match git(workspace, &args) {
        Err(e) => (false, e),
        Ok(names) => {
            let changed = !names.trim().is_empty();
            let against = if committed {
                "HEAD"
            } else {
                "the working tree"
            };
            (
                changed,
                if changed {
                    format!("{path} changed between the seed and {against}")
                } else {
                    format!("{path} is unchanged between the seed and {against}")
                },
            )
        }
    }
}

// ---- files -------------------------------------------------------------------------------

/// The entries of `dir` (not recursive) whose file NAME matches `name_pattern`.
fn select(workspace: &Path, dir: &str, name_pattern: &str) -> Result<Vec<PathBuf>, String> {
    let re =
        Regex::new(name_pattern).map_err(|e| format!("invalid regex /{name_pattern}/: {e}"))?;
    let full = workspace.join(dir);
    let rd = match std::fs::read_dir(&full) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("could not list {dir}: {e}")),
    };
    let mut hits: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| re.is_match(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    hits.sort();
    Ok(hits)
}

fn names_of(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `file_exists`, by path or by selector.
pub fn file_exists(
    workspace: &Path,
    path: &str,
    dir: Option<&str>,
    name_pattern: Option<&str>,
) -> Verdict {
    match (path.is_empty(), dir, name_pattern) {
        (false, _, _) => {
            let ok = workspace.join(path).exists();
            (
                ok,
                if ok {
                    format!("{path} exists")
                } else {
                    format!("{path} does not exist")
                },
            )
        }
        (true, Some(d), Some(np)) => match select(workspace, d, np) {
            Err(e) => (false, e),
            Ok(hits) if hits.is_empty() => (false, format!("nothing in {d}/ matches /{np}/")),
            Ok(hits) => (true, format!("{d}/ has {}", names_of(&hits))),
        },
        _ => (
            false,
            "needs `path`, or both `dir` and `name_pattern`".to_string(),
        ),
    }
}

/// `file_absent`, by path or by selector.
pub fn file_absent(
    workspace: &Path,
    path: &str,
    dir: Option<&str>,
    name_pattern: Option<&str>,
) -> Verdict {
    match (path.is_empty(), dir, name_pattern) {
        (false, _, _) => {
            let gone = !workspace.join(path).exists();
            (
                gone,
                if gone {
                    format!("{path} is absent")
                } else {
                    format!("{path} still exists")
                },
            )
        }
        (true, Some(d), Some(np)) => match select(workspace, d, np) {
            Err(e) => (false, e),
            Ok(hits) if hits.is_empty() => (true, format!("nothing in {d}/ matches /{np}/")),
            Ok(hits) => (false, format!("{d}/ still has {}", names_of(&hits))),
        },
        _ => (
            false,
            "needs `path`, or both `dir` and `name_pattern`".to_string(),
        ),
    }
}

/// `file_matches` in selector form: some selected file's content matches `pattern`.
pub fn file_matches_selected(
    workspace: &Path,
    dir: &str,
    name_pattern: &str,
    pattern: &str,
) -> Verdict {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return (false, format!("invalid regex /{pattern}/: {e}")),
    };
    match select(workspace, dir, name_pattern) {
        Err(e) => (false, e),
        Ok(hits) if hits.is_empty() => {
            (false, format!("nothing in {dir}/ matches /{name_pattern}/"))
        }
        Ok(hits) => {
            for h in &hits {
                if let Ok(body) = std::fs::read_to_string(h) {
                    if re.is_match(&body) {
                        return (
                            true,
                            format!(
                                "/{pattern}/ matched in {dir}/{}",
                                h.file_name().unwrap_or_default().to_string_lossy()
                            ),
                        );
                    }
                }
            }
            (
                false,
                format!("/{pattern}/ matched in none of {}", names_of(&hits)),
            )
        }
    }
}

// ---- CSV -------------------------------------------------------------------------------

/// `csv_last_row`: the last data row's named cells, each matched IN FULL.
pub fn csv_last_row(
    workspace: &Path,
    path: &str,
    columns: &BTreeMap<String, String>,
    row_count: Option<usize>,
) -> Verdict {
    let mut rdr = match csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_path(workspace.join(path))
    {
        Ok(r) => r,
        Err(e) => return (false, format!("could not open {path}: {e}")),
    };
    let headers = match rdr.headers() {
        Ok(h) => h.clone(),
        Err(e) => return (false, format!("could not read the header of {path}: {e}")),
    };
    let mut last: Option<csv::StringRecord> = None;
    let mut n = 0usize;
    for rec in rdr.records() {
        match rec {
            // A row with the wrong number of cells is the defect a real reader exists to
            // catch: an unquoted comma shifts every later column.
            Err(e) => return (false, format!("{path} is not well-formed CSV: {e}")),
            Ok(r) => {
                n += 1;
                last = Some(r);
            }
        }
    }
    if let Some(want) = row_count {
        if n != want {
            return (
                false,
                format!("{path} has {n} data row(s), expected exactly {want}"),
            );
        }
    }
    let Some(row) = last else {
        return (false, format!("{path} has no data rows"));
    };
    for (col, pattern) in columns {
        let Some(idx) = headers.iter().position(|h| h == col) else {
            return (false, format!("{path} has no column {col:?}"));
        };
        let re = match Regex::new(&format!("^(?:{pattern})$")) {
            Ok(r) => r,
            Err(e) => return (false, format!("invalid regex /{pattern}/: {e}")),
        };
        let cell = row.get(idx).unwrap_or("");
        if !re.is_match(cell) {
            return (
                false,
                format!("last row of {path}: {col} = {cell:?}, which does not match /{pattern}/"),
            );
        }
    }
    (
        true,
        format!(
            "last row of {path} matches {} column(s){}",
            columns.len(),
            row_count.map(|c| format!(", {c} rows")).unwrap_or_default()
        ),
    )
}

// ---- generated .js data files ----------------------------------------------------------

/// The object literal inside a generated data file: leading `//` and `/* */` comments
/// dropped, everything through the first `=` stripped, a trailing `;` removed.
pub fn js_assignment_body(src: &str) -> Result<&str, String> {
    let mut rest = src;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.split_once('\n').map(|(_, r)| r).unwrap_or("");
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after
                .split_once("*/")
                .map(|(_, r)| r)
                .ok_or("an unterminated /* comment")?;
        } else {
            break;
        }
    }
    let (_, body) = rest
        .split_once('=')
        .ok_or("no assignment (`name = …`) after the leading comments")?;
    let body = body.trim();
    Ok(body.strip_suffix(';').unwrap_or(body).trim_end())
}

/// `json_path_equals`: the value at `pointer` equals `value`.
pub fn json_path_equals(
    workspace: &Path,
    path: &str,
    pointer: &str,
    value: &serde_json::Value,
) -> Verdict {
    let src = match std::fs::read_to_string(workspace.join(path)) {
        Ok(s) => s,
        Err(e) => return (false, format!("could not read {path}: {e}")),
    };
    let body = match js_assignment_body(&src) {
        Ok(b) => b,
        Err(e) => return (false, format!("{path}: {e}")),
    };
    let doc: serde_json::Value = match json5::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return (
                false,
                format!("{path} does not parse as a JS object literal: {e}"),
            )
        }
    };
    match doc.pointer(pointer) {
        None => (false, format!("{path} has nothing at {pointer}")),
        Some(got) if got == value => (true, format!("{path}{pointer} = {got}")),
        Some(got) => (false, format!("{path}{pointer} = {got}, expected {value}")),
    }
}

// ---- the closed validator table --------------------------------------------------------

/// The `node` binary: `JESSE_EVAL_NODE` when set (a path, never a command line), else `node`
/// on `PATH`.
fn node_bin() -> PathBuf {
    std::env::var_os("JESSE_EVAL_NODE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("node"))
}

/// The exact argv a validator runs with, as data, so a test can assert its shape.
pub fn validator_argv(validator: Validator, day: Option<&str>) -> Result<Vec<String>, String> {
    let mut argv = vec![validator.script().to_string()];
    if let Some(d) = day {
        if !is_iso_date(d) {
            return Err(format!("day {d:?} is not YYYY-MM-DD"));
        }
        argv.push("--day".to_string());
        argv.push(d.to_string());
    }
    Ok(argv)
}

/// `process_exit_zero`: run one validator from the closed table in the workspace.
pub fn process_exit_zero(workspace: &Path, validator: Validator, day: Option<&str>) -> Verdict {
    let argv = match validator_argv(validator, day) {
        Ok(a) => a,
        Err(e) => return (false, e),
    };
    if !workspace.join(validator.script()).is_file() {
        return (
            false,
            format!(
                "{} is not in the workspace (vendor it in the fixture)",
                validator.script()
            ),
        );
    }
    match Command::new(node_bin())
        .args(&argv)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .output()
    {
        Err(e) => (false, format!("could not run node: {e}")),
        Ok(out) => {
            let ok = out.status.success();
            let tail = |b: &[u8]| {
                let s = String::from_utf8_lossy(b);
                let t = s.trim();
                let start = t.len().saturating_sub(300);
                let start = (start..t.len())
                    .find(|i| t.is_char_boundary(*i))
                    .unwrap_or(t.len());
                t[start..].to_string()
            };
            (
                ok,
                if ok {
                    format!("{} exited 0: {}", argv.join(" "), tail(&out.stdout))
                } else {
                    format!(
                        "{} exited {}: {}",
                        argv.join(" "),
                        out.status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or("by signal".into()),
                        tail(&out.stderr)
                    )
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    // ---- file_exists / file_absent / selector file_matches ----

    #[test]
    fn file_exists_passes_on_a_present_path_and_fails_on_a_missing_one() {
        let d = ws();
        write(d.path(), "a/b.md", "x");
        assert!(file_exists(d.path(), "a/b.md", None, None).0);
        assert!(!file_exists(d.path(), "a/c.md", None, None).0);
    }

    #[test]
    fn file_exists_by_selector_both_directions() {
        let d = ws();
        write(d.path(), "drafts/2026-10-06-0915-plan.md", "x");
        let pat = r"^\d{4}-\d{2}-\d{2}-\d{4}-.+\.md$";
        assert!(file_exists(d.path(), "", Some("drafts"), Some(pat)).0);
        write(d.path(), "other/plan.md", "x");
        assert!(!file_exists(d.path(), "", Some("other"), Some(pat)).0);
        assert!(!file_exists(d.path(), "", Some("missing"), Some(pat)).0);
        assert!(
            !file_exists(d.path(), "", None, None).0,
            "no target at all fails"
        );
    }

    #[test]
    fn file_absent_both_directions() {
        let d = ws();
        write(d.path(), "drafts/plan.md", "x");
        assert!(!file_absent(d.path(), "drafts/plan.md", None, None).0);
        assert!(file_absent(d.path(), "drafts/gone.md", None, None).0);
        assert!(!file_absent(d.path(), "", Some("drafts"), Some(r"plan")).0);
        assert!(file_absent(d.path(), "", Some("drafts"), Some(r"^zzz")).0);
        assert!(file_absent(d.path(), "", Some("no-such-dir"), Some(r".")).0);
    }

    #[test]
    fn selected_file_matches_both_directions() {
        let d = ws();
        write(
            d.path(),
            "drafts/2026-10-06-0915-plan.md",
            "body\n- [ ] Archive\n",
        );
        assert!(file_matches_selected(d.path(), "drafts", r"\.md$", r"- \[ \] Archive").0);
        assert!(!file_matches_selected(d.path(), "drafts", r"\.md$", r"Deep extract").0);
        assert!(!file_matches_selected(d.path(), "drafts", r"\.txt$", r".").0);
    }

    // ---- csv_last_row ----

    const FOOD: &str = "Date,Meal,Item,Notes,TZ\n\
2026-10-05,Lunch,\"Pasta, red sauce\",\"from the \"\"usual\"\" place\",Europe/Rome\n\
2026-10-06,Snack,Banana,one medium,Europe/Rome\n";

    fn cols(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn csv_last_row_passes_on_the_last_row() {
        let d = ws();
        write(d.path(), "log.csv", FOOD);
        let r = csv_last_row(
            d.path(),
            "log.csv",
            &cols(&[
                ("Date", "2026-10-06"),
                ("Item", "(?i)banana"),
                ("TZ", "Europe/Rome"),
            ]),
            Some(2),
        );
        assert!(r.0, "{}", r.1);
    }

    #[test]
    fn csv_last_row_is_anchored_and_fails_on_a_partial_match() {
        let d = ws();
        write(d.path(), "log.csv", FOOD);
        assert!(!csv_last_row(d.path(), "log.csv", &cols(&[("Item", "Ban")]), None).0);
        assert!(!csv_last_row(d.path(), "log.csv", &cols(&[("Date", "2026-10-05")]), None).0);
        assert!(!csv_last_row(d.path(), "log.csv", &cols(&[("Nope", ".*")]), None).0);
    }

    #[test]
    fn csv_last_row_reads_quoted_commas_as_one_cell() {
        let d = ws();
        // The quoted comma would shift every later column under a naive split.
        write(
            d.path(),
            "log.csv",
            "Date,Item,TZ\n2026-10-06,\"Pasta, red sauce\",Europe/Rome\n",
        );
        let r = csv_last_row(
            d.path(),
            "log.csv",
            &cols(&[("Item", "Pasta, red sauce"), ("TZ", "Europe/Rome")]),
            None,
        );
        assert!(r.0, "{}", r.1);
    }

    #[test]
    fn csv_last_row_fails_on_a_wrong_count_and_on_a_shifted_row() {
        let d = ws();
        write(d.path(), "log.csv", FOOD);
        assert!(!csv_last_row(d.path(), "log.csv", &BTreeMap::new(), Some(3)).0);
        write(
            d.path(),
            "bad.csv",
            "Date,Item,TZ\n2026-10-06,Pasta, red sauce,Europe/Rome\n",
        );
        let r = csv_last_row(d.path(), "bad.csv", &BTreeMap::new(), None);
        assert!(!r.0);
        assert!(r.1.contains("not well-formed"), "{}", r.1);
    }

    // ---- git ----

    fn repo() -> tempfile::TempDir {
        let d = ws();
        git(d.path(), &["init", "-q", "-b", "main"]).unwrap();
        git(d.path(), &["config", "user.name", "Eval"]).unwrap();
        git(d.path(), &["config", "user.email", "eval@example.invalid"]).unwrap();
        git(d.path(), &["config", "commit.gpgsign", "false"]).unwrap();
        write(d.path(), "diet-logs/food-log.csv", "Date,Item\n");
        write(d.path(), "notes.md", "n\n");
        git(d.path(), &["add", "-A"]).unwrap();
        git(d.path(), &["commit", "-q", "-m", "seed"]).unwrap();
        git(d.path(), &["update-ref", SEED_REF, "HEAD"]).unwrap();
        d
    }

    #[test]
    fn git_head_message_both_directions() {
        let d = repo();
        assert!(git_head_message_matches(d.path(), None, "^seed").0);
        write(
            d.path(),
            "diet-logs/food-log.csv",
            "Date,Item\n2026-10-06,Banana\n",
        );
        git(
            d.path(),
            &["commit", "-q", "-am", "diet: log 2026-10-06 08:50"],
        )
        .unwrap();
        assert!(git_head_message_matches(d.path(), None, r"^diet: log").0);
        assert!(!git_head_message_matches(d.path(), None, r"^seed").0);
        assert!(!git_head_message_matches(&d.path().join("nope"), None, ".").0);
    }

    #[test]
    fn git_path_changed_both_directions_and_committed_vs_worktree() {
        let d = repo();
        assert!(!git_path_changed_since(d.path(), "diet-logs/food-log.csv", true).0);
        write(
            d.path(),
            "diet-logs/food-log.csv",
            "Date,Item\n2026-10-06,Banana\n",
        );
        // Edited but not committed: the worktree comparison sees it, the committed one does not.
        assert!(git_path_changed_since(d.path(), "diet-logs/food-log.csv", false).0);
        assert!(!git_path_changed_since(d.path(), "diet-logs/food-log.csv", true).0);
        git(d.path(), &["commit", "-q", "-am", "diet"]).unwrap();
        assert!(git_path_changed_since(d.path(), "diet-logs/food-log.csv", true).0);
        assert!(!git_path_changed_since(d.path(), "notes.md", true).0);
    }

    // ---- json_path_equals ----

    const DIET_JS: &str = "// Diet tracking data\n// rewritten on each log\n\
window.DIET_TODAY = {\n  date: \"2026-10-06\",\n  dayStyle: \"normal\",\n  meals: [\n    { meal: \"Snack\", items: [ { item: \"Banana\", cal: 105, }, ], },\n  ],\n  targets: { calories: 1700 },\n};\n";

    #[test]
    fn js_body_strips_comments_and_the_assignment() {
        let b = js_assignment_body(DIET_JS).unwrap();
        assert!(b.starts_with('{') && b.ends_with('}'), "{b}");
        assert!(js_assignment_body("// only a comment\n").is_err());
    }

    #[test]
    fn json_path_equals_both_directions() {
        let d = ws();
        write(d.path(), "vault/diet-today.js", DIET_JS);
        let p = "vault/diet-today.js";
        assert!(json_path_equals(d.path(), p, "/date", &serde_json::json!("2026-10-06")).0);
        assert!(json_path_equals(d.path(), p, "/meals/0/items/0/cal", &serde_json::json!(105)).0);
        assert!(!json_path_equals(d.path(), p, "/date", &serde_json::json!("2026-10-07")).0);
        assert!(!json_path_equals(d.path(), p, "/weight", &serde_json::json!(null)).0);
        write(d.path(), "broken.js", "window.X = { a: ;\n");
        assert!(!json_path_equals(d.path(), "broken.js", "/a", &serde_json::json!(1)).0);
    }

    // ---- process_exit_zero ----

    #[test]
    fn the_validator_argv_is_closed() {
        assert_eq!(
            validator_argv(Validator::ValidateDietToday, Some("2026-10-06")).unwrap(),
            ["vault/validate-diet-today.js", "--day", "2026-10-06"]
        );
        assert_eq!(
            validator_argv(Validator::VerifyDietConsistency, None).unwrap(),
            ["vault/verify-diet-consistency.js"]
        );
        assert!(validator_argv(Validator::ValidateDietToday, Some("--eval=1")).is_err());
    }

    fn node_present() -> bool {
        Command::new(node_bin())
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn process_exit_zero_both_directions() {
        let d = ws();
        // Missing script: a fail that names the vendoring problem, with or without node.
        let r = process_exit_zero(d.path(), Validator::ValidateDietToday, None);
        assert!(!r.0);
        assert!(r.1.contains("not in the workspace"), "{}", r.1);
        if !node_present() {
            eprintln!("node not on PATH: the run half of this test is skipped");
            return;
        }
        // Stand-ins with the table's names: one exits 0 when given the day, one exits 3.
        write(
            d.path(),
            "vault/validate-diet-today.js",
            "process.exit(process.argv[3] === '2026-10-06' ? 0 : 1);\n",
        );
        write(
            d.path(),
            "vault/verify-diet-consistency.js",
            "console.error('FAIL day mixes zoned and blank rows'); process.exit(3);\n",
        );
        let ok = process_exit_zero(d.path(), Validator::ValidateDietToday, Some("2026-10-06"));
        assert!(ok.0, "{}", ok.1);
        assert!(!process_exit_zero(d.path(), Validator::ValidateDietToday, Some("2026-10-07")).0);
        let bad = process_exit_zero(d.path(), Validator::VerifyDietConsistency, None);
        assert!(!bad.0);
        assert!(bad.1.contains("exited 3"), "{}", bad.1);
    }
}
