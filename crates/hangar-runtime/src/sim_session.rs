//! Apps that share the running simulator's Wine and Steam sandbox session.
//!
//! Discovery is read-only. `Runtime::default()` refuses every action; only
//! `Runtime::live()` can enter a namespace or run Wine. No global Wine shutdown.

use crate::runner::{self, LaunchRequest, Launched, RunMode, Sink};
use crate::{proc, ProtonEnv, RunningApp};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;

const PROC: &str = "/proc";
const MAX_PROC: u64 = 2 * 1024 * 1024;
const CLOSE_WAIT: Duration = Duration::from_secs(15);
/// How long to keep watching after `CLOSE_WAIT` (15 s) for an app that asked
/// to confirm its own close (BeyondATC: a person confirmed at +43 s).
pub const CONFIRM_WAIT: Duration = Duration::from_secs(2 * 60);
const CONFIRM_POLL: Duration = Duration::from_millis(100);
/// What the row and Activity say while [`CONFIRM_WAIT`] runs.
pub const CONFIRM_NOTE: &str = "It may be asking you to confirm in its own window.";
const EXIT_GRACE: Duration = Duration::from_secs(5);
const CD_EXEC: &str = "cd -- \"$1\" && shift && exec \"$@\"";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Sim session actions are not enabled here.")]
    Inert,
    #[error("Start the simulator first.")]
    NoSim,
    #[error("More than one simulator session was found.")]
    Ambiguous,
    #[error("The simulator session changed; try Launch again.")]
    Changed,
    #[error("The app is running in another prefix or session; close it first.")]
    OtherSession,
    #[error("The simulator's Wine environment could not be verified.")]
    Environment,
    #[error("This executable cannot be managed as a sim session app.")]
    InvalidApp,
    #[error("Could not read the simulator session: {0}")]
    Read(#[from] io::Error),
    #[error("Could not close the app: {0}")]
    Close(String),
    #[error("The app is restarting after an update; it will be closed when it is back.")]
    Updating,
    #[error("More than one copy of the app is running in the simulator session; close the extra one first.")]
    Copies,
}

/// PID plus kernel start time, so shutdown never follows a recycled PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    pid: u32,
    start: u64,
}

impl Identity {
    fn read(root: &Path, pid: u32) -> io::Result<Self> {
        let stat = fs::read_to_string(root.join(pid.to_string()).join("stat"))?;
        let fields: Vec<_> = stat
            .rsplit_once(") ")
            .ok_or_else(|| io::Error::other("invalid process stat"))?
            .1
            .split_whitespace()
            .collect();
        if fields.first().is_none_or(|s| matches!(*s, "Z" | "X")) {
            return Err(io::Error::other("process has exited"));
        }
        let start = fields
            .get(19)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| io::Error::other("missing process start time"))?;
        Ok(Self { pid, start })
    }

    fn alive(self, root: &Path) -> bool {
        Self::read(root, self.pid).is_ok_and(|now| now == self)
    }
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_PROC + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROC {
        return Err(io::Error::other("process data exceeds the size limit"));
    }
    Ok(bytes)
}

fn basename(value: &str) -> &str {
    value
        .trim_matches('"')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
}

fn is_exe(args: &[String], name: &str) -> bool {
    args.first()
        .is_some_and(|a| basename(a).eq_ignore_ascii_case(name))
}

fn environment(raw: &[u8]) -> Result<Vec<(OsString, OsString)>, Error> {
    let mut vars = BTreeMap::new();
    for entry in raw.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let at = entry
            .iter()
            .position(|b| *b == b'=')
            .ok_or(Error::Environment)?;
        let (key, rest) = entry.split_at(at);
        if key.is_empty() {
            return Err(Error::Environment);
        }
        if key != b"LD_PRELOAD" {
            vars.insert(
                OsString::from_vec(key.to_vec()),
                OsString::from_vec(rest[1..].to_vec()),
            );
        }
    }
    Ok(vars.into_iter().collect())
}

fn prefix_matches(vars: &[(OsString, OsString)], expected: &Path) -> bool {
    vars.iter()
        .find(|(k, _)| k == "WINEPREFIX")
        .is_some_and(|(_, v)| {
            fs::canonicalize(v)
                .ok()
                .zip(fs::canonicalize(expected).ok())
                .is_some_and(|(a, b)| a == b)
        })
}

