//! **EGRESS TIERS** — what a turn may reach on the open web.
//!
//! Three values, chosen per turn and signed into the turn token's `tier` claim:
//!
//!   * [`Egress::Closed`]: the egress proxy admits nothing, and the gateway hides every tool
//!     classed `egress`.
//!   * [`Egress::Allowlist`]: only the listed hosts (an exact host, or a leading-dot suffix
//!     `.example.com` that matches `example.com` and every subdomain), reads and sends alike.
//!     `egress`-class tools are hidden, as on `closed`: their destination is not a host the
//!     list can name.
//!   * [`Egress::OpenLogged`]: any public host for a `read`, only the send list for a `send`
//!     (see [`crate::proxy::egress`]), and every request logged by host.
//!
//! ---- SELECTION ---------------------------------------------------------------------
//!
//! [`select`] is the one place a turn's tier is decided:
//!
//!   * a scheduler-fired turn is `closed` unless its `[[schedule]]` entry says
//!     `egress = "allowlist"` with `hosts = [...]`, or `egress = "open"`;
//!   * a phone (or any other client) turn takes `[sandbox] egress_phone`, default
//!     `open-logged`;
//!   * the bridge's own one-shots (diet extraction, the title, vault QA) are `closed`.

use serde::Serialize;

/// A turn's egress tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Egress {
    Closed,
    Allowlist(Vec<String>),
    OpenLogged,
}

impl Egress {
    /// The token claim and the ledger's word for it.
    pub fn label(&self) -> &'static str {
        match self {
            Egress::Closed => "closed",
            Egress::Allowlist(_) => "allowlist",
            Egress::OpenLogged => "open-logged",
        }
    }

    /// Whether `egress`-class gateway tools are listed and callable on this tier. Only on
    /// `open-logged`: an allowlist names web hosts, and an egress tool's destination is not
    /// one of them.
    pub fn admits_egress_tools(&self) -> bool {
        matches!(self, Egress::OpenLogged)
    }

    /// Whether the egress proxy admits `host` at all on this tier (before the read/send split
    /// and the content check, which are the proxy's).
    pub fn admits_host(&self, host: &str) -> bool {
        match self {
            Egress::Closed => false,
            Egress::Allowlist(hosts) => hosts.iter().any(|h| host_matches(h, host)),
            Egress::OpenLogged => true,
        }
    }

    /// Parse a schedule entry's `egress` value with its `hosts`. `None` is `closed`.
    pub fn from_schedule(egress: Option<&str>, hosts: &[String]) -> Result<Egress, String> {
        match egress {
            None | Some("closed") => {
                if hosts.is_empty() {
                    Ok(Egress::Closed)
                } else {
                    Err("hosts is set but egress is not \"allowlist\"".into())
                }
            }
            Some("allowlist") => {
                if hosts.is_empty() {
                    return Err("egress = \"allowlist\" needs a non-empty hosts list".into());
                }
                for h in hosts {
                    if !valid_host_pattern(h) {
                        return Err(format!("hosts entry {h:?} is not a host name or .suffix"));
                    }
                }
                Ok(Egress::Allowlist(
                    hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
                ))
            }
            Some("open") | Some("open-logged") => {
                if hosts.is_empty() {
                    Ok(Egress::OpenLogged)
                } else {
                    Err("hosts is set but egress is \"open\"; an open job reaches any host".into())
                }
            }
            Some(other) => Err(format!(
                "egress = {other:?}: only \"closed\", \"allowlist\" and \"open\""
            )),
        }
    }

    /// Parse `[sandbox] egress_phone`.
    pub fn from_phone_setting(s: &str) -> Result<Egress, String> {
        match s {
            "closed" => Ok(Egress::Closed),
            "open" | "open-logged" => Ok(Egress::OpenLogged),
            other => Err(format!(
                "[sandbox] egress_phone = {other:?}: only \"closed\" or \"open-logged\" (an \
                 allowlist is a per-job setting)"
            )),
        }
    }
}

/// `pattern` (an exact host, or `.suffix`) against `host`. Case-insensitive; a trailing dot on
/// the host is ignored. `.example.com` matches `example.com` and `a.example.com`, never
/// `badexample.com`.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    match pattern.strip_prefix('.') {
        Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
        None => host == pattern,
    }
}

fn valid_host_pattern(h: &str) -> bool {
    let body = h.strip_prefix('.').unwrap_or(h);
    !body.is_empty()
        && body.len() <= 253
        && body.contains('.')
        && body
            .split('.')
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// What kind of turn is asking. The caller knows; [`select`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnKind<'a> {
    /// A turn the scheduler fired for the job with this id.
    Scheduled(&'a str),
    /// A turn a client submitted (the phone, the Mac, the CLI).
    Client,
    /// The bridge's own one-shot children: diet extraction, the title, vault QA.
    OneShot,
}

/// The tier for a turn of `kind`. `job_tier` is the scheduled job's own parsed tier (from
/// [`Egress::from_schedule`]), `None` when the job is unknown — which is then `closed`.
pub fn select(kind: TurnKind<'_>, job_tier: Option<&Egress>, phone: &Egress) -> Egress {
    match kind {
        TurnKind::Scheduled(_) => job_tier.cloned().unwrap_or(Egress::Closed),
        TurnKind::Client => phone.clone(),
        TurnKind::OneShot => Egress::Closed,
    }
}

/// The turn kind an activity-ledger origin names: `schedule:<id>` is scheduled; every other
/// origin (`phone`, `cli`, the strand tick) is a client turn.
pub fn kind_from_origin(origin: &str) -> TurnKind<'_> {
    match origin.strip_prefix("schedule:") {
        Some(id) => TurnKind::Scheduled(id),
        None => TurnKind::Client,
    }
}

