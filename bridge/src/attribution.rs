//! **NO AI ATTRIBUTION** — nothing the bridge starts may leave a Claude, Anthropic or AI tool
//! attribution in a commit, a push or a pull request, in any repository.
//!
//! Three layers, each owned by the bridge rather than by a prompt:
//!
//! 1. **Settings.** Every `claude` the bridge spawns (chat turns, scheduled jobs, Studio runs)
//!    gets a bridge-owned `--settings` file that turns Claude Code's own attribution off
//!    ([`settings_value`]) and denies the `git` spellings that skip hooks ([`DENY_RULES`]).
//! 2. **Git hooks.** A bridge-owned hooks directory ([`install`]), written from files compiled
//!    into this binary at every start, reached by every process the bridge spawns through
//!    `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n=core.hooksPath` ([`git_env`]). `commit-msg` and
//!    `pre-push` refuse attribution; every hook then chains to the repository's own.
//! 3. **The post-run check.** Before a run that pushed is reported, its pull requests and
//!    commits are read back from GitHub ([`scan_github`]) or from the local clone, and a hit
//!    makes the run `needs_you` with a report line starting [`REPORT_PREFIX`].
//!
//! The hooks directory and the settings file live in the bridge's state dir, which no child
//! can reach, and are rewritten from the binary at every start, so an edit on disk does not
//! last.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

/// `<state_dir>/attribution`: the hooks and the settings file.
pub const DIR: &str = "attribution";
/// The hooks directory, under [`DIR`].
pub const HOOKS_DIR: &str = "hooks";
/// The settings file a spawn with no write lock is given, under [`DIR`].
pub const SETTINGS_FILE: &str = "settings.json";
/// The functions every hook sources. Not a hook name, and not executable.
pub const LIB_NAME: &str = "jesse-hooks-lib.sh";
/// The first word of the report line a hit adds.
pub const REPORT_PREFIX: &str = "ATTRIBUTION:";

const LIB_SH: &str = include_str!("attribution/hooks/lib.sh");
const COMMIT_MSG_SH: &str = include_str!("attribution/hooks/commit-msg.sh");
const PRE_PUSH_SH: &str = include_str!("attribution/hooks/pre-push.sh");
const CHAIN_SH: &str = include_str!("attribution/hooks/chain.sh");

/// Every client-side hook git runs from `core.hooksPath` other than the two that check: each
/// is a pass-through to the repository's own. `push-to-checkout`, `proc-receive` and
/// `fsmonitor-watchman` are left out on purpose: a present `push-to-checkout` replaces git's
/// own behaviour even when it does nothing, and the other two are not looked up by name.
pub const CHAINED_HOOKS: [&str; 23] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-receive",
    "update",
    "post-receive",
    "post-update",
    "reference-transaction",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "post-index-change",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
];

/// The `git` spellings that would skip the hooks, denied to every Claude Code child. A
/// speed bump in front of the hooks, not the guarantee: the post-run check is the backstop.
pub const DENY_RULES: [&str; 12] = [
    "Bash(git commit --no-verify*)",
    "Bash(git commit *--no-verify*)",
    "Bash(git commit -n*)",
    "Bash(git commit * -n)",
    "Bash(git commit * -n *)",
    "Bash(git push --no-verify*)",
    "Bash(git push *--no-verify*)",
    "Bash(git -c core.hooksPath*)",
    "Bash(git -c core.hookspath*)",
    "Bash(git * -c core.hooksPath*)",
    "Bash(git * -c core.hookspath*)",
    "Bash(git config core.hooksPath /dev/null*)",
];

/// `<state_dir>/attribution`.
pub fn root(state_dir: &Path) -> PathBuf {
    state_dir.join(DIR)
}

/// `<state_dir>/attribution/hooks`.
pub fn hooks_dir(state_dir: &Path) -> PathBuf {
    root(state_dir).join(HOOKS_DIR)
}

/// `<state_dir>/attribution/settings.json`.
pub fn settings_path(state_dir: &Path) -> PathBuf {
    root(state_dir).join(SETTINGS_FILE)
}

/// The hooks directory of a bridge with a state dir; `None` without one (no persistence, and
/// so nowhere a child cannot reach).
pub fn hooks_dir_of(cfg: &crate::Config) -> Option<PathBuf> {
    cfg.state_dir.as_deref().map(|d| hooks_dir(Path::new(d)))
}

/// The settings file of a bridge with a state dir.
pub fn settings_path_of(cfg: &crate::Config) -> Option<PathBuf> {
    cfg.state_dir
        .as_deref()
        .map(|d| settings_path(Path::new(d)))
}