fn same_file(a: &File, b: &File) -> io::Result<bool> {
    let (a, b) = (a.metadata()?, b.metadata()?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

/// Keep namespace descriptors open, including after the sim process exits.
/// The environment is deliberately never Debug-printed or sent to the console.
struct Session {
    sim: Identity,
    user: File,
    mount: File,
    enter_user: bool,
    enter_mount: bool,
    vars: Vec<(OsString, OsString)>,
    wine: PathBuf,
    prefix: PathBuf,
}

impl Session {
    fn discover(root: &Path, expected: &ProtonEnv) -> Result<Self, Error> {
        let mut processes = Vec::new();
        for entry in fs::read_dir(root)?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|v| v.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(raw) = read_bounded(&entry.path().join("cmdline")) else {
                continue;
            };
            let args = proc::split_cmdline(&raw);
            if is_exe(&args, crate::sim::MSFS_2024_EXE)
                || (is_exe(&args, "steam.exe")
                    && args
                        .iter()
                        .skip(1)
                        .any(|a| basename(a).eq_ignore_ascii_case(crate::sim::MSFS_2024_EXE)))
            {
                processes.push((pid, args));
            }
        }
        processes.sort_by_key(|(pid, _)| *pid);
        let mut sessions = Vec::new();
        let mut failure = Error::NoSim;
        for (pid, args) in &processes {
            if !is_exe(args, crate::sim::MSFS_2024_EXE) {
                continue;
            }
            let Ok(identity) = Identity::read(root, *pid) else {
                continue;
            };
            // Prefer the steam.exe Wine wrapper used by the verified launch.
            // Both namespaces must match the actual simulator executable.
            for (source, _) in processes
                .iter()
                .filter(|(_, a)| is_exe(a, "steam.exe"))
                .chain(std::iter::once(&(*pid, args.clone())))
            {
                match Self::capture(root, identity, *source, &expected.wineprefix) {
                    Ok(session) => {
                        sessions.push(session);
                        break;
                    }
                    Err(e) => failure = e,
                }
            }
        }
        match sessions.len() {
            0 => Err(failure),
            1 => Ok(sessions.remove(0)),
            _ => Err(Error::Ambiguous),
        }
    }

    fn capture(root: &Path, sim: Identity, source: u32, prefix: &Path) -> Result<Self, Error> {
        let dir = root.join(source.to_string());
        let source_id = Identity::read(root, source)?;
        if fs::metadata(&dir)?.uid() != fs::metadata(root.join("self"))?.uid() {
            return Err(Error::Environment);
        }
        let user = File::open(dir.join("ns/user"))?;
        let mount = File::open(dir.join("ns/mnt"))?;
        let sim_dir = root.join(sim.pid.to_string());
        if !same_file(&user, &File::open(sim_dir.join("ns/user"))?)?
            || !same_file(&mount, &File::open(sim_dir.join("ns/mnt"))?)?
        {
            return Err(Error::Changed);
        }
        let vars = environment(&read_bounded(&dir.join("environ"))?)?;
        if !prefix_matches(&vars, prefix) {
            return Err(Error::Environment);
        }
        // argv[0] establishes identity. /exe is used ONLY to locate the Wine
        // toolchain actually running this session, rather than a changed setting.
        let loader = fs::read_link(dir.join("exe"))?;
        if !matches!(
            loader.file_name().and_then(OsStr::to_str),
            Some("wine" | "wine64" | "wine-preloader" | "wine64-preloader")
        ) {
            return Err(Error::Environment);
        }
        let wine = loader.parent().ok_or(Error::Environment)?.join("wine");
        if !wine.is_file() {
            return Err(Error::Environment);
        }
        if !sim.alive(root) || !source_id.alive(root) {
            return Err(Error::Changed);
        }
        Ok(Self {
            sim,
            enter_user: !same_file(&user, &File::open(root.join("self/ns/user"))?)?,
            enter_mount: !same_file(&mount, &File::open(root.join("self/ns/mnt"))?)?,
            user,
            mount,
            vars,
            wine,
            prefix: prefix.to_owned(),
        })
    }

    fn contains(&self, root: &Path, pid: u32) -> bool {
        let dir = root.join(pid.to_string());
        let check = || -> Result<bool, Error> {
            Ok(same_file(&self.user, &File::open(dir.join("ns/user"))?)?
                && same_file(&self.mount, &File::open(dir.join("ns/mnt"))?)?
                && prefix_matches(
                    &environment(&read_bounded(&dir.join("environ"))?)?,
                    &self.prefix,
                ))
        };
        check().unwrap_or(false)
    }

    fn command(&self, cwd: &Path, exe: &OsStr, args: &[OsString]) -> Command {
        let mut command = if self.enter_user || self.enter_mount {
            let mut c = Command::new("/usr/bin/nsenter");
            // nsenter opens all namespace descriptors before setns. Parent fd
            // paths let us pin the namespaces without unsafe fork callbacks.
            for (enter, option, file) in [
                (self.enter_user, "--user", &self.user),
                (self.enter_mount, "--mount", &self.mount),
            ] {
                if enter {
                    c.arg(format!(
                        "{option}=/proc/{}/fd/{}",
                        std::process::id(),
                        file.as_raw_fd()
                    ));
                }
            }
            c.args(["--preserve-credentials", "--", "/usr/bin/env"]);
            c
        } else {
            Command::new("/usr/bin/env")
        };
        command.arg("-i");
        for (key, value) in &self.vars {
            let mut entry = key.clone();
            entry.push("=");
            entry.push(value);
            command.arg(entry);
        }
        // No Command::current_dir and no nsenter --wd: cd must run INSIDE
        // the new mount namespace. Paths and user arguments are positional data.
        command
            .args(["/bin/sh", "-c", CD_EXEC, "hangar-sim-session"])
            .arg(cwd)
            .arg(&self.wine)
            .arg(exe)
            .args(args);
        command
    }

    fn app_command(&self, request: &LaunchRequest) -> Result<Command, Error> {
        validate_app(request)?;
        if !self.sim.alive(Path::new(PROC)) {
            return Err(Error::Changed);
        }
        Ok(self.command(
            request.exe.parent().ok_or(Error::InvalidApp)?,
            request.exe.as_os_str(),
            &request.args.iter().map(OsString::from).collect::<Vec<_>>(),
        ))
    }
}

fn validate_app(request: &LaunchRequest) -> Result<(), Error> {
    if request.mode != RunMode::SimSession {
        return Err(Error::InvalidApp);
    }
    let name = request
        .exe
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(Error::InvalidApp)?;
    if !request.exe.is_absolute()
        || !name.to_ascii_lowercase().ends_with(".exe")
        || name.contains(['*', '?', '\\', '/'])
        || [
            crate::sim::MSFS_2024_EXE,
            "steam.exe",
            "wineserver.exe",
            "services.exe",
            "explorer.exe",
            "taskkill.exe",
            "cmd.exe",
            "start.exe",
        ]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
    {
        return Err(Error::InvalidApp);
    }
    Ok(())
}

/// One launch this runtime started, followed across self-update restarts.
///
/// `identity` is the instance running now; `launch` names the launch, and
/// stays the same when an updater replaces the process (see [`crate::respawn`]).
#[derive(Clone)]
struct Owned {
    session: Arc<Session>,
    identity: Identity,
    request: LaunchRequest,
    launch: u64,
    launched: std::time::Instant,
    /// Exited inside the respawn window and not back yet.
    updating: Option<std::time::Instant>,
    /// A close was asked for while it was updating: close it when it returns.
    close_on_return: bool,
}

/// One launch owned by this runtime, for scoped cancellation.
///
/// Names the launch rather than the process, so it still reaches the app after
/// its updater restarts it — but never a different launch or an adopted app.
#[derive(Clone, Debug)]
pub struct Token {
    key: String,
    launch: u64,
}

/// Explicit live capability shared by manual controls and preflight.
/// The registry owns only apps this runtime started, never adopted apps.
#[derive(Clone, Default)]
pub struct Runtime {
    live: bool,
    owned: Arc<Mutex<BTreeMap<String, Owned>>>,
    operations: Arc<tokio::sync::Mutex<()>>,
    launches: Arc<std::sync::atomic::AtomicU64>,
    /// When HangarRS last asked each app to close, by match key. An exit
    /// after it is a close, never an update restart.
    closing: Arc<Mutex<BTreeMap<String, std::time::Instant>>>,
    /// Apps still open after [`CLOSE_WAIT`], being watched for [`CONFIRM_WAIT`].
    confirming: Arc<Mutex<BTreeMap<String, std::time::Instant>>>,
    /// The process table; `/proc` unless a test points it at a fixture.
    root: Option<Arc<PathBuf>>,
}

impl Runtime {
    pub fn live() -> Self {
        Self {
            live: true,
            ..Self::default()
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    fn root(&self) -> &Path {
        self.root
            .as_deref()
            .map_or(Path::new(PROC), PathBuf::as_path)
    }

    /// Record that HangarRS asked the app under `key` to close.
    ///
    /// Recording touches nothing outside the process, so an inert runtime
    /// records too. Every close path calls it before the close is sent.
    pub fn close_requested(&self, key: &str) {
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_owned(), std::time::Instant::now());
    }

    /// When HangarRS last asked the app under `key` to close.
    pub fn closing_since(&self, key: &str) -> Option<std::time::Instant> {
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .copied()
    }

    /// Whether a close was asked for during this launch.
    fn is_closing(&self, owned: &Owned) -> bool {
        self.closing_since(&owned.request.match_key())
            .is_some_and(|at| at >= owned.launched)
    }

    /// Since when the app under `key` has been still open after a close
    /// request, possibly waiting for someone to confirm in its window.
    pub fn confirming(&self, key: &str) -> Option<std::time::Instant> {
        self.confirming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .copied()
    }

    /// Mark the app under `key` as still open after a close request, the way
    /// the wait for a confirmation does.
    ///
    /// Recording only, like [`Runtime::close_requested`]: it touches nothing
    /// outside the process. It exists so the whole-app render tests can draw
    /// a row reading "Closing".
    pub fn note_confirming(&self, key: &str) {
        self.confirming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_owned())
            .or_insert_with(std::time::Instant::now);
    }

    fn guard(&self) -> Result<(), Error> {
        if self.live {
            Ok(())
        } else {
            Err(Error::Inert)
        }
    }

    pub async fn launch(
        &self,
        env: &ProtonEnv,
        request: &LaunchRequest,
        sink: Option<Sink>,
    ) -> Launched {
        if let Err(e) = self.guard() {
            return Launched::SpawnError(e.to_string());
        }
        if request.mode != RunMode::SimSession {
            return Launched::SpawnError(Error::InvalidApp.to_string());
        }
        let _operation = self.operations.lock().await;
        match self.launch_inner(env, request, sink).await {
            Ok(result) => result,
            Err(e) => Launched::SpawnError(e.to_string()),
        }
    }

    async fn launch_inner(
        &self,
        env: &ProtonEnv,
        request: &LaunchRequest,
        sink: Option<Sink>,
    ) -> Result<Launched, Error> {
        validate_app(request)?;
        let session = Arc::new(Session::discover(Path::new(PROC), env)?);
        tracing::info!(target: "hangar_launch", app = %request.label, "simulator prefix and namespaces verified");
        let key = request.match_key();
        let processes = exact_processes(&key);
        if processes
            .iter()
            .any(|p| !session.contains(Path::new(PROC), p.pid))
        {
            return Err(Error::OtherSession);
        }
        let found = proc::classify(processes);
        if let Some(pid) = found.main_pid().filter(|p| proc::is_alive(*p)) {
            if !session.contains(Path::new(PROC), pid) {
                return Err(Error::OtherSession);
            }
            if let Some(app) = RunningApp::adopted(&request.label, &found) {
                return Ok(Launched::Adopted(app));
            }
        }
        let command = session.app_command(request)?;
        let result = runner::observe_command(command, request, sink.clone()).await;
        if let Launched::Started(app) = &result {
            let identity = Identity::read(Path::new(PROC), app.pid)?;
            if !session.contains(Path::new(PROC), app.pid) {
                return Err(Error::OtherSession);
            }
            let owned = Owned {
                session,
                identity,
                request: request.clone(),
                launch: self
                    .launches
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1,
                launched: std::time::Instant::now(),
                updating: None,
                close_on_return: false,
            };
            self.owned
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, owned.clone());
            self.watch(owned, sink);
        }
        Ok(result)
    }

    fn watch(&self, owned: Owned, sink: Option<Sink>) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut owned = owned;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if !owned.identity.alive(Path::new(PROC)) {
                    match runtime.await_respawn(&owned, sink.as_ref()).await {
                        Some(next) => {
                            owned = next;
                            continue;
                        }
                        None => break,
                    }
                }
                if !owned.session.sim.alive(Path::new(PROC)) {
                    tokio::time::sleep(EXIT_GRACE).await;
                    // A close already waiting on a confirmation is not sent again.
                    if runtime.confirming(&owned.request.match_key()).is_some() {
                        continue;
                    }
                    let operation = runtime.operations.lock().await;
                    if !runtime.is_current(&owned) {
                        return;
                    }
                    runtime.close_requested(&owned.request.match_key());
                    let result = runtime.close(&owned, sink.as_ref(), Some(operation)).await;
                    if let Some(sink) = &sink {
                        sink(&match result {
                            Ok(()) => "Simulator exited; app closed.".into(),
                            Err(e) => e.to_string(),
                        });
                    }
                    // One close attempt. A failed close stays available for
                    // the Stop button; no force kill and no retry storm.
                    if owned.identity.alive(Path::new(PROC)) {
                        return;
                    }
                    break;
                }
            }
            runtime.remove(&owned);
        });
    }

    /// The owned instance exited. Inside the respawn window, wait for the same
    /// app to come back in the same session and adopt it as this launch.
    async fn await_respawn(&self, owned: &Owned, sink: Option<&Sink>) -> Option<Owned> {
        let root = self.root();
        // An exit HangarRS asked for is final: a Stop confirmed late in BATC's
        // own dialog must not read as an update restart.
        if self.is_closing(owned) {
            tracing::info!(target: "hangar_launch", app = %owned.request.label, pid = owned.identity.pid, "exited after a close request");
            return None;
        }
        if owned.launched.elapsed() >= crate::respawn::RESPAWN_WINDOW
            || !owned.session.sim.alive(root)
        {
            return None;
        }
        let key = owned.request.match_key();
        let since = std::time::Instant::now();
        {
            let mut registry = self.owned.lock().unwrap_or_else(|e| e.into_inner());
            match registry.get_mut(&key) {
                Some(o) if o.launch == owned.launch && o.identity == owned.identity => {
                    o.updating = Some(since)
                }
                _ => return None,
            }
        }
        tracing::info!(target: "hangar_launch", app = %owned.request.label, pid = owned.identity.pid, "exited soon after launch; waiting for an update restart");
        if let Some(sink) = sink {
            sink("Exited soon after launch; waiting for it to restart after an update.");
        }
        while since.elapsed() < crate::respawn::RESPAWN_WAIT {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if !self.is_current(owned) || !owned.session.sim.alive(root) {
                break;
            }
            let [identity] = respawn_candidates(root, &owned.session, &key)[..] else {
                continue;
            };
            let operation = self.operations.lock().await;
            let next = self.adopt_respawn(owned, identity)?;
            tracing::info!(target: "hangar_launch", app = %owned.request.label, was = owned.identity.pid, pid = identity.pid, "update restart adopted");
            if let Some(sink) = sink {
                sink(&format!(
                    "Restarted after an update as pid {}.",
                    identity.pid
                ));
            }
            if next.close_on_return {
                let result = self.close(&next, sink, Some(operation)).await;
                if let Some(sink) = sink {
                    sink(&match &result {
                        Ok(()) => "Closed as requested.".to_owned(),
                        Err(e) => e.to_string(),
                    });
                }
                if result.is_ok() {
                    self.remove(&next);
                    return None;
                }
            }
            return Some(next);
        }
        self.remove(owned);
        if let Some(sink) = sink {
            sink("Did not restart; no longer followed.");
        }
        None
    }

    /// Replace the exited instance with its restart, keeping the launch.
    fn adopt_respawn(&self, owned: &Owned, identity: Identity) -> Option<Owned> {
        let mut registry = self.owned.lock().unwrap_or_else(|e| e.into_inner());
        let current = registry.get_mut(&owned.request.match_key())?;
        if current.launch != owned.launch || current.identity != owned.identity {
            return None;
        }
        current.identity = identity;
        current.updating = None;
        Some(current.clone())
    }

    /// Whether the app this runtime launched under `key` is restarting after an update.
    pub fn updating(&self, key: &str) -> Option<std::time::Instant> {
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .and_then(|o| o.updating)
    }

    /// A close requested while the app is away: remember it, report it.
    fn close_when_back(&self, owned: &Owned) -> Result<(), Error> {
        if let Some(o) = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&owned.request.match_key())
            .filter(|o| o.launch == owned.launch)
        {
            o.close_on_return = true;
        }
        Err(Error::Updating)
    }

    fn is_current(&self, owned: &Owned) -> bool {
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&owned.request.match_key())
            .is_some_and(|o| o.identity == owned.identity)
    }

    fn remove(&self, owned: &Owned) {
        let mut registry = self.owned.lock().unwrap_or_else(|e| e.into_inner());
        if registry
            .get(&owned.request.match_key())
            .is_some_and(|o| o.identity == owned.identity)
        {
            registry.remove(&owned.request.match_key());
        }
    }

    /// Stop also supports a same-session app started elsewhere, on explicit
    /// user request. Automatic shutdown calls `close` only for an Owned record.
    ///
    /// An app still open after `CLOSE_WAIT` is watched for [`CONFIRM_WAIT`]
    /// more, with `sink` told why; the close is never sent twice or forced.
    pub async fn stop(
        &self,
        env: Option<&ProtonEnv>,
        request: &LaunchRequest,
        sink: Option<Sink>,
    ) -> Result<(), Error> {
        self.guard()?;
        validate_app(request)?;
        self.close_requested(&request.match_key());
        let operation = self.operations.lock().await;
        self.stop_inner(env, request, sink.as_ref(), Some(operation))
            .await
    }

    pub fn owned_token(&self, request: &LaunchRequest, pid: u32) -> Option<Token> {
        let key = request.match_key();
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .filter(|o| o.identity.pid == pid)
            .map(|o| Token {
                key,
                launch: o.launch,
            })
    }

    /// Preflight cancellation must not close an adopted or replacement instance.
    pub async fn stop_started(&self, token: &Token, sink: Option<Sink>) -> Result<(), Error> {
        self.guard()?;
        self.close_requested(&token.key);
        let operation = self.operations.lock().await;
        let saved = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&token.key)
            .filter(|o| o.launch == token.launch)
            .cloned();
        if let Some(owned) = saved {
            if owned.updating.is_some() {
                return self.close_when_back(&owned);
            }
            let Some((owned, adopted)) = self.current_instance(owned)? else {
                return Ok(());
            };
            self.close_owned(&owned, adopted, sink.as_ref(), Some(operation))
                .await?;
        }
        Ok(())
    }

    /// The process a Stop of `owned` means now, by identity.
    ///
    /// The watch notices an exit at most a second late, so the record can
    /// still name an instance that has exited. Inside the respawn window, with
    /// the simulator up, the one same-exe main process in the launch's own
    /// session is its update restart: it is adopted as this launch and is what
    /// gets closed. Nothing there: gone (`None`). Several: refused, never
    /// guessed. A recycled pid is never the instance: identity is pid plus
    /// start time. The flag is whether this call adopted the restart into the
    /// registry.
    fn current_instance(&self, owned: Owned) -> Result<Option<(Owned, bool)>, Error> {
        let root = self.root();
        if owned.identity.alive(root) {
            return Ok(Some((owned, false)));
        }
        if owned.launch == 0
            || owned.launched.elapsed() >= crate::respawn::RESPAWN_WINDOW
            || !owned.session.sim.alive(root)
        {
            return Ok(None);
        }
        let key = owned.request.match_key();
        match respawn_candidates(root, &owned.session, &key)[..] {
            [] => Ok(None),
            [identity] => {
                tracing::info!(target: "hangar_launch", app = %owned.request.label, was = owned.identity.pid, pid = identity.pid, "stop reaches the update restart");
                Ok(Some(match self.adopt_respawn(&owned, identity) {
                    Some(next) => (next, true),
                    // The launch's record is gone; the restart is still its.
                    None => (
                        Owned {
                            identity,
                            updating: None,
                            ..owned
                        },
                        false,
                    ),
                }))
            }
            _ => Err(Error::Copies),
        }
    }

    /// Close an owned launch and forget it; a failed close keeps it followed.
    async fn close_owned<'a>(
        &'a self,
        owned: &Owned,
        adopted: bool,
        sink: Option<&Sink>,
        operation: Option<tokio::sync::MutexGuard<'a, ()>>,
    ) -> Result<(), Error> {
        let result = self.close(owned, sink, operation).await;
        if result.is_ok() {
            self.remove(owned);
        } else if adopted && self.is_current(owned) {
            // Adopted here rather than by its watch, whose task ends on the
            // old instance's exit (a close was requested): keep the sim-exit
            // close and status following the new one.
            self.watch(owned.clone(), sink.cloned());
        }
        result
    }

    /// `operation` is released while waiting for a confirmation; `None` when
    /// the caller keeps the lock for what follows (restart).
    async fn stop_inner<'a>(
        &'a self,
        env: Option<&ProtonEnv>,
        request: &LaunchRequest,
        sink: Option<&Sink>,
        operation: Option<tokio::sync::MutexGuard<'a, ()>>,
    ) -> Result<(), Error> {
        let saved = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&request.match_key())
            .cloned();
        let owned = if let Some(owned) = saved {
            if owned.updating.is_some() {
                return self.close_when_back(&owned);
            }
            let Some((owned, adopted)) = self.current_instance(owned)? else {
                return Ok(());
            };
            return self.close_owned(&owned, adopted, sink, operation).await;
        } else {
            let Some(pid) = exact_app(&request.match_key())
                .main_pid()
                .filter(|p| proc::is_alive(*p))
            else {
                return Ok(());
            };
            let identity = Identity::read(Path::new(PROC), pid)?;
            let env = env.ok_or(Error::Environment)?;
            // Prefer the simulator's own wrapper environment — the one launches
            // and owned closes use — when the app is inside it. An app restarted
            // by its own updater carries whatever environment the updater gave
            // it. Otherwise the app still holds the session after the sim exits.
            let session = match Session::discover(Path::new(PROC), env) {
                Ok(session) if session.contains(Path::new(PROC), pid) => session,
                _ => Session::capture(Path::new(PROC), identity, pid, &env.wineprefix)?,
            };
            Owned {
                session: Arc::new(session),
                identity,
                request: request.clone(),
                launch: 0,
                launched: std::time::Instant::now(),
                updating: None,
                close_on_return: false,
            }
        };
        self.close(&owned, sink, operation).await?;
        self.remove(&owned);
        Ok(())
    }

    pub async fn restart(
        &self,
        env: &ProtonEnv,
        request: &LaunchRequest,
        sink: Option<Sink>,
    ) -> Launched {
        if let Err(e) = self.guard() {
            return Launched::SpawnError(e.to_string());
        }
        let _operation = self.operations.lock().await;
        let result = async {
            validate_app(request)?;
            // Do not close a working app unless a simulator session is available.
            Session::discover(Path::new(PROC), env)?;
            self.close_requested(&request.match_key());
            self.stop_inner(Some(env), request, None, None).await?;
            self.launch_inner(env, request, sink).await
        }
        .await;
        result.unwrap_or_else(|e: Error| Launched::SpawnError(e.to_string()))
    }

    /// Ask the app to close and wait for it to go.
    ///
    /// Still open after [`CLOSE_WAIT`]: it may be asking to confirm in its own
    /// window (BATC does), so keep watching for [`CONFIRM_WAIT`] with the
    /// operation lock released. A close already being confirmed is joined,
    /// never sent again.
    async fn close<'a>(
        &'a self,
        owned: &Owned,
        sink: Option<&Sink>,
        operation: Option<tokio::sync::MutexGuard<'a, ()>>,
    ) -> Result<(), Error> {
        if self.confirming(&owned.request.match_key()).is_none()
            && request_close(owned).await? == Requested::Closed
        {
            return Ok(());
        }
        drop(operation);
        self.confirm_wait(owned, sink, CONFIRM_WAIT).await
    }

    /// Watch a close request's app for up to `wait` more. Never sends anything.
    async fn confirm_wait(
        &self,
        owned: &Owned,
        sink: Option<&Sink>,
        wait: Duration,
    ) -> Result<(), Error> {
        let key = owned.request.match_key();
        let (since, first) = {
            let mut confirming = self.confirming.lock().unwrap_or_else(|e| e.into_inner());
            match confirming.get(&key) {
                Some(since) => (*since, false),
                None => {
                    let now = std::time::Instant::now();
                    confirming.insert(key.clone(), now);
                    (now, true)
                }
            }
        };
        if first {
            tracing::info!(target: "hangar_launch", app = %owned.request.label, pid = owned.identity.pid, "still open after the close request; waiting for a confirmation");
            if let Some(sink) = sink {
                sink(CONFIRM_NOTE);
            }
        }
        let root = self.root();
        let exited = loop {
            if !owned.identity.alive(root) {
                break true;
            }
            if since.elapsed() >= wait {
                break false;
            }
            tokio::time::sleep(CONFIRM_POLL).await;
        };
        {
            let mut confirming = self.confirming.lock().unwrap_or_else(|e| e.into_inner());
            if confirming.get(&key) == Some(&since) {
                confirming.remove(&key);
            }
        }
        if exited {
            tracing::info!(target: "hangar_launch", app = %owned.request.label, pid = owned.identity.pid, "closed after the wait for a confirmation");
            Ok(())
        } else {
            Err(Error::Close(
                "the app stayed open; close its window manually.".into(),
            ))
        }
    }
}

