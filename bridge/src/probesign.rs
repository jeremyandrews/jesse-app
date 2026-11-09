//! **THE BATTERY SIGNS ITSELF** — one stable code identity for `containment-probe` across
//! rebuilds, so a macOS Local Network grant given to it once survives the next build.
//!
//! The linker gives every build a fresh ad hoc signature (identifier
//! `containment_probe-<hash>`), and macOS keys Local Network grants to the code identity, so
//! each rebuild is a new app to the OS. With `[gateway] enabled` the battery's LAN MCP
//! traffic (the `house` server, among others) is opened by the in-process gateway, which is
//! THIS binary — so a fresh build's first connection to a LAN host fails with `No route to
//! host (os error 65)` until someone approves it at the screen, and the write row loses every
//! tool that server has. That is how 2026-11-09's run lost all of `house`.
//!
//! The fix is the sentinel's, applied to the one binary a deploy never touches: at startup,
//! before any network call, the probe checks its own executable against the deployment's
//! identity (`JESSE_SENTINEL_CODESIGN_IDENTITY`, else `~/.jesse-sentinel/codesign-identity`)
//! and the identifier [`signing_identifier`]`(<bridge label>, "containment-probe")`. When it
//! does not satisfy them, it signs a copy, verifies it, renames it over itself and re-executes
//! once. `cargo run --features containment-probe --bin containment-probe` stays the command.
//!
//! Nothing here grants anything. The first signed run still needs Jeremy to approve the
//! `containment-probe` row in System Settings → Privacy & Security → Local Network; every
//! rebuild after that is the same identity, so the approval holds.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::sentinel::{
    configured_codesign_identity, parse_plist_string_key, signing_identifier, ServiceSlot,
    CODESIGN_TIMEOUT,
};

/// The binary's name, and its half of the signing identifier.
pub const PROBE_BIN: &str = "containment-probe";

/// Set on the one re-exec after a re-sign, so a signature that still does not satisfy the
/// requirement is an error rather than a loop.
pub const RESIGNED_ENV: &str = "JESSE_CONTAINMENT_PROBE_RESIGNED";

/// The one line printed when no identity is configured. The run goes on, as it did before
/// signing existed.
pub const AD_HOC_NOTE: &str = "containment-probe: no signing identity configured \
     (JESSE_SENTINEL_CODESIGN_IDENTITY or ~/.jesse-sentinel/codesign-identity), so the battery \
     runs ad hoc and LAN MCP servers may fail macOS Local Network (os error 65)";

/// What to sign with: the identity `codesign -s` takes and the identifier `-i` sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningPlan {
    pub identity: String,
    pub identifier: String,
}

impl SigningPlan {
    /// The plan for `identity` under the bridge's launchd label: the sentinel's scheme for a
    /// non-bridge binary, so this never shares (and fights over) the bridge's own grant.
    pub fn new(identity: &str, bridge_label: &str) -> SigningPlan {
        SigningPlan {
            identity: identity.to_string(),
            identifier: signing_identifier(bridge_label, PROBE_BIN),
        }
    }

    /// The code requirement a correctly signed probe satisfies. A SHA-1 identity hash pins the
    /// leaf certificate exactly; a name (which `codesign -s` matches as a substring) can only
    /// demand an Apple-issued, non ad hoc signature.
    pub fn requirement(&self) -> String {
        let id = &self.identifier;
        let hash = &self.identity;
        if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            format!("identifier \"{id}\" and certificate leaf = H\"{hash}\"")
        } else {
            format!("identifier \"{id}\" and anchor apple generic")
        }
    }
}

/// The sentinel's state directory, by the sentinel's own rule.
pub fn sentinel_state_dir() -> PathBuf {
    crate::env_string("JESSE_SENTINEL_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".jesse-sentinel"))
}

fn home() -> PathBuf {
    PathBuf::from(crate::env_string("HOME").unwrap_or_else(|| ".".to_string()))
}

/// The bridge's launchd label: `JESSE_SENTINEL_LABEL_BRIDGE` when this shell has it, else the
/// value the sentinel's own LaunchAgent sets (a battery run from a terminal does not inherit
/// the sentinel's environment), else the sentinel's default.
pub fn bridge_label() -> String {
    bridge_label_from(
        crate::env_string(ServiceSlot::Bridge.label_env()),
        &home().join("Library/LaunchAgents"),
    )
}

/// [`bridge_label`] with its inputs named, for the tests.
pub fn bridge_label_from(env: Option<String>, launch_agents: &Path) -> String {
    if let Some(l) = env {
        return l;
    }
    let key = ServiceSlot::Bridge.label_env();
    let mut plists: Vec<PathBuf> = std::fs::read_dir(launch_agents)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "plist"))
        .collect();
    plists.sort();
    plists
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|xml| parse_plist_string_key(&xml, key))
        .unwrap_or_else(|| ServiceSlot::Bridge.default_label().to_string())
}

