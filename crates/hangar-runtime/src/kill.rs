//! Stopping an app, including ones this process never started.
//!
//! Per-app kill is a real requirement rather than a convenience. A companion
//! app whose window is closed can be completely unreachable: Project Airports
//! overrides Electron's quit-on-last-window-closed to do nothing, its tray icon
//! never appears under KDE/Wayland with Wine, and Electron's `second-instance`
//! focus message does not cross the Wine boundary. No window, no tray, no
//! focus IPC. Killing is the only way back.
//!
//! And by definition the process most in need of killing is the one that was
//! lost track of, so this works from a `/proc` scan and never needs a handle.

use crate::observe::{self, Found, Identity};
use crate::proc::{self, AppProcesses};
use crate::respawn::{Target, Tracker};
use nix::sys::signal::{kill as send_signal, Signal};
use nix::unistd::Pid;
use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long to let a process exit gracefully before insisting.
pub const GRACE: Duration = Duration::from_secs(5);

/// What stopping an app did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// It exited after a Windows close request, as if its window were closed.
    Closed {
        pid: u32,
        took: Duration,
    },
    /// It exited after `SIGTERM`.
    Terminated {
        pid: u32,
        took: Duration,
    },
    /// It ignored `SIGTERM` and needed `SIGKILL`.
    Killed {
        pid: u32,
    },
    /// It was not running.
    NotRunning,
    Failed {
        pid: u32,
        reason: String,
    },
    /// Nothing was sent: which process is meant could not be confirmed (a
    /// recycled pid, several copies, an app away updating).
    Refused {
        pid: Option<u32>,
        reason: String,
    },
}

impl KillOutcome {
    pub fn is_stopped(&self) -> bool {
        matches!(
            self,
            KillOutcome::Closed { .. }
                | KillOutcome::Terminated { .. }
                | KillOutcome::Killed { .. }
                | KillOutcome::NotRunning
        )
    }

    pub fn summary(&self) -> String {
        match self {
            KillOutcome::Closed { pid, took } => {
                format!("closed {pid} after {:.1}s", took.as_secs_f32())
            }
            KillOutcome::Terminated { pid, took } => {
                format!("stopped {pid} after {:.1}s", took.as_secs_f32())
            }
            KillOutcome::Killed { pid } => format!("force-killed {pid}"),
            KillOutcome::NotRunning => "was not running".into(),
            KillOutcome::Failed { pid, reason } => format!("could not stop {pid}: {reason}"),
            KillOutcome::Refused {
                pid: Some(pid),
                reason,
            } => {
                format!("did not stop {pid}: {reason}")
            }
            KillOutcome::Refused { pid: None, reason } => format!("did not stop it: {reason}"),
        }
    }
}

/// Stop the app matching `needle`, whoever started it.
///
/// Only the main process is signalled: the GPU, network and renderer children
/// plus the `xalia` helpers wind down on their own once it exits, so no
/// process-group handling is needed.
///
/// Resolved by identity, not by key: exactly one main instance whose `argv[0]`
/// is `needle` is stopped, and only while its pid still carries the same start
/// time. Several copies are refused, never the lowest pid guessed. To reach
/// the one a launch started, use [`stop_app`] with its tracker.
pub async fn kill_app(needle: &str) -> KillOutcome {
    stop_app(None, needle, None).await
}

