//! The operating system half of `hostrun`: launchd, signals, the disk, Xcode's DerivedData and
//! the pull request state. Each outside effect sits behind a small trait ([`Launcher`],
//! [`Forge`]) so the watcher's tests run on fixture directories with no launchd and no GitHub.

use std::io::Read as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The run label prefix when the bridge's own launchd label is unknown: a placeholder, never a
/// deployment's real namespace (which stays out of tracked files).
pub const DEFAULT_LABEL_PREFIX: &str = "com.example.jesse-run.";

/// The prefix of every run's launchd label, from the bridge's own launchd label (launchd sets
/// `XPC_SERVICE_NAME` to it): `<ns>.jesse-bridge` gives `<ns>.jesse-run.`. Anything else, or no
/// label at all, gives [`DEFAULT_LABEL_PREFIX`].
pub fn label_prefix_from(service_name: Option<&str>) -> String {
    match service_name.and_then(|s| s.strip_suffix("jesse-bridge")) {
        Some(ns) if !ns.is_empty() && ns.ends_with('.') => format!("{ns}jesse-run."),
        _ => DEFAULT_LABEL_PREFIX.to_string(),
    }
}

// ---- LAUNCHD -------------------------------------------------------------------------

/// Starts and stops a run's launchd job, and says whether one is alive.
pub trait Launcher: Send + Sync {
    /// `launchctl bootstrap gui/<uid> <plist>`.
    fn bootstrap(&self, plist: &Path, label: &str) -> Result<(), String>;
    /// `launchctl bootout gui/<uid>/<label>`. A label that is not loaded is not an error.
    fn bootout(&self, label: &str) -> Result<(), String>;
    /// Whether the label is loaded AND its program is running.
    fn is_running(&self, label: &str) -> bool;
    /// Every loaded label that starts with `prefix`.
    fn loaded_labels(&self, prefix: &str) -> Vec<String>;
    /// The plist a loaded label was bootstrapped from, when launchd says.
    fn plist_of(&self, label: &str) -> Option<PathBuf>;
}

/// The real launchd, through `launchctl` in the bridge user's GUI domain.
pub struct Launchctl {
    uid: u32,
}

impl Default for Launchctl {
    fn default() -> Self {
        // SAFETY: getuid has no preconditions and cannot fail.
        Launchctl {
            uid: unsafe { libc::getuid() },
        }
    }
}

