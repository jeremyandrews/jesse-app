//! The bridge's launch environment, read from its launchd plist.
//!
//! The live bridge runs with the `EnvironmentVariables` of its LaunchAgent plist
//! (`~/Library/LaunchAgents/<bridge label>.plist`), PATH included. An interactive shell has none
//! of it, so a tool that must behave like the bridge (the containment battery, the transcriber)
//! reads the plist itself. The plist is read with `plutil`, never PlistBuddy, and nothing is
//! ever written back.
//!
//! Two policies, one per caller:
//!
//!   * [`load_plist_env`] (`jesse-transcribe --env-plist`): fill only what the shell has not set.
//!   * [`bridge_env_plan`] (`containment-probe`, by default): BE the bridge's environment. The
//!     plist wins for every variable; PATH is the plist's PATH followed by any shell entries
//!     it lacks, so a `~/.cargo/bin` from the shell still resolves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The suffix every bridge LaunchAgent plist's file name ends with: `<reverse dns>.jesse-bridge`
/// plus `.plist`, the same rule `scripts/install-sentinel.sh` finds it by.
pub const BRIDGE_PLIST_SUFFIX: &str = ".jesse-bridge.plist";

/// The bridge's LaunchAgent plist in `launch_agents`: `<label>.plist` when `label` is given and
/// that file exists, else the ONE file whose name ends [`BRIDGE_PLIST_SUFFIX`]. `None` when
/// neither settles it (none, or more than one).
pub fn find_bridge_plist(launch_agents: &Path, label: Option<&str>) -> Option<PathBuf> {
    if let Some(l) = label {
        let p = launch_agents.join(format!("{l}.plist"));
        if p.is_file() {
            return Some(p);
        }
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(launch_agents)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(BRIDGE_PLIST_SUFFIX))
        })
        .collect();
    if found.len() == 1 {
        found.pop()
    } else {
        None
    }
}

/// Every string in a launchd plist's `EnvironmentVariables`. Non-string values are skipped.
pub fn read_plist_env(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let shown = path.display();
    let out = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "EnvironmentVariables", "json", "-o", "-"])
        .arg(path)
        .output()
        .map_err(|e| format!("could not run plutil: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{shown} has no readable EnvironmentVariables: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{shown}: EnvironmentVariables is not a dictionary ({e})"))?;
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect())
}

/// Fill every variable the shell has not set from a launchd plist's `EnvironmentVariables`.
/// Returns how many were filled. Call it before any other thread reads the environment.
pub fn load_plist_env(path: &str) -> Result<usize, String> {
    let mut filled = 0;
    for (k, v) in read_plist_env(Path::new(path))? {
        if std::env::var_os(&k).is_none() {
            std::env::set_var(&k, v);
            filled += 1;
        }
    }
    Ok(filled)
}

/// The plist's PATH, then every shell entry it does not already have, in the shell's order.
/// Empty entries are dropped and no entry appears twice.
pub fn merge_path(plist: &str, shell: Option<&str>) -> String {
    let mut out: Vec<&str> = Vec::new();
    for e in plist.split(':').chain(shell.unwrap_or("").split(':')) {
        if !e.is_empty() && !out.contains(&e) {
            out.push(e);
        }
    }
    out.join(":")
}

/// Where `containment-probe` takes its environment from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvSource {
    /// `--no-env-plist`: the shell's environment, untouched.
    Off,
    /// The plist at this path (the live bridge's by default, `--env-plist` to override).
    Plist(PathBuf),
}

/// What loading the bridge environment came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvPlan {
    /// Nothing was read and nothing is to change.
    Off,
    /// The plist was read: `set` is every variable to set, already merged.
    Loaded {
        plist: PathBuf,
        set: BTreeMap<String, String>,
    },
    /// The plist could not be read.
    Unreadable { plist: PathBuf, error: String },
}