/// The plan this deployment configures, or `None` when no identity is set.
pub fn plan_from_env() -> Option<SigningPlan> {
    configured_codesign_identity(&sentinel_state_dir())
        .map(|identity| SigningPlan::new(&identity, &bridge_label()))
}

/// The three `codesign` operations, behind a trait so the decisions are testable without a
/// keychain.
pub trait Codesign {
    /// Whether `path`'s signature is valid AND satisfies `requirement`.
    fn satisfies(&self, path: &Path, requirement: &str) -> bool;
    /// `codesign -f -s <identity> -i <identifier> <path>`.
    fn sign(&self, path: &Path, identity: &str, identifier: &str) -> Result<(), String>;
    /// `codesign -v <path>`.
    fn verify(&self, path: &Path) -> Result<(), String>;
}

/// The real `codesign`, each call bounded by `timeout`: the one way it hangs is a keychain
/// waiting on a dialog, and that must stop the run, not hold it.
pub struct SystemCodesign {
    pub bin: PathBuf,
    pub timeout: Duration,
}

impl Default for SystemCodesign {
    fn default() -> Self {
        SystemCodesign {
            bin: PathBuf::from("/usr/bin/codesign"),
            timeout: CODESIGN_TIMEOUT,
        }
    }
}

impl SystemCodesign {
    fn run(&self, args: &[&str]) -> Result<(), String> {
        run_bounded(&self.bin, args, self.timeout)
    }
}

/// Run `bin args` with a deadline. `Err` carries the exit and stderr, or says it timed out.
fn run_bounded(bin: &Path, args: &[&str], timeout: Duration) -> Result<(), String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not finish inside {} s (a keychain waiting on a prompt?)",
                    bin.display(),
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("waiting on {}: {e}", bin.display())),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut err = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    Err(format!("{status}: {}", err.trim()))
}

impl Codesign for SystemCodesign {
    fn satisfies(&self, path: &Path, requirement: &str) -> bool {
        let p = path.to_string_lossy();
        self.run(&["-v", "-R", &format!("={requirement}"), &p])
            .is_ok()
    }

    fn sign(&self, path: &Path, identity: &str, identifier: &str) -> Result<(), String> {
        let p = path.to_string_lossy();
        self.run(&["-f", "-s", identity, "-i", identifier, &p])
            .map_err(|e| format!("codesign failed on {p}: {e}"))
    }

    fn verify(&self, path: &Path) -> Result<(), String> {
        let p = path.to_string_lossy();
        self.run(&["-v", &p])
            .map_err(|e| format!("codesign -v rejected {p} after signing: {e}"))
    }
}

/// What [`ensure_signed`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum SelfSign {
    /// No identity configured: print [`AD_HOC_NOTE`] and go on.
    AdHoc,
    /// The executable already carries the expected identity and identifier.
    AlreadySigned,
    /// The executable on disk was replaced by a signed copy; the running process is still the
    /// old image, so the caller must re-exec ([`reexec`]) before any network call.
    Resigned,
}

