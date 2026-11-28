//! The bare clones and their worktrees.
//!
//! **EVERY GIT COMMAND THAT CHANGES A BARE CLONE RUNS UNDER ITS REPOSITORY LOCK**: an exclusive
//! `flock` on `<root>/locks/<owner>__<name>.lock` ([`RepoLock`]). Two runs starting at once, or a
//! start racing a cleanup, take turns. The lock is held only for those commands, never while an
//! agent works.
//!
//! A bare clone keeps the remote's branches under `refs/remotes/origin/*` (not the `clone
//! --bare` mirror refspec), so a fetch never tries to move a branch a worktree has checked out.
//!
//! **A LOCAL REPOSITORY** (`local/<name>`, [`crate::devjob::is_local_repo`]) is a bare
//! repository at `<root>/local/<name>.git` with no remote at all: it is created on first use
//! with one empty commit on `main`, its worktrees branch from its own `main` with no fetch, and
//! a run's branch is deleted only once it is merged into that `main`, since nothing else holds
//! the work. Nothing here ever deletes `<root>/local/`.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::devjob::is_local_repo;

/// An exclusive `flock` on one repository's lock file, released on drop.
pub struct RepoLock {
    _file: File,
}

/// `<owner>__<name>.lock`.
pub fn lock_name(repo: &str) -> String {
    format!("{}.lock", repo.replace('/', "__"))
}

/// Take the repository's lock, blocking until it is free.
pub fn lock_repo(root: &Path, repo: &str) -> Result<RepoLock, String> {
    let dir = root.join("locks");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(lock_name(repo));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    flock_exclusive(&file).map_err(|e| format!("could not lock {}: {e}", path.display()))?;
    Ok(RepoLock { _file: file })
}

/// `flock(LOCK_EX)` on an open file, retried on EINTR.
pub fn flock_exclusive(file: &File) -> std::io::Result<()> {
    loop {
        // SAFETY: flock on a descriptor this process owns for the life of `file`.
        let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// `<root>/repos/<owner>/<name>.git`, or `<root>/local/<name>.git` for `local/<name>`.
pub fn bare_path(root: &Path, repo: &str) -> PathBuf {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, repo));
    if is_local_repo(repo) {
        return root.join(LOCAL_DIR).join(format!("{name}.git"));
    }
    root.join("repos").join(owner).join(format!("{name}.git"))
}

/// The folder under `root` that holds the local repositories.
pub const LOCAL_DIR: &str = "local";
/// The branch a local repository starts with and its runs merge into.
pub const LOCAL_DEFAULT_BRANCH: &str = "main";
/// Who makes a local repository's first commit.
pub const LOCAL_AUTHOR_NAME: &str = "Jesse Bridge";
pub const LOCAL_AUTHOR_EMAIL: &str = "jesse-bridge@localhost";

/// Run git with `args` in `dir`, stdin empty and the bridge's own identity on any commit.
fn git_as_bridge(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", LOCAL_AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", LOCAL_AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", LOCAL_AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", LOCAL_AUTHOR_EMAIL)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Create a local repository when it is missing (under the lock the caller holds): a bare
/// repository whose `HEAD` is `main`, with one empty commit `init` by [`LOCAL_AUTHOR_NAME`].
/// It has no remote. An existing one is left as it is.
pub fn ensure_local_bare(bare: &Path) -> Result<(), String> {
    let main_ref = format!("refs/heads/{LOCAL_DEFAULT_BRANCH}");
    if bare.join("HEAD").is_file()
        && git(bare, &["rev-parse", "--verify", "--quiet", &main_ref]).is_ok()
    {
        return Ok(());
    }
    std::fs::create_dir_all(bare)
        .map_err(|e| format!("could not create {}: {e}", bare.display()))?;
    git(bare, &["init", "--bare", "--quiet"])?;
    git(bare, &["symbolic-ref", "HEAD", &main_ref])?;
    // The empty tree, written from empty stdin, then a commit of it.
    let tree = git_as_bridge(bare, &["mktree"])?.trim().to_string();
    let commit = git_as_bridge(bare, &["commit-tree", &tree, "-m", "init"])?
        .trim()
        .to_string();
    git(bare, &["update-ref", &main_ref, &commit, ""])?;
    Ok(())
}

