// Stamps the commit this bridge was built from into `JESSE_GIT_SHA`, so every turn's
// `BRIDGE:` header line (`prompt::bridge_line`) and the boot ledger line can say which build
// is answering, not just which version: two builds at one version (a hotfix off a branch)
// otherwise look identical.
//
// The value is `git rev-parse --short=7 HEAD`, with `-dirty` when tracked files differ from
// HEAD, and the literal `unknown` when git is missing or this is not a checkout. It is read
// HERE, at build time, and never from a checkout at runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Rebuild whenever HEAD moves: `HEAD` itself (a checkout), the ref it points at (a
/// commit on the current branch), `packed-refs` (a ref that was packed), and the index (a
/// stage or a commit, which is what flips `-dirty`).
fn watch_git(git_dir: &Path) {
    let head = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    if let Ok(text) = std::fs::read_to_string(&head) {
        if let Some(r) = text.trim().strip_prefix("ref: ") {
            // A linked worktree keeps its branch refs in the COMMON dir, not its own.
            let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .map(PathBuf::from)
                .unwrap_or_else(|| git_dir.to_path_buf());
            println!("cargo:rerun-if-changed={}", common.join(r).display());
            println!(
                "cargo:rerun-if-changed={}",
                common.join("packed-refs").display()
            );
        }
    }
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let sha = match (
        git(&["rev-parse", "--absolute-git-dir"]),
        git(&["rev-parse", "--short=7", "HEAD"]),
    ) {
        (Some(dir), Some(sha)) if !sha.is_empty() => {
            watch_git(Path::new(&dir));
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            if dirty {
                format!("{sha}-dirty")
            } else {
                sha
            }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=JESSE_GIT_SHA={sha}");
}