/// Make `exe` carry `plan`'s identity, or say why it cannot.
///
/// The copy is signed, verified and checked against the requirement BEFORE it is renamed over
/// `exe`, so a failed sign leaves the old binary in place, and the running image (whose pages
/// are still mapped) is never rewritten underneath itself. `already_reexeced` is true on the
/// run after a re-sign: if the binary still does not satisfy the requirement then, something
/// is wrong that another round would not fix.
pub fn ensure_signed(
    exe: &Path,
    plan: Option<&SigningPlan>,
    already_reexeced: bool,
    cs: &dyn Codesign,
) -> Result<SelfSign, String> {
    let Some(plan) = plan else {
        return Ok(SelfSign::AdHoc);
    };
    let requirement = plan.requirement();
    if cs.satisfies(exe, &requirement) {
        return Ok(SelfSign::AlreadySigned);
    }
    if already_reexeced {
        return Err(format!(
            "{} was re-signed as {} and re-executed, and still does not satisfy `{requirement}`",
            exe.display(),
            plan.identifier
        ));
    }
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| PROBE_BIN.to_string());
    let staged = exe.with_file_name(format!(".{name}.signing"));
    // Found BEFORE the rename, while the two still share an inode.
    let original = cargo_original(exe);
    let result = (|| {
        std::fs::copy(exe, &staged).map_err(|e| {
            format!(
                "could not copy {} to {}: {e}",
                exe.display(),
                staged.display()
            )
        })?;
        cs.sign(&staged, &plan.identity, &plan.identifier)?;
        cs.verify(&staged)?;
        if !cs.satisfies(&staged, &requirement) {
            return Err(format!(
                "{} was signed but does not satisfy `{requirement}`",
                staged.display()
            ));
        }
        std::fs::rename(&staged, exe).map_err(|e| {
            format!(
                "could not move {} over {}: {e}",
                staged.display(),
                exe.display()
            )
        })?;
        // Cargo re-copies `target/<profile>/<bin>` from `deps/` on every `cargo run`, fresh or
        // not, so the signed file is copied back over the original too; otherwise each run
        // would find the ad hoc build again and re-sign it. Best effort: a miss only costs
        // the next run a re-sign under the same identity.
        if let Some(orig) = original {
            let tmp = orig.with_file_name(format!(".{name}.signed-copy"));
            if std::fs::copy(exe, &tmp).is_err() || std::fs::rename(&tmp, &orig).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result.map(|()| SelfSign::Resigned)
}

/// The file in `<exe dir>/deps/` that cargo uplifted `exe` from, if any: a
/// `<crate name>-<hash>` file with exactly `exe`'s bytes. Cargo links or clones it to
/// `target/<profile>/<bin>`, so before signing the two are identical.
pub fn cargo_original(exe: &Path) -> Option<PathBuf> {
    let name = exe.file_name()?.to_string_lossy().replace('-', "_");
    let prefix = format!("{name}-");
    let len = std::fs::metadata(exe).ok()?.len();
    let mut bytes: Option<Vec<u8>> = None;
    std::fs::read_dir(exe.parent()?.join("deps"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && !n.contains('.')
            }) && std::fs::metadata(p).is_ok_and(|m| m.len() == len)
        })
        .find(|p| {
            let mine = bytes.get_or_insert_with(|| std::fs::read(exe).unwrap_or_default());
            std::fs::read(p).is_ok_and(|theirs| &theirs == mine)
        })
}

/// Replace this process with a fresh exec of `exe`, same arguments, [`RESIGNED_ENV`] set.
/// Returns only on failure.
#[cfg(unix)]
pub fn reexec(exe: &Path) -> String {
    use std::os::unix::process::CommandExt;
    let err = Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(RESIGNED_ENV, "1")
        .exec();
    format!("could not re-execute {}: {err}", exe.display())
}