/// Every file of the hooks directory: `(name, contents, executable)`.
pub fn hook_files() -> Vec<(String, String, bool)> {
    let mut out = vec![
        (LIB_NAME.to_string(), LIB_SH.to_string(), false),
        ("commit-msg".to_string(), COMMIT_MSG_SH.to_string(), true),
        ("pre-push".to_string(), PRE_PUSH_SH.to_string(), true),
    ];
    for name in CHAINED_HOOKS {
        out.push((name.to_string(), CHAIN_SH.replace("@HOOK@", name), true));
    }
    out
}

/// Claude Code's attribution off, and the deny rules. Verified against claude 2.1.322, which
/// reads `attribution.commit` (the commit trailer), `attribution.pr` (the pull request
/// footer), `attribution.sessionUrl` (the `Claude-Session:` trailer and its pull request
/// link) and, deprecated but still honoured, `includeCoAuthoredBy`.
pub fn settings_value() -> Value {
    json!({
        "attribution": { "commit": "", "pr": "", "sessionUrl": false },
        "includeCoAuthoredBy": false,
        "permissions": { "deny": DENY_RULES },
    })
}

/// Merge [`settings_value`] into another settings document (the write lock's), keeping its
/// own keys and any deny rules it already has.
pub fn merge_into(doc: &mut Value) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    let add = settings_value();
    obj.insert("attribution".into(), add["attribution"].clone());
    obj.insert("includeCoAuthoredBy".into(), json!(false));
    let perms = obj.entry("permissions").or_insert_with(|| json!({}));
    if let Some(p) = perms.as_object_mut() {
        let deny = p.entry("deny").or_insert_with(|| json!([]));
        if let Some(d) = deny.as_array_mut() {
            for rule in DENY_RULES {
                if !d.iter().any(|r| r == rule) {
                    d.push(json!(rule));
                }
            }
        }
    }
}

/// Write the hooks directory and the settings file under `state_dir`, from the binary. Any
/// other file in the hooks directory is removed, so a hook a child added does not survive a
/// start either.
pub fn install(state_dir: &Path) -> std::io::Result<PathBuf> {
    install_root(&root(state_dir))
}

/// [`install`] into `root` itself (`<state_dir>/attribution`): what the Studio runner calls
/// before each round, with the directory the bridge handed it.
pub fn install_root(root: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let hooks = root.join(HOOKS_DIR);
    std::fs::create_dir_all(&hooks)?;
    let files = hook_files();
    let known: HashSet<&str> = files.iter().map(|(n, _, _)| n.as_str()).collect();
    for e in std::fs::read_dir(&hooks)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !known.contains(name.as_str()) {
            let p = e.path();
            if p.is_dir() {
                std::fs::remove_dir_all(&p)?;
            } else {
                std::fs::remove_file(&p)?;
            }
        }
    }
    for (name, body, exec) in &files {
        let path = hooks.join(name);
        crate::write_atomic(&path, body.as_bytes())?;
        let mode = if *exec { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
    }
    let settings = serde_json::to_vec_pretty(&settings_value()).map_err(std::io::Error::other)?;
    crate::write_atomic(&root.join(SETTINGS_FILE), &settings)?;
    Ok(hooks)
}

/// The hooks directory every bridge-side `git` uses once [`set_global`] has run (at startup).
static GLOBAL_HOOKS: OnceLock<PathBuf> = OnceLock::new();

/// Record the installed hooks directory for the bridge's own `git` calls.
pub fn set_global(hooks: PathBuf) {
    let _ = GLOBAL_HOOKS.set(hooks);
}

/// The installed hooks directory, once the bridge has started.
pub fn global() -> Option<&'static Path> {
    GLOBAL_HOOKS.get().map(PathBuf::as_path)
}

/// The variables that point `git` at `hooks`, appended after whatever `GIT_CONFIG_*` the
/// environment already carries (`existing` reads it): `GIT_CONFIG_KEY_<n>`,
/// `GIT_CONFIG_VALUE_<n>` and the new `GIT_CONFIG_COUNT`.
pub fn git_env(existing: impl Fn(&str) -> Option<String>, hooks: &Path) -> Vec<(String, String)> {
    let n: usize = existing("GIT_CONFIG_COUNT")
        .and_then(|c| c.trim().parse().ok())
        .unwrap_or(0);
    vec![
        (format!("GIT_CONFIG_KEY_{n}"), "core.hooksPath".to_string()),
        (
            format!("GIT_CONFIG_VALUE_{n}"),
            hooks.to_string_lossy().into_owned(),
        ),
        ("GIT_CONFIG_COUNT".to_string(), (n + 1).to_string()),
    ]
}