fn exact_processes(key: &str) -> Vec<proc::ProcInfo> {
    let mut found = proc::scan(|p| is_exe(&p.args, key) && proc::is_alive(p.pid));
    found.sort_by_key(|p| p.pid);
    found
}

fn exact_app(key: &str) -> proc::AppProcesses {
    proc::classify(exact_processes(key))
}

/// Main instances of `key` inside `session` (same namespaces and prefix).
fn respawn_candidates(root: &Path, session: &Session, key: &str) -> Vec<Identity> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<Identity> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            read_bounded(&root.join(pid.to_string()).join("cmdline")).is_ok_and(|raw| {
                let args = proc::split_cmdline(&raw);
                is_exe(&args, key) && !args.iter().any(|a| a.starts_with("--type="))
            })
        })
        .filter_map(|pid| Identity::read(root, pid).ok())
        .filter(|id| session.contains(root, id.pid))
        .collect();
    found.sort_by_key(|id| id.pid);
    found
}

/// The last thing a failed close request said, for the person reading Activity.
fn said(output: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr).into_owned()
        + &String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.contains("fixme:"))
        .unwrap_or("");
    line.chars().take(200).collect()
}

/// What one close request left behind.
#[derive(Debug, PartialEq, Eq)]
enum Requested {
    Closed,
    /// Asked, and still open after [`CLOSE_WAIT`].
    StillOpen,
}

