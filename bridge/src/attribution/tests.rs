//! The attribution guard, layer by layer: the shell hooks driven in throwaway git
//! repositories, the settings file, the environment of every kind of spawn, and the post-run
//! check against fixture pull requests and commits.
//!
//! Every `git` here runs with a cleared environment (no global or system config, no inherited
//! `GIT_CONFIG_*`), so a run of this suite inside a bridge-spawned agent, whose own git is
//! pointed at the deployed hooks, tests these hooks and not those.

use super::*;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

const TRAILER: &str = "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";

struct Scratch {
    dir: PathBuf,
    hooks: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "jesse-attribution-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hooks = install(&dir.join("state")).expect("the hooks install");
    Scratch { dir, hooks }
}

/// `git` in `cwd`, hermetic, pointed at the scratch hooks, plus `envs`.
fn git_in(s: &Scratch, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &s.dir)
        .env("TMPDIR", std::env::temp_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(git_env(|_| None, &s.hooks))
        .envs(envs.iter().copied())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn ok(o: &Output) -> bool {
    o.status.success()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A repository with one commit on `main`, its own identity set.
fn repo(s: &Scratch, name: &str) -> PathBuf {
    let r = s.dir.join(name);
    std::fs::create_dir_all(&r).unwrap();
    assert!(ok(&git_in(s, &r, &["init", "-q", "-b", "main"], &[])));
    assert!(ok(&git_in(s, &r, &["config", "user.name", "Tester"], &[])));
    assert!(ok(&git_in(
        s,
        &r,
        &["config", "user.email", "tester@example.com"],
        &[]
    )));
    std::fs::write(r.join("README.md"), "# r\n").unwrap();
    assert!(ok(&git_in(s, &r, &["add", "README.md"], &[])));
    assert!(ok(&git_in(s, &r, &["commit", "-q", "-m", "first"], &[])));
    r
}

/// Stage one new file and commit it with `message` (paragraphs) and any extra args.
fn commit(
    s: &Scratch,
    r: &Path,
    message: &[&str],
    extra: &[&str],
    envs: &[(&str, &str)],
) -> Output {
    let n = N.fetch_add(1, Ordering::SeqCst);
    std::fs::write(r.join(format!("f{n}.txt")), format!("{n}\n")).unwrap();
    assert!(ok(&git_in(s, r, &["add", "-A"], &[])));
    let mut args = vec!["commit", "-q"];
    for m in message {
        args.push("-m");
        args.push(m);
    }
    args.extend_from_slice(extra);
    git_in(s, r, &args, envs)
}

fn write_hook(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ---- commit-msg ----------------------------------------------------------------------

#[test]
fn a_commit_with_each_attribution_form_is_refused_and_a_clean_one_lands() {
    let s = scratch("forms");
    let r = repo(&s, "r");
    for line in [
        TRAILER,
        "co-authored-by: Claude <claude@anthropic.com>",
        "Co-authored-by: Someone <noreply@anthropic.com>",
        "Claude-Session: https://claude.ai/code/session_01ABC",
        "🤖 Generated with [Claude Code](https://claude.com/claude-code)",
        "Generated with Claude Code",
        "Generated with an AI assistant",
    ] {
        let o = commit(&s, &r, &["Add a thing", line], &[], &[]);
        assert!(!ok(&o), "a commit carrying {line:?} landed");
        let e = err(&o);
        assert!(e.contains("commit rejected"), "{e}");
        assert!(e.contains(line), "the error names the line: {e}");
        assert!(e.contains("Remove"), "the error says to remove it: {e}");
    }
    // Clean, and clean while discussing Claude as the subject of the work.
    for msg in [
        &["Add a thing"][..],
        &[
            "Turn Claude Code attribution off",
            "The bridge passes attribution settings to claude.",
        ],
        &[
            "Co-authored work",
            "Co-Authored-By: Jane Doe <jane@example.com>",
        ],
        &[
            "Generated with care",
            "Docs generated with mdbook, maintained by hand.",
        ],
    ] {
        let o = commit(&s, &r, msg, &[], &[]);
        assert!(ok(&o), "{msg:?} was refused: {}", err(&o));
    }
}

#[test]
fn the_rust_scanner_agrees_with_the_hook_line_for_line() {
    let s = scratch("parity");
    let r = repo(&s, "r");
    for line in [
        TRAILER,
        "  Co-Authored-By : Claude",
        "Claude-Session: https://claude.ai/code/x",
        "claude-session: nothing here",
        "Generated with AI",
        "generated with ai.",
        "Generated with Claude",
        "Generated with maintainers",
        "Generated with detail",
        "Co-Authored-By: Jane <jane@example.com>",
        "The Claude Code CLI writes Co-Authored-By: Claude by default",
        "Explains anthropic.com docs",
    ] {
        let hook_refused = !ok(&commit(&s, &r, &["Subject", line], &[], &[]));
        let rust_hits = !message_hits(&format!("Subject\n\n{line}\n")).is_empty();
        assert_eq!(
            hook_refused, rust_hits,
            "the hook and the scanner disagree on {line:?}"
        );
    }
}

#[test]
fn a_commit_authored_or_committed_as_claude_is_refused() {
    let s = scratch("identity");
    let r = repo(&s, "r");
    let o = commit(
        &s,
        &r,
        &["x"],
        &["--author=Claude <noreply@anthropic.com>"],
        &[],
    );
    assert!(!ok(&o));
    assert!(err(&o).contains("author"), "{}", err(&o));
    let o = commit(
        &s,
        &r,
        &["x"],
        &[],
        &[("GIT_COMMITTER_EMAIL", "bot@anthropic.com")],
    );
    assert!(!ok(&o));
    assert!(err(&o).contains("committer"), "{}", err(&o));
    let o = commit(&s, &r, &["x"], &[], &[("GIT_AUTHOR_NAME", "claude")]);
    assert!(!ok(&o));
    // A person who happens to be called Claude something is not Claude.
    let o = commit(
        &s,
        &r,
        &["x"],
        &["--author=Claude Monet <claude@example.com>"],
        &[],
    );
    assert!(ok(&o), "{}", err(&o));
}

#[test]
fn the_repositorys_own_commit_msg_hook_still_runs_and_its_failure_fails_the_commit() {
    let s = scratch("chain-commit-msg");
    let r = repo(&s, "r");
    let marker = s.dir.join("ran");
    write_hook(
        &r.join(".git/hooks/commit-msg"),
        &format!(
            "#!/bin/sh\necho ran >> '{}'\ngrep -q WIP \"$1\" && {{ echo 'no WIP' >&2; exit 3; }}\nexit 0\n",
            marker.display()
        ),
    );
    let o = commit(&s, &r, &["WIP: half done"], &[], &[]);
    assert!(!ok(&o), "the repository's hook refused it");
    assert!(err(&o).contains("no WIP"), "{}", err(&o));
    let o = commit(&s, &r, &["Done"], &[], &[]);
    assert!(ok(&o), "{}", err(&o));
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 2);

    // A repository whose own core.hooksPath points elsewhere (husky and the like): that one.
    let husky = r.join(".husky");
    write_hook(
        &husky.join("commit-msg"),
        "#!/bin/sh\necho 'husky says no' >&2\nexit 1\n",
    );
    assert!(ok(&git_in(
        &s,
        &r,
        &["config", "core.hooksPath", ".husky"],
        &[]
    )));
    let o = commit(&s, &r, &["Done again"], &[], &[]);
    assert!(!ok(&o));
    assert!(err(&o).contains("husky says no"), "{}", err(&o));
}

#[test]
fn a_pass_through_hook_runs_the_repositorys_own() {
    let s = scratch("chain-pre-commit");
    let r = repo(&s, "r");
    write_hook(
        &r.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho 'lint failed' >&2\nexit 1\n",
    );
    let o = commit(&s, &r, &["x"], &[], &[]);
    assert!(!ok(&o));
    assert!(err(&o).contains("lint failed"), "{}", err(&o));
    std::fs::remove_file(r.join(".git/hooks/pre-commit")).unwrap();
    assert!(ok(&commit(&s, &r, &["x"], &[], &[])));
}

// ---- pre-push -----------------------------------------------------------------------

/// A repository with a bare `origin` that already has `main`.
fn with_origin(s: &Scratch) -> PathBuf {
    let r = repo(s, "work");
    let origin = s.dir.join("origin.git");
    assert!(ok(&git_in(
        s,
        &s.dir,
        &["init", "-q", "--bare", "origin.git"],
        &[]
    )));
    assert!(ok(&git_in(
        s,
        &r,
        &["remote", "add", "origin", &origin.to_string_lossy()],
        &[]
    )));
    let o = git_in(s, &r, &["push", "-q", "origin", "main"], &[]);
    assert!(ok(&o), "{}", err(&o));
    r
}

#[test]
fn a_push_of_a_branch_holding_a_bypassed_trailer_commit_is_refused() {
    let s = scratch("pre-push-new");
    let r = with_origin(&s);
    assert!(ok(&git_in(
        &s,
        &r,
        &["checkout", "-q", "-b", "feature"],
        &[]
    )));
    assert!(ok(&commit(&s, &r, &["Clean"], &[], &[])));
    let o = commit(&s, &r, &["Sneaky", TRAILER], &["--no-verify"], &[]);
    assert!(ok(&o), "--no-verify skips commit-msg: {}", err(&o));
    let o = git_in(&s, &r, &["push", "-q", "origin", "feature"], &[]);
    assert!(!ok(&o), "the push went through");
    let e = err(&o);
    assert!(e.contains("push rejected"), "{e}");
    assert!(e.contains("Sneaky"), "names the commit: {e}");
    assert!(e.contains(TRAILER), "names the line: {e}");
    assert!(!e.contains(" Clean"), "a clean commit is not listed: {e}");
}

#[test]
fn an_update_of_a_pushed_branch_is_checked_from_the_remote_tip() {
    let s = scratch("pre-push-update");
    let r = with_origin(&s);
    assert!(ok(&commit(&s, &r, &["Clean one"], &[], &[])));
    assert!(ok(&git_in(&s, &r, &["push", "-q", "origin", "main"], &[])));
    assert!(ok(&commit(
        &s,
        &r,
        &["Bad"],
        &["--no-verify", "--author=Claude <noreply@anthropic.com>"],
        &[]
    )));
    let o = git_in(&s, &r, &["push", "-q", "origin", "main"], &[]);
    assert!(!ok(&o));
    assert!(err(&o).contains("author: Claude"), "{}", err(&o));
}

#[test]
fn history_already_on_the_remote_does_not_block_a_clean_push() {
    let s = scratch("pre-push-history");
    let r = with_origin(&s);
    // An attributed commit that reached main before the guard existed.
    assert!(ok(&commit(
        &s,
        &r,
        &["Old", TRAILER],
        &["--no-verify"],
        &[]
    )));
    assert!(ok(&git_in(
        &s,
        &r,
        &["push", "-q", "--no-verify", "origin", "main"],
        &[]
    )));
    assert!(ok(&git_in(&s, &r, &["checkout", "-q", "-b", "next"], &[])));
    assert!(ok(&commit(&s, &r, &["New and clean"], &[], &[])));
    let o = git_in(&s, &r, &["push", "-q", "origin", "next"], &[]);
    assert!(ok(&o), "{}", err(&o));
    // And a deletion pushes nothing to check.
    let o = git_in(&s, &r, &["push", "-q", "origin", ":next"], &[]);
    assert!(ok(&o), "{}", err(&o));
}

#[test]
fn the_repositorys_own_pre_push_gets_the_same_stdin_and_its_failure_fails_the_push() {
    let s = scratch("pre-push-chain");
    let r = with_origin(&s);
    let seen = s.dir.join("seen");
    write_hook(
        &r.join(".git/hooks/pre-push"),
        &format!(
            "#!/bin/sh\necho \"$1 $2\" > '{0}'\ncat >> '{0}'\nexit 4\n",
            seen.display()
        ),
    );
    assert!(ok(&commit(&s, &r, &["Clean"], &[], &[])));
    let o = git_in(&s, &r, &["push", "-q", "origin", "main"], &[]);
    assert!(
        !ok(&o),
        "the repository's pre-push failed, so the push fails"
    );
    let text = std::fs::read_to_string(&seen).unwrap();
    assert!(text.starts_with("origin "), "{text}");
    assert!(
        text.contains("refs/heads/main refs/heads/main") || text.contains("refs/heads/main "),
        "{text}"
    );
}

// ---- the hooks directory ------------------------------------------------------------

#[test]
fn install_rewrites_an_edited_hook_and_removes_a_planted_one() {
    let s = scratch("reinstall");
    let state = s.dir.join("state");
    std::fs::write(s.hooks.join("commit-msg"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(s.hooks.join("pre-push-local"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(settings_path(&state), "{}").unwrap();
    install(&state).unwrap();
    assert_eq!(
        std::fs::read_to_string(s.hooks.join("commit-msg")).unwrap(),
        COMMIT_MSG_SH
    );
    assert!(!s.hooks.join("pre-push-local").exists());
    let v: Value = serde_json::from_slice(&std::fs::read(settings_path(&state)).unwrap()).unwrap();
    assert_eq!(v, settings_value());
    use std::os::unix::fs::PermissionsExt as _;
    for (name, _, exec) in hook_files() {
        let mode = std::fs::metadata(s.hooks.join(&name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111 != 0, exec, "{name}");
    }
}

#[test]
fn every_hook_is_posix_sh() {
    let s = scratch("syntax");
    for (name, _, _) in hook_files() {
        let o = Command::new("/bin/sh")
            .arg("-n")
            .arg(s.hooks.join(&name))
            .output()
            .unwrap();
        assert!(o.status.success(), "{name}: {}", err(&o));
        if Path::new("/bin/dash").exists() {
            let o = Command::new("/bin/dash")
                .arg("-n")
                .arg(s.hooks.join(&name))
                .output()
                .unwrap();
            assert!(o.status.success(), "dash {name}: {}", err(&o));
        }
    }
}

// ---- the settings file --------------------------------------------------------------

#[test]
fn the_settings_file_holds_the_attribution_keys_and_the_deny_rules() {
    let v = settings_value();
    assert_eq!(v["attribution"]["commit"], json!(""));
    assert_eq!(v["attribution"]["pr"], json!(""));
    assert_eq!(v["attribution"]["sessionUrl"], json!(false));
    assert_eq!(v["includeCoAuthoredBy"], json!(false));
    let deny: Vec<&str> = v["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    for rule in [
        "Bash(git commit *--no-verify*)",
        "Bash(git commit -n*)",
        "Bash(git push *--no-verify*)",
        "Bash(git -c core.hooksPath*)",
    ] {
        assert!(deny.contains(&rule), "{rule} is missing: {deny:?}");
    }
}

#[test]
fn the_write_lock_settings_file_carries_the_attribution_keys_too() {
    let s = scratch("write-lock");
    let mut cfg = crate::testutil::test_config();
    cfg.state_dir = Some(s.dir.join("state").to_string_lossy().into_owned());
    let wl = crate::WriteLockChild {
        socket: s.dir.join("lock.sock"),
        turn: "turn-1".to_string(),
        conversation: "conv-1".to_string(),
        helper: s.dir.join("jesse-hook"),
        rules_root: None,
    };
    let path = crate::install_write_lock_settings(&cfg, &wl).expect("the file");
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v["hooks"]["PreToolUse"].is_array(), "the lock's hooks stay");
    assert_eq!(v["attribution"], settings_value()["attribution"]);
    assert_eq!(v["includeCoAuthoredBy"], json!(false));
    assert_eq!(v["permissions"], settings_value()["permissions"]);

    // Merging twice adds no duplicate rule, and keeps a rule already there.
    let mut doc = json!({ "permissions": { "deny": ["Bash(rm -rf /)"] } });
    merge_into(&mut doc);
    merge_into(&mut doc);
    let deny = doc["permissions"]["deny"].as_array().unwrap();
    assert_eq!(deny.len(), DENY_RULES.len() + 1);
}

// ---- the environment of every spawn -------------------------------------------------

fn env_of(cmd: &std::process::Command) -> std::collections::HashMap<String, String> {
    cmd.get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
        .collect()
}

fn assert_hooks_path(env: &std::collections::HashMap<String, String>, hooks: &Path) {
    let n: usize = env["GIT_CONFIG_COUNT"].parse().unwrap();
    assert!(n >= 1);
    let i = n - 1;
    assert_eq!(env[&format!("GIT_CONFIG_KEY_{i}")], "core.hooksPath");
    assert_eq!(
        env[&format!("GIT_CONFIG_VALUE_{i}")],
        hooks.to_string_lossy()
    );
}

#[test]
fn git_env_appends_after_the_entries_already_there() {
    let v = git_env(
        |k| (k == "GIT_CONFIG_COUNT").then(|| "2".to_string()),
        Path::new("/h"),
    );
    assert_eq!(
        v,
        vec![
            ("GIT_CONFIG_KEY_2".to_string(), "core.hooksPath".to_string()),
            ("GIT_CONFIG_VALUE_2".to_string(), "/h".to_string()),
            ("GIT_CONFIG_COUNT".to_string(), "3".to_string()),
        ]
    );
    let mut cmd = std::process::Command::new("true");
    cmd.env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "a.b")
        .env("GIT_CONFIG_VALUE_0", "c");
    apply_git_env_std(&mut cmd, Path::new("/h"), false);
    let env = env_of(&cmd);
    assert_eq!(env["GIT_CONFIG_KEY_0"], "a.b");
    assert_hooks_path(&env, Path::new("/h"));
    assert_eq!(env["GIT_CONFIG_COUNT"], "2");
}

#[test]
fn a_chat_turn_carries_core_hooks_path_and_the_attribution_settings() {
    let s = scratch("chat-turn");
    let mut cfg = crate::testutil::test_config();
    let state = s.dir.join("state");
    cfg.state_dir = Some(state.to_string_lossy().into_owned());
    let active = crate::ActiveModel::ambient();
    let cmd = crate::build_claude_command(
        &cfg,
        "hello",
        None,
        &active,
        crate::turn_capability(&active),
        crate::main_mcp_config(&cfg, &crate::ClaudeCode),
    );
    assert_hooks_path(&env_of(cmd.as_std()), &s.hooks);
    let args: Vec<String> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let at = args
        .iter()
        .position(|a| a == "--settings")
        .expect("--settings");
    assert_eq!(args[at + 1], settings_path(&state).to_string_lossy());

    // Without a state dir there is nowhere a child cannot reach, and nothing is added.
    let bare = crate::testutil::test_config();
    let cmd = crate::build_claude_command(
        &bare,
        "hello",
        None,
        &active,
        crate::turn_capability(&active),
        crate::main_mcp_config(&bare, &crate::ClaudeCode),
    );
    assert!(!env_of(cmd.as_std()).contains_key("GIT_CONFIG_COUNT"));
}

#[test]
fn a_codex_turn_carries_core_hooks_path() {
    let s = scratch("codex-turn");
    let mut cfg = crate::testutil::test_config();
    cfg.state_dir = Some(s.dir.join("state").to_string_lossy().into_owned());
    cfg.home = s.dir.to_string_lossy().into_owned();
    let m = crate::ActiveModel::ambient();
    let req = crate::TurnRequest {
        prompt: "hi",
        session_id: None,
        active: &m,
        capability: crate::Capability::Read,
        cwd: std::env::temp_dir(),
        mcp_config: crate::EMPTY_MCP_CONFIG,
        write_lock: None,
        turn_id: "test-turn",
        artifact_dir: None,
        attachment_dir: None,
    };
    let cmd = crate::Codex.command(&cfg, &req).expect("a codex child");
    assert_hooks_path(&env_of(cmd.as_std()), &s.hooks);
}

#[test]
fn a_studio_run_carries_core_hooks_path_and_the_settings() {
    use crate::hostrun::runner;
    let hooks = Path::new("/state/attribution/hooks");
    // After the repository's env file, which may carry GIT_CONFIG_* of its own.
    let base = runner::agent_env_with(
        Path::new("/run"),
        "s-x",
        |_| None,
        vec![
            ("GIT_CONFIG_COUNT".into(), "1".into()),
            ("GIT_CONFIG_KEY_0".into(), "user.useConfigOnly".into()),
            ("GIT_CONFIG_VALUE_0".into(), "true".into()),
        ],
    );
    let env: std::collections::HashMap<String, String> =
        runner::with_attribution(base, Some(hooks))
            .into_iter()
            .collect();
    assert_hooks_path(&env, hooks);
    assert_eq!(env["GIT_CONFIG_COUNT"], "2");
    assert_eq!(env["GIT_CONFIG_KEY_0"], "user.useConfigOnly");
    // And nothing at all without a hooks directory.
    let none = runner::with_attribution(Vec::new(), None);
    assert!(none.is_empty());
}

// ---- the post-run check -------------------------------------------------------------

const FOOTER_BODY: &str = "## Summary\n\nFixes the thing.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n";
const PRODUCT_BODY: &str =
    "## Summary\n\nTurns Claude Code's attribution off: the bridge now passes \
`\"attribution\": {\"commit\": \"\", \"pr\": \"\"}` to every claude it starts, so the \
`Generated with` footer and the `Co-Authored-By: Claude` trailer are no longer added. \
Claude is the subject here, not the author. See claude.com/claude-code docs.\n";

#[test]
fn a_pull_request_body_with_the_footer_is_a_hit_and_one_about_claude_is_not() {
    assert_eq!(pr_text_hits("Fix", FOOTER_BODY).len(), 1);
    assert!(pr_text_hits("Fix", "Body\n\nGenerated with Claude Code\n").len() == 1);
    assert!(pr_text_hits("Fix", &format!("Body\n\n{TRAILER}\n")).len() == 1);
    assert!(pr_text_hits("Fix", "See https://claude.ai/code/session_01X\n").len() == 1);
    assert!(pr_text_hits("Teach Claude to count", PRODUCT_BODY).is_empty());
    assert!(pr_text_hits("Fix", "    Co-Authored-By: Claude (quoted hook output)\n").is_empty());
}

#[test]
fn a_commit_with_the_trailer_or_a_claude_identity_is_a_hit() {
    let c = CommitInfo {
        sha: "0123456789abcdef".into(),
        message: format!("Fix\n\n{TRAILER}\n"),
        author_name: "Jeremy".into(),
        author_email: "j@example.com".into(),
        committer_name: "Jeremy".into(),
        committer_email: "j@example.com".into(),
    };
    assert_eq!(commit_hits(&c), vec![TRAILER.to_string()]);
    let c2 = CommitInfo {
        message: "Teach the bridge about Claude Code settings\n".into(),
        author_name: "Claude".into(),
        author_email: "noreply@anthropic.com".into(),
        ..c.clone()
    };
    assert_eq!(
        commit_hits(&c2),
        vec!["author Claude <noreply@anthropic.com>".to_string()]
    );
    let clean = CommitInfo {
        message: "Teach the bridge about Claude Code settings\n".into(),
        ..c.clone()
    };
    assert!(commit_hits(&clean).is_empty());
}

/// A GitHub that answers from fixtures.
#[derive(Default)]
struct FakeGitHub {
    pulls: Vec<PullText>,
    commits: Vec<CommitInfo>,
}

impl GitHubView for FakeGitHub {
    fn pulls_for_branch(&self, _repo: &str, branch: &str) -> Option<Vec<PullText>> {
        Some(
            self.pulls
                .iter()
                .filter(|p| p.head == branch)
                .cloned()
                .collect(),
        )
    }
    fn pull(&self, url: &str) -> Option<PullText> {
        self.pulls.iter().find(|p| p.url == url).cloned()
    }
    fn commits(&self, _repo: &str, _base: &str, _head: &str) -> Option<Vec<CommitInfo>> {
        Some(self.commits.clone())
    }
}

fn fixture(body: &str, message: &str) -> FakeGitHub {
    FakeGitHub {
        pulls: vec![PullText {
            repo: "o/n".into(),
            url: "https://github.com/o/n/pull/7".into(),
            title: "[AD2] Fix".into(),
            body: body.into(),
            base: "main".into(),
            head: "ad2-fix".into(),
        }],
        commits: vec![CommitInfo {
            sha: "feedfacecafe0000".into(),
            message: message.into(),
            author_name: "Jeremy".into(),
            author_email: "j@example.com".into(),
            committer_name: "Jeremy".into(),
            committer_email: "j@example.com".into(),
        }],
    }
}

#[test]
fn the_post_run_check_flags_the_footer_and_the_trailer_and_passes_a_product_body() {
    let gh = fixture(FOOTER_BODY, &format!("Fix\n\n{TRAILER}\n"));
    let hits = scan_github(&gh, "o/n", "ad2-fix", "main", None);
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(hits[0].starts_with("pull request https://github.com/o/n/pull/7"));
    assert!(
        hits[1].starts_with("commit feedfacecafe \"Fix\""),
        "{hits:?}"
    );
    let line = report_line(&hits).unwrap();
    assert!(line.starts_with(REPORT_PREFIX));
    assert!(line.contains("pull/7") && line.contains("feedfacecafe"));

    let gh = fixture(PRODUCT_BODY, "Turn Claude Code attribution off\n");
    let hits = scan_github(&gh, "o/n", "ad2-fix", "main", None);
    assert!(hits.is_empty(), "{hits:?}");
    assert!(report_line(&hits).is_none());

    // The report's pull request is read even when its head is another branch.
    let gh = fixture(FOOTER_BODY, "Fix\n");
    let hits = scan_github(
        &gh,
        "o/n",
        "s-run-branch",
        "main",
        Some("https://github.com/o/n/pull/7"),
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn a_cluster_run_gets_the_same_check() {
    let spec = crate::devjob::SpecAnnotation {
        repo: "o/n".into(),
        branch_slug: "fix".into(),
        ..Default::default()
    };
    let report = crate::devjob::DevJobReport {
        status: "ok".into(),
        pull_request: Some("https://github.com/o/n/pull/7".into()),
        ..Default::default()
    };
    let gh = fixture(FOOTER_BODY, "Fix\n");
    let line = crate::devjob::cluster_attribution(&gh, &spec, &report).expect("a hit");
    assert!(line.starts_with(REPORT_PREFIX), "{line}");
    let gh = fixture(PRODUCT_BODY, "Fix\n");
    assert!(crate::devjob::cluster_attribution(&gh, &spec, &report).is_none());

    let text = "---\ndevjob: j\n---\n\n# Dev job\n";
    assert_eq!(
        crate::devjob::lead_with(text, "ATTRIBUTION: x"),
        "---\ndevjob: j\n---\n\nATTRIBUTION: x\n\n# Dev job\n"
    );
}

#[test]
fn a_local_repository_is_checked_against_its_own_history() {
    let s = scratch("local");
    let r = repo(&s, "r");
    let base = String::from_utf8(git_in(&s, &r, &["rev-parse", "HEAD"], &[]).stdout).unwrap();
    assert!(ok(&commit(&s, &r, &["Clean"], &[], &[])));
    assert!(ok(&commit(
        &s,
        &r,
        &["Sneaky", TRAILER],
        &["--no-verify"],
        &[]
    )));
    let commits = local_commits(&r, &format!("{}..main", base.trim())).unwrap();
    assert_eq!(commits.len(), 2);
    let hits = commit_entries(&commits, &mut HashSet::new());
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("Sneaky") && hits[0].contains(TRAILER),
        "{hits:?}"
    );
}