/// The value `cmd` will hand its child for `key`: what it sets, else (when it inherits the
/// bridge's environment) the bridge's own.
fn effective_env(cmd: &std::process::Command, inherits: bool, key: &str) -> Option<String> {
    for (k, v) in cmd.get_envs() {
        if k == key {
            return v.map(|v| v.to_string_lossy().into_owned());
        }
    }
    if inherits {
        std::env::var(key).ok()
    } else {
        None
    }
}

/// Point a std `Command`'s `git` at `hooks`. `inherits` is false when the command's
/// environment was cleared.
pub fn apply_git_env_std(cmd: &mut std::process::Command, hooks: &Path, inherits: bool) {
    let vars = git_env(|k| effective_env(cmd, inherits, k), hooks);
    cmd.envs(vars);
}

/// [`apply_git_env_std`] for a tokio `Command`.
pub fn apply_git_env(cmd: &mut tokio::process::Command, hooks: &Path, inherits: bool) {
    let vars = git_env(|k| effective_env(cmd.as_std(), inherits, k), hooks);
    cmd.envs(vars);
}

/// [`apply_git_env_std`] with the installed hooks directory, when there is one: for the git
/// the bridge itself runs for a run (worktrees, fetches, local checkouts).
pub fn apply_global_std(cmd: &mut std::process::Command) {
    if let Some(h) = global() {
        apply_git_env_std(cmd, h, true);
    }
}

// ---- THE PATTERNS ----------------------------------------------------------------------
//
// The same three the hooks use (`lib.sh`), matched the same way: ASCII case folding, and
// `[[:alnum:]]` as ASCII letters and digits.

/// `^[[:space:]]*(co-authored-by|claude-session)[[:space:]]*:.*(claude|anthropic)`
fn trailer_hit(line: &str, leading_space: bool) -> bool {
    let l = line.to_ascii_lowercase();
    let l = if leading_space { l.trim_start() } else { &l };
    ["co-authored-by", "claude-session"].iter().any(|key| {
        l.strip_prefix(key)
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix(':'))
            .is_some_and(|r| r.contains("claude") || r.contains("anthropic"))
    })
}

/// `ai` as a word in `s`: neither side an ASCII letter or digit.
fn has_ai_word(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(at) = s[i..].find("ai") {
        let p = i + at;
        let before = p == 0 || !b[p - 1].is_ascii_alphanumeric();
        let after = p + 2 >= b.len() || !b[p + 2].is_ascii_alphanumeric();
        if before && after {
            return true;
        }
        i = p + 2;
    }
    false
}

/// `generated with .*(claude|anthropic)` or `generated with (.*[^[:alnum:]])?ai([^[:alnum:]]|$)`,
/// anywhere in the line.
fn generated_hit(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    const KEY: &str = "generated with ";
    let mut from = 0;
    while let Some(at) = l[from..].find(KEY) {
        let tail = &l[from + at + KEY.len()..];
        if tail.contains("claude") || tail.contains("anthropic") || has_ai_word(tail) {
            return true;
        }
        from += at + KEY.len();
    }
    false
}

/// The lines of a commit message that carry attribution, as the `commit-msg` hook finds them.
pub fn message_hits(msg: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in msg.lines() {
        if line.starts_with("# ") && line.contains(" >8 ") {
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        if trailer_hit(line, true) || generated_hit(line) {
            out.push(line.trim().to_string());
        }
    }
    out
}

/// Whether an identity is Claude's: the name `Claude`, or an `anthropic.com` email.
pub fn identity_hit(name: &str, email: &str) -> bool {
    name.trim().eq_ignore_ascii_case("claude")
        || email.to_ascii_lowercase().contains("anthropic.com")
}

/// The lines of a pull request's title and body that carry attribution: a trailer at the
/// start of a line, a line that IS a "Generated with" footer (after any emoji or markup), or
/// a link to Claude Code or a Claude session. Prose that discusses Claude is not a hit.
pub fn pr_text_hits(title: &str, body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in title.lines().chain(body.lines()) {
        let l = line.to_ascii_lowercase();
        let bare = l.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
        let footer = bare.starts_with("generated with ") && generated_hit(bare);
        let link = l.contains("://claude.com/claude-code")
            || l.contains("](claude.com/claude-code")
            || l.contains("://claude.ai/code");
        if trailer_hit(line, false) || footer || link {
            out.push(line.trim().to_string());
        }
    }
    out
}

/// One commit, as GitHub or `git log` gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub committer_name: String,
    pub committer_email: String,
}

