//! Fake apps for the process tests: a uniquely-named symlink to `/bin/sh`.
//!
//! The `/proc` scan matches on `argv[0]`, so each test needs a program running
//! under a name nobody else uses. The symlink used to point at `sleep`, `echo`
//! or `true` — which works with GNU coreutils and fails on Ubuntu 26.04
//! runners, where coreutils is the uutils multicall binary
//! (`/usr/lib/cargo/bin/coreutils/…`): it reads its own `argv[0]`, finds no
//! utility called `hangar-…exe`, and exits with "Security violation: Requested
//! utility … does not match executable name". The shell does not care what it
//! is called, so the fake app is always `/bin/sh` and its behaviour is the
//! `-c` script.
//!
//! A plain `sh -c 'sleep 60'` would not do: the shell exec-optimises a lone
//! command into a bare `sleep`, and the unique name is gone from `/proc`. The
//! scripts below keep the shell itself as the long-lived process.

use std::path::{Path, PathBuf};

/// Runs until signalled. `sleep` goes to the background and the shell waits
/// on it, so the process with the unique `argv[0]` is the shell; the trap
/// takes the `sleep` down with it on SIGTERM. Its stdio is detached so an
/// orphan left by a SIGKILL cannot hold the test runner's output open.
pub(crate) const LONG_RUNNING: [&str; 2] = [
    "-c",
    "trap 'kill $! 2>/dev/null; exit 143' TERM; sleep 60 </dev/null >/dev/null 2>&1 & wait",
];

/// Exits 0 at once and prints nothing.
pub(crate) const SILENT_EXIT: [&str; 2] = ["-c", ":"];

/// A uniquely-named symlink to `/bin/sh` at `dir/name`.
///
/// A symlink, not a copy: copying then immediately executing races with
/// `fork`, since a descriptor still open for writing in any thread makes the
/// exec fail with ETXTBSY. `argv[0]` is whatever path the caller passed, so the
/// unique name still reaches `/proc`.
pub(crate) fn fake_app(dir: &Path, name: &str) -> PathBuf {
    let exe = dir.join(name);
    std::os::unix::fs::symlink("/bin/sh", &exe).expect("symlink to /bin/sh");
    exe
}
