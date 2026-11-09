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
}

/// `gh pr list`, as the bridge user (its own `gh` login).
pub struct GhForge;

impl Forge for GhForge {
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