fn run_quiet(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{:?} exited {}: {}",
            cmd.get_program(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

impl Launcher for Launchctl {
    fn bootstrap(&self, plist: &Path, _label: &str) -> Result<(), String> {
        run_quiet(
            Command::new("/bin/launchctl")
                .arg("bootstrap")
                .arg(format!("gui/{}", self.uid))
                .arg(plist),
        )
        .map(|_| ())
    }

    fn bootout(&self, label: &str) -> Result<(), String> {
        if !self.loaded(label) {
            return Ok(());
        }
        run_quiet(
            Command::new("/bin/launchctl")
                .arg("bootout")
                .arg(format!("gui/{}/{label}", self.uid)),
        )
        .map(|_| ())
    }

    fn is_running(&self, label: &str) -> bool {
        match run_quiet(
            Command::new("/bin/launchctl")
                .arg("print")
                .arg(format!("gui/{}/{label}", self.uid)),
        ) {
            Ok(text) => text.lines().any(|l| l.trim_start().starts_with("pid = ")),
            Err(_) => false,
        }
    }

    fn loaded_labels(&self, prefix: &str) -> Vec<String> {
        let Ok(text) = run_quiet(Command::new("/bin/launchctl").arg("list")) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| l.split('\t').nth(2))
            .filter(|l| l.starts_with(prefix))
            .map(str::to_string)
            .collect()
    }

    fn plist_of(&self, label: &str) -> Option<PathBuf> {
        self.print(label)?
            .lines()
            .find_map(|l| l.trim_start().strip_prefix("path = "))
            .map(|p| PathBuf::from(p.trim()))
    }
}

impl Launchctl {
    fn print(&self, label: &str) -> Option<String> {
        run_quiet(
            Command::new("/bin/launchctl")
                .arg("print")
                .arg(format!("gui/{}/{label}", self.uid)),
        )
        .ok()
    }

    fn loaded(&self, label: &str) -> bool {
        run_quiet(
            Command::new("/bin/launchctl")
                .arg("print")
                .arg(format!("gui/{}/{label}", self.uid)),
        )
        .is_ok()
    }
}

/// The job description a run is started from. It lives in the run directory, never in
/// `~/Library/LaunchAgents`, so a reboot does not start it again by itself.
pub fn render_plist(
    label: &str,
    runner: &Path,
    run_dir: &Path,
    log: &Path,
    env: &[(String, String)],
) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    s.push_str(&format!(
        "  <key>Label</key>\n  <string>{}</string>\n",
        esc(label)
    ));
    s.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    s.push_str(&format!(
        "    <string>{}</string>\n    <string>{}</string>\n",
        esc(&runner.to_string_lossy()),
        esc(&run_dir.to_string_lossy())
    ));
    s.push_str("  </array>\n");
    s.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    s.push_str("  <key>KeepAlive</key>\n  <false/>\n");
    s.push_str("  <key>ProcessType</key>\n  <string>Interactive</string>\n");
    s.push_str(&format!(
        "  <key>WorkingDirectory</key>\n  <string>{}</string>\n",
        esc(&run_dir.to_string_lossy())
    ));
    s.push_str(&format!(
        "  <key>StandardOutPath</key>\n  <string>{}</string>\n",
        esc(&log.to_string_lossy())
    ));
    s.push_str(&format!(
        "  <key>StandardErrorPath</key>\n  <string>{}</string>\n",
        esc(&log.to_string_lossy())
    ));
    s.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
    for (k, v) in env {
        s.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            esc(k),
            esc(v)
        ));
    }
    s.push_str("  </dict>\n</dict>\n</plist>\n");
    s
}

// ---- SIGNALS -------------------------------------------------------------------------

/// Signal a whole process group. `false` when no process is left in it.
pub fn signal_group(pgid: i32, sig: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    // SAFETY: killpg on a pgid the runner recorded for its own child. A failure (ESRCH, the
    // group has exited) is reported as `false`, never acted on.
    unsafe { libc::killpg(pgid, sig) == 0 }
}

/// Whether any process is left in the group.
pub fn group_alive(pgid: i32) -> bool {
    signal_group(pgid, 0)
}

// ---- THE DISK ------------------------------------------------------------------------

/// Free bytes available to this user on the volume holding `path` (or its nearest existing
/// ancestor).
pub fn free_bytes(path: &Path) -> Option<u64> {
    let p = nearest_existing(path)?;
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs writes into the zeroed struct we own; the path is a valid C string.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return None;
        }
        Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
    }
}

/// The path itself, or the closest ancestor that exists.
pub fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut p = Some(path);
    while let Some(cur) = p {
        if cur.exists() {
            return Some(cur.to_path_buf());
        }
        p = cur.parent();
    }
    None
}

/// The device id of the volume holding `path` (or its nearest existing ancestor).
pub fn volume_of(path: &Path) -> Option<u64> {
    let p = nearest_existing(path)?;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

/// Bytes a tree takes on disk (allocated blocks), not following symlinks. A missing path is 0.
pub fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let own = meta.blocks().saturating_mul(512);
    if !meta.is_dir() {
        return own;
    }
    let mut total = own;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            total = total.saturating_add(tree_bytes(&e.path()));
        }
    }
    total
}

/// Remove a tree if it exists. Symlinks are removed, never followed.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---- DERIVEDDATA ---------------------------------------------------------------------

