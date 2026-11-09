//! The bare clones and their worktrees.
//!
//! **EVERY GIT COMMAND THAT CHANGES A BARE CLONE RUNS UNDER ITS REPOSITORY LOCK**: an exclusive
//! `flock` on `<root>/locks/<owner>__<name>.lock` ([`RepoLock`]). Two runs starting at once, or a
//! start racing a cleanup, take turns. The lock is held only for those commands, never while an
//! agent works.
//!
//! A bare clone keeps the remote's branches under `refs/remotes/origin/*` (not the `clone
//! --bare` mirror refspec), so a fetch never tries to move a branch a worktree has checked out.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// `<root>/repos/<owner>/<name>.git`.
pub fn bare_path(root: &Path, repo: &str) -> PathBuf {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, repo));
    root.join("repos").join(owner).join(format!("{name}.git"))
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
/// under the repository lock.
pub fn add_worktree(
    root: &Path,
    repo: &str,
    url: &str,
    worktree: &Path,
    branch: &str,
) -> Result<NewWorktree, String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
    ensure_bare(&bare, url)?;
    let default_branch = fetch_default(&bare)?;
    let base = format!("origin/{default_branch}");
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

/// Re-create a removed worktree from the run's pushed branch. `Err` when the branch is not on
/// the remote.
pub fn restore_worktree(
    root: &Path,
    repo: &str,
    url: &str,
    worktree: &Path,
    branch: &str,
) -> Result<(), String> {
    let bare = bare_path(root, repo);
    let _lock = lock_repo(root, repo)?;
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

/// Remove a run's worktree and its local branch, then prune. Under the repository lock.
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
    let _ = git(&bare, &["branch", "-D", branch]);
    git(&bare, &["worktree", "prune"])?;
    Ok(())
}

/// Whether `branch` is on the remote right now (`ls-remote`, no lock: it changes nothing).
/// `None` when the remote could not be asked.
pub fn branch_on_origin(root: &Path, repo: &str, branch: &str) -> Option<bool> {
    let bare = bare_path(root, repo);
    if !bare.join("HEAD").is_file() {
        return None;
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