/// Stop one PID: `SIGTERM`, wait, then `SIGKILL` if it is still there.
pub async fn kill_pid(pid: u32, grace: Duration) -> KillOutcome {
    let target = Pid::from_raw(pid as i32);

    match send_signal(target, Signal::SIGTERM) {
        Ok(()) => {}
        // Already gone between the scan and the signal.
        Err(nix::errno::Errno::ESRCH) => return KillOutcome::NotRunning,
        Err(e) => {
            return KillOutcome::Failed {
                pid,
                reason: format!("SIGTERM: {e}"),
            };
        }
    }

    // SIGTERM was enough for every case observed; the escalation below exists
    // because "enough so far" is not a guarantee.
    let started = std::time::Instant::now();
    while started.elapsed() < grace {
        // `is_alive`, not `pid_exists`: a child we signalled becomes a zombie
        // until it is reaped, and its /proc entry outlives it. Waiting on the
        // directory to vanish escalates to SIGKILL on a process that already
        // exited cleanly.
        if !proc::is_alive(pid) {
            return KillOutcome::Terminated {
                pid,
                took: started.elapsed(),
            };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    match send_signal(target, Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => KillOutcome::Killed { pid },
        Err(e) => KillOutcome::Failed {
            pid,
            reason: format!("SIGKILL: {e}"),
        },
    }
}

/// How long a Windows close request may take to answer, and then how long the
/// app gets to exit after it, before falling back to `SIGTERM`.
pub const CLOSE_WAIT: Duration = Duration::from_secs(10);

/// Image names a close request must never name: the prefix's own machinery.
const NEVER_CLOSE: [&str; 7] = [
    "steam.exe",
    "wineserver.exe",
    "services.exe",
    "explorer.exe",
    "taskkill.exe",
    "cmd.exe",
    "start.exe",
];

/// The `taskkill` arguments for a Windows close request, or `None` when one
/// must not be sent: a key that is not one plain `.exe` name, one of the
/// prefix's own processes, or anything but exactly one instance in charge
/// (`/IM` selects by name, so with two it could close the wrong one).
///
/// `/IM` exact, without `/F` or `/T`: the request is the window's own close,
/// the same thing as a person clicking its close button.
pub fn close_request_args(key: &str, instances: usize) -> Option<Vec<std::ffi::OsString>> {
    let plain = key.to_ascii_lowercase().ends_with(".exe")
        && !key.contains(['*', '?', '\\', '/'])
        && !NEVER_CLOSE.iter().any(|n| key.eq_ignore_ascii_case(n))
        && !key.eq_ignore_ascii_case(crate::sim::MSFS_2024_EXE);
    (plain && instances == 1).then(|| {
        vec![
            r"C:\windows\system32\taskkill.exe".into(),
            "/IM".into(),
            key.into(),
        ]
    })
}

/// Stop a Windows app in a companion prefix: a close request first, then the
/// signal path if it is still open.
///
/// `SIGTERM` ends a Wine process without its window ever being asked to
/// close, so Little Navmap reports "did not exit properly" on its next start.
/// A close request lets it save and exit; an app that ignores one (Project
/// Airports hides to a tray it cannot show) still stops, [`CLOSE_WAIT`] later.
///
/// Resolved by identity as [`kill_app`] is; [`stop_app`] reaches a tracked launch.
pub async fn close_then_kill(env: &crate::ProtonEnv, key: &str) -> KillOutcome {
    stop_app(Some(env), key, None).await
}

// ── Stopping by identity ─────────────────────────────────────────────────

/// Stop one app by identity: the instance `followed` tracks (following its
/// update restart), or with no tracker the one main instance of `key`.
///
/// With `env`, a Windows close request goes first ([`close_request_args`]:
/// one `/IM`, no `/F`, no `/T`, only when exactly one instance has that
/// `argv[0]`), then the signal path. Without it, the signal path alone.
///
/// Every signal is preceded by a fresh read of the pid's start time and
/// `argv[0]`. A pid that now names another process is refused, never
/// signalled; several copies or an app away updating are refused too, and the
/// outcome says so ([`KillOutcome::Refused`]).
pub async fn stop_app(
    env: Option<&crate::ProtonEnv>,
    key: &str,
    followed: Option<&Tracker>,
) -> KillOutcome {
    let observation = observe::observe(Path::new(observe::PROC), &[key.to_owned()]);
    let target = resolve_in(
        observation.found(key),
        followed,
        Instant::now(),
        env.map(|env| env.wineprefix.as_path()),
    );
    let host = Live {
        root: Path::new(observe::PROC),
        env,
    };
    stop_target(&host, key, target, env.is_some(), &Timing::LIVE).await
}

/// Which process a Stop means, from one observation of `key`.
///
/// A tracked launch decides through [`Tracker::target`]. Without one, exactly
/// one main instance is the target; several are [`Target::Ambiguous`].
pub fn resolve(found: Option<&Found>, followed: Option<&Tracker>, now: Instant) -> Target {
    resolve_in(found, followed, now, None)
}

/// [`resolve`], knowing the row's own prefix.
///
/// Only an untracked Stop with several copies uses it (decided 2026-11-02, A2):
/// exactly one copy in `prefix` is the target, and copies elsewhere are left
/// alone. A lone copy is the target wherever it runs, so a `WINEPREFIX` that
/// reads differently from the row's never turns a running app into "not
/// running".
pub fn resolve_in(
    found: Option<&Found>,
    followed: Option<&Tracker>,
    now: Instant,
    prefix: Option<&Path>,
) -> Target {
    if let Some(tracker) = followed {
        return tracker.target(found, now);
    }
    let mains: Vec<&observe::Process> = found.map(|f| f.mains().collect()).unwrap_or_default();
    match mains[..] {
        [] => Target::NotRunning,
        [only] => Target::Instance(only.identity),
        ref many => {
            let ours: Vec<Identity> = prefix
                .map(observe::canonical)
                .map(|prefix| {
                    many.iter()
                        .filter(|p| matches!(&p.prefix, observe::Fact::Known(at) if *at == prefix))
                        .map(|p| p.identity)
                        .collect()
                })
                .unwrap_or_default();
            match ours[..] {
                [only] => Target::Instance(only),
                _ => Target::Ambiguous(many.len()),
            }
        }
    }
}

/// What a pid is now, compared with the process it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The same process: same start time, `argv[0]` still the key.
    Same,
    /// Exited (a zombie counts), or unreadable.
    Gone,
    /// The pid is alive but is another process: recycled, or exec'd away.
    Other,
}

/// Read `pid` under `root` and compare it with `identity`.
pub fn probe(root: &Path, key: &str, identity: Identity) -> Probe {
    let Some(now) = observe::read_identity(root, identity.pid) else {
        return Probe::Gone;
    };
    if now.start != identity.start {
        return Probe::Other;
    }
    let raw = std::fs::read(root.join(identity.pid.to_string()).join("cmdline"));
    let runs_key = raw.is_ok_and(|raw| {
        proc::split_cmdline(&raw).first().is_some_and(|argv0| {
            argv0
                .trim_matches('"')
                .rsplit(['/', '\\'])
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(key))
        })
    });
    if runs_key {
        Probe::Same
    } else {
        Probe::Other
    }
}