/// Whether a local repository's `branch` is already in its `main` (an ancestor of it, or the
/// same commit). `None` when either is missing or git could not say.
pub fn merged_into_local_main(bare: &Path, branch: &str) -> Option<bool> {
    let b = format!("refs/heads/{branch}");
    let main = format!("refs/heads/{LOCAL_DEFAULT_BRANCH}");
    git(bare, &["rev-parse", "--verify", "--quiet", &b]).ok()?;
    git(bare, &["rev-parse", "--verify", "--quiet", &main]).ok()?;
    let out = git_command()
        .arg("-C")
        .arg(bare)
        .args(["merge-base", "--is-ancestor", &b, &main])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Whether a local repository has `branch` (a local branch, the only kind it has).
pub fn local_branch_exists(bare: &Path, branch: &str) -> bool {
    git(
        bare,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// The commit a local repository's `main` is at.
pub fn local_main_sha(root: &Path, repo: &str) -> Option<String> {
    let bare = bare_path(root, repo);
    git(
        &bare,
        &["rev-parse", &format!("refs/heads/{LOCAL_DEFAULT_BRANCH}")],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// Whether a worktree has nothing uncommitted (`status --porcelain` is empty). False when git
/// could not say.
pub fn worktree_clean(worktree: &Path) -> bool {
    git(worktree, &["status", "--porcelain"]).is_ok_and(|s| s.trim().is_empty())
}

/// Bring the plain checkout of a local repository's `main` at `checkout` up to date: clone it
/// on first use, then `pull --ff-only`. It is a copy to read; nothing is ever written back.
pub fn update_local_checkout(root: &Path, repo: &str, checkout: &Path) -> Result<(), String> {
    let bare = bare_path(root, repo);
    if !bare.join("HEAD").is_file() {
        return Err(format!("{repo} has no repository at {}", bare.display()));
    }
    if checkout.join(".git").exists() {
        git(checkout, &["pull", "--ff-only", "--quiet"]).map(|_| ())
    } else {
        let parent = checkout.parent().unwrap_or(checkout);
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        let out = git_command()
            .args(["clone", "--quiet", "--branch", LOCAL_DEFAULT_BRANCH])
            .arg(&bare)
            .arg(checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git clone {} failed: {}",
                bare.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// Run git with `args` in `dir`, with a clean, non-interactive environment for its prompts.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Create the bare clone when it is missing (under the lock the caller holds).
pub fn ensure_bare(bare: &Path, url: &str) -> Result<(), String> {
    if bare.join("HEAD").is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(bare)
        .map_err(|e| format!("could not create {}: {e}", bare.display()))?;
    git(bare, &["init", "--bare", "--quiet"])?;
    git(bare, &["remote", "add", "origin", url])?;
    git(
        bare,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )?;
    Ok(())
}

/// `fetch --prune`, then the remote's default branch (`main`, say).
pub fn fetch_default(bare: &Path) -> Result<String, String> {
    git(bare, &["fetch", "--prune", "--quiet", "origin"])?;
    let _ = git(bare, &["remote", "set-head", "origin", "--auto"]);
    let head = git(
        bare,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    let branch = head.strip_prefix("origin/").unwrap_or("").to_string();
    if !branch.is_empty() {
        return Ok(branch);
    }
    for b in ["main", "master"] {
        if git(
            bare,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/origin/{b}"),
            ],
        )
        .is_ok()
        {
            return Ok(b.to_string());
        }
    }
    Err("the remote has no default branch".into())
}

/// A cloud run's pushed `branch`, fetched read only into the bridge's bare clone of `repo`
/// (cloned first when the Studio never had it), with the default branch: `(default, head)`.
/// Nothing is pushed and no worktree is made.
pub fn fetch_branch_head(
    root: &Path,
    repo: &str,
    url: &str,
    branch: &str,
) -> Result<(String, String), String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    ensure_bare(&bare, url)?;
    let default = fetch_default(&bare)?;
    let head = git(
        &bare,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}^{{commit}}"),
        ],
    )
    .map_err(|_| format!("its branch {branch} is not on origin"))?;
    Ok((default, head.trim().to_string()))
}

/// The head of pull request `number` of `repo`, fetched read only into the bridge's bare clone
/// (under `refs/jesse/pull/<number>`), whether or not its branch was deleted on merge.
pub fn fetch_pull_head(root: &Path, repo: &str, url: &str, number: u64) -> Result<String, String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    ensure_bare(&bare, url)?;
    let local = format!("refs/jesse/pull/{number}");
    git(
        &bare,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "origin",
            &format!("+refs/pull/{number}/head:{local}"),
        ],
    )?;
    git(&bare, &["rev-parse", "--verify", "--quiet", &local]).map(|s| s.trim().to_string())
}

/// What a new worktree started from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorktree {
    pub default_branch: String,
    pub base_sha: String,
}

/// Clone if needed, fetch, and add `worktree` on a new `branch` from `origin/<default>`. All
/// under the repository lock. A local repository is created if needed and the branch is cut
/// from its `main`, with no fetch (`url` is not used).
pub fn add_worktree(
    root: &Path,
    repo: &str,
    url: &str,
    worktree: &Path,
    branch: &str,
) -> Result<NewWorktree, String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    let (default_branch, base) = if is_local_repo(repo) {
        // No remote and no fetch: the run branches from the repository's own `main`.
        ensure_local_bare(&bare)?;
        (
            LOCAL_DEFAULT_BRANCH.to_string(),
            format!("refs/heads/{LOCAL_DEFAULT_BRANCH}"),
        )
    } else {
        ensure_bare(&bare, url)?;
        let default_branch = fetch_default(&bare)?;
        let base = format!("origin/{default_branch}");
        (default_branch, base)
    };
    let base_sha = git(&bare, &["rev-parse", &base])?.trim().to_string();
    let wt = worktree.to_string_lossy().into_owned();
    git(
        &bare,
        &["worktree", "add", "--quiet", "-b", branch, &wt, &base_sha],
    )?;
    let _ = git(&bare, &["gc", "--auto", "--quiet"]);
    Ok(NewWorktree {
        default_branch,
        base_sha,
    })
}

// ---- SAVING A RUN'S WORK -------------------------------------------------------------------
//
// NOTHING THAT EXISTS ONLY ON THIS MACHINE IS EVER REMOVED. Before any worktree of a run goes,
// [`unsaved_work`] asks git what in it is on no remote and in no saved ref; anything it names
// is first saved by [`save_work`] (pushed to `jesse/<branch>-wip` on origin, or kept under
// `refs/jesse/saved/` in a local repository). When saving fails, nothing is removed.

/// Where the bridge keeps its own record of saved work in a bare clone:
/// `refs/jesse/saved/<run_id>/<unix seconds>`.
pub const SAVED_PREFIX: &str = "refs/jesse/saved";

/// One piece of a run's work saved by [`save_work`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRef {
    /// Where it is: `jesse/<branch>-wip` (or `-wip-<unix seconds>`) on origin, or
    /// `refs/jesse/saved/<run_id>/<unix seconds>` in a local repository.
    #[serde(rename = "ref")]
    pub name: String,
    /// The commit saved: the worktree's `HEAD`, or a snapshot commit on top of it.
    pub sha: String,
    /// The bridge's own ref for it in the bare clone (`refs/jesse/saved/...`).
    #[serde(default)]
    pub local: String,
    /// The worktree it came from, relative to the run directory (`worktree`, `wt4`).
    #[serde(default)]
    pub worktree: String,
    /// When it was saved (RFC 3339).
    #[serde(default)]
    pub at: String,
}

