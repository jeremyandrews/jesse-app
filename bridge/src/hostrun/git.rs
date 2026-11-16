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
    let out = Command::new("git")
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
    let out = Command::new("git")
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
        let out = Command::new("git")
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
    let out = Command::new("git")
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

/// Re-create a removed worktree from the run's pushed branch (a local repository's: its local
/// branch, else, once merged and deleted, `main`). `Err` when the branch is not on the remote.
pub fn restore_worktree(
    root: &Path,
    repo: &str,
    url: &str,
    worktree: &Path,
    branch: &str,
) -> Result<(), String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    if is_local_repo(repo) {
        // The branch never left this machine: it is restored from the local branch itself. A
        // branch that is gone was deleted only once merged, so `main` holds all of its work and
        // the branch is cut again from there.
        let _ = git(&bare, &["worktree", "prune"]);
        let wt = worktree.to_string_lossy().into_owned();
        if local_branch_exists(&bare, branch) {
            git(&bare, &["worktree", "add", "--quiet", &wt, branch])?;
        } else if local_branch_exists(&bare, LOCAL_DEFAULT_BRANCH) {
            git(
                &bare,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    branch,
                    &wt,
                    LOCAL_DEFAULT_BRANCH,
                ],
            )?;
        } else {
            return Err(format!(
                "its worktree was removed and {repo} has neither the branch {branch} nor main, so there is nothing to continue from"
            ));
        }
        return Ok(());
    }
    ensure_bare(&bare, url)?;
    fetch_default(&bare)?;
    let remote = format!("refs/remotes/origin/{branch}");
    if git(&bare, &["rev-parse", "--verify", "--quiet", &remote]).is_err() {
        return Err(format!(
            "its worktree was removed and the branch {branch} is not on origin, so there is nothing to continue from"
        ));
    }
    let _ = git(&bare, &["worktree", "prune"]);
    let wt = worktree.to_string_lossy().into_owned();
    git(
        &bare,
        &[
            "worktree",
            "add",
            "--quiet",
            "-B",
            branch,
            &wt,
            &format!("origin/{branch}"),
        ],
    )?;
    Ok(())
}

/// Remove a run's worktree and its local branch, then prune. Under the repository lock. A local
/// repository's branch is kept unless it is already merged into its `main`.
pub fn remove_worktree(
    root: &Path,
    repo: &str,
    worktree: &Path,
    branch: &str,
) -> Result<(), String> {
    let bare = bare_path(root, repo);
    if !bare.join("HEAD").is_file() {
        return crate::hostrun::sys::remove_tree(worktree)
            .map_err(|e| format!("could not remove {}: {e}", worktree.display()));
    }
    let _lock = lock_repo(root, repo)?;
    let wt = worktree.to_string_lossy().into_owned();
    if worktree.exists() {
        if let Err(e) = git(&bare, &["worktree", "remove", "--force", &wt]) {
            // A worktree git no longer knows: remove the directory itself, it is inside root.
            eprintln!("jesse-bridge: hostrun: {e}; removing the directory");
            crate::hostrun::sys::remove_tree(worktree)
                .map_err(|e| format!("could not remove {}: {e}", worktree.display()))?;
        }
    }
    // A GitHub branch was pushed, so the local copy may go. A local repository's branch is
    // the only copy of the work: it goes only once it is merged into its `main`.
    if !is_local_repo(repo) || merged_into_local_main(&bare, branch) == Some(true) {
        let _ = git(&bare, &["branch", "-D", branch]);
    }
    git(&bare, &["worktree", "prune"])?;
    Ok(())
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
