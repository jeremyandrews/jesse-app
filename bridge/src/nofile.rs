//! **THE OPEN FILES LIMIT**, raised by the bridge itself at startup.
//!
//! A gateway-on turn costs the bridge descriptors for every admitted server: the listening unix
//! socket, the stub's connection to it, and the pipes of the upstream it starts. launchd starts
//! an agent with a soft `RLIMIT_NOFILE` of 256, and two turns at once overran it: upstreams
//! failed to start and a turn's socket failed to bind with `EMFILE` (2026-10-26 23:30 and
//! 2026-10-29 13:04, among others in the same week).
//!
//! [`raise`] lifts the soft limit to the hard limit, capped at [`NOFILE_CAP`], and on macOS
//! also at `kern.maxfilesperproc`: `setrlimit` there refuses `RLIM_INFINITY` (and anything above
//! that ceiling) for `RLIMIT_NOFILE`. It never lowers a limit, and a failure is the caller's
//! warning, never a refused boot. The plist's `SoftResourceLimits` (see the README) is the
//! backstop for a bridge that cannot raise its own.

use libc::rlim_t;

/// The highest soft limit the bridge asks for. Far above what [`MIN_CONCURRENT_TURNS`] turns
/// need, and low enough that a child which walks every possible descriptor (a `close_fds`
/// loop) inherits a bounded range.
pub const NOFILE_CAP: rlim_t = 8192;

/// One gateway-on turn's peak descriptor cost to the bridge, above its idle count.
///
/// MEASURED by `tests/gateway_fds.rs` (`cargo test --features containment-probe --test
/// gateway_fds -- --nocapture`): the shipped main set with the gateway and `devjob` on, every
/// admitted server bound, connected and its upstream started (stdio upstreams stand in for the
/// HTTP ones, so this is the upper bound), counted from `/dev/fd` on macOS and `/proc/self/fd`
/// on Linux. On 2026-10-29 (bridge 0.188.0) that turn admitted 25 servers. On macOS it went
/// from 10 descriptors to 132 and back to 10: 122, five per stdio upstream (listener,
/// connection, three pipes) and two for the in-process `devjob`. On Linux (CI) it went from 12
/// to 158 and back to 12: 146, one more per stdio upstream (the child's pidfd, through which
/// tokio reaps it). This is the larger, since the bridge runs on both. Under launchd's 256 a
/// turn of either size leaves room for two. The test fails when a turn costs more than this, so
/// the constant cannot fall behind a growing server list.
pub const TURN_PEAK_FDS: rlim_t = 146;

/// Fewer whole turns than this fitting under the soft limit is a startup warning.
pub const MIN_CONCURRENT_TURNS: rlim_t = 8;

/// A process's `RLIMIT_NOFILE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NofileLimit {
    pub soft: rlim_t,
    pub hard: rlim_t,
}

/// The soft limit to ask for: the hard limit, but no higher than `cap`, and never below the
/// soft limit the process already has.
pub fn target_soft(soft: rlim_t, hard: rlim_t, cap: rlim_t) -> rlim_t {
    soft.max(hard.min(cap))
}

/// How many turns of [`TURN_PEAK_FDS`] fit under `limit`.
pub fn turn_headroom(limit: rlim_t) -> rlim_t {
    limit / TURN_PEAK_FDS
}

/// A limit as a log or `/health` shows it: `unlimited` for `RLIM_INFINITY`.
pub fn label(v: rlim_t) -> String {
    if v == libc::RLIM_INFINITY {
        "unlimited".to_string()
    } else {
        v.to_string()
    }
}

/// The process's limit now, or `None` when `getrlimit` fails.
pub fn current() -> Option<NofileLimit> {
    let mut r = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes one `rlimit` through a pointer to a live local.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) };
    (rc == 0).then_some(NofileLimit {
        soft: r.rlim_cur,
        hard: r.rlim_max,
    })
}

/// Raise the soft limit to [`target_soft`] of the current limits, under [`NOFILE_CAP`] and the
/// platform's per-process ceiling. Returns the limit before and after; unchanged when it was
/// already there.
pub fn raise() -> Result<(NofileLimit, NofileLimit), String> {
    let before = current().ok_or_else(|| {
        format!(
            "getrlimit(RLIMIT_NOFILE): {}",
            std::io::Error::last_os_error()
        )
    })?;
    let cap = per_process_ceiling().map_or(NOFILE_CAP, |c| c.min(NOFILE_CAP));
    let soft = target_soft(before.soft, before.hard, cap);
    if soft == before.soft {
        return Ok((before, before));
    }
    let r = libc::rlimit {
        rlim_cur: soft,
        rlim_max: before.hard,
    };
    // SAFETY: `setrlimit` reads one `rlimit` through a pointer to a live local.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) } != 0 {
        return Err(format!(
            "setrlimit(RLIMIT_NOFILE, {soft}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok((before, current().unwrap_or(NofileLimit { soft, ..before })))
}

/// Whether `e` is the process (`EMFILE`) or the system (`ENFILE`) running out of descriptors.
pub fn is_out_of_files(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE))
}

/// `kern.maxfilesperproc`: the most `setrlimit` will grant on macOS.
#[cfg(target_os = "macos")]
fn per_process_ceiling() -> Option<rlim_t> {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated, and `v` and `len` are live locals of the size passed.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&mut v as *mut libc::c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && v > 0).then_some(v as rlim_t)
}

/// Elsewhere the hard limit is the ceiling.
#[cfg(not(target_os = "macos"))]
fn per_process_ceiling() -> Option<rlim_t> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launchd_default_is_raised_to_the_cap_under_an_unlimited_hard_limit() {
        assert_eq!(
            target_soft(256, libc::RLIM_INFINITY, NOFILE_CAP),
            NOFILE_CAP
        );
    }

    #[test]
    fn a_hard_limit_below_the_cap_is_the_target() {
        assert_eq!(target_soft(256, 4096, NOFILE_CAP), 4096);
    }

    #[test]
    fn a_lower_per_process_ceiling_wins_over_the_cap() {
        assert_eq!(target_soft(256, libc::RLIM_INFINITY, 2048), 2048);
    }

    #[test]
    fn a_soft_limit_is_never_lowered() {
        assert_eq!(
            target_soft(100_000, libc::RLIM_INFINITY, NOFILE_CAP),
            100_000
        );
        assert_eq!(target_soft(4096, 4096, NOFILE_CAP), 4096);
    }

    #[test]
    fn the_cap_leaves_room_for_the_minimum_concurrent_turns() {
        assert!(turn_headroom(NOFILE_CAP) >= MIN_CONCURRENT_TURNS);
        assert!(
            turn_headroom(256) < MIN_CONCURRENT_TURNS,
            "the launchd default is what the warning is for"
        );
    }

    #[test]
    fn unlimited_is_labelled() {
        assert_eq!(label(libc::RLIM_INFINITY), "unlimited");
        assert_eq!(label(256), "256");
    }

    #[test]
    fn emfile_and_enfile_are_out_of_files_and_nothing_else_is() {
        assert!(is_out_of_files(&std::io::Error::from_raw_os_error(
            libc::EMFILE
        )));
        assert!(is_out_of_files(&std::io::Error::from_raw_os_error(
            libc::ENFILE
        )));
        assert!(!is_out_of_files(&std::io::Error::from_raw_os_error(
            libc::EADDRINUSE
        )));
        assert!(!is_out_of_files(&std::io::Error::other("x")));
    }
}