/// Plan the bridge environment: read the plist with `read`, and merge it over the shell's
/// values from `shell`. Pure apart from what `read` does, so a test can drive it.
pub fn bridge_env_plan(
    source: &EnvSource,
    read: impl Fn(&Path) -> Result<BTreeMap<String, String>, String>,
    shell: impl Fn(&str) -> Option<String>,
) -> EnvPlan {
    let plist = match source {
        EnvSource::Off => return EnvPlan::Off,
        EnvSource::Plist(p) => p.clone(),
    };
    let vars = match read(&plist) {
        Ok(v) => v,
        Err(error) => return EnvPlan::Unreadable { plist, error },
    };
    let set = vars
        .into_iter()
        .map(|(k, v)| {
            if k == "PATH" {
                let merged = merge_path(&v, shell("PATH").as_deref());
                (k, merged)
            } else {
                (k, v)
            }
        })
        .collect();
    EnvPlan::Loaded { plist, set }
}

impl EnvPlan {
    /// Whether the run may go on. `Err` is the refusal: a `--write` run must not record a
    /// posture probed in an environment the bridge never runs with. `Ok(Some(_))` is a
    /// warning for a run that records nothing.
    pub fn verdict(&self, write: bool) -> Result<Option<String>, String> {
        match self {
            EnvPlan::Unreadable { plist, error } if write => Err(format!(
                "--write refuses to start: the bridge's environment could not be loaded from \
                 {} ({error}). Without it the MCP servers start without their PATH and \
                 credentials and the record would vouch for a posture the bridge never runs. \
                 Fix the plist, name another with --env-plist <path>, or pass --no-env-plist \
                 on purpose.",
                plist.display()
            )),
            EnvPlan::Unreadable { plist, error } => Ok(Some(format!(
                "WARNING: the bridge's environment could not be loaded from {} ({error}); \
                 running with this shell's environment, which is NOT what the bridge runs with.",
                plist.display()
            ))),
            _ => Ok(None),
        }
    }

    /// The one line said before the battery starts. Names no value but PATH's first three
    /// entries.
    pub fn summary(&self) -> String {
        match self {
            EnvPlan::Off => "environment: --no-env-plist, this shell's environment as is".into(),
            EnvPlan::Unreadable { plist, .. } => {
                format!("environment: {} NOT loaded", plist.display())
            }
            EnvPlan::Loaded { plist, set } => {
                let head: Vec<&str> = set
                    .get("PATH")
                    .map(|p| p.split(':').take(3).collect())
                    .unwrap_or_default();
                format!(
                    "environment: {} loaded, {} variable(s) taken, PATH starts {}",
                    plist.display(),
                    set.len(),
                    if head.is_empty() {
                        "(the plist sets no PATH)".to_string()
                    } else {
                        head.join(":")
                    }
                )
            }
        }
    }

