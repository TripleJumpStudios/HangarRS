//! Launching a companion app — and, first, checking whether it is already up.

use crate::env::ProtonEnv;
use crate::proc::{self, AppProcesses};
use crate::run_state::{is_app_output, judge_launch, LaunchOutcome, RunningApp, IMMEDIATE_EXIT};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// How to run an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// A native Linux binary.
    Native,
    /// A Windows executable, through Proton.
    Prefix,
    /// A Windows app inside a running simulator session; requires an explicit capability.
    SimSession,
}

/// What to launch.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub label: String,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub mode: RunMode,
}

impl LaunchRequest {
    /// The `/proc` needle for this app — its executable's file name.
    pub fn match_key(&self) -> String {
        proc::match_key(&self.exe)
    }
}

/// How a launch attempt ended.
#[derive(Debug)]
pub enum Launched {
    /// It was already running; we attached to it instead of starting a copy.
    Adopted(RunningApp),
    /// We started it and it is running.
    Started(RunningApp),
    /// It did not survive.
    Failed {
        outcome: LaunchOutcome,
        log: Vec<String>,
    },
    /// It could not be started at all.
    SpawnError(String),
}

/// Somewhere for a launch's console output to go as it arrives.
///
/// # Why this is not the `log` on [`Launched::Failed`]
///
/// That log only exists for a process that **exited**. `spawn` drops the
/// reader handles for one that is still running, on purpose — the readers are
/// meant to detach and go on collecting — and the buffer they write into goes
/// with the caller's return. So a running app's output has nowhere to be seen:
/// there is nothing to poll, and nothing kept.
///
/// A sink is the difference between a console that replays what a failed
/// launch said and one that shows what a running app is saying now. It is
/// called from the reader tasks, so it must be cheap and must not block —
/// pushing onto a `Mutex<Vec<_>>` is what it is for.
///
/// The same shape as `hangar_install::run::OnLine`, deliberately: an install
/// and a launch are the same problem, and the Console tab reads both.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Check whether an app is already running.
pub fn already_running(request: &LaunchRequest) -> AppProcesses {
    proc::find_app(&request.match_key())
}

/// Launch an app, adopting an existing instance rather than starting a second.
///
/// **Adoption comes first, and it is not an optimisation.** An app with a
/// single-instance lock does not merely duplicate when launched twice: the new
/// process quits before any of its own code runs, exits 0, and looks from the
/// outside exactly like a successful start. Meanwhile the instance that is
/// actually running may have no window, no tray icon and no reachable focus
/// path. Checking first is the difference between finding it and losing it.
pub async fn launch(env: &ProtonEnv, request: &LaunchRequest) -> Launched {
    launch_with(env, request, None).await
}

/// [`launch`], with somewhere for the console output to go as it arrives.
pub async fn launch_with(env: &ProtonEnv, request: &LaunchRequest, sink: Option<Sink>) -> Launched {
    if request.mode == RunMode::SimSession {
        return Launched::SpawnError("Sim session launches are not enabled here.".into());
    }
    let found = already_running(request);
    if found.is_running() {
        tracing::info!(
            "{}: already running as pid {:?}, adopting",
            request.label,
            found.main_pid()
        );
        if let Some(app) = RunningApp::adopted(&request.label, &found) {
            return Launched::Adopted(app);
        }
    }

    spawn(env, request, sink).await
}

/// Stop an app and start it again.
///
/// **The only way to get a closed window back.** A companion app can keep
/// running with no window: Project Airports overrides Electron's
/// quit-on-last-window-closed to do nothing, so its X button hides the app
/// rather than ending it. Nothing can reopen that window from outside —
/// Electron's `second-instance` focus message does not cross the Wine boundary,
/// the tray icon never appears under KDE/Wayland, and the window is destroyed
/// rather than hidden, so no window manager can raise it. Only the app itself
/// can make a new one, and it only does that at startup.
///
/// The wait between the two halves is load-bearing: [`launch`] adopts anything
/// already running, so restarting without waiting would re-adopt the very
/// process just asked to exit and report success without restarting anything.
pub async fn restart(env: &ProtonEnv, request: &LaunchRequest) -> Launched {
    restart_with(env, request, None).await
}

/// [`restart`], with somewhere for the console output to go as it arrives.
///
/// Stops the one main instance of the app by identity; several copies are
/// refused rather than the lowest pid guessed. [`restart_followed`] reaches
/// the instance a tracked launch started.
pub async fn restart_with(
    env: &ProtonEnv,
    request: &LaunchRequest,
    sink: Option<Sink>,
) -> Launched {
    restart_followed(env, request, None, sink).await
}