/// The `WorkspacePath` of a DerivedData folder's `info.plist`: read as XML when it is, else
/// through `plutil` (a binary plist).
pub fn workspace_path(info_plist: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(info_plist)
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.starts_with(b"bplist") {
        let out = Command::new("/usr/bin/plutil")
            .args(["-extract", "WorkspacePath", "raw", "-o", "-"])
            .arg(info_plist)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (!s.is_empty()).then_some(s);
    }
    let text = String::from_utf8_lossy(&bytes);
    let at = text.find("<key>WorkspacePath</key>")?;
    let rest = &text[at + "<key>WorkspacePath</key>".len()..];
    let open = rest.find("<string>")?;
    let rest = &rest[open + "<string>".len()..];
    let close = rest.find("</string>")?;
    Some(
        rest[..close]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Every folder directly under `derived_root` whose `info.plist` names a workspace under
/// `worktree`.
pub fn derived_data_for(derived_root: &Path, worktree: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(derived_root) else {
        return Vec::new();
    };
    let wt = worktree.to_string_lossy().trim_end_matches('/').to_string();
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            workspace_path(&p.join("info.plist")).is_some_and(|w| {
                let w = w.trim_end_matches('/');
                w == wt || w.starts_with(&format!("{wt}/"))
            })
        })
        .collect();
    out.sort();
    out
}

// ---- PULL REQUESTS -------------------------------------------------------------------

/// A pull request's state on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// Asks GitHub about a branch's pull request.
pub trait Forge: Send + Sync {
    /// The newest pull request whose head is `branch`, or `None` when there is none or the
    /// question could not be answered.
    fn pr_state(&self, repo: &str, branch: &str) -> Option<PrState>;

    /// What the post-run attribution check reads from GitHub; `None` for a forge that cannot
    /// ask, which makes the check find nothing.
    fn github(&self) -> Option<&dyn crate::attribution::GitHubView> {
        None
    }

    /// One pull request's merge state, read by its URL, for a run that waits on Jeremy's
    /// merge. `Err` says why it could not be read (network, rate limit, auth); the watch asks
    /// again at its next check.
    fn pull_merge(&self, url: &str) -> Result<PullMerge, String> {
        Err(format!("this forge cannot read {url}"))
    }

    /// The tip of `jesse/<slug>-wip` on `repo`, for a cloud run to continue from. `Ok(None)`
    /// when the ref does not exist. Whether the tip actually looks like one of the entrypoint's
    /// checkpoints ([`Checkpoint::is_valid_for`]) is for the caller to decide: this answers only
    /// what GitHub holds. `Err` says why it could not be read.
    fn checkpoint(&self, repo: &str, slug: &str) -> Result<Option<Checkpoint>, String> {
        Err(format!(
            "this forge cannot read a checkpoint for {repo} jesse/{slug}-wip"
        ))
    }
}

/// The tip commit of a `jesse/<slug>-wip` ref, as [`Forge::checkpoint`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub sha: String,
    /// The commit's tree, so two checkpoints that changed nothing compare equal
    /// ([`crate::devjob::cloud::continuation_decision`]'s "no progress" rule).
    pub tree: String,
    /// The commit message's first line.
    pub subject: String,
    pub parent_count: usize,
}

impl Checkpoint {
    /// Whether this is one of the entrypoint's own checkpoints of `jesse/<slug>`: its subject is
    /// `checkpoint of jesse/<slug> at <UTC time> [skip ci]` and it has exactly one parent.
    pub fn is_valid_for(&self, slug: &str) -> bool {
        self.parent_count == 1
            && self
                .subject
                .starts_with(&format!("checkpoint of jesse/{slug} at "))
            && self.subject.ends_with(" [skip ci]")
    }

    /// Why this ref is not usable, when [`Checkpoint::is_valid_for`] says so; `None` when it is.
    pub fn invalid_reason(&self, slug: &str) -> Option<String> {
        if self.is_valid_for(slug) {
            return None;
        }
        Some(format!(
            "jesse/{slug}-wip exists but is not a checkpoint of jesse/{slug} (subject {:?}, \
             {} parent(s))",
            self.subject, self.parent_count
        ))
    }
}

