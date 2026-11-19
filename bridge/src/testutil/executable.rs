//! Writing a script a test is about to spawn, so the spawn cannot fail with ETXTBSY.
//!
//! ONE FILE, TWO KINDS OF TEST. The crate's unit tests reach it as `crate::testutil::
//! write_executable`; every integration test target that writes a fake binary includes this
//! same file with `#[path]` (directly, or through `tests/common`). There is deliberately no
//! second copy: the rule below is the kind that is right in one copy and wrong in the other.
//!
//! THE RACE. Linux refuses to exec a file that any process holds open for writing ("Text file
//! busy"). Writing a script in the test process opens a write descriptor on it. Rust opens it
//! close-on-exec, but close-on-exec acts only AT exec: when another test thread forks (any
//! `Command::spawn`, and the test binary runs tests in parallel) while that descriptor is open,
//! the forked child holds a copy of it until it execs its own program. Closing ours first does
//! not help, and neither does writing elsewhere and renaming: the copy is on the same inode. A
//! spawn of the script inside that window fails with ETXTBSY. That is how
//! `codex_session_home::a_turn_cancelled_mid_flight_can_be_retried` failed in ci.yml.
//!
//! THE FIX: this process never opens the executable for writing. The body goes to a staging
//! file that is never executed; a `cp` CHILD copies it to a temporary name beside the target,
//! so the only write descriptor that inode ever has belongs to a process that has exited by the
//! time `status()` returns; a chmod (no descriptor) and a rename put it in place. A target that
//! already exists is replaced, never written into.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static STAGED: AtomicU64 = AtomicU64::new(0);

/// Write `body` to `path` as a mode 0755 executable, safe to spawn the moment this returns
/// however many other threads are spawning children. Returns `path`.
pub fn write_executable(path: &Path, body: impl AsRef<[u8]>) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let name = path
        .file_name()
        .unwrap_or_else(|| panic!("write_executable: {} has no file name", path.display()))
        .to_string_lossy()
        .into_owned();
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let n = STAGED.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let staged = dir.join(format!(".{name}.body-{pid}-{n}"));
    let placed = dir.join(format!(".{name}.exec-{pid}-{n}"));

    std::fs::write(&staged, body)
        .unwrap_or_else(|e| panic!("write_executable: write {}: {e}", staged.display()));
    let copied = std::process::Command::new("/bin/cp")
        .arg(&staged)
        .arg(&placed)
        .status()
        .unwrap_or_else(|e| panic!("write_executable: run /bin/cp: {e}"));
    let _ = std::fs::remove_file(&staged);
    assert!(
        copied.success(),
        "write_executable: /bin/cp {} failed: {copied}",
        placed.display()
    );
    std::fs::set_permissions(&placed, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("write_executable: chmod {}: {e}", placed.display()));
    std::fs::rename(&placed, path)
        .unwrap_or_else(|e| panic!("write_executable: rename to {}: {e}", path.display()));
    path.to_path_buf()
}