/// [`restart_with`] for a launch HangarRS is following: the process stopped
/// is the one `followed` tracks, or its update restart, and no other copy of
/// the same exe.
///
/// Signals only, as before: a restart has never sent a Windows close request.
pub async fn restart_followed(
    env: &ProtonEnv,
    request: &LaunchRequest,
    followed: Option<&crate::respawn::Tracker>,
    sink: Option<Sink>,
) -> Launched {
    if request.mode == RunMode::SimSession {
        return Launched::SpawnError("Sim session launches are not enabled here.".into());
    }
    let key = request.match_key();
    let root = Path::new(crate::observe::PROC);
    let observation = crate::observe::observe(root, std::slice::from_ref(&key));
    let target = crate::kill::resolve_in(
        observation.found(&key),
        followed,
        Instant::now(),
        Some(&env.wineprefix),
    );
    let host = crate::kill::Live { root, env: None };
    match crate::kill::stop_for_restart(
        &host,
        &key,
        target,
        &crate::kill::Timing::LIVE,
        STOP_TIMEOUT,
    )
    .await
    {
        Ok(()) => spawn(env, request, sink).await,
        Err(why) => Launched::SpawnError(why),
    }
}

/// How long to wait for an app to disappear before giving up on a restart.
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long to let the output readers finish after the process has exited.
///
/// Its pipes are closed by then, so they finish in microseconds. The timeout
/// exists for the case where a grandchild inherited the pipe and is holding it
/// open — the parent is gone, so there is nothing left to wait for, and a
/// launch must never hang on a stranger's file descriptor.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Start the app, then watch long enough to tell a launch from a bounce.
async fn spawn(env: &ProtonEnv, request: &LaunchRequest, sink: Option<Sink>) -> Launched {
    let mut command = match request.mode {
        RunMode::SimSession => {
            return Launched::SpawnError("Sim session launches are not enabled here.".into());
        }
        RunMode::Native => {
            let mut c = Command::new(&request.exe);
            c.args(&request.args);
            c
        }
        RunMode::Prefix => {
            let (program, mut args) = env.invocation();
            args.push(request.exe.as_os_str().to_owned());
            args.extend(request.args.iter().map(Into::into));
            let mut c = Command::new(program);
            c.args(args).envs(env.vars());
            c
        }
    };

    // Run from the executable's own directory. Many Windows programs resolve
    // bundled resources against the working directory rather than the binary —
    // the TeamSpeak SDK that PilotEdge uses for voice loads `soundbackends/`
    // from the CWD and reports no audio devices if it is not there.
    if let Some(dir) = request.exe.parent() {
        command.current_dir(dir);
    }

    observe_command(command, request, sink).await
}

/// Observe an already prepared command. Session launches set their CWD after namespace entry.
pub(crate) async fn observe_command(
    mut command: Command,
    request: &LaunchRequest,
    sink: Option<Sink>,
) -> Launched {
    let mut child = match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Launched::SpawnError(e.to_string()),
    };

    let Some(pid) = child.id() else {
        return Launched::SpawnError("the process exited before it could be tracked".into());
    };

    let started = Instant::now();
    let log = Arc::new(Mutex::new(Vec::new()));
    // Tracks whether the *app* said anything, as opposed to Wine's startup
    // chatter. This is the signal that separates a failed launch from a working
    // one, because the exit code cannot.
    let spoke = Arc::new(Mutex::new(false));

    let mut pumps = Vec::new();
    for stream in [
        child.stdout.take().map(StreamKind::Out),
        child.stderr.take().map(StreamKind::Err),
    ]
    .into_iter()
    .flatten()
    {
        let log = Arc::clone(&log);
        let spoke = Arc::clone(&spoke);
        let sink = sink.clone();
        pumps.push(tokio::spawn(async move {
            match stream {
                StreamKind::Out(s) => pump(BufReader::new(s).lines(), log, spoke, sink).await,
                StreamKind::Err(s) => pump(BufReader::new(s).lines(), log, spoke, sink).await,
            }
        }));
    }

    // Wait out the immediate-exit window, or until it exits, whichever first.
    let exited = tokio::time::timeout(IMMEDIATE_EXIT, child.wait())
        .await
        .is_ok();

    // `child.wait()` returns the moment the process dies, but the readers above
    // are separate tasks and may not have drained the pipes yet. Judging
    // straight away races them, and the loser is the diagnostic: an app that
    // failed *and said why* gets recorded as having exited silently, with an
    // empty log. Draining first is the whole difference.
    //
    // Only when it exited. A process that is still running has readers that
    // never finish, and they are meant to keep collecting — dropping the
    // handles detaches them, which is what should happen there.
    if exited {
        for pump in pumps {
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, pump).await;
        }
    }

    let elapsed = started.elapsed();
    let produced_output = *spoke.lock().unwrap_or_else(|e| e.into_inner());
    let outcome = judge_launch(elapsed, produced_output, !exited);

    match outcome {
        LaunchOutcome::Started => {
            let mut app = RunningApp::owned(&request.label, pid);
            app.produced_output = produced_output;
            // Pick up helper processes so the UI can say "4 processes".
            app.helpers = proc::find_app(&request.match_key())
                .helpers
                .iter()
                .map(|p| p.pid)
                .collect();
            Launched::Started(app)
        }
        outcome => Launched::Failed {
            outcome,
            log: log.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        },
    }
}