    /// Set every planned variable in this process. Call it before any other thread exists.
    pub fn apply(&self) {
        if let EnvPlan::Loaded { set, .. } = self {
            for (k, v) in set {
                std::env::set_var(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plist(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn shell<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn path_is_the_plists_first_then_the_shell_entries_it_lacks() {
        assert_eq!(
            merge_path(
                "/nvm/bin:/home/.local/bin:/usr/bin:/bin",
                Some("/home/.cargo/bin:/usr/bin:/opt/homebrew/bin::/bin")
            ),
            "/nvm/bin:/home/.local/bin:/usr/bin:/bin:/home/.cargo/bin:/opt/homebrew/bin"
        );
        assert_eq!(merge_path("/a:/b", None), "/a:/b");
        assert_eq!(merge_path("", Some("/c")), "/c");
    }

    #[test]
    fn the_plan_merges_path_and_lets_the_plist_win_everywhere_else() {
        let source = EnvSource::Plist(PathBuf::from("/x/bridge.plist"));
        let plan = bridge_env_plan(
            &source,
            |_| {
                Ok(plist(&[
                    ("PATH", "/nvm/bin:/home/.local/bin:/usr/bin"),
                    ("JESSE_GITHUB_PAT", "from-plist"),
                    ("TZ", "Europe/Rome"),
                ]))
            },
            shell(&[
                ("PATH", "/home/.cargo/bin:/usr/bin"),
                ("JESSE_GITHUB_PAT", "from-shell"),
                ("SHELL_ONLY", "kept"),
            ]),
        );
        let EnvPlan::Loaded { set, .. } = &plan else {
            panic!("expected a loaded plan, got {plan:?}");
        };
        assert_eq!(
            set["PATH"],
            "/nvm/bin:/home/.local/bin:/usr/bin:/home/.cargo/bin"
        );
        assert_eq!(set["JESSE_GITHUB_PAT"], "from-plist");
        assert_eq!(set["TZ"], "Europe/Rome");
        // A variable only the shell has is not the plan's to touch.
        assert!(!set.contains_key("SHELL_ONLY"));
        assert_eq!(plan.verdict(true), Ok(None));
    }

    #[test]
    fn the_summary_names_the_plist_the_count_and_three_path_entries_and_no_other_value() {
        let plan = EnvPlan::Loaded {
            plist: PathBuf::from("/x/bridge.plist"),
            set: plist(&[
                ("PATH", "/a:/b:/c:/d"),
                ("JESSE_GITHUB_PAT", "SECRET-VALUE"),
            ]),
        };
        let line = plan.summary();
        assert_eq!(
            line,
            "environment: /x/bridge.plist loaded, 2 variable(s) taken, PATH starts /a:/b:/c"
        );
        assert!(!line.contains("SECRET-VALUE"));
        assert!(!line.contains("/d"));
    }

    #[test]
    fn a_missing_plist_refuses_write_and_only_warns_without_it() {
        let missing = PathBuf::from("/nonexistent/jesse/com.example.jesse-bridge.plist");
        // The real reader, against a path that is not there.
        let plan = bridge_env_plan(&EnvSource::Plist(missing.clone()), read_plist_env, |_| None);
        assert!(
            matches!(&plan, EnvPlan::Unreadable { plist, .. } if *plist == missing),
            "{plan:?}"
        );
        let refusal = plan.verdict(true).expect_err("--write must refuse");
        assert!(refusal.contains("--write refuses to start"), "{refusal}");
        assert!(
            refusal.contains(&missing.display().to_string()),
            "{refusal}"
        );
        let warning = plan.verdict(false).expect("a dry run goes on");
        assert!(warning.is_some_and(|w| w.starts_with("WARNING")));
    }

    #[test]
    fn no_env_plist_reads_nothing_and_changes_nothing() {
        let plan = bridge_env_plan(
            &EnvSource::Off,
            |p| panic!("--no-env-plist must not read {}", p.display()),
            |k| panic!("--no-env-plist must not look at {k}"),
        );
        assert_eq!(plan, EnvPlan::Off);
        assert_eq!(plan.verdict(true), Ok(None));
        // `apply` on the Off plan sets nothing: a sentinel variable stays unset.
        let sentinel = "JESSE_PLISTENV_TEST_SENTINEL_UNSET";
        plan.apply();
        assert!(std::env::var_os(sentinel).is_none());
    }

    #[test]
    fn the_bridge_plist_is_found_by_label_then_by_its_one_suffix_match() {
        let dir = std::env::temp_dir().join(format!("jesse-plistfind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(find_bridge_plist(&dir, None), None);
        for f in [
            "com.example.jesse-bridge.plist",
            "com.example.jesse-bridge.plist.bak-1",
            "com.example.jesse-pro-bridge.plist",
            "com.example.jesse-sentinel.plist",
        ] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let only = dir.join("com.example.jesse-bridge.plist");
        assert_eq!(find_bridge_plist(&dir, None), Some(only.clone()));
        // A label whose plist is not there falls back to the suffix match.
        assert_eq!(
            find_bridge_plist(&dir, Some("com.example.missing-bridge")),
            Some(only)
        );
        // Two candidates and no label: not settled.
        std::fs::write(dir.join("org.second.jesse-bridge.plist"), "").unwrap();
        assert_eq!(find_bridge_plist(&dir, None), None);
        // The label settles it.
        assert_eq!(
            find_bridge_plist(&dir, Some("org.second.jesse-bridge")),
            Some(dir.join("org.second.jesse-bridge.plist"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_real_plist_is_read_through_plutil() {
        let dir = std::env::temp_dir().join(format!("jesse-plistenv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bridge.plist");
        std::fs::write(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.example.test</string>
<key>EnvironmentVariables</key><dict>
<key>PATH</key><string>/nvm/bin:/usr/bin</string>
<key>JESSE_X</key><string>y</string>
</dict></dict></plist>
"#,
        )
        .unwrap();
        let vars = read_plist_env(&path).expect("plutil reads it");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            vars,
            plist(&[("PATH", "/nvm/bin:/usr/bin"), ("JESSE_X", "y")])
        );
    }
}