// ---- PER-JOB TIER REGISTRY -------------------------------------------------------------
//
// The handler that accepts a turn knows its origin; the code that opens the gateway turn
// (and signs the token) knows only the job id. Rather than thread a tier through every
// signature between the two, the handler files the tier here under the job id and the
// gateway reads it back. A job with nothing filed is `closed`: forgetting to file a tier
// fails shut.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn registry() -> &'static Mutex<HashMap<String, Egress>> {
    static R: OnceLock<Mutex<HashMap<String, Egress>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// File `tier` for `job_id`. The returned guard removes it when the turn ends.
pub fn file_tier(job_id: &str, tier: Egress) -> TierGuard {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(job_id.to_string(), tier);
    TierGuard(job_id.to_string())
}

/// The tier filed for `job_id`, or `closed`.
pub fn tier_for(job_id: &str) -> Egress {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(job_id)
        .cloned()
        .unwrap_or(Egress::Closed)
}

/// Removes a filed tier on drop.
pub struct TierGuard(String);

impl Drop for TierGuard {
    fn drop(&mut self) {
        registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_and_exact_host_matching() {
        assert!(host_matches("example.com", "EXAMPLE.com."));
        assert!(!host_matches("example.com", "a.example.com"));
        assert!(host_matches(".example.com", "example.com"));
        assert!(host_matches(".example.com", "a.b.example.com"));
        assert!(!host_matches(".example.com", "badexample.com"));
        assert!(!host_matches(".example.com", "example.com.evil.net"));
    }

    #[test]
    fn tiers_admit_hosts_as_documented() {
        assert!(!Egress::Closed.admits_host("example.com"));
        let a = Egress::Allowlist(vec!["api.github.com".into(), ".wikipedia.org".into()]);
        assert!(a.admits_host("api.github.com"));
        assert!(a.admits_host("en.wikipedia.org"));
        assert!(!a.admits_host("github.com"));
        assert!(Egress::OpenLogged.admits_host("anything.example"));
    }

    #[test]
    fn only_open_logged_admits_egress_tools() {
        assert!(!Egress::Closed.admits_egress_tools());
        assert!(!Egress::Allowlist(vec!["a.example".into()]).admits_egress_tools());
        assert!(Egress::OpenLogged.admits_egress_tools());
    }

    #[test]
    fn schedule_entries_parse_to_tiers_and_default_closed() {
        assert_eq!(Egress::from_schedule(None, &[]), Ok(Egress::Closed));
        assert_eq!(Egress::from_schedule(Some("closed"), &[]), Ok(Egress::Closed));
        assert_eq!(Egress::from_schedule(Some("open"), &[]), Ok(Egress::OpenLogged));
        assert_eq!(
            Egress::from_schedule(Some("allowlist"), &["API.github.com".into()]),
            Ok(Egress::Allowlist(vec!["api.github.com".into()]))
        );
        assert!(Egress::from_schedule(Some("allowlist"), &[]).is_err());
        assert!(Egress::from_schedule(None, &["a.example".into()]).is_err());
        assert!(Egress::from_schedule(Some("open"), &["a.example".into()]).is_err());
        assert!(Egress::from_schedule(Some("wide-open"), &[]).is_err());
        assert!(Egress::from_schedule(Some("allowlist"), &["not a host".into()]).is_err());
        assert!(Egress::from_schedule(Some("allowlist"), &["localhost".into()]).is_err());
    }

    #[test]
    fn selection_per_turn_kind() {
        let phone = Egress::OpenLogged;
        let job = Egress::Allowlist(vec!["api.github.com".into()]);
        assert_eq!(
            select(TurnKind::Scheduled("nightly"), Some(&job), &phone),
            job
        );
        assert_eq!(
            select(TurnKind::Scheduled("nightly"), None, &phone),
            Egress::Closed
        );
        assert_eq!(select(TurnKind::Client, None, &phone), Egress::OpenLogged);
        assert_eq!(
            select(TurnKind::Client, None, &Egress::Closed),
            Egress::Closed
        );
        assert_eq!(select(TurnKind::OneShot, Some(&job), &phone), Egress::Closed);
        assert_eq!(kind_from_origin("schedule:x"), TurnKind::Scheduled("x"));
        assert_eq!(kind_from_origin("phone"), TurnKind::Client);
        assert_eq!(kind_from_origin("strand-tick"), TurnKind::Client);
    }

    #[test]
    fn the_phone_setting_parses() {
        assert_eq!(Egress::from_phone_setting("open-logged"), Ok(Egress::OpenLogged));
        assert_eq!(Egress::from_phone_setting("closed"), Ok(Egress::Closed));
        assert!(Egress::from_phone_setting("allowlist").is_err());
    }

    #[test]
    fn an_unfiled_job_is_closed_and_a_filed_one_is_forgotten_on_drop() {
        assert_eq!(tier_for("k04-unfiled"), Egress::Closed);
        {
            let _g = file_tier("k04-filed", Egress::OpenLogged);
            assert_eq!(tier_for("k04-filed"), Egress::OpenLogged);
        }
        assert_eq!(tier_for("k04-filed"), Egress::Closed);
    }
}