enum StreamKind {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

/// Collect a stream into the log, noting whether any of it was the app's own.
async fn pump<R>(
    mut lines: tokio::io::Lines<BufReader<R>>,
    log: Arc<Mutex<Vec<String>>>,
    spoke: Arc<Mutex<bool>>,
    sink: Option<Sink>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    while let Ok(Some(line)) = lines.next_line().await {
        if is_app_output(&line) {
            *spoke.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }
        // Before the cap below, because the cap exists to bound *this* buffer.
        // A sink has its own bound and its own reader, and a chatty app that
        // has filled the local log is exactly the one still worth watching.
        if let Some(sink) = &sink {
            sink(&line);
        }
        let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
        // Bounded: a chatty app should not be able to grow this without limit.
        if log.len() < 500 {
            log.push(line);
        }
    }
}

/// Make sure a file is executable, which the Proton script's chain requires.
pub fn ensure_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    if mode & 0o111 == 0 {
        perms.set_mode(mode | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-runner-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn native(exe: PathBuf, args: &[&str]) -> LaunchRequest {
        LaunchRequest {
            label: "Test App".into(),
            exe,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            mode: RunMode::Native,
        }
    }

    fn env() -> ProtonEnv {
        ProtonEnv {
            wineprefix: PathBuf::from("/nonexistent/pfx"),
            proton_script: PathBuf::from("/nonexistent/proton"),
            wine_binary: PathBuf::from("/nonexistent/wine"),
            compat_data: PathBuf::from("/nonexistent"),
            compat_install: PathBuf::from("/nonexistent"),
            steam_client: PathBuf::from("/nonexistent"),
            is_flatpak: false,
        }
    }

    /// A uniquely-named symlink to a real program, so `argv[0]` carries a name
    /// the `/proc` scan can find.
    fn fake_app(dir: &Path, target: &str) -> PathBuf {
        let exe = dir.join(format!(
            "hangar-runner-app-{}-{}.exe",
            std::process::id(),
            dir.file_name().unwrap().to_string_lossy()
        ));
        let target = ["/bin", "/usr/bin"]
            .iter()
            .map(|p| Path::new(p).join(target))
            .find(|p| p.exists())
            .expect("program is available");
        std::os::unix::fs::symlink(target, &exe).unwrap();
        exe
    }

    #[test]
    fn the_match_key_is_the_executable_name() {
        let r = native(PathBuf::from("/a/long/path/Project Airports.exe"), &[]);
        assert_eq!(r.match_key(), "Project Airports.exe");
    }

    /// **Adoption.** A second launch must attach to what is running rather than
    /// start a copy that would die on the single-instance lock.
    #[tokio::test]
    async fn a_running_app_is_adopted_instead_of_launched_twice() {
        let dir = scratch();
        let exe = fake_app(&dir, "sleep");
        let request = native(exe.clone(), &["60"]);

        let first = launch(&env(), &request).await;
        let first_pid = match first {
            Launched::Started(ref app) => app.pid,
            other => panic!("expected a start, got {other:?}"),
        };

        let second = launch(&env(), &request).await;
        match second {
            Launched::Adopted(app) => {
                assert_eq!(
                    app.pid, first_pid,
                    "attached to the process already running"
                );
                assert_eq!(app.ownership, crate::run_state::Ownership::Adopted);
            }
            other => panic!("expected adoption, got {other:?}"),
        }

        // Exactly one process, not two.
        assert_eq!(proc::find_app(&request.match_key()).count(), 1);

        let _ = crate::kill::kill_app(&request.match_key()).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An app already running that this process never started.
    #[tokio::test]
    async fn an_app_started_elsewhere_is_adopted() {
        let dir = scratch();
        let exe = fake_app(&dir, "sleep");
        // Started through std, never awaited — as far as `launch` knows, this
        // is somebody else's process.
        let mut outsider = std::process::Command::new(&exe)
            .arg("60")
            .spawn()
            .expect("spawns");

        let request = native(exe, &["60"]);
        for _ in 0..200 {
            if already_running(&request).is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        match launch(&env(), &request).await {
            Launched::Adopted(app) => {
                assert_eq!(app.pid, outsider.id());
                assert!(
                    !app.ownership.has_child_handle(),
                    "there is no handle to wait on"
                );
                assert!(!app.ownership.streams_output(), "and no output to read");
            }
            other => panic!("expected adoption, got {other:?}"),
        }

        let _ = crate::kill::kill_pid(outsider.id(), std::time::Duration::from_secs(2)).await;
        let _ = outsider.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The failure that exits 0.** A program that returns immediately and
    /// says nothing must be reported as a failure, not a successful launch.
    #[tokio::test]
    async fn an_instant_silent_exit_is_reported_as_a_failure() {
        let dir = scratch();
        let exe = fake_app(&dir, "true");
        let request = native(exe, &[]);

        match launch(&env(), &request).await {
            Launched::Failed { outcome, .. } => {
                assert!(
                    matches!(outcome, LaunchOutcome::ImmediateExit { .. }),
                    "exit 0 in under a second is the lock, not a success: {outcome:?}"
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dying quickly but saying why is a different diagnosis.
    #[tokio::test]
    async fn a_quick_exit_that_printed_something_is_not_the_lock() {
        let dir = scratch();
        let exe = fake_app(&dir, "echo");
        let request = native(exe, &["app started, then gave up"]);

        match launch(&env(), &request).await {
            Launched::Failed { outcome, log } => {
                assert!(
                    matches!(outcome, LaunchOutcome::ExitedWithOutput { .. }),
                    "it told us why: {outcome:?}"
                );
                assert!(log.iter().any(|l| l.contains("gave up")), "{log:?}");
            }
            other => panic!("expected a failure with output, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A restart must produce a genuinely new process, not re-adopt the old
    /// one — which is what would happen if it did not wait for the exit.
    #[tokio::test]
    async fn a_restart_replaces_the_process_rather_than_adopting_it() {
        let dir = scratch();
        let exe = fake_app(&dir, "sleep");
        let request = native(exe, &["60"]);

        let first = match launch(&env(), &request).await {
            Launched::Started(app) => app.pid,
            other => panic!("expected a start, got {other:?}"),
        };

        match restart(&env(), &request).await {
            Launched::Started(app) => {
                assert_ne!(app.pid, first, "a restart must be a new process");
                assert_eq!(app.ownership, crate::run_state::Ownership::Owned);
            }
            other => panic!("expected a restart, got {other:?}"),
        }

        // Exactly one, not the old one alongside the new.
        assert_eq!(proc::find_app(&request.match_key()).count(), 1);

        let _ = crate::kill::kill_app(&request.match_key()).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restarting something that is not running just starts it.
    #[tokio::test]
    async fn restarting_something_stopped_simply_starts_it() {
        let dir = scratch();
        let exe = fake_app(&dir, "sleep");
        let request = native(exe, &["60"]);

        assert!(!already_running(&request).is_running());
        match restart(&env(), &request).await {
            Launched::Started(_) => {}
            other => panic!("expected a start, got {other:?}"),
        }

        let _ = crate::kill::kill_app(&request.match_key()).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_executable_is_a_spawn_error() {
        let request = native(PathBuf::from("/nonexistent/nothing-here.exe"), &[]);
        assert!(matches!(
            launch(&env(), &request).await,
            Launched::SpawnError(_)
        ));
    }

    #[test]
    fn a_non_executable_file_is_made_executable() {
        let dir = scratch();
        let file = dir.join("installer.exe");
        std::fs::write(&file, b"MZ").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        ensure_executable(&file).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "execute bit set");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