/// How long each wait is. Tests shrink these; live code uses [`Timing::LIVE`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub close_wait: Duration,
    pub grace: Duration,
    pub poll: Duration,
}

impl Timing {
    pub const LIVE: Self = Self {
        close_wait: CLOSE_WAIT,
        grace: GRACE,
        poll: Duration::from_millis(100),
    };
}

/// Everything the stop path does to the world, so a test can stand in for it.
pub(crate) trait Host {
    fn probe(&self, key: &str, identity: Identity) -> Probe;
    /// Main instances whose `argv[0]` is exactly `key`: what `/IM` would select.
    fn instances(&self, key: &str) -> usize;
    fn signal(&self, pid: u32, signal: Signal) -> nix::Result<()>;
    /// Send the Windows close request; returns what came back, for the log.
    fn close_request(&self, args: Vec<OsString>) -> impl Future<Output = String> + Send;
}

/// The real process table, real signals and Wine's `taskkill`.
pub(crate) struct Live<'a> {
    pub root: &'a Path,
    pub env: Option<&'a crate::ProtonEnv>,
}

impl Host for Live<'_> {
    fn probe(&self, key: &str, identity: Identity) -> Probe {
        probe(self.root, key, identity)
    }

    fn instances(&self, key: &str) -> usize {
        observe::observe(self.root, &[key.to_owned()])
            .found(key)
            .map_or(0, |f| f.mains().count())
    }

    fn signal(&self, pid: u32, signal: Signal) -> nix::Result<()> {
        send_signal(Pid::from_raw(pid as i32), signal)
    }

    fn close_request(&self, args: Vec<OsString>) -> impl Future<Output = String> + Send {
        let command = self.env.map(|env| {
            let (program, mut invocation) = env.invocation();
            invocation.extend(args);
            let mut command = tokio::process::Command::new(program);
            command
                .args(invocation)
                .envs(env.vars())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            command
        });
        async move {
            match command {
                Some(mut command) => {
                    format!(
                        "{:?}",
                        tokio::time::timeout(CLOSE_WAIT, command.status()).await
                    )
                }
                None => "no prefix to send it in".to_owned(),
            }
        }
    }
}

/// Turn a resolution that names no process into the outcome that says why.
fn unresolved(target: Target) -> Result<Identity, KillOutcome> {
    match target {
        Target::Instance(identity) => Ok(identity),
        Target::NotRunning => Err(KillOutcome::NotRunning),
        Target::Updating { was } => Err(KillOutcome::Refused {
            pid: Some(was.pid),
            reason: "it is restarting after an update; Stop again once it is back".into(),
        }),
        Target::Ambiguous(n) => Err(KillOutcome::Refused {
            pid: None,
            reason: format!(
                "{n} copies are running and none is known to be this one; close it by hand"
            ),
        }),
    }
}

/// Stop the resolved process: close request (when `close`), then signals.
pub(crate) async fn stop_target<H: Host>(
    host: &H,
    key: &str,
    target: Target,
    close: bool,
    timing: &Timing,
) -> KillOutcome {
    let identity = match unresolved(target) {
        Ok(identity) => identity,
        Err(outcome) => return outcome,
    };
    let pid = identity.pid;
    match host.probe(key, identity) {
        Probe::Same => {}
        Probe::Gone => return KillOutcome::NotRunning,
        Probe::Other => return recycled(pid),
    }
    if close {
        if let Some(args) = close_request_args(key, host.instances(key)) {
            let started = Instant::now();
            let asked = host.close_request(args).await;
            tracing::info!(target: "hangar_launch", app = %key, pid, start = identity.start, result = %asked, "windows close request");
            while started.elapsed() < timing.close_wait * 2 {
                // Gone, or the pid already belongs to someone else: it closed.
                if host.probe(key, identity) != Probe::Same {
                    return KillOutcome::Closed {
                        pid,
                        took: started.elapsed(),
                    };
                }
                tokio::time::sleep(timing.poll).await;
            }
        }
    }
    signal_target(host, key, identity, timing).await
}

fn recycled(pid: u32) -> KillOutcome {
    KillOutcome::Refused {
        pid: Some(pid),
        reason: "that pid is now a different process (start time or executable changed); \
                 nothing was sent"
            .into(),
    }
}

/// `SIGTERM`, wait, then `SIGKILL` — each signal only after the pid is
/// confirmed to be the same process.
async fn signal_target<H: Host>(
    host: &H,
    key: &str,
    identity: Identity,
    timing: &Timing,
) -> KillOutcome {
    let pid = identity.pid;
    match host.probe(key, identity) {
        Probe::Same => {}
        Probe::Gone => return KillOutcome::NotRunning,
        Probe::Other => return recycled(pid),
    }
    match host.signal(pid, Signal::SIGTERM) {
        Ok(()) => {}
        Err(nix::errno::Errno::ESRCH) => return KillOutcome::NotRunning,
        Err(e) => {
            return KillOutcome::Failed {
                pid,
                reason: format!("SIGTERM: {e}"),
            }
        }
    }
    let started = Instant::now();
    while started.elapsed() < timing.grace {
        if host.probe(key, identity) != Probe::Same {
            return KillOutcome::Terminated {
                pid,
                took: started.elapsed(),
            };
        }
        tokio::time::sleep(timing.poll).await;
    }
    if host.probe(key, identity) != Probe::Same {
        return KillOutcome::Terminated {
            pid,
            took: started.elapsed(),
        };
    }
    match host.signal(pid, Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => KillOutcome::Killed { pid },
        Err(e) => KillOutcome::Failed {
            pid,
            reason: format!("SIGKILL: {e}"),
        },
    }
}