/// [`Forge::checkpoint`]'s answer, built from `gh api repos/<repo>/git/ref/heads/<ref>`'s JSON
/// (for the sha) and `gh api repos/<repo>/git/commits/<sha>`'s (for the tree, subject and
/// parent count). Split out so it is tested with fixture JSON, with no `gh` to run.
pub fn checkpoint_of(
    ref_json: &serde_json::Value,
    commit_json: &serde_json::Value,
) -> Result<Checkpoint, String> {
    let sha = ref_json
        .get("object")
        .and_then(|o| o.get("sha"))
        .and_then(|s| s.as_str())
        .ok_or("the ref answer has no object.sha")?
        .to_string();
    let tree = commit_json
        .get("tree")
        .and_then(|t| t.get("sha"))
        .and_then(|s| s.as_str())
        .ok_or("the commit answer has no tree.sha")?
        .to_string();
    let message = commit_json
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    let subject = message.lines().next().unwrap_or("").to_string();
    let parent_count = commit_json
        .get("parents")
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Ok(Checkpoint {
        sha,
        tree,
        subject,
        parent_count,
    })
}

/// What `gh pr view <url> --json state,mergedAt,mergeCommit,title` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullMerge {
    pub state: PrState,
    pub merged_at: Option<String>,
    pub merge_commit: Option<String>,
    pub title: Option<String>,
}

/// [`PullMerge`] from `gh pr view`'s JSON.
pub fn parse_pull_merge(v: &serde_json::Value) -> Result<PullMerge, String> {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .filter(|x| !x.is_empty())
            .map(str::to_string)
    };
    let state = match v.get("state").and_then(|x| x.as_str()) {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        Some("OPEN") => PrState::Open,
        other => return Err(format!("gh answered state {other:?}")),
    };
    Ok(PullMerge {
        state,
        merged_at: s("mergedAt"),
        merge_commit: v
            .get("mergeCommit")
            .and_then(|c| c.get("oid"))
            .and_then(|x| x.as_str())
            .filter(|x| !x.is_empty())
            .map(str::to_string),
        title: s("title"),
    })
}

/// `gh pr list`, as the bridge user (its own `gh` login).
pub struct GhForge;

impl Forge for GhForge {
    fn github(&self) -> Option<&dyn crate::attribution::GitHubView> {
        Some(self)
    }

    fn pr_state(&self, repo: &str, branch: &str) -> Option<PrState> {
        let out = Command::new("gh")
            .args([
                "pr", "list", "--repo", repo, "--head", branch, "--state", "all", "--json",
                "state", "--limit", "1",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        match v.get(0)?.get("state")?.as_str()? {
            "MERGED" => Some(PrState::Merged),
            "CLOSED" => Some(PrState::Closed),
            "OPEN" => Some(PrState::Open),
            _ => None,
        }
    }

    fn pull_merge(&self, url: &str) -> Result<PullMerge, String> {
        let out = Command::new("gh")
            .args([
                "pr",
                "view",
                url,
                "--json",
                "state,mergedAt,mergeCommit,title",
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("could not run gh: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "gh pr view exited {}: {}",
                out.status,
                err.trim().lines().next().unwrap_or("")
            ));
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("gh pr view answered something unreadable: {e}"))?;
        parse_pull_merge(&v)
    }

    fn checkpoint(&self, repo: &str, slug: &str) -> Result<Option<Checkpoint>, String> {
        let ref_path = format!("repos/{repo}/git/ref/heads/jesse/{slug}-wip");
        let out = Command::new("gh")
            .args(["api", &ref_path])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("could not run gh: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.to_lowercase().contains("not found") || err.contains("404") {
                return Ok(None);
            }
            return Err(format!(
                "gh api {ref_path} exited {}: {}",
                out.status,
                err.trim().lines().next().unwrap_or("")
            ));
        }
        let ref_json: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("gh api {ref_path} answered something unreadable: {e}"))?;
        let sha = ref_json
            .get("object")
            .and_then(|o| o.get("sha"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| format!("gh api {ref_path} answered no object.sha"))?;
        let commit_path = format!("repos/{repo}/git/commits/{sha}");
        let out = Command::new("gh")
            .args(["api", &commit_path])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("could not run gh: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "gh api {commit_path} exited {}: {}",
                out.status,
                err.trim().lines().next().unwrap_or("")
            ));
        }
        let commit_json: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("gh api {commit_path} answered something unreadable: {e}"))?;
        checkpoint_of(&ref_json, &commit_json).map(Some)
    }
}

