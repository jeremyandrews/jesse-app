//! **Workspace preparation** for a fixture task: the shared fixture base, the inline files
//! on top of it, the seed commit, the bare "remote" repositories a task clones from, and the
//! placeholders that let a prompt and its assertions name things that only exist once the
//! workspace does.
//!
//! Every step is deterministic. The seed commit and every remote commit are made with a
//! fixed identity and a fixed date, so a remote's head SHA is the same on every machine and
//! an assertion can quote it.

use crate::state::{git, SEED_REF};
use crate::suite::{RemoteSpec, Task};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The identity and date every harness-made commit carries.
const EVAL_NAME: &str = "Jesse Eval";
const EVAL_EMAIL: &str = "eval@example.invalid";
const EVAL_DATE: &str = "2026-01-01T00:00:00+00:00";

/// Copy `src` into `dst` recursively, dotfiles included. Symlinks are refused rather than
/// followed: a fixture is plain files, and a link could point anywhere on the host.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("could not create {}: {e}", dst.display()))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| format!("could not read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", src.display()))?;
        let ty = entry
            .file_type()
            .map_err(|e| format!("could not stat {}: {e}", entry.path().display()))?;
        let to = dst.join(entry.file_name());
        if ty.is_symlink() {
            return Err(format!(
                "fixture {} is a symlink; fixtures must be plain files",
                entry.path().display()
            ));
        } else if ty.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)
                .map_err(|e| format!("could not copy {}: {e}", entry.path().display()))?;
            // Keep the executable bit: a vendored hook that loses it fails as "permission
            // denied", which reads as a model miss.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(entry.path())
                    .map(|m| m.permissions().mode())
                    .unwrap_or(0o644);
                let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

/// Write the task's inline `fixture_files` under `dir`.
pub fn write_files(dir: &Path, files: &BTreeMap<String, String>) -> Result<(), String> {
    for (rel, content) in files {
        if !crate::suite::is_plain_relative(rel) {
            return Err(format!("fixture path {rel:?} is not a plain relative path"));
        }
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&full, content)
            .map_err(|e| format!("could not write fixture {rel}: {e}"))?;
    }
    Ok(())
}

/// `git` with the eval identity and date pinned, for the commits the harness itself makes.
fn git_eval(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", EVAL_NAME)
        .env("GIT_AUTHOR_EMAIL", EVAL_EMAIL)
        .env("GIT_COMMITTER_NAME", EVAL_NAME)
        .env("GIT_COMMITTER_EMAIL", EVAL_EMAIL)
        .env("GIT_AUTHOR_DATE", EVAL_DATE)
        .env("GIT_COMMITTER_DATE", EVAL_DATE)
        .env("GIT_TERMINAL_PROMPT", "0")
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

/// Make `dir` a repository with one seed commit of everything in it, recorded at
/// [`SEED_REF`]. The repository's own identity is set too, because the turn commits as
/// well (the diet hook does, on every log) and a commit with no identity fails.
pub fn git_seed(dir: &Path) -> Result<(), String> {
    git_eval(dir, &["init", "-q", "-b", "main"])?;
    git(dir, &["config", "user.name", EVAL_NAME])?;
    git(dir, &["config", "user.email", EVAL_EMAIL])?;
    git(dir, &["config", "commit.gpgsign", "false"])?;
    git_eval(dir, &["add", "-A"])?;
    git_eval(
        dir,
        &["commit", "-q", "--allow-empty", "-m", "eval: seed fixture"],
    )?;
    git(dir, &["update-ref", SEED_REF, "HEAD"])?;
    Ok(())
}

/// A remote, built: where it is and what its branch head is.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltRemote {
    pub url: String,
    pub head: String,
}

/// Build every remote a task names, as bare repositories under `root`, and return them by
/// name. `root` is OUTSIDE the workspace: the turn reaches a remote only by cloning it.
pub fn build_remotes(
    root: &Path,
    remotes: &[RemoteSpec],
) -> Result<BTreeMap<String, BuiltRemote>, String> {
    let mut out = BTreeMap::new();
    for r in remotes {
        let bare = root.join(format!("{}.git", r.name));
        let work = root.join(format!("{}.work", r.name));
        std::fs::create_dir_all(&work)
            .map_err(|e| format!("could not create {}: {e}", work.display()))?;
        git_eval(&work, &["init", "-q", "-b", &r.branch])?;
        for c in &r.commits {
            write_files(&work, &c.files)?;
            git_eval(&work, &["add", "-A"])?;
            git_eval(&work, &["commit", "-q", "--allow-empty", "-m", &c.message])?;
        }
        let head = git_eval(&work, &["rev-parse", "HEAD"])?.trim().to_string();
        if let Some(parent) = bare.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let bare_s = bare.to_string_lossy().into_owned();
        git_eval(
            root,
            &["clone", "-q", "--bare", &work.to_string_lossy(), &bare_s],
        )?;
        let _ = std::fs::remove_dir_all(&work);
        out.insert(r.name.clone(), BuiltRemote { url: bare_s, head });
    }
    Ok(out)
}

/// Replace `{{remote_url:N}}`, `{{remote_head:N}}` and `{{remote_head_short:N}}` in `s`.
pub fn substitute(s: &str, remotes: &BTreeMap<String, BuiltRemote>) -> String {
    let mut out = s.to_string();
    for (name, r) in remotes {
        out = out
            .replace(&format!("{{{{remote_url:{name}}}}}"), &r.url)
            .replace(&format!("{{{{remote_head:{name}}}}}"), &r.head)
            .replace(
                &format!("{{{{remote_head_short:{name}}}}}"),
                &r.head[..r.head.len().min(7)],
            );
    }
    out
}