/// Send the one Windows close request and wait [`CLOSE_WAIT`] for the exit.
async fn request_close(owned: &Owned) -> Result<Requested, Error> {
    let root = Path::new(PROC);
    if !owned.identity.alive(root) {
        return Ok(Requested::Closed);
    }
    if !owned.session.contains(root, owned.identity.pid) {
        return Err(Error::Changed);
    }
    // /IM is exact, without wildcards, /F or /T. Refuse ambiguity so the
    // Windows close request cannot select another instance with the same name.
    let matches = exact_processes(&owned.request.match_key())
        .into_iter()
        .filter(|p| {
            is_exe(&p.args, &owned.request.match_key())
                && proc::is_alive(p.pid)
                && owned.session.contains(root, p.pid)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 || matches[0].pid != owned.identity.pid {
        return Err(Error::OtherSession);
    }
    let mut command = owned.session.command(
        Path::new("/"),
        OsStr::new(r"C:\windows\system32\taskkill.exe"),
        &["/IM".into(), owned.request.match_key().into()],
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(CLOSE_WAIT, command.output())
        .await
        .map_err(|_| Error::Close("the close request timed out.".into()))??;
    if !output.status.success() && owned.identity.alive(root) {
        // taskkill's own words go with the failure, so a refusal says why.
        let said = said(&output);
        tracing::info!(target: "hangar_launch", app = %owned.request.label, status = ?output.status.code(), said = %said, "close request failed");
        return Err(Error::Close(if said.is_empty() {
            "the Windows close request failed; close its window manually.".into()
        } else {
            format!("the Windows close request failed ({said}); close its window manually.")
        }));
    }
    let deadline = tokio::time::Instant::now() + CLOSE_WAIT;
    while owned.identity.alive(root) {
        if tokio::time::Instant::now() >= deadline {
            return Ok(Requested::StillOpen);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(Requested::Closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture {
        dir: PathBuf,
        root: PathBuf,
        env: ProtonEnv,
    }

    impl Fixture {
        fn new(sandboxed: bool) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "hangar-session-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            let root = dir.join("proc");
            fs::create_dir_all(root.join("self/ns")).unwrap();
            for ns in ["user", "mnt"] {
                fs::write(root.join("self/ns").join(ns), b"host").unwrap();
            }
            let prefix = dir.join("sim/pfx");
            fs::create_dir_all(&prefix).unwrap();
            let wine = dir.join("Proton - Experimental/files/bin/wine");
            fs::create_dir_all(wine.parent().unwrap()).unwrap();
            fs::write(&wine, b"fixture only").unwrap();
            let env = ProtonEnv {
                wineprefix: prefix.clone(),
                wine_binary: wine.clone(),
                proton_script: dir.join("proton"),
                compat_data: prefix.parent().unwrap().into(),
                compat_install: dir.join("sim"),
                steam_client: dir.join("steam"),
                is_flatpak: false,
            };
            let fixture = Self { dir, root, env };
            fixture.process(101, &[r"C:\games\FlightSimulator2024.exe"], 111);
            fixture.process(
                100,
                &[
                    r"c:\windows\system32\steam.exe",
                    r"C:\games\FlightSimulator2024.exe",
                ],
                110,
            );
            for ns in ["user", "mnt"] {
                let target = fixture.root.join("101/ns").join(ns);
                if sandboxed {
                    fs::write(&target, b"container").unwrap();
                } else {
                    fs::hard_link(fixture.root.join("self/ns").join(ns), &target).unwrap();
                }
                fs::hard_link(target, fixture.root.join("100/ns").join(ns)).unwrap();
            }
            fixture
        }

        fn process(&self, pid: u32, args: &[&str], start: u64) {
            let dir = self.root.join(pid.to_string());
            fs::create_dir_all(dir.join("ns")).unwrap();
            fs::write(dir.join("cmdline"), args.join("\0") + "\0").unwrap();
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = "S".into();
            fields[19] = start.to_string();
            fs::write(
                dir.join("stat"),
                format!("{pid} (name with ) parens) {}", fields.join(" ")),
            )
            .unwrap();
            fs::write(
                dir.join("environ"),
                format!(
                    "WINEPREFIX={}/\0DISPLAY=:9\0LD_PRELOAD=overlay.so\0FROM_WRAPPER={pid}\0",
                    self.env.wineprefix.display()
                ),
            )
            .unwrap();
            symlink(
                self.env.wine_binary.with_file_name("wine64-preloader"),
                dir.join("exe"),
            )
            .unwrap();
        }

        fn request(&self) -> LaunchRequest {
            LaunchRequest {
                label: "BeyondATC".into(),
                exe: self.dir.join("companion/BeyondATC.exe"),
                args: vec![],
                mode: RunMode::SimSession,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn discovery_uses_the_actual_sim_and_its_matching_wine_wrapper() {
        let f = Fixture::new(true);
        // A diagnostic command mentioning the sim is not a simulator.
        f.process(9, &["pgrep", "FlightSimulator2024.exe"], 9);
        let session = Session::discover(&f.root, &f.env).unwrap();
        assert_eq!(session.sim.pid, 101);
        assert!(session.enter_user && session.enter_mount);
        assert!(session
            .vars
            .contains(&("FROM_WRAPPER".into(), "100".into())));
        assert!(!session.vars.iter().any(|(k, _)| k == "LD_PRELOAD"));
        assert_eq!(session.wine, f.env.wine_binary);
        assert!(session.contains(&f.root, 101));
    }

    #[test]
    fn same_namespace_launch_does_not_require_nsenter() {
        let f = Fixture::new(false);
        let session = Session::discover(&f.root, &f.env).unwrap();
        let command = session.command(Path::new("/"), OsStr::new("app.exe"), &[]);
        assert_eq!(command.as_std().get_program(), "/usr/bin/env");
        assert!(!session.enter_user && !session.enter_mount);
    }

    #[test]
    fn wrong_prefix_unreadable_environment_and_exited_sim_refuse() {
        let f = Fixture::new(true);
        let mut env = f.env.clone();
        env.wineprefix = f.dir.join("another-prefix");
        fs::create_dir(&env.wineprefix).unwrap();
        assert!(Session::discover(&f.root, &env).is_err());
        fs::remove_file(f.root.join("100/environ")).unwrap();
        fs::remove_file(f.root.join("101/environ")).unwrap();
        assert!(Session::discover(&f.root, &f.env).is_err());
        fs::remove_file(f.root.join("101/stat")).unwrap();
        assert!(matches!(
            Session::discover(&f.root, &f.env),
            Err(Error::NoSim)
        ));
    }

    #[test]
    fn a_wrapper_in_another_namespace_is_never_borrowed() {
        let f = Fixture::new(true);
        fs::remove_file(f.root.join("100/ns/user")).unwrap();
        fs::write(f.root.join("100/ns/user"), b"other").unwrap();
        let session = Session::discover(&f.root, &f.env).unwrap();
        assert!(session
            .vars
            .contains(&("FROM_WRAPPER".into(), "101".into())));
        assert!(!session.contains(&f.root, 100));
    }

    #[test]
    fn namespaces_are_pinned_and_cwd_is_not_set_by_the_host() {
        let f = Fixture::new(true);
        let session = Session::discover(&f.root, &f.env).unwrap();
        fs::remove_dir_all(f.root.join("100")).unwrap();
        fs::remove_dir_all(f.root.join("101")).unwrap();
        let command = session.command(
            Path::new("/path with spaces"),
            OsStr::new("BeyondATC.exe"),
            &[],
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), "/usr/bin/nsenter");
        assert!(command.get_current_dir().is_none());
        let args: Vec<_> = command.get_args().map(|s| s.to_string_lossy()).collect();
        assert!(args[0].starts_with("--user=/proc/"));
        assert!(args[1].starts_with("--mount=/proc/"));
        assert_eq!(args[2], "--preserve-credentials");
        assert!(!args.iter().any(|a| a.starts_with("--wd")));
        assert!(args.iter().any(|a| a == CD_EXEC));
        assert!(session.user.metadata().is_ok());
    }

    #[tokio::test]
    async fn cwd_environment_and_literal_arguments_survive_the_shell() {
        let f = Fixture::new(false);
        let mut session = Session::discover(&f.root, &f.env).unwrap();
        session.wine = "/bin/sh".into(); // scratch command only; never Wine
        let cwd = f.dir.join("a folder ' $HOME `literal`");
        fs::create_dir(&cwd).unwrap();
        let literal = "$(touch SHOULD_NOT_EXIST) ' \" ; * $HOME";
        let args = [
            OsString::from(
                "printf '%s\\n' \"$PWD\" \"$DISPLAY\" \"${LD_PRELOAD-unset}\" \"${HOST_ONLY-unset}\" \"$1\"",
            ),
            "fixture".into(),
            literal.into(),
        ];
        let mut cmd = session.command(&cwd, OsStr::new("-c"), &args);
        cmd.env("HOST_ONLY", "must not leak");
        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n:9\nunset\nunset\n{literal}\n", cwd.display())
        );
        assert!(!cwd.join("SHOULD_NOT_EXIST").exists());
    }

    #[test]
    fn non_utf8_environment_values_and_equals_are_preserved() {
        let vars = environment(b"A=one=two\0RAW=\xff\0LD_PRELOAD=bad\0").unwrap();
        assert!(vars.contains(&("A".into(), "one=two".into())));
        assert!(vars.contains(&("RAW".into(), OsString::from_vec(vec![255]))));
        assert!(environment(b"MALFORMED\0").is_err());
    }

    #[test]
    fn process_identity_rejects_zombies_and_reused_pids() {
        let f = Fixture::new(false);
        let identity = Identity::read(&f.root, 101).unwrap();
        assert!(identity.alive(&f.root));
        let path = f.root.join("101/stat");
        let stat = fs::read_to_string(&path).unwrap();
        fs::write(&path, stat.replace("111", "112")).unwrap();
        assert!(!identity.alive(&f.root));
        fs::write(path, stat.replace(") S ", ") Z ")).unwrap();
        assert!(Identity::read(&f.root, 101).is_err());
    }

    #[tokio::test]
    async fn inert_actions_and_unguarded_runner_routes_refuse_before_effects() {
        let f = Fixture::new(false);
        let runtime = Runtime::default();
        let request = f.request();
        assert!(!runtime.is_live());
        assert!(matches!(
            runtime.launch(&f.env, &request, None).await,
            Launched::SpawnError(_)
        ));
        assert!(matches!(
            runtime.restart(&f.env, &request, None).await,
            Launched::SpawnError(_)
        ));
        assert!(matches!(
            runtime.stop(Some(&f.env), &request, None).await,
            Err(Error::Inert)
        ));
        assert!(matches!(
            runtime
                .stop_started(
                    &Token {
                        key: request.match_key(),
                        launch: 1
                    },
                    None
                )
                .await,
            Err(Error::Inert)
        ));
        assert!(matches!(
            runner::launch(&f.env, &request).await,
            Launched::SpawnError(_)
        ));
        assert!(matches!(
            runner::restart(&f.env, &request).await,
            Launched::SpawnError(_)
        ));
        assert!(runtime.owned.lock().unwrap().is_empty());
    }

    #[test]
    fn reserved_processes_and_wildcards_cannot_be_session_apps() {
        let f = Fixture::new(false);
        for exe in [
            "FlightSimulator2024.exe",
            "steam.exe",
            "taskkill.exe",
            "*.exe",
            "a?.exe",
        ] {
            let mut request = f.request();
            request.exe = f.dir.join(exe);
            assert!(
                matches!(validate_app(&request), Err(Error::InvalidApp)),
                "{exe}"
            );
        }
        assert!(validate_app(&f.request()).is_ok());
    }

    /// A process in the fixture's simulator session (namespaces of pid 101).
    fn session_process(f: &Fixture, pid: u32, args: &[&str]) {
        f.process(pid, args, pid as u64);
        for ns in ["user", "mnt"] {
            fs::hard_link(
                f.root.join("101/ns").join(ns),
                f.root.join(pid.to_string()).join("ns").join(ns),
            )
            .unwrap();
        }
    }

    fn owned(f: &Fixture, pid: u32) -> Owned {
        Owned {
            session: Arc::new(Session::discover(&f.root, &f.env).unwrap()),
            identity: Identity::read(&f.root, pid).unwrap(),
            request: f.request(),
            launch: 7,
            launched: std::time::Instant::now(),
            updating: None,
            close_on_return: false,
        }
    }

    #[test]
    fn an_update_restart_in_the_same_session_is_the_only_candidate() {
        // BATC: 38377 exited, the updater started 39513 in the same
        // container with no HangarRS launch.
        let f = Fixture::new(true);
        let session = Session::discover(&f.root, &f.env).unwrap();
        session_process(&f, 39513, &[r"C:\BeyondATC\BeyondATC.exe"]);
        // Same name in the host namespaces (another copy), a helper, a
        // different name and a search command: none is the app.
        f.process(500, &[r"Z:\companion\BeyondATC.exe"], 500);
        for ns in ["user", "mnt"] {
            fs::write(f.root.join("500/ns").join(ns), b"host-copy").unwrap();
        }
        session_process(&f, 501, &[r"C:\BeyondATC\BeyondATC.exe", "--type=renderer"]);
        session_process(&f, 502, &[r"C:\BeyondATC\UnityCrashHandler64.exe"]);
        session_process(&f, 503, &["pgrep", "BeyondATC.exe"]);
        let found = respawn_candidates(&f.root, &session, "BeyondATC.exe");
        assert_eq!(found.iter().map(|i| i.pid).collect::<Vec<_>>(), vec![39513]);
    }

    #[test]
    fn adoption_keeps_the_launch_so_its_token_still_reaches_the_app() {
        let f = Fixture::new(true);
        session_process(&f, 200, &[r"C:\BeyondATC\BeyondATC.exe"]);
        session_process(&f, 201, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = Runtime::default();
        let old = owned(&f, 200);
        let key = old.request.match_key();
        runtime
            .owned
            .lock()
            .unwrap()
            .insert(key.clone(), old.clone());
        let token = runtime.owned_token(&old.request, 200).unwrap();
        runtime
            .owned
            .lock()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .updating = Some(std::time::Instant::now());
        assert!(runtime.updating(&key).is_some());

        let next = runtime
            .adopt_respawn(&old, Identity::read(&f.root, 201).unwrap())
            .unwrap();
        assert_eq!(next.identity.pid, 201);
        assert_eq!(next.launch, token.launch);
        assert!(runtime.updating(&key).is_none());
        assert!(runtime.is_current(&next) && !runtime.is_current(&old));
        // The stale record cannot adopt twice, and a new launch is not reached.
        assert!(runtime
            .adopt_respawn(&old, Identity::read(&f.root, 200).unwrap())
            .is_none());
        let mut other = next.clone();
        other.launch = 8;
        runtime
            .owned
            .lock()
            .unwrap()
            .insert(key.clone(), other.clone());
        assert!(runtime
            .adopt_respawn(&next, Identity::read(&f.root, 200).unwrap())
            .is_none());
    }

    #[test]
    fn a_close_while_updating_is_remembered_not_lost() {
        let f = Fixture::new(true);
        session_process(&f, 300, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = Runtime::default();
        let mut away = owned(&f, 300);
        away.updating = Some(std::time::Instant::now());
        let key = away.request.match_key();
        runtime
            .owned
            .lock()
            .unwrap()
            .insert(key.clone(), away.clone());
        assert!(matches!(
            runtime.close_when_back(&away),
            Err(Error::Updating)
        ));
        assert!(runtime.owned.lock().unwrap()[&key].close_on_return);
        assert!(Error::Updating
            .to_string()
            .contains("closed when it is back"));
    }

    #[test]
    fn a_failed_close_quotes_what_taskkill_said() {
        use std::os::unix::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: b"Close message sent to top-level windows of process with PID 32.\nError: Process with PID 32 could not be terminated.\n".to_vec(),
            stderr: b"0024:fixme:ntdll:NtQuerySystemInformation info_class 0xb5\n".to_vec(),
        };
        assert_eq!(
            said(&output),
            "Error: Process with PID 32 could not be terminated."
        );
        assert_eq!(
            said(&std::process::Output {
                stdout: vec![],
                stderr: vec![],
                ..output
            }),
            ""
        );
    }

    /// A runtime that reads the fixture's process table.
    fn fixture_runtime(f: &Fixture) -> Runtime {
        Runtime {
            root: Some(Arc::new(f.root.clone())),
            ..Runtime::default()
        }
    }

    fn register(runtime: &Runtime, owned: &Owned) {
        runtime
            .owned
            .lock()
            .unwrap()
            .insert(owned.request.match_key(), owned.clone());
    }

    fn exits(f: &Fixture, pid: u32) {
        let path = f.root.join(pid.to_string()).join("stat");
        let stat = fs::read_to_string(&path).unwrap();
        fs::write(path, stat.replace(") S ", ") Z ")).unwrap();
    }

    fn recorder() -> (Sink, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&lines);
        (
            Arc::new(move |line: &str| into.lock().unwrap().push(line.to_owned())),
            lines,
        )
    }

    /// BATC as observed: launched 05:17:58, Stop at 05:26:37,
    /// still open when `CLOSE_WAIT` ran out because its own dialog asked
    /// "Are you sure…?", Confirm 25 s later, exit at 9 min 22 s. The close is
    /// a success, and the exit is never read as an update restart.
    #[tokio::test]
    async fn the_l3_timeline_closes_normally_and_never_reads_as_updating() {
        let f = Fixture::new(true);
        session_process(&f, 252944, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        let mut batc = owned(&f, 252944);
        batc.launched = std::time::Instant::now()
            .checked_sub(Duration::from_secs(9 * 60 + 22))
            .unwrap();
        let key = batc.request.match_key();
        register(&runtime, &batc);
        runtime.close_requested(&key);
        let (sink, lines) = recorder();

        // The close request went and `CLOSE_WAIT` ran out: watch, never resend.
        let waiting = {
            let runtime = runtime.clone();
            let batc = batc.clone();
            let sink = sink.clone();
            tokio::spawn(
                async move { runtime.confirm_wait(&batc, Some(&sink), CONFIRM_WAIT).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(runtime.confirming(&key).is_some(), "the row says Closing");
        assert_eq!(*lines.lock().unwrap(), vec![CONFIRM_NOTE.to_owned()]);
        exits(&f, 252944); // Confirm
        waiting.await.unwrap().unwrap();
        assert!(runtime.confirming(&key).is_none());

        // The watch sees the exit inside the respawn window: final.
        assert!(runtime.await_respawn(&batc, Some(&sink)).await.is_none());
        assert!(runtime.updating(&key).is_none());
        assert!(!lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("restart after an update")));
    }

    /// Nobody confirms: after the extra wait the existing message stands, and
    /// a later exit (closing the window by hand) is still final.
    #[tokio::test]
    async fn an_app_still_open_after_the_extra_wait_reports_stayed_open() {
        let f = Fixture::new(true);
        session_process(&f, 252944, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        let batc = owned(&f, 252944);
        let key = batc.request.match_key();
        register(&runtime, &batc);
        runtime.close_requested(&key);
        let error = runtime
            .confirm_wait(&batc, None, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Could not close the app: the app stayed open; close its window manually."
        );
        assert!(runtime.confirming(&key).is_none());
        exits(&f, 252944);
        assert!(runtime.await_respawn(&batc, None).await.is_none());
        assert!(runtime.updating(&key).is_none());
    }

    /// An exit nobody asked for still follows the update-restart rule, even
    /// with a close request on record from before this launch.
    #[tokio::test]
    async fn an_unrequested_exit_still_waits_for_and_adopts_the_restart() {
        let f = Fixture::new(true);
        session_process(&f, 38377, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        runtime.close_requested("BeyondATC.exe"); // an earlier launch's Stop
        let mut batc = owned(&f, 38377);
        batc.launched = std::time::Instant::now() + Duration::from_millis(1);
        let key = batc.request.match_key();
        register(&runtime, &batc);
        exits(&f, 38377);
        session_process(&f, 39513, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let (sink, lines) = recorder();
        let next = runtime.await_respawn(&batc, Some(&sink)).await.unwrap();
        assert_eq!(next.identity.pid, 39513);
        assert_eq!(next.launch, batc.launch);
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("waiting for it to restart after an update")));
        assert!(runtime.updating(&key).is_none());
    }

    #[tokio::test]
    async fn an_exited_owned_process_is_not_replaced_by_an_adopted_target() {
        let f = Fixture::new(false);
        let session = Arc::new(Session::discover(&f.root, &f.env).unwrap());
        let request = f.request();
        let old = Owned {
            session,
            identity: Identity {
                pid: std::process::id(),
                start: 0,
            },
            request,
            launch: 1,
            launched: std::time::Instant::now(),
            updating: None,
            close_on_return: false,
        };
        // Deliberately stale identity: must return before preparing any Wine command.
        assert_eq!(request_close(&old).await.unwrap(), Requested::Closed);
        let runtime = Runtime::default();
        let mut replacement = old.clone();
        replacement.identity.start = 1;
        runtime
            .owned
            .lock()
            .unwrap()
            .insert(old.request.match_key(), replacement.clone());
        runtime.remove(&old);
        assert!(runtime.is_current(&replacement));
    }

    /// Rewrite a fixture process's `stat` start time and `cmdline`: the
    /// kernel handed its pid to another process.
    fn recycle(f: &Fixture, pid: u32, start: u64, args: &[&str]) {
        let dir = f.root.join(pid.to_string());
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[19] = start.to_string();
        fs::write(dir.join("stat"), format!("{pid} (x) {}", fields.join(" "))).unwrap();
        fs::write(dir.join("cmdline"), args.join("\0") + "\0").unwrap();
    }

    /// Stop on a respawned BATC, before the watch's next tick has seen the
    /// exit: the record still names the exited pid. The Stop reaches the
    /// update restart in the launch's own session, keeps the launch id, and
    /// never the host-side copy with the same exe.
    #[test]
    fn stop_with_a_stale_record_reaches_the_update_restart() {
        let f = Fixture::new(true);
        session_process(&f, 38377, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        let batc = owned(&f, 38377);
        register(&runtime, &batc);
        exits(&f, 38377);
        session_process(&f, 39513, &[r"C:\BeyondATC\BeyondATC.exe"]);
        f.process(500, &[r"Z:\companion\BeyondATC.exe"], 500);
        for ns in ["user", "mnt"] {
            fs::write(f.root.join("500/ns").join(ns), b"host-copy").unwrap();
        }

        let (next, adopted) = runtime.current_instance(batc.clone()).unwrap().unwrap();
        assert_eq!(next.identity.pid, 39513);
        assert_eq!(next.launch, batc.launch, "the same launch, followed");
        assert!(adopted);
        assert!(runtime.is_current(&next) && !runtime.is_current(&batc));

        // A live record is its own target, unchanged.
        let (same, adopted) = runtime.current_instance(next.clone()).unwrap().unwrap();
        assert_eq!(same.identity, next.identity);
        assert!(!adopted);
    }

    /// Two same-exe processes in the session after the exit: refused.
    #[test]
    fn stop_with_a_stale_record_and_two_restarts_is_refused() {
        let f = Fixture::new(true);
        session_process(&f, 38377, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        let batc = owned(&f, 38377);
        register(&runtime, &batc);
        exits(&f, 38377);
        session_process(&f, 39513, &[r"C:\BeyondATC\BeyondATC.exe"]);
        session_process(&f, 39600, &[r"C:\BeyondATC\BeyondATC.exe"]);
        assert!(matches!(
            runtime.current_instance(batc.clone()),
            Err(Error::Copies)
        ));
        assert!(runtime.is_current(&batc), "nothing adopted");
    }

    /// The owned pid now belongs to another program (a recycled pid with a
    /// new start time): not the instance, and nothing to close. The same pid
    /// reused by the update restart itself is the restart, start time and all.
    #[test]
    fn a_recycled_owned_pid_is_never_the_instance() {
        let f = Fixture::new(true);
        session_process(&f, 200, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        let batc = owned(&f, 200);
        register(&runtime, &batc);

        recycle(&f, 200, 999, &["/usr/bin/bash"]);
        assert!(!batc.identity.alive(&f.root));
        assert!(runtime.current_instance(batc.clone()).unwrap().is_none());

        recycle(&f, 200, 999, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let (next, _) = runtime.current_instance(batc.clone()).unwrap().unwrap();
        assert_eq!(
            next.identity,
            Identity {
                pid: 200,
                start: 999
            }
        );
    }

    /// Outside the respawn window, or for an instance only adopted (launch
    /// 0), an exited record is simply gone: a later copy is not reached.
    #[test]
    fn a_stale_record_outside_the_window_or_not_launched_here_is_gone() {
        let f = Fixture::new(true);
        session_process(&f, 300, &[r"C:\BeyondATC\BeyondATC.exe"]);
        let runtime = fixture_runtime(&f);
        let mut late = owned(&f, 300);
        late.launched = std::time::Instant::now()
            .checked_sub(crate::respawn::RESPAWN_WINDOW)
            .unwrap();
        register(&runtime, &late);
        exits(&f, 300);
        session_process(&f, 301, &[r"C:\BeyondATC\BeyondATC.exe"]);
        assert!(runtime.current_instance(late.clone()).unwrap().is_none());

        let mut adopted_elsewhere = owned(&f, 301);
        adopted_elsewhere.identity = Identity {
            pid: 300,
            start: 300,
        };
        adopted_elsewhere.launch = 0;
        assert!(runtime
            .current_instance(adopted_elsewhere)
            .unwrap()
            .is_none());
    }
}