impl SavedRef {
    /// `Its work is saved on <ref> at <sha>.`, the line a cloud report uses too.
    pub fn report_line(&self) -> String {
        format!("Its work is saved on {} at {}.", self.name, self.sha)
    }
}

/// A git worktree registered with a bare clone ([`run_worktrees`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWorktree {
    pub path: PathBuf,
    /// The branch it has checked out; `None` when detached.
    pub branch: Option<String>,
}

/// Every worktree of `bare` whose path is inside `run_dir` and exists: the bridge's own
/// `worktree/` and any an agent made beside it (`runs/<ID>/wt4`).
pub fn run_worktrees(bare: &Path, run_dir: &Path) -> Vec<RunWorktree> {
    let Ok(text) = git(bare, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = canon(run_dir);
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut path: Option<PathBuf> = None;
        let mut branch: Option<String> = None;
        for l in block.lines() {
            if let Some(p) = l.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = l.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            }
        }
        let Some(path) = path else { continue };
        if !path.exists() {
            continue;
        }
        let c = canon(&path);
        if c != dir && c.starts_with(&dir) {
            out.push(RunWorktree { path, branch });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Whether `bare` has an `origin` (a GitHub clone) rather than being a local repository.
fn has_origin(bare: &Path) -> bool {
    git(bare, &["remote", "get-url", "origin"]).is_ok()
}

/// Whether `dir` is a git checkout of its own (has a `.git`), so git run in it never answers
/// for some repository above it.
fn is_checkout(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn rev(dir: &Path, what: &str) -> Option<String> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{what}^{{commit}}"),
        ],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// The commits worth keeping: the worktree's `HEAD` and the branch, when they exist.
fn tips(bare: &Path, worktree: &Path, branch: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if is_checkout(worktree) {
        if let Some(h) = rev(worktree, "HEAD") {
            out.push((format!("{}'s HEAD", worktree.display()), h));
        }
    }
    if let Some(h) = rev(bare, &format!("refs/heads/{branch}")) {
        if !out.iter().any(|(_, x)| *x == h) {
            out.push((format!("branch {branch}"), h));
        }
    }
    out
}

/// How many commits of `tip` no remote branch, no saved ref, no pull request head the bridge
/// fetched (`refs/jesse/pull/*`) and, in a local repository, no `main` contains. On GitHub, a
/// tip that is exactly the head of one of origin's pull requests (`refs/pull/<n>/head`, kept by
/// GitHub after a squash merge deletes the branch) is on origin too: it is recorded as
/// `refs/jesse/pull/<n>` and counts as saved. When origin cannot be asked it does not.
fn unsaved_commits(bare: &Path, tip: &str) -> Result<u64, String> {
    let main = format!("refs/heads/{LOCAL_DEFAULT_BRANCH}");
    let origin = has_origin(bare);
    let count = || -> Result<u64, String> {
        let mut args = vec![
            "rev-list",
            "--count",
            tip,
            "--not",
            "--remotes",
            "--glob=refs/jesse/saved",
            "--glob=refs/jesse/pull",
        ];
        if !origin && rev(bare, &main).is_some() {
            args.push(&main);
        }
        let out = git(bare, &args)?;
        out.trim()
            .parse::<u64>()
            .map_err(|e| format!("git rev-list said {:?}: {e}", out.trim()))
    };
    let n = count()?;
    if n == 0 || !origin {
        return Ok(n);
    }
    let Ok(heads) = git(bare, &["ls-remote", "origin", "refs/pull/*/head"]) else {
        return Ok(n);
    };
    let pull = heads.lines().find_map(|l| {
        let (sha, r) = l.split_once('\t')?;
        (sha.trim() == tip)
            .then(|| r.strip_prefix("refs/pull/")?.strip_suffix("/head"))
            .flatten()
            .map(str::to_string)
    });
    match pull {
        Some(p) => {
            git(bare, &["update-ref", &format!("refs/jesse/pull/{p}"), tip])?;
            count()
        }
        None => Ok(n),
    }
}

/// What in `worktree` (and on `branch`) exists nowhere else, in one line: uncommitted or
/// untracked changes (`status --porcelain`, so `.gitignore`d paths do not count), or commits no
/// remote branch and no `refs/jesse/saved/*` ref contains (nor a pull request head on origin,
/// [`unsaved_commits`]). Run under the repository lock: it may record a pull request head. `Ok(None)` when everything is saved.
/// `Err` when git could not say, which every caller treats as unsaved: never a guess of "safe".
pub fn unsaved_work(bare: &Path, worktree: &Path, branch: &str) -> Result<Option<String>, String> {
    let mut parts: Vec<String> = Vec::new();
    if worktree.exists() {
        if !is_checkout(worktree) {
            return Err(format!("{} is not a git checkout", worktree.display()));
        }
        let status = git(worktree, &["status", "--porcelain"])?;
        let n = status.lines().filter(|l| !l.trim().is_empty()).count();
        if n > 0 {
            parts.push(format!("{n} uncommitted or untracked path(s)"));
        }
    }
    for (what, tip) in tips(bare, worktree, branch) {
        let n = unsaved_commits(bare, &tip)?;
        if n > 0 {
            parts.push(format!(
                "{n} commit(s) on {what} that no remote branch or saved ref has"
            ));
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join(", ")))
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Git as the bridge, with extra environment (a temporary `GIT_INDEX_FILE`).
fn git_env(dir: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Result<String, String> {
    let mut cmd = git_command();
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", LOCAL_AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", LOCAL_AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", LOCAL_AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", LOCAL_AUTHOR_EMAIL)
        .stdin(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The snapshot commit's subject: `[<run_id>] wip snapshot <time>`.
pub fn snapshot_subject_prefix(run_id: &str) -> String {
    format!("[{run_id}] wip snapshot")
}

/// A commit of everything in `worktree` (tracked, changed and untracked, `.gitignore` obeyed)
/// on top of `head`, made in a copy of its index so the worktree's own index and files are
/// never touched. A snapshot this run already saved of exactly this tree on this `head` is
/// used again, so saving an unchanged worktree twice never makes a second, diverging one.
fn snapshot_commit(
    bare: &Path,
    worktree: &Path,
    head: Option<&str>,
    run_id: &str,
) -> Result<String, String> {
    let src = git(worktree, &["rev-parse", "--git-path", "index"])?;
    let src = PathBuf::from(src.trim());
    let src = if src.is_absolute() {
        src
    } else {
        worktree.join(src)
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("jesse-wip-index-{}-{nanos}", std::process::id()));
    let made = (|| {
        if src.is_file() {
            std::fs::copy(&src, &tmp)
                .map_err(|e| format!("could not copy the index {}: {e}", src.display()))?;
        }
        let env = [("GIT_INDEX_FILE", tmp.as_os_str())];
        if !src.is_file() {
            if let Some(h) = head {
                git_env(worktree, &["read-tree", h], &env)?;
            }
        }
        git_env(worktree, &["add", "-A"], &env)?;
        let tree = git_env(worktree, &["write-tree"], &env)?.trim().to_string();
        let again = git(
            bare,
            &[
                "for-each-ref",
                "--format=%(objectname) %(tree) %(parent)",
                &format!("{SAVED_PREFIX}/{run_id}/"),
            ],
        )
        .unwrap_or_default()
        .lines()
        .find_map(|l| {
            let mut f = l.split(' ');
            let (c, t, p) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            (t == tree && p == head.unwrap_or("")).then(|| c.to_string())
        });
        if let Some(c) = again {
            return Ok(c);
        }
        let msg = format!(
            "{} {}",
            snapshot_subject_prefix(run_id),
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(h) = head {
            args.extend(["-p", h]);
        }
        args.extend(["-m", msg.as_str()]);
        Ok(git_env(worktree, &args, &[])?.trim().to_string())
    })();
    let _ = std::fs::remove_file(&tmp);
    made
}

/// A ref under `refs/jesse/saved/<run_id>/<secs>` that does not exist yet.
fn new_saved_ref(bare: &Path, run_id: &str, secs: u64) -> String {
    let base = format!("{SAVED_PREFIX}/{run_id}/{secs}");
    let mut name = base.clone();
    let mut n = 2;
    while rev(bare, &name).is_some() {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// Whether a push failed only because the remote ref moved elsewhere (not a fast-forward).
fn not_fast_forward(err: &str) -> bool {
    err.contains("[rejected]") && (err.contains("non-fast-forward") || err.contains("fetch first"))
}

/// Save what `worktree` (on `branch`) holds that exists nowhere else, without touching its index
/// or files: a snapshot commit on top of `HEAD` when there are uncommitted changes, else `HEAD`
/// itself. A GitHub clone pushes it to `refs/heads/jesse/<branch>-wip` with a plain push (a new
/// ref or a fast-forward); when that is not a fast-forward it goes to
/// `jesse/<branch>-wip-<unix seconds>` instead. Nothing is ever force pushed. A local repository
/// keeps it under `refs/jesse/saved/<run_id>/<unix seconds>`. Either way that local ref is
/// written once the work is safe, so [`unsaved_work`] counts it as saved. Run under the
/// repository lock.
pub fn save_work(
    bare: &Path,
    worktree: &Path,
    branch: &str,
    run_id: &str,
) -> Result<SavedRef, String> {
    let checkout = is_checkout(worktree);
    if worktree.exists() && !checkout {
        return Err(format!("{} is not a git checkout", worktree.display()));
    }
    let head = if checkout {
        rev(worktree, "HEAD")
    } else {
        rev(bare, &format!("refs/heads/{branch}"))
    };
    let dirty = checkout && !git(worktree, &["status", "--porcelain"])?.trim().is_empty();
    let sha = if dirty {
        snapshot_commit(bare, worktree, head.as_deref(), run_id)?
    } else {
        head.ok_or_else(|| format!("{} has no commit to save", worktree.display()))?
    };
    let secs = unix_secs();
    let name = if has_origin(bare) {
        let wip = format!("jesse/{branch}-wip");
        let push = |to: &str| {
            git(
                bare,
                &[
                    "push",
                    "--quiet",
                    "origin",
                    &format!("{sha}:refs/heads/{to}"),
                ],
            )
        };
        match push(&wip) {
            Ok(_) => wip,
            Err(e) if not_fast_forward(&e) => {
                let alt = format!("{wip}-{secs}");
                push(&alt).map_err(|e2| {
                    format!("the push to {wip} was not a fast-forward and the push to {alt} failed: {e2}")
                })?;
                alt
            }
            Err(e) => return Err(format!("the push to {wip} failed: {e}")),
        }
    } else {
        String::new()
    };
    // The bridge's own ref for it: one already pointing at it is used again.
    let local = match refs_matching(bare, &format!("{SAVED_PREFIX}/{run_id}/"))
        .into_iter()
        .find(|(_, s)| *s == sha)
    {
        Some((r, _)) => r,
        None => {
            let local = new_saved_ref(bare, run_id, secs);
            git(bare, &["update-ref", &local, &sha, ""])?;
            local
        }
    };
    let name = if name.is_empty() { local.clone() } else { name };
    let rel = worktree
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(SavedRef {
        name,
        sha,
        local,
        worktree: rel,
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

/// Save `worktree`'s work when [`unsaved_work`] names any (a git error counts as unsaved).
/// `Ok(None)`: everything was already saved. Under the repository lock.
pub fn save_if_unsaved(
    root: &Path,
    repo: &str,
    worktree: &Path,
    branch: &str,
    run_id: &str,
) -> Result<Option<SavedRef>, String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    save_if_unsaved_locked(&bare, worktree, branch, run_id)
}

fn save_if_unsaved_locked(
    bare: &Path,
    worktree: &Path,
    branch: &str,
    run_id: &str,
) -> Result<Option<SavedRef>, String> {
    let what = match unsaved_work(bare, worktree, branch) {
        Ok(None) => return Ok(None),
        Ok(Some(w)) => w,
        Err(e) => format!("git could not say what is saved: {e}"),
    };
    save_work(bare, worktree, branch, run_id)
        .map(Some)
        .map_err(|e| format!("it holds unsaved work ({what}) and saving it failed: {e}"))
}

// ---- RESTORING A REMOVED WORKTREE ----------------------------------------------------------

/// Where a removed worktree comes back from ([`restore_worktree`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePoint {
    pub sha: String,
    /// What it was read from: `origin/<branch>`, `jesse/<branch>-wip`, `refs/jesse/saved/...`.
    pub from: String,
    /// From saved work (a `-wip` ref, a saved ref, the record's `saved_refs`), not the branch.
    pub saved: bool,
    /// Its top commit is a [`save_work`] snapshot of uncommitted changes.
    pub snapshot: bool,
}

impl RestorePoint {
    /// What the resumed agent is told, when it was restored from saved work.
    pub fn continue_note(&self) -> Option<String> {
        if !self.saved {
            return None;
        }
        let mut s = format!(
            "Your previous worktree was removed; it was restored from {}.",
            self.from
        );
        if self.snapshot {
            s.push_str(
                " The top commit is a snapshot of uncommitted work: review it and fold it into \
                 proper commits.",
            );
        }
        Some(s)
    }
}

fn commit_time(bare: &Path, sha: &str) -> i64 {
    git(bare, &["log", "-1", "--format=%ct", sha])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn is_ancestor(bare: &Path, a: &str, b: &str) -> bool {
    git(bare, &["merge-base", "--is-ancestor", a, b]).is_ok()
}

/// Refs matching `pattern` (for-each-ref), each `(refname, sha)`.
fn refs_matching(bare: &Path, pattern: &str) -> Vec<(String, String)> {
    git(
        bare,
        &["for-each-ref", "--format=%(refname) %(objectname)", pattern],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| l.split_once(' '))
    .map(|(r, s)| (r.to_string(), s.to_string()))
    .collect()
}

/// The newest place a removed worktree can come back from: the branch (`origin/<branch>`, or
/// a local repository's own), unless saved work newer than it and built on it exists:
/// `origin/jesse/<branch>-wip*`, then `refs/jesse/saved/<run_id>/*`, then the record's
/// `saved_refs`, then a local branch the cleanup kept. `Err` names every place it looked.
fn find_restore_point(
    bare: &Path,
    local_repo: bool,
    branch: &str,
    run_id: &str,
    saved: &[SavedRef],
) -> Result<RestorePoint, String> {
    let base: Option<(String, String)> = if local_repo {
        rev(bare, &format!("refs/heads/{branch}")).map(|s| (format!("the branch {branch}"), s))
    } else {
        rev(bare, &format!("refs/remotes/origin/{branch}")).map(|s| (format!("origin/{branch}"), s))
    };
    // Saved work, in the order the places are looked in; the newest commit wins.
    let mut found: Vec<(String, String)> = Vec::new();
    if !local_repo {
        let mut wip = refs_matching(bare, &format!("refs/remotes/origin/jesse/{branch}-wip"));
        wip.extend(refs_matching(
            bare,
            &format!("refs/remotes/origin/jesse/{branch}-wip-*"),
        ));
        for (r, s) in wip {
            found.push((r.trim_start_matches("refs/remotes/origin/").to_string(), s));
        }
    }
    found.extend(refs_matching(bare, &format!("{SAVED_PREFIX}/{run_id}/")));
    for s in saved.iter().rev() {
        if let Some(sha) = rev(bare, &s.sha) {
            found.push((s.name.clone(), sha));
        }
    }
    if !local_repo {
        if let Some(sha) = rev(bare, &format!("refs/heads/{branch}")) {
            found.push((format!("the local branch {branch}"), sha));
        }
    }
    let mut newest: Option<(String, String, i64)> = None;
    for (r, s) in found {
        let t = commit_time(bare, &s);
        if newest.as_ref().is_none_or(|(_, _, nt)| t > *nt) {
            newest = Some((r, s, t));
        }
    }
    let pick = |from: String, sha: String, saved: bool| {
        let subject = git(bare, &["log", "-1", "--format=%s", &sha]).unwrap_or_default();
        RestorePoint {
            snapshot: subject.starts_with(&snapshot_subject_prefix(run_id)),
            sha,
            from,
            saved,
        }
    };
    match (base, newest) {
        (Some((from, sha)), Some((r, s, _))) => {
            // Saved work replaces the branch only when it is built on it: work never goes
            // backwards, and a branch pushed since is never set aside for an older snapshot.
            if s != sha && !is_ancestor(bare, &s, &sha) && is_ancestor(bare, &sha, &s) {
                Ok(pick(r, s, true))
            } else {
                Ok(pick(from, sha, false))
            }
        }
        (Some((from, sha)), None) => Ok(pick(from, sha, false)),
        (None, Some((r, s, _))) => {
            // A local repository's branch is deleted only once merged into `main`: saved work
            // `main` already holds is not worth going back to.
            let main = format!("refs/heads/{LOCAL_DEFAULT_BRANCH}");
            match rev(bare, &main) {
                Some(m) if local_repo && is_ancestor(bare, &s, &m) => {
                    Ok(pick(LOCAL_DEFAULT_BRANCH.to_string(), m, false))
                }
                _ => Ok(pick(r, s, true)),
            }
        }
        (None, None) => {
            if local_repo {
                if let Some(m) = rev(bare, &format!("refs/heads/{LOCAL_DEFAULT_BRANCH}")) {
                    return Ok(pick(LOCAL_DEFAULT_BRANCH.to_string(), m, false));
                }
                return Err(format!(
                    "its worktree was removed and there is nothing to continue from: looked for \
                     the branch {branch}, {SAVED_PREFIX}/{run_id}/*, the run record's saved refs \
                     ({}) and {LOCAL_DEFAULT_BRANCH}",
                    saved.len()
                ));
            }
            Err(format!(
                "its worktree was removed and there is nothing to continue from: looked for \
                 origin/{branch}, origin/jesse/{branch}-wip*, {SAVED_PREFIX}/{run_id}/*, the run \
                 record's saved refs ({}) and the local branch {branch}",
                saved.len()
            ))
        }
    }
}

/// [`find_restore_point`] for `repo`, fetched first (under the lock). A fetch that fails is
/// named in the refusal, and what is already in the bare clone is still looked at.
fn restore_point_locked(
    bare: &Path,
    repo: &str,
    url: &str,
    branch: &str,
    run_id: &str,
    saved: &[SavedRef],
) -> Result<RestorePoint, String> {
    let local_repo = is_local_repo(repo);
    let fetched = if local_repo {
        Ok(())
    } else {
        ensure_bare(bare, url)?;
        fetch_default(bare).map(|_| ())
    };
    find_restore_point(bare, local_repo, branch, run_id, saved).map_err(|e| match fetched {
        Ok(()) => e,
        Err(f) => format!("{e} (the fetch from origin failed: {f})"),
    })
}

/// Where a removed worktree of the run would come back from ([`restore_worktree`]), changing
/// nothing but the bare clone's fetched refs.
pub fn restore_point(
    root: &Path,
    repo: &str,
    url: &str,
    branch: &str,
    run_id: &str,
    saved: &[SavedRef],
) -> Result<RestorePoint, String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    if is_local_repo(repo) && !bare.join("HEAD").is_file() {
        return Err(format!(
            "its worktree was removed and {repo} has no repository, so there is nothing to continue from"
        ));
    }
    restore_point_locked(&bare, repo, url, branch, run_id, saved)
}

/// Re-create a removed worktree on `branch`, from the newest of the run's pushed branch and its
/// saved work ([`find_restore_point`]). A snapshot of uncommitted changes comes back as the top
/// commit. A local branch the restore would move off commits it alone has is saved under
/// `refs/jesse/saved/<run_id>/` first. `Err` names every place it looked.
pub fn restore_worktree(
    root: &Path,
    repo: &str,
    url: &str,
    worktree: &Path,
    branch: &str,
    run_id: &str,
    saved: &[SavedRef],
) -> Result<RestorePoint, String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    if is_local_repo(repo) && !bare.join("HEAD").is_file() {
        return Err(format!(
            "its worktree was removed and {repo} has no repository, so there is nothing to continue from"
        ));
    }
    let point = restore_point_locked(&bare, repo, url, branch, run_id, saved)?;
    let _ = git(&bare, &["worktree", "prune"]);
    if let Some(cur) = rev(&bare, &format!("refs/heads/{branch}")) {
        if cur != point.sha && !is_ancestor(&bare, &cur, &point.sha) {
            let keep = new_saved_ref(&bare, run_id, unix_secs());
            git(&bare, &["update-ref", &keep, &cur, ""])?;
        }
    }
    let wt = worktree.to_string_lossy().into_owned();
    git(
        &bare,
        &["worktree", "add", "--quiet", "-B", branch, &wt, &point.sha],
    )?;
    Ok(point)
}

/// Remove one worktree of a run, and its branch, then prune. Under the repository lock.
///
/// It refuses to lose work: [`unsaved_work`] is asked first, and anything it names (or a git
/// error) is saved by [`save_work`] before anything is removed. When saving fails (no network,
/// a refused push) nothing is removed and the error says why. The branch is deleted only when
/// no commit of it is left outside origin's branches and the saved refs; a local repository's
/// branch, besides, only once it is merged into its `main`. `Ok(Some(saved))` names what was
/// saved on the way.
pub fn remove_worktree(
    root: &Path,
    repo: &str,
    worktree: &Path,
    branch: &str,
    run_id: &str,
) -> Result<Option<SavedRef>, String> {
    let bare = bare_path(root, repo);
    if !bare.join("HEAD").is_file() {
        if worktree.exists() && std::fs::read_dir(worktree).is_ok_and(|mut d| d.next().is_some()) {
            return Err(format!(
                "{} was not removed: {repo} has no clone here, so what it holds cannot be checked or saved",
                worktree.display()
            ));
        }
        return crate::hostrun::sys::remove_tree(worktree)
            .map(|_| None)
            .map_err(|e| format!("could not remove {}: {e}", worktree.display()));
    }
    let _lock = lock_repo(root, repo)?;
    let saved = save_if_unsaved_locked(&bare, worktree, branch, run_id)
        .map_err(|e| format!("{} was not removed: {e}", worktree.display()))?;
    // Checked once more: every commit worth keeping is on a remote branch or a saved ref now.
    for (what, tip) in tips(&bare, worktree, branch) {
        match unsaved_commits(&bare, &tip) {
            Ok(0) => {}
            Ok(n) => {
                return Err(format!(
                    "{} was not removed: {n} commit(s) on {what} are still on no remote branch or saved ref",
                    worktree.display()
                ))
            }
            Err(e) => {
                return Err(format!(
                    "{} was not removed: git could not say what is saved: {e}",
                    worktree.display()
                ))
            }
        }
    }
    let wt = worktree.to_string_lossy().into_owned();
    if worktree.exists() {
        // `--force` only here, once the check above (and the save) left nothing unsaved: what
        // was uncommitted is in the snapshot.
        if let Err(e) = git(&bare, &["worktree", "remove", "--force", &wt]) {
            // A worktree git no longer knows: remove the directory itself, it is inside root.
            eprintln!("jesse-bridge: hostrun: {e}; removing the directory");
            crate::hostrun::sys::remove_tree(worktree)
                .map_err(|e| format!("could not remove {}: {e}", worktree.display()))?;
        }
    }
    // The branch's commits are all on origin or in a saved ref (checked above). A local
    // repository's branch is the copy people use: it goes only once merged into its `main`.
    if !is_local_repo(repo) || merged_into_local_main(&bare, branch) == Some(true) {
        let _ = git(&bare, &["branch", "-D", branch]);
    }
    git(&bare, &["worktree", "prune"])?;
    Ok(saved)
}

/// Whether `branch` is on the remote right now (`ls-remote`, no lock: it changes nothing).
/// `None` when the remote could not be asked. A local repository has no remote: this is
/// whether the work can be continued, from the branch or (once merged and deleted) from `main`.
pub fn branch_on_origin(root: &Path, repo: &str, branch: &str) -> Option<bool> {
    let bare = bare_path(root, repo);
    if !bare.join("HEAD").is_file() {
        return None;
    }
    if is_local_repo(repo) {
        // A local branch is deleted only once it is in `main`, so either holds the work.
        return Some(
            local_branch_exists(&bare, branch) || local_branch_exists(&bare, LOCAL_DEFAULT_BRANCH),
        );
    }
    git(
        &bare,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()
    .map(|s| !s.trim().is_empty())
}

/// The worktree paths a bare clone has registered (`git worktree list --porcelain`), the bare
/// clone itself excluded.
pub fn registered_worktrees(bare: &Path) -> Vec<PathBuf> {
    let Ok(text) = git(bare, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let bare_canon = std::fs::canonicalize(bare).unwrap_or_else(|_| bare.to_path_buf());
    text.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|p| {
            let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            c != bare_canon
        })
        .collect()
}

/// `worktree prune` under the lock.
pub fn prune(root: &Path, repo: &str) -> Result<(), String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    git(&bare, &["worktree", "prune"]).map(|_| ())
}

/// Every `owner/name` with a bare clone under `<root>/repos`.
pub fn cloned_repos(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(owners) = std::fs::read_dir(root.join("repos")) else {
        return out;
    };
    for o in owners.flatten() {
        let owner = o.file_name().to_string_lossy().into_owned();
        let Ok(names) = std::fs::read_dir(o.path()) else {
            continue;
        };
        for n in names.flatten() {
            let name = n.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".git") {
                out.push(format!("{owner}/{stem}"));
            }
        }
    }
    out.sort();
    out
}

/// A `git` command for the bridge's own work on a run (clones, worktrees, fetches, the local
/// checkout), pointed at the bridge's attribution hooks once they are installed, so the
/// repository's own hooks run through them exactly as they do for the agent.
fn git_command() -> Command {
    let mut cmd = Command::new("git");
    crate::attribution::apply_global_std(&mut cmd);
    cmd
}

/// Add a DETACHED worktree of `repo` at `commit`, for a reviewer to read (under the repository
/// lock). One left at `path` by an earlier attempt is removed first. It has no branch, so
/// nothing a reviewer does there could move one.
pub fn add_detached_worktree(
    root: &Path,
    repo: &str,
    path: &Path,
    commit: &str,
) -> Result<(), String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    let p = path.to_string_lossy().into_owned();
    if path.exists() {
        let _ = git(&bare, &["worktree", "remove", "--force", &p]);
        if path.exists() {
            crate::hostrun::sys::remove_tree(path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        }
    }
    let _ = git(&bare, &["worktree", "prune"]);
    git(
        &bare,
        &["worktree", "add", "--quiet", "--detach", &p, commit],
    )
    .map(|_| ())
}

/// Remove a worktree [`add_detached_worktree`] made, and prune. Best effort: a leftover is
/// removed by the next attempt.
pub fn remove_detached_worktree(root: &Path, repo: &str, path: &Path) {
    let bare = bare_path(root, repo);
    let Ok(_lock) = lock_repo(root, repo) else {
        return;
    };
    let p = path.to_string_lossy().into_owned();
    let _ = git(&bare, &["worktree", "remove", "--force", &p]);
    if path.exists() {
        let _ = crate::hostrun::sys::remove_tree(path);
    }
    let _ = git(&bare, &["worktree", "prune"]);
}