/// The task as the driver and the assertions see it, with every placeholder resolved.
///
/// Done through the assertions' JSON form so every string field of every kind is covered
/// without this function knowing the kinds.
pub fn resolve_task(task: &Task, remotes: &BTreeMap<String, BuiltRemote>) -> Result<Task, String> {
    if remotes.is_empty() {
        return Ok(task.clone());
    }
    let mut t = task.clone();
    t.prompt = substitute(&t.prompt, remotes);
    t.followups = t.followups.iter().map(|f| substitute(f, remotes)).collect();
    let raw = serde_json::to_string(&t.assertions).map_err(|e| e.to_string())?;
    // Substituted in the SERIALIZED text, so a path with a quote or a backslash in it would
    // need escaping; build_remotes names everything itself and produces neither.
    t.assertions = serde_json::from_str(&substitute(&raw, remotes))
        .map_err(|e| format!("placeholder substitution broke the assertions: {e}"))?;
    Ok(t)
}

/// Prepare one fixture workspace at `dir`: base, files, seed. Returns the remotes built
/// beside it (under `remotes_root`).
pub fn prepare_fixture(
    task: &Task,
    dir: &Path,
    fixtures_root: Option<&Path>,
    remotes_root: &Path,
) -> Result<BTreeMap<String, BuiltRemote>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create fixture dir: {e}"))?;
    if let Some(base) = &task.fixture_base {
        let root: PathBuf = fixtures_root
            .ok_or_else(|| {
                format!(
                    "task '{}' names fixture_base '{base}' but the suite has no fixtures root",
                    task.id
                )
            })?
            .to_path_buf();
        copy_tree(&root.join(base), dir)?;
    }
    write_files(dir, &task.fixture_files)?;
    if task.git_init {
        git_seed(dir)?;
    }
    build_remotes(remotes_root, &task.remotes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::{RemoteCommit, Suite};

    fn task(json: serde_json::Value) -> Task {
        Suite::from_json(
            serde_json::json!({"name": "s", "tasks": [json]})
                .to_string()
                .as_bytes(),
        )
        .unwrap()
        .tasks[0]
            .clone()
    }

    #[test]
    fn the_base_is_copied_with_dotfiles_and_the_files_overlay_it() {
        let fx = tempfile::tempdir().unwrap();
        write_files(
            &fx.path().join("b"),
            &BTreeMap::from([
                (".claude/settings.json".to_string(), "{}".to_string()),
                ("vault/Today.md".to_string(), "base\n".to_string()),
            ]),
        )
        .unwrap();
        let t = task(serde_json::json!({
            "id": "t", "class": "c", "prompt": "p", "workspace": "fixture",
            "fixture_base": "b", "git_init": true,
            "fixture_files": {"vault/Today.md": "overlay\n"},
            "assertions": []
        }));
        let ws = tempfile::tempdir().unwrap();
        let remotes = tempfile::tempdir().unwrap();
        prepare_fixture(&t, ws.path(), Some(fx.path()), remotes.path()).unwrap();
        assert!(ws.path().join(".claude/settings.json").is_file());
        assert_eq!(
            std::fs::read_to_string(ws.path().join("vault/Today.md")).unwrap(),
            "overlay\n"
        );
        // Seeded and clean: nothing the fixture wrote is left uncommitted.
        assert_eq!(git(ws.path(), &["status", "--porcelain"]).unwrap(), "");
        assert!(git(ws.path(), &["rev-parse", SEED_REF]).is_ok());
    }

    #[test]
    fn a_remote_head_is_deterministic_and_substitutes_into_the_task() {
        let spec = RemoteSpec {
            name: "github.com/acme/widget".into(),
            branch: "main".into(),
            commits: vec![
                RemoteCommit {
                    message: "init".into(),
                    files: BTreeMap::from([("README.md".into(), "# widget\n".into())]),
                },
                RemoteCommit {
                    message: "add parser".into(),
                    files: BTreeMap::from([("src/parse.rs".into(), "fn p() {}\n".into())]),
                },
            ],
        };
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ra = build_remotes(a.path(), std::slice::from_ref(&spec)).unwrap();
        let rb = build_remotes(b.path(), &[spec]).unwrap();
        let (ha, hb) = (&ra["github.com/acme/widget"], &rb["github.com/acme/widget"]);
        assert_eq!(
            ha.head, hb.head,
            "same content, identity and date: same SHA"
        );
        assert_eq!(ha.head.len(), 40);
        let t = task(serde_json::json!({
            "id": "t", "class": "c", "workspace": "fixture",
            "prompt": "clone {{remote_url:github.com/acme/widget}}",
            "assertions": [{"type": "answer_matches",
                            "pattern": "{{remote_head_short:github.com/acme/widget}}"}]
        }));
        let r = resolve_task(&t, &ra).unwrap();
        assert!(r.prompt.ends_with(".git") && r.prompt.contains(&ha.url));
        match &r.assertions[0] {
            crate::suite::Assertion::AnswerMatches { pattern } => {
                assert_eq!(pattern, &ha.head[..7])
            }
            other => panic!("unexpected {other:?}"),
        }
        // And it really is a repository one can clone.
        let clone = tempfile::tempdir().unwrap();
        let dst = clone.path().join("w");
        git(
            clone.path(),
            &["clone", "-q", &ha.url, &dst.to_string_lossy()],
        )
        .unwrap();
        assert_eq!(
            git(&dst, &["rev-parse", "HEAD"]).unwrap().trim(),
            ha.head.as_str()
        );
    }

    #[test]
    fn a_fixture_path_that_escapes_is_refused() {
        let ws = tempfile::tempdir().unwrap();
        let err = write_files(
            ws.path(),
            &BTreeMap::from([("../escape.md".to_string(), "x".to_string())]),
        )
        .unwrap_err();
        assert!(err.contains("plain relative"));
    }
}