/// The summary lines for upstreams that failed with `os error 65` (`EHOSTUNREACH`, what a
/// Local Network denial looks like to a socket): one per server, naming it as a likely denial
/// for `exe` rather than a tool set change.
pub fn local_network_lines(failures: &[(String, String)], exe: &str) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut lines = Vec::new();
    for (server, error) in failures {
        if !error.contains("os error 65") || seen.contains(&server.as_str()) {
            continue;
        }
        seen.push(server);
        lines.push(format!(
            "'{server}' failed with os error 65 (No route to host): likely a macOS Local \
             Network denial for this binary ({exe}), NOT a tool set change. Approve \
             containment-probe in System Settings > Privacy & Security > Local Network and \
             re-run."
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const HASH: &str = "CCEECD92CC2AC902E07976A4F5588C80FDEE61F2";

    /// A `codesign` that records what it was asked and answers from a script.
    #[derive(Default)]
    struct Fake {
        /// Paths that satisfy the requirement before anything is signed.
        signed: RefCell<Vec<PathBuf>>,
        sign_error: Option<String>,
        verify_error: Option<String>,
        calls: RefCell<Vec<String>>,
    }

    impl Codesign for Fake {
        fn satisfies(&self, path: &Path, requirement: &str) -> bool {
            self.calls
                .borrow_mut()
                .push(format!("satisfies {} {requirement}", path.display()));
            self.signed.borrow().iter().any(|p| p == path)
        }
        fn sign(&self, path: &Path, identity: &str, identifier: &str) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("sign {identity} {identifier}"));
            if let Some(e) = &self.sign_error {
                return Err(e.clone());
            }
            self.signed.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn verify(&self, _path: &Path) -> Result<(), String> {
            self.verify_error.clone().map_or(Ok(()), Err)
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "probesign-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn exe_in(dir: &Path) -> PathBuf {
        let exe = dir.join(PROBE_BIN);
        std::fs::write(&exe, b"old image").unwrap();
        exe
    }

    #[test]
    fn identifier_is_the_sentinel_scheme_under_the_bridge_label() {
        let plan = SigningPlan::new(HASH, "com.example.jesse-bridge");
        assert_eq!(
            plan.identifier,
            "com.example.jesse-bridge.containment-probe"
        );
        assert_eq!(
            plan.identifier,
            signing_identifier("com.example.jesse-bridge", "containment-probe")
        );
        // Never the bridge's own identifier, whose grant it must not share.
        assert_ne!(plan.identifier, "com.example.jesse-bridge");
    }

    #[test]
    fn a_hash_identity_pins_the_leaf_and_a_name_demands_apple_issued() {
        let plan = SigningPlan::new(HASH, "l");
        assert_eq!(
            plan.requirement(),
            format!("identifier \"l.containment-probe\" and certificate leaf = H\"{HASH}\"")
        );
        let named = SigningPlan::new("Apple Development: J (X)", "l");
        assert_eq!(
            named.requirement(),
            "identifier \"l.containment-probe\" and anchor apple generic"
        );
    }

    #[test]
    fn the_label_comes_from_the_env_then_the_sentinel_plist_then_the_default() {
        let d = scratch("label");
        assert_eq!(
            bridge_label_from(Some("com.example.env-bridge".into()), &d),
            "com.example.env-bridge"
        );
        assert_eq!(
            bridge_label_from(None, &d),
            ServiceSlot::Bridge.default_label()
        );
        std::fs::write(
            d.join("com.example.jesse-sentinel.plist"),
            "<dict><key>JESSE_SENTINEL_LABEL_BRIDGE</key>\n<string>com.example.other-bridge</string></dict>",
        )
        .unwrap();
        // A backup beside it is not a plist and is never read.
        std::fs::write(
            d.join("com.example.jesse-sentinel.plist.bak"),
            "<key>JESSE_SENTINEL_LABEL_BRIDGE</key><string>stale</string>",
        )
        .unwrap();
        assert_eq!(bridge_label_from(None, &d), "com.example.other-bridge");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn identity_absent_runs_ad_hoc_and_touches_nothing() {
        let d = scratch("absent");
        let exe = exe_in(&d);
        let fake = Fake::default();
        assert_eq!(ensure_signed(&exe, None, false, &fake), Ok(SelfSign::AdHoc));
        assert!(fake.calls.borrow().is_empty());
        assert!(AD_HOC_NOTE.contains("runs ad hoc"));
        assert!(AD_HOC_NOTE.contains("Local Network"));
        assert!(!AD_HOC_NOTE.contains('\n'), "one line");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn identity_present_and_already_signed_does_nothing_more() {
        let d = scratch("signed");
        let exe = exe_in(&d);
        let fake = Fake::default();
        fake.signed.borrow_mut().push(exe.clone());
        let plan = SigningPlan::new(HASH, "l");
        assert_eq!(
            ensure_signed(&exe, Some(&plan), false, &fake),
            Ok(SelfSign::AlreadySigned)
        );
        assert_eq!(fake.calls.borrow().len(), 1, "{:?}", fake.calls.borrow());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn identity_present_and_ad_hoc_build_is_resigned_through_a_copy() {
        let d = scratch("resign");
        let exe = exe_in(&d);
        let fake = Fake::default();
        let plan = SigningPlan::new(HASH, "com.example.jesse-bridge");
        assert_eq!(
            ensure_signed(&exe, Some(&plan), false, &fake),
            Ok(SelfSign::Resigned)
        );
        assert!(fake.calls.borrow().contains(&format!(
            "sign {HASH} com.example.jesse-bridge.containment-probe"
        )));
        // The copy was renamed over the executable; nothing is left beside it.
        assert_eq!(std::fs::read(&exe).unwrap(), b"old image");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_cargo_original_is_replaced_by_the_signed_file() {
        let d = scratch("uplift");
        std::fs::create_dir_all(d.join("deps")).unwrap();
        let orig = d.join("deps").join("containment_probe-0123");
        std::fs::write(&orig, b"ad hoc build").unwrap();
        // An older build of the same name, and the build's own metadata, are not it.
        std::fs::write(
            d.join("deps").join("containment_probe-9999"),
            b"older build!",
        )
        .unwrap();
        std::fs::write(
            d.join("deps").join("containment_probe-0123.d"),
            b"ad hoc build",
        )
        .unwrap();
        let exe = d.join(PROBE_BIN);
        std::fs::copy(&orig, &exe).unwrap();
        assert_eq!(cargo_original(&exe), Some(orig.clone()));
        let fake = Fake::default();
        let plan = SigningPlan::new(HASH, "l");
        assert_eq!(
            ensure_signed(&exe, Some(&plan), false, &fake),
            Ok(SelfSign::Resigned)
        );
        // The fake signer changes no bytes, so the original still matches: what matters is
        // that it was replaced by a copy of the signed file, and nothing was left beside it.
        assert_eq!(std::fs::read(&orig).unwrap(), std::fs::read(&exe).unwrap());
        assert_eq!(std::fs::read_dir(d.join("deps")).unwrap().count(), 3);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_sign_failure_stops_and_leaves_the_binary_alone() {
        let d = scratch("signfail");
        let exe = exe_in(&d);
        let fake = Fake {
            sign_error: Some("codesign failed on x: errSecItemNotFound".into()),
            ..Fake::default()
        };
        let plan = SigningPlan::new(HASH, "l");
        let err = ensure_signed(&exe, Some(&plan), false, &fake).unwrap_err();
        assert!(err.contains("errSecItemNotFound"), "{err}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"old image");
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            1,
            "staged copy removed"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_verify_failure_stops() {
        let d = scratch("verifyfail");
        let exe = exe_in(&d);
        let fake = Fake {
            verify_error: Some("codesign -v rejected x".into()),
            ..Fake::default()
        };
        let plan = SigningPlan::new(HASH, "l");
        let err = ensure_signed(&exe, Some(&plan), false, &fake).unwrap_err();
        assert!(err.contains("codesign -v rejected"), "{err}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn still_unsigned_after_the_reexec_is_an_error_not_a_loop() {
        let d = scratch("loop");
        let exe = exe_in(&d);
        let fake = Fake::default();
        let plan = SigningPlan::new(HASH, "l");
        let err = ensure_signed(&exe, Some(&plan), true, &fake).unwrap_err();
        assert!(err.contains("still does not satisfy"), "{err}");
        assert!(!fake.calls.borrow().iter().any(|c| c.starts_with("sign")));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_hung_codesign_times_out() {
        let err =
            run_bounded(Path::new("/bin/sleep"), &["5"], Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("did not finish inside"), "{err}");
    }

    #[test]
    fn os_error_65_is_named_as_a_local_network_denial() {
        let failures = vec![
            (
                "house".to_string(),
                "error sending request for url (https://house.pozza/mcp): client error \
                 (Connect): tcp connect error: No route to host (os error 65)"
                    .to_string(),
            ),
            (
                "house".to_string(),
                "... No route to host (os error 65)".to_string(),
            ),
            (
                "github".to_string(),
                "upstream 'github' would not list its tools: timed out".to_string(),
            ),
        ];
        let lines = local_network_lines(&failures, "/t/containment-probe");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("'house' failed with os error 65"));
        assert!(lines[0].contains("likely a macOS Local Network denial for this binary"));
        assert!(lines[0].contains("/t/containment-probe"));
        assert!(lines[0].contains("NOT a tool set change"));
        assert!(local_network_lines(&failures[2..], "x").is_empty());
    }
}