/// Stop the resolved process with signals only, then wait until it is gone:
/// the first half of a restart. `Err` carries what to tell the person.
pub(crate) async fn stop_for_restart<H: Host>(
    host: &H,
    key: &str,
    target: Target,
    timing: &Timing,
    wait: Duration,
) -> Result<(), String> {
    let identity = match target {
        Target::NotRunning => return Ok(()),
        Target::Instance(identity) => identity,
        other => {
            return Err(format!(
                "could not stop it first: {}",
                stop_target(host, key, other, false, timing).await.summary()
            ))
        }
    };
    let stopped = stop_target(host, key, target, false, timing).await;
    if !stopped.is_stopped() {
        return Err(format!("could not stop it first: {}", stopped.summary()));
    }
    // Wait for *that* process to be gone, not for the key to vanish: another
    // copy with the same exe may be running and is not ours to wait on.
    let started = Instant::now();
    while started.elapsed() < wait {
        if host.probe(key, identity) != Probe::Same {
            return Ok(());
        }
        tokio::time::sleep(timing.poll).await;
    }
    Err("it did not exit, so it was not restarted".into())
}

/// Whether an app appears to be running, without touching it.
pub fn is_running(needle: &str) -> AppProcesses {
    proc::find_app(needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// Start a process that ignores nothing and lives until killed.
    fn spawn_sleeper() -> std::process::Child {
        Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("sleep is available")
    }

    #[test]
    fn a_close_request_names_one_plain_exe_and_never_forces() {
        let args = close_request_args("littlenavmap.exe", 1).expect("one instance");
        let args: Vec<_> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                r"C:\windows\system32\taskkill.exe",
                "/IM",
                "littlenavmap.exe"
            ]
        );
        assert!(!args
            .iter()
            .any(|a| a.eq_ignore_ascii_case("/F") || a.eq_ignore_ascii_case("/T")));

        assert!(
            close_request_args("littlenavmap.exe", 2).is_none(),
            "ambiguous"
        );
        assert!(close_request_args("littlenavmap.exe", 0).is_none());
        for key in [
            "explorer.exe",
            "Steam.exe",
            "FlightSimulator2024.exe",
            "*.exe",
            "lnm",
            r"C:\x\a.exe",
        ] {
            assert!(close_request_args(key, 1).is_none(), "{key}");
        }
    }

    #[tokio::test]
    async fn terminates_a_process_it_never_spawned() {
        // Spawned through std, not tokio, and never awaited — as far as the
        // kill path is concerned this is somebody else's process.
        let mut child = spawn_sleeper();
        let pid = child.id();
        assert!(proc::pid_exists(pid));

        let outcome = kill_pid(pid, GRACE).await;
        assert!(
            matches!(outcome, KillOutcome::Terminated { .. }),
            "SIGTERM should have been enough: {outcome:?}"
        );
        assert!(outcome.is_stopped());
        assert!(!proc::is_alive(pid), "it is a zombie at most, not running");

        let _ = child.wait();
        assert!(!proc::pid_exists(pid), "and gone entirely once reaped");
    }

    #[tokio::test]
    async fn killing_something_already_gone_is_not_an_error() {
        let mut child = spawn_sleeper();
        let pid = child.id();
        let _ = child.kill();
        let _ = child.wait();

        let outcome = kill_pid(pid, Duration::from_millis(200)).await;
        assert!(outcome.is_stopped(), "{outcome:?}");
    }

    /// The whole point, end to end: find an app by the file name of its
    /// executable, then stop it — without ever having spawned it ourselves.
    ///
    /// The fixture is a real binary under a unique name, so the test drives
    /// the same match key the app uses. A marker in the arguments does not
    /// work: `sleep 300 marker` makes GNU sleep try to sum the arguments and
    /// exit, and `sh -c 'sleep 300 # marker'` is exec-optimised by the shell
    /// into a bare `sleep`, dropping the comment before it reaches `/proc`.
    #[tokio::test]
    async fn finds_and_stops_an_app_by_its_executable_name() {
        let dir = std::env::temp_dir().join(format!("hangar-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(format!("hangar-test-app-{}.exe", std::process::id()));
        // A symlink, not a copy. Copying then immediately executing races with
        // `fork`: a descriptor still open for writing in any thread makes the
        // exec fail with ETXTBSY, which is easy to hit when tests run in
        // parallel. `argv[0]` is whatever path the caller passed, so the unique
        // name still reaches /proc.
        let target = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("sleep is available");
        std::os::unix::fs::symlink(target, &exe).unwrap();

        let mut child = Command::new(&exe)
            .arg("60")
            .spawn()
            .expect("the copied binary runs");

        // The needle is derived exactly as the app derives it.
        let needle = proc::match_key(&exe);
        assert!(needle.starts_with("hangar-test-app-"));

        for _ in 0..200 {
            if is_running(&needle).is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let found = is_running(&needle);
        assert!(found.is_running(), "should be found by its executable name");
        assert_eq!(found.main_pid(), Some(child.id()));

        let outcome = kill_app(&needle).await;
        assert!(
            matches!(outcome, KillOutcome::Terminated { .. }),
            "{outcome:?}"
        );

        let _ = child.wait();
        assert!(!is_running(&needle).is_running(), "and it is gone");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stopping_something_that_is_not_running_says_so() {
        let outcome = kill_app("hangar-no-such-process-zzzz").await;
        assert_eq!(outcome, KillOutcome::NotRunning);
        assert!(outcome.is_stopped());
    }

    #[test]
    fn outcomes_describe_themselves() {
        assert!(KillOutcome::Terminated {
            pid: 1,
            took: Duration::from_millis(1500)
        }
        .summary()
        .contains("1.5s"));
        assert!(KillOutcome::Killed { pid: 2 }.summary().contains("force"));
        assert!(!KillOutcome::Failed {
            pid: 3,
            reason: "denied".into()
        }
        .is_stopped());
    }
}

/// Stop by identity, against an in-memory process table. Nothing here reads
/// the real `/proc`, sends a signal or starts a process.
#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::observe::{Fact, Namespace, Process};
    use crate::respawn::{Tracker, RESPAWN_WINDOW};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    const PA: &str = "Project Airports.exe";

    /// One process in the fake table.
    #[derive(Clone)]
    struct Row {
        start: u64,
        argv0: String,
        alive: bool,
        helper: bool,
    }

    /// A process table that exits a process on `SIGTERM`/`SIGKILL` (unless
    /// told to ignore `SIGTERM`) and on a close request (unless told to
    /// ignore those), and records everything sent.
    #[derive(Default)]
    struct Fake {
        table: Mutex<BTreeMap<u32, Row>>,
        signals: Mutex<Vec<(u32, Signal)>>,
        closes: Mutex<Vec<Vec<String>>>,
        ignores_close: bool,
        ignores_term: bool,
    }

    impl Fake {
        fn with(rows: &[(u32, u64, &str)]) -> Self {
            let fake = Self::default();
            for (pid, start, argv0) in rows {
                fake.add(*pid, *start, argv0, false);
            }
            fake
        }

        fn add(&self, pid: u32, start: u64, argv0: &str, helper: bool) {
            self.table.lock().unwrap().insert(
                pid,
                Row {
                    start,
                    argv0: argv0.into(),
                    alive: true,
                    helper,
                },
            );
        }

        fn alive(&self, pid: u32) -> bool {
            self.table
                .lock()
                .unwrap()
                .get(&pid)
                .is_some_and(|r| r.alive)
        }

        fn signalled(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().unwrap().clone()
        }

        fn base(path: &str) -> &str {
            path.rsplit(['/', '\\']).next().unwrap_or(path)
        }
    }

    impl Host for Fake {
        fn probe(&self, key: &str, identity: Identity) -> Probe {
            match self.table.lock().unwrap().get(&identity.pid) {
                Some(r) if r.alive && r.start == identity.start => {
                    if Self::base(&r.argv0).eq_ignore_ascii_case(key) {
                        Probe::Same
                    } else {
                        Probe::Other
                    }
                }
                Some(r) if r.alive => Probe::Other,
                _ => Probe::Gone,
            }
        }

        fn instances(&self, key: &str) -> usize {
            self.table
                .lock()
                .unwrap()
                .values()
                .filter(|r| r.alive && !r.helper && Self::base(&r.argv0).eq_ignore_ascii_case(key))
                .count()
        }

        fn signal(&self, pid: u32, signal: Signal) -> nix::Result<()> {
            self.signals.lock().unwrap().push((pid, signal));
            let mut table = self.table.lock().unwrap();
            match table.get_mut(&pid) {
                Some(r) if r.alive => {
                    if signal == Signal::SIGKILL || !self.ignores_term {
                        r.alive = false;
                    }
                    Ok(())
                }
                _ => Err(nix::errno::Errno::ESRCH),
            }
        }

        fn close_request(&self, args: Vec<OsString>) -> impl Future<Output = String> + Send {
            let args: Vec<String> = args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            if !self.ignores_close {
                // `/IM` closes every process with that image name.
                let name = args.last().cloned().unwrap_or_default();
                for r in self.table.lock().unwrap().values_mut() {
                    if Self::base(&r.argv0).eq_ignore_ascii_case(&name) {
                        r.alive = false;
                    }
                }
            }
            self.closes.lock().unwrap().push(args);
            async { "sent".to_owned() }
        }
    }

    const FAST: Timing = Timing {
        close_wait: Duration::from_millis(20),
        grace: Duration::from_millis(20),
        poll: Duration::from_millis(1),
    };

    fn id(pid: u32, start: u64) -> Identity {
        Identity { pid, start }
    }

    fn ns(ino: u64) -> Fact<Namespace> {
        Fact::Known(Namespace::fixture(ino))
    }

    /// A main process as `observe` would report it.
    fn process(pid: u32, start: u64, prefix: &str, user: u64) -> Process {
        Process {
            identity: id(pid, start),
            helper: false,
            prefix: Fact::Known(PathBuf::from(prefix)),
            user_ns: ns(user),
            mount_ns: ns(user + 1),
        }
    }

    fn found(ps: &[Process]) -> Found {
        Found {
            exact: ps.to_vec(),
            similar: vec![],
        }
    }

    /// (a) Project Airports updated itself between two polls: the tracker
    /// still names the old pid, which the kernel has since handed to an
    /// unrelated process. A second copy runs in another prefix. Stop reaches
    /// the restart and nothing else.
    #[tokio::test]
    async fn stop_after_a_self_update_targets_the_respawned_process() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(PA, t0);
        let old = process(30051, 900, "/companion/pfx", 100);
        tracker.observe(Some(&found(std::slice::from_ref(&old))), t0);
        assert_eq!(tracker.current(), Some(old.identity));

        // Four seconds later, before the next poll.
        let now = t0 + Duration::from_secs(27);
        let restart = process(30402, 950, "/companion/pfx", 100);
        let stranger = process(31000, 960, "/other/pfx", 100);
        let seen = found(&[restart.clone(), stranger.clone()]);
        assert_eq!(
            resolve(Some(&seen), Some(&tracker), now),
            Target::Instance(restart.identity)
        );

        let host = Fake::with(&[
            (30051, 990, "/usr/bin/bash"), // the old pid, recycled
            (30402, 950, PA),
            (31000, 960, PA),
        ]);
        let outcome = stop_target(
            &host,
            PA,
            resolve(Some(&seen), Some(&tracker), now),
            true,
            &FAST,
        )
        .await;
        assert_eq!(
            outcome,
            KillOutcome::Terminated {
                pid: 30402,
                took: match outcome {
                    KillOutcome::Terminated { took, .. } => took,
                    _ => unreachable!("{outcome:?}"),
                }
            }
        );
        // Two copies by name: no `/IM` close request, which would hit both.
        assert!(host.closes.lock().unwrap().is_empty());
        assert_eq!(host.signalled(), vec![(30402, Signal::SIGTERM)]);
        assert!(host.alive(30051), "the stale pid's new owner is untouched");
        assert!(
            host.alive(31000),
            "the stranger with the same exe is untouched"
        );
    }

    /// (a, continued) The tracker already saw the exit and is waiting: the
    /// restart is the target once it is there, and before that Stop refuses
    /// rather than reaching for anything with the same name.
    #[test]
    fn a_tracker_waiting_for_its_update_reaches_only_the_restart() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new("BeyondATC.exe", t0);
        let old = process(38377, 1, "/sim/pfx", 7);
        tracker.observe(Some(&found(std::slice::from_ref(&old))), t0);
        let gone = t0 + Duration::from_secs(30);
        tracker.observe(Some(&found(&[])), gone);

        // Away: only a host-side copy in another namespace is running.
        let host_copy = process(500, 2, "/sim/pfx", 50);
        assert_eq!(
            tracker.target(Some(&found(std::slice::from_ref(&host_copy))), gone),
            Target::Updating { was: old.identity }
        );
        let back = process(39513, 3, "/sim/pfx", 7);
        assert_eq!(
            tracker.target(Some(&found(&[host_copy.clone(), back.clone()])), gone),
            Target::Instance(back.identity)
        );
        // Two restarts in the launch's own session: never treated as one.
        let twin = process(39600, 4, "/sim/pfx", 7);
        assert_eq!(
            tracker.target(Some(&found(&[back, twin])), gone),
            Target::Ambiguous(2)
        );
    }

    /// After the window, or after HangarRS asked it to close, the launch is
    /// over: a later same-exe process is a stranger and Stop leaves it alone.
    #[tokio::test]
    async fn a_finished_launch_never_reaches_a_later_copy() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(PA, t0);
        let old = process(1, 10, "/companion/pfx", 7);
        tracker.observe(Some(&found(std::slice::from_ref(&old))), t0);
        let later = process(2, 20, "/companion/pfx", 7);
        let seen = found(std::slice::from_ref(&later));
        assert_eq!(
            tracker.target(Some(&seen), t0 + RESPAWN_WINDOW),
            Target::NotRunning
        );
        let mut closing = tracker.clone();
        closing.closing(t0 + Duration::from_secs(5));
        assert_eq!(
            closing.target(Some(&seen), t0 + Duration::from_secs(6)),
            Target::NotRunning
        );

        let host = Fake::with(&[(2, 20, PA)]);
        let outcome = stop_target(
            &host,
            PA,
            tracker.target(Some(&seen), t0 + RESPAWN_WINDOW),
            true,
            &FAST,
        )
        .await;
        assert_eq!(outcome, KillOutcome::NotRunning);
        assert!(host.signalled().is_empty() && host.closes.lock().unwrap().is_empty());
    }

    /// (b) Two copies of one exe, one started by hand before HangarRS
    /// launched its own. Stop of the launched one signals only that one.
    #[tokio::test]
    async fn two_instances_stop_touches_only_the_launched_one() {
        let t0 = Instant::now();
        let mine = process(4100, 41, "/companion/pfx", 7);
        let theirs = process(4000, 40, "/companion/pfx", 7);
        let both = found(&[theirs.clone(), mine.clone()]);

        // Without the launch's pid, two first candidates are never one.
        let mut blind = Tracker::new(PA, t0);
        blind.observe(Some(&both), t0);
        assert_eq!(blind.target(Some(&both), t0), Target::Ambiguous(2));

        // With it, the launch is followed from the first observation.
        let mut tracker = Tracker::new(PA, t0);
        tracker.launched_as(4100);
        assert_eq!(
            tracker.observe(Some(&both), t0),
            crate::respawn::Event::Seen(mine.identity)
        );
        let target = resolve(Some(&both), Some(&tracker), t0 + Duration::from_secs(60));
        assert_eq!(target, Target::Instance(mine.identity));

        let host = Fake::with(&[(4000, 40, PA), (4100, 41, PA)]);
        let outcome = stop_target(&host, PA, target, true, &FAST).await;
        assert!(
            matches!(outcome, KillOutcome::Terminated { pid: 4100, .. }),
            "{outcome:?}"
        );
        assert!(
            host.closes.lock().unwrap().is_empty(),
            "`/IM` would close both, so no close request"
        );
        assert_eq!(host.signalled(), vec![(4100, Signal::SIGTERM)]);
        assert!(host.alive(4000));
    }

    /// (b) Restart of the launched copy stops only it and waits for *it* to
    /// go, not for every process with the key to vanish.
    #[tokio::test]
    async fn two_instances_restart_stops_only_the_launched_one() {
        let t0 = Instant::now();
        let mine = process(4100, 41, "/companion/pfx", 7);
        let theirs = process(4000, 40, "/companion/pfx", 7);
        let mut tracker = Tracker::new(PA, t0);
        tracker.observe(Some(&found(std::slice::from_ref(&mine))), t0);
        let both = found(&[theirs, mine.clone()]);

        let host = Fake {
            ignores_term: true, // needs SIGKILL, so the whole path runs
            ..Fake::with(&[(4000, 40, PA), (4100, 41, PA)])
        };
        let target = resolve(Some(&both), Some(&tracker), t0);
        assert_eq!(
            stop_for_restart(&host, PA, target, &FAST, Duration::from_millis(50)).await,
            Ok(())
        );
        assert_eq!(
            host.signalled(),
            vec![(4100, Signal::SIGTERM), (4100, Signal::SIGKILL)]
        );
        assert!(host.alive(4000) && !host.alive(4100));
        assert!(
            host.closes.lock().unwrap().is_empty(),
            "restart sends no close request"
        );
    }

    /// (b) With no launch to go by, two copies are refused: nothing is sent,
    /// and the outcome says why instead of picking the lowest pid.
    #[tokio::test]
    async fn two_unfollowed_instances_are_refused_not_guessed() {
        let both = found(&[process(4000, 40, "/p", 7), process(4100, 41, "/p", 7)]);
        let target = resolve(Some(&both), None, Instant::now());
        assert_eq!(target, Target::Ambiguous(2));

        let host = Fake::with(&[(4000, 40, PA), (4100, 41, PA)]);
        let outcome = stop_target(&host, PA, target, true, &FAST).await;
        assert!(
            matches!(outcome, KillOutcome::Refused { pid: None, .. }),
            "{outcome:?}"
        );
        assert!(!outcome.is_stopped());
        assert!(
            outcome.summary().contains("2 copies"),
            "{}",
            outcome.summary()
        );
        assert!(host.signalled().is_empty() && host.closes.lock().unwrap().is_empty());

        let restart = stop_for_restart(&host, PA, target, &FAST, Duration::from_millis(50)).await;
        assert!(restart.unwrap_err().contains("could not stop it first"));
        assert!(host.signalled().is_empty());
    }

    /// Several copies, one in the row's own prefix: that one is the target and
    /// the copy elsewhere is untouched. Two in the row's prefix stay refused,
    /// and a lone copy in another prefix is still found.
    #[test]
    fn untracked_stop_narrows_to_the_rows_prefix() {
        let now = Instant::now();
        let split = found(&[process(4000, 40, "/sim", 7), process(4100, 41, "/ours", 7)]);
        assert_eq!(
            resolve_in(Some(&split), None, now, Some(Path::new("/ours"))),
            Target::Instance(id(4100, 41))
        );
        assert_eq!(
            resolve_in(Some(&split), None, now, None),
            Target::Ambiguous(2)
        );
        let both_ours = found(&[process(4000, 40, "/ours", 7), process(4100, 41, "/ours", 7)]);
        assert_eq!(
            resolve_in(Some(&both_ours), None, now, Some(Path::new("/ours"))),
            Target::Ambiguous(2)
        );
        let elsewhere = found(&[process(4000, 40, "/sim", 7)]);
        assert_eq!(
            resolve_in(Some(&elsewhere), None, now, Some(Path::new("/ours"))),
            Target::Instance(id(4000, 40))
        );
    }

    /// (c) The pid is alive but its start time changed: another process got
    /// the number. Refused before anything is sent.
    #[tokio::test]
    async fn a_recycled_pid_with_a_different_start_time_is_refused() {
        let host = Fake::with(&[(4242, 999, PA)]);
        let outcome = stop_target(&host, PA, Target::Instance(id(4242, 100)), true, &FAST).await;
        assert!(
            matches!(
                outcome,
                KillOutcome::Refused {
                    pid: Some(4242),
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(!outcome.is_stopped());
        assert!(host.signalled().is_empty());
        assert!(host.closes.lock().unwrap().is_empty());
        assert!(host.alive(4242));

        // Same start time, but the pid now runs something else (exec'd).
        let host = Fake::with(&[(4242, 100, "/usr/bin/bash")]);
        let outcome = stop_target(&host, PA, Target::Instance(id(4242, 100)), false, &FAST).await;
        assert!(
            matches!(outcome, KillOutcome::Refused { .. }),
            "{outcome:?}"
        );
        assert!(host.signalled().is_empty());

        // Gone altogether is simply not running.
        let host = Fake::default();
        assert_eq!(
            stop_target(&host, PA, Target::Instance(id(4242, 100)), true, &FAST).await,
            KillOutcome::NotRunning
        );
    }

    /// The close request still goes first for a lone instance, with no
    /// `/F` or `/T`, and an app that honours it is never signalled.
    #[tokio::test]
    async fn a_lone_instance_is_asked_to_close_before_any_signal() {
        let host = Fake::with(&[(7, 70, "C:\\Programs\\littlenavmap.exe")]);
        let outcome = stop_target(
            &host,
            "littlenavmap.exe",
            Target::Instance(id(7, 70)),
            true,
            &FAST,
        )
        .await;
        assert!(
            matches!(outcome, KillOutcome::Closed { pid: 7, .. }),
            "{outcome:?}"
        );
        assert_eq!(
            *host.closes.lock().unwrap(),
            vec![vec![
                r"C:\windows\system32\taskkill.exe".to_owned(),
                "/IM".into(),
                "littlenavmap.exe".into()
            ]]
        );
        assert!(host.signalled().is_empty());

        // Ignored (Project Airports hides to a tray): the signal path follows,
        // still aimed at the same identity.
        let host = Fake {
            ignores_close: true,
            ..Fake::with(&[(8, 80, PA)])
        };
        let outcome = stop_target(&host, PA, Target::Instance(id(8, 80)), true, &FAST).await;
        assert!(
            matches!(outcome, KillOutcome::Terminated { pid: 8, .. }),
            "{outcome:?}"
        );
        assert_eq!(host.closes.lock().unwrap().len(), 1);
        assert_eq!(host.signalled(), vec![(8, Signal::SIGTERM)]);
    }

    /// An app away updating is refused with its old pid named, never matched
    /// by key.
    #[tokio::test]
    async fn stop_while_away_updating_is_refused() {
        let host = Fake::with(&[(9, 90, PA)]);
        let outcome =
            stop_target(&host, PA, Target::Updating { was: id(5, 50) }, true, &FAST).await;
        assert!(
            matches!(outcome, KillOutcome::Refused { pid: Some(5), .. }),
            "{outcome:?}"
        );
        assert!(outcome.summary().contains("restarting after an update"));
        assert!(host.signalled().is_empty() && host.alive(9));
    }

    /// Helpers never count as the instance, followed or not.
    #[test]
    fn a_helper_is_never_the_target() {
        let mut helper = process(11, 1, "/p", 7);
        helper.helper = true;
        let main = process(10, 1, "/p", 7);
        assert_eq!(
            resolve(
                Some(&found(&[main.clone(), helper.clone()])),
                None,
                Instant::now()
            ),
            Target::Instance(main.identity)
        );
        assert_eq!(
            resolve(Some(&found(&[helper])), None, Instant::now()),
            Target::NotRunning
        );
    }

    /// The real reader against a scratch `/proc`: same, recycled, exec'd
    /// away, zombie and missing.
    #[test]
    fn probe_reads_start_time_and_argv0_from_a_scratch_proc() {
        let root = std::env::temp_dir().join(format!("hangar-kill-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |pid: u32, state: &str, start: u64, argv0: &str| {
            let dir = root.join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = state.into();
            fields[19] = start.to_string();
            std::fs::write(
                dir.join("stat"),
                format!("{pid} (a ) name) {}", fields.join(" ")),
            )
            .unwrap();
            std::fs::write(dir.join("cmdline"), format!("{argv0}\0--x\0")).unwrap();
        };
        write(1, "S", 100, r"C:\Programs\Project Airports.exe");
        write(2, "S", 999, PA);
        write(3, "S", 300, "/usr/bin/bash");
        write(4, "Z", 400, PA);

        assert_eq!(probe(&root, PA, id(1, 100)), Probe::Same);
        assert_eq!(probe(&root, PA, id(2, 200)), Probe::Other, "recycled");
        assert_eq!(probe(&root, PA, id(3, 300)), Probe::Other, "exec'd away");
        assert_eq!(probe(&root, PA, id(4, 400)), Probe::Gone, "a zombie");
        assert_eq!(probe(&root, PA, id(5, 500)), Probe::Gone, "missing");
        let _ = std::fs::remove_dir_all(&root);
    }
}