/// A [`Forge`] that knows nothing: every question is unanswered.
pub struct NoForge;

impl Forge for NoForge {
    fn pr_state(&self, _repo: &str, _branch: &str) -> Option<PrState> {
        None
    }
}

/// Whether `path` lies under `dir`, as written or once symlinks are resolved (`/var` is
/// `/private/var` on macOS, and launchd reports the resolved form).
pub fn is_under(path: &Path, dir: &Path) -> bool {
    if path.starts_with(dir) {
        return true;
    }
    let canon_dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let canon_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canon_path.starts_with(&canon_dir) || path.starts_with(&canon_dir)
}

/// `path` on PATH, or as written when it already names a file.
pub fn resolve_program(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path_var = path_var
        .map(str::to_string)
        .or_else(|| std::env::var("PATH").ok())?;
    path_var
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| is_executable(p))
}

/// A regular file with an execute bit.
pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

// ---- THE ATTRIBUTION CHECK'S VIEW OF GITHUB ------------------------------------------

/// `gh` with `args`, as the bridge user, parsed as JSON; `None` on any failure.
fn gh_json(args: &[&str]) -> Option<serde_json::Value> {
    let out = Command::new("gh")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// `owner/name` out of `https://github.com/<owner>/<name>/pull/<n>`.
fn repo_of_pull_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

const PULL_FIELDS: &str = "url,title,body,baseRefName,headRefName";

fn pull_of(v: &serde_json::Value, repo: &str) -> Option<crate::attribution::PullText> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let url = s("url");
    if url.is_empty() {
        return None;
    }
    Some(crate::attribution::PullText {
        repo: repo_of_pull_url(&url).unwrap_or_else(|| repo.to_string()),
        title: s("title"),
        body: s("body"),
        base: s("baseRefName"),
        head: s("headRefName"),
        url,
    })
}

impl crate::attribution::GitHubView for GhForge {
    fn pulls_for_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> Option<Vec<crate::attribution::PullText>> {
        let v = gh_json(&[
            "pr",
            "list",
            "--repo",
            repo,
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            PULL_FIELDS,
            "--limit",
            "20",
        ])?;
        Some(
            v.as_array()?
                .iter()
                .filter_map(|p| pull_of(p, repo))
                .collect(),
        )
    }

    fn pull(&self, url: &str) -> Option<crate::attribution::PullText> {
        let repo = repo_of_pull_url(url)?;
        pull_of(
            &gh_json(&["pr", "view", url, "--json", PULL_FIELDS])?,
            &repo,
        )
    }

    fn commits(
        &self,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Option<Vec<crate::attribution::CommitInfo>> {
        let v = gh_json(&["api", &format!("repos/{repo}/compare/{base}...{head}")])?;
        let s = |c: &serde_json::Value, path: &[&str]| {
            let mut x = c;
            for p in path {
                x = x.get(*p).unwrap_or(&serde_json::Value::Null);
            }
            x.as_str().unwrap_or("").to_string()
        };
        Some(
            v.get("commits")?
                .as_array()?
                .iter()
                .map(|c| crate::attribution::CommitInfo {
                    sha: s(c, &["sha"]),
                    message: s(c, &["commit", "message"]),
                    author_name: s(c, &["commit", "author", "name"]),
                    author_email: s(c, &["commit", "author", "email"]),
                    committer_name: s(c, &["commit", "committer", "name"]),
                    committer_email: s(c, &["commit", "committer", "email"]),
                })
                .collect(),
        )
    }
}