/// Every attribution in one commit: message lines, then a Claude author or committer.
pub fn commit_hits(c: &CommitInfo) -> Vec<String> {
    let mut out = message_hits(&c.message);
    if identity_hit(&c.author_name, &c.author_email) {
        out.push(format!("author {} <{}>", c.author_name, c.author_email));
    }
    if identity_hit(&c.committer_name, &c.committer_email) {
        out.push(format!(
            "committer {} <{}>",
            c.committer_name, c.committer_email
        ));
    }
    out
}

/// One pull request's text and refs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullText {
    /// `owner/name`.
    pub repo: String,
    pub url: String,
    pub title: String,
    pub body: String,
    pub base: String,
    pub head: String,
}

/// What the post-run check reads from GitHub. [`crate::hostrun::sys::GhForge`] answers it
/// through `gh`; `None` is "could not ask", which finds nothing.
pub trait GitHubView: Send + Sync {
    /// Every pull request (any state) whose head is `branch`.
    fn pulls_for_branch(&self, _repo: &str, _branch: &str) -> Option<Vec<PullText>> {
        None
    }
    /// One pull request by URL.
    fn pull(&self, _url: &str) -> Option<PullText> {
        None
    }
    /// The commits in `base..head` on GitHub; `None` when `head` is not there.
    fn commits(&self, _repo: &str, _base: &str, _head: &str) -> Option<Vec<CommitInfo>> {
        None
    }
}

fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s.to_string()
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// Each hit, one entry per commit: `commit <sha12> "<subject>": <hit> | <hit>`.
pub fn commit_entries(commits: &[CommitInfo], seen: &mut HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for c in commits {
        if !seen.insert(c.sha.clone()) {
            continue;
        }
        let hits = commit_hits(c);
        if !hits.is_empty() {
            let short: String = c.sha.chars().take(12).collect();
            out.push(format!(
                "commit {short} \"{}\": {}",
                clip(first_line(&c.message)),
                hits.iter().map(|h| clip(h)).collect::<Vec<_>>().join(" | ")
            ));
        }
    }
    out
}

/// The post-run check against GitHub: every pull request whose head is `branch` (and the one
/// the report names, when it is another), its title and body, and every commit in
/// `base..head` of each, plus `default_branch..branch`. One entry per offending pull request
/// or commit.
pub fn scan_github(
    gh: &dyn GitHubView,
    repo: &str,
    branch: &str,
    default_branch: &str,
    report_url: Option<&str>,
) -> Vec<String> {
    let mut pulls = gh.pulls_for_branch(repo, branch).unwrap_or_default();
    if let Some(url) = report_url {
        if !pulls.iter().any(|p| p.url == url) {
            if let Some(p) = gh.pull(url) {
                pulls.push(p);
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut ranges: Vec<(String, String, String)> = Vec::new();
    for p in &pulls {
        let hits = pr_text_hits(&p.title, &p.body);
        if !hits.is_empty() {
            out.push(format!(
                "pull request {}: {}",
                p.url,
                hits.iter().map(|h| clip(h)).collect::<Vec<_>>().join(" | ")
            ));
        }
        let r = if p.repo.is_empty() { repo } else { &p.repo };
        ranges.push((r.to_string(), p.base.clone(), p.head.clone()));
    }
    ranges.push((
        repo.to_string(),
        default_branch.to_string(),
        branch.to_string(),
    ));
    let mut done = HashSet::new();
    for (r, base, head) in ranges {
        if base.is_empty()
            || head.is_empty()
            || !done.insert((r.clone(), base.clone(), head.clone()))
        {
            continue;
        }
        if let Some(commits) = gh.commits(&r, &base, &head) {
            out.extend(commit_entries(&commits, &mut seen));
        }
    }
    out
}

/// The commits of `range` in the git repository `dir` (a local repository's clone).
pub fn local_commits(dir: &Path, range: &str) -> Option<Vec<CommitInfo>> {
    const SEP: &str = "\u{1f}";
    const END: &str = "\u{1e}";
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "log",
            "--format=%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%B%x1e",
            range,
            "--",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for rec in text.split(END) {
        let rec = rec.trim_start_matches('\n');
        if rec.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.splitn(6, SEP).collect();
        if f.len() < 6 {
            continue;
        }
        commits.push(CommitInfo {
            sha: f[0].to_string(),
            author_name: f[1].to_string(),
            author_email: f[2].to_string(),
            committer_name: f[3].to_string(),
            committer_email: f[4].to_string(),
            message: f[5].to_string(),
        });
    }
    Some(commits)
}

/// The report line for a set of hits, or `None` for none: it starts with [`REPORT_PREFIX`]
/// and names each pull request and commit.
pub fn report_line(entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    Some(format!(
        "{REPORT_PREFIX} AI attribution was pushed and must be removed: {}",
        entries.join("; ")
    ))
}

#[cfg(test)]
mod tests;
