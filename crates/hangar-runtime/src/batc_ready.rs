//! BeyondATC connection evidence from its own logs.
//!
//! An app-specific readiness adapter. HangarRS's own SimConnect handshake, a
//! listening port or Project Airports' detection of BATC cannot stand in for
//! BATC saying it is connected, so this reads what BATC writes and nothing
//! else. The line shapes below were taken from real BATC sessions; the test
//! fixtures are copies of them.
//!
//! # Where BATC says it
//!
//! BATC is a Unity app. It writes `Player.log` under the **simulator** prefix's
//! `LocalLow/Skirmish Mode Games, Inc/BeyondATC/` ([`LOG_DIR`]). On every start
//! Unity renames the old one to `Player-prev.log` and begins a new file, so the
//! directory only ever holds two runs. On BATC 1.10.4 a connected session reads:
//!
//! ```text
//! Mono path[0] = 'C:/BeyondATC/BeyondATC_Data/Managed'
//! [Updater] Latest version: 1.10.4.Experimental, current: 1.10.4.Experimental
//! [SimConnectBridge] SimConnect session opened (SimConnect.dll: …)
//! [SimConnectBridge] SimConnect OPEN: app='SunRise' version=12.2.282174.999 -> Msfs2024
//! …
//! [QuitGuard] wantsToQuit | programStarted=True | approved=True | allowQuit=True
//! [Simconnect] SimConnect DLL client closed
//! ```
//!
//! `SimConnect OPEN` names the simulator's own application name and version,
//! which only arrive in the simulator's reply to the client's open request, so
//! it is the completed handshake as BATC saw it rather than a socket.
//!
//! # The honesty rules, as in `readiness.rs`
//!
//! * **Only lines written after the watch began count.** [`Watch::begin`]
//!   records where `Player.log` had got to; a file Unity rotated in since then
//!   is a new run and is read from its start.
//! * **Unknown stays unknown.** No log, an unreadable directory, a version this
//!   adapter has not seen, or a version line that has not been written yet all
//!   read as [`Connection::Unverified`] with the reason. No marker is inferred
//!   from a version it was not observed in.
//! * **Absence is not failure.** The one explicit failure BATC has been seen
//!   to log is its `Flight Simulator is not running!` dialog (1.10.2, in the
//!   companion prefix). Only that reads as [`Connection::Failed`]; a
//!   connection that never comes is `Unverified`, and after the caller's
//!   timeout it says so.

use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// BATC's log directory, relative to a prefix's `pfx/`.
pub const LOG_DIR: &str =
    "drive_c/users/steamuser/AppData/LocalLow/Skirmish Mode Games, Inc/BeyondATC";

/// The running (or most recent) instance's log.
pub const PLAYER_LOG: &str = "Player.log";

/// Where Unity moves [`PLAYER_LOG`] when the next instance starts. Not read:
/// it belongs to an instance that has already been replaced.
pub const PREVIOUS_LOG: &str = "Player-prev.log";

/// Versions whose log lines this adapter was built from. Exact strings, as
/// BATC's updater prints them. Anything else is reported, not interpreted.
pub const SUPPORTED_VERSIONS: &[&str] = &["1.10.4.Experimental"];

/// How much of the log just before the watch's offset is kept, to notice a
/// file that was truncated and rewritten in place.
const FINGERPRINT: u64 = 64;

/// The log directory inside a prefix. `pfx` is the prefix's `pfx/` directory,
/// e.g. `…/compatdata/2537590/pfx`.
pub fn log_dir(pfx: &Path) -> PathBuf {
    pfx.join(LOG_DIR)
}

/// One thing BATC's log says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// `Mono path[0] = '…'`, the first line of every run. Names which install
    /// ran: `C:/BeyondATC` in the simulator prefix, a `Z:/…` path for a copy
    /// elsewhere. Also the boundary of a run: nothing before it belongs to
    /// the instance after it.
    Started { install_dir: String },
    /// `[Updater] Latest version: …, current: …`.
    Version { current: String, latest: String },
    /// `[SimConnectBridge] SimConnect session opened (…)`: the DLL client was
    /// created. Not a handshake.
    BridgeOpened,
    /// `[SimConnectBridge] SimConnect OPEN: …`: the simulator answered.
    Connected { detail: String },
    /// `[Simconnect] SimConnect DLL client closed`, or a line naming
    /// `SIMCONNECT_RECV_QUIT`.
    Closed { detail: String },
    /// `[QuitGuard] wantsToQuit … allowQuit=True`: BATC is exiting. Every run
    /// that exited ends with this field set, whether or not a prompt was
    /// approved (`approved=False` when nothing had started yet).
    QuitAllowed,
    /// `ERROR DIALOG: Flight Simulator is not running! …`: BATC looked for the
    /// simulator and did not find it.
    SimNotRunning { message: String },
    /// Any other `ERROR DIALOG: …`, shown to the person. Recorded, never read
    /// as a connection failure: those observed (a flight-plan import error, a
    /// missing SimBrief ID) came in sessions that connected or would have.
    ErrorDialog { message: String },
}

/// A piece of evidence and the line it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// 1-based line number in the file it was read from.
    pub line: usize,
    /// The line as written, without its line ending.
    pub text: String,
    pub evidence: Evidence,
}

/// Read log text. `first_line` is the number of the first line in `text`.
///
/// Lines end in `\r\n` (some in `\r\r\n`); both are handled. Unrecognised
/// lines are stepped over — most of the file is Vulkan, Unity and model
/// loading output.
pub fn parse(text: &str, first_line: usize) -> Vec<Found> {
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut found = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        let Some(evidence) = recognise(line, lines.get(i + 1).map(|n| n.trim())) else {
            continue;
        };
        found.push(Found {
            line: first_line + i,
            text: (*raw).to_string(),
            evidence,
        });
    }
    found
}

fn recognise(line: &str, next: Option<&str>) -> Option<Evidence> {
    if let Some(rest) = line.strip_prefix("Mono path[0] = '") {
        let path = rest.strip_suffix('\'').unwrap_or(rest);
        let install_dir = path
            .strip_suffix("/BeyondATC_Data/Managed")
            .unwrap_or(path)
            .to_string();
        return Some(Evidence::Started { install_dir });
    }
    if let Some(rest) = line.strip_prefix("[Updater] Latest version: ") {
        let (latest, current) = rest.split_once(", current: ")?;
        return Some(Evidence::Version {
            current: current.trim().to_string(),
            latest: latest.trim().to_string(),
        });
    }
    if line.starts_with("[SimConnectBridge] SimConnect session opened") {
        return Some(Evidence::BridgeOpened);
    }
    if let Some(rest) = line.strip_prefix("[SimConnectBridge] SimConnect OPEN: ") {
        return Some(Evidence::Connected {
            detail: rest.to_string(),
        });
    }
    if line.starts_with("[Simconnect] SimConnect DLL client closed") {
        return Some(Evidence::Closed {
            detail: "SimConnect client closed".to_string(),
        });
    }
    if line.contains("SIMCONNECT_RECV_QUIT") {
        return Some(Evidence::Closed {
            detail: "the simulator quit".to_string(),
        });
    }
    if line.starts_with("[QuitGuard] wantsToQuit") && line.contains("allowQuit=True") {
        return Some(Evidence::QuitAllowed);
    }
    if let Some(rest) = line.strip_prefix("ERROR DIALOG:") {
        let rest = rest.trim();
        // The dialog's text continues on the next line when this one ends
        // with a colon: `Failed to read MSFS flight plan:` / `PLN file has …`.
        let message = match next {
            Some(n) if rest.ends_with(':') && !n.is_empty() => format!("{rest} {n}"),
            _ => rest.to_string(),
        };
        if message.starts_with("Flight Simulator is not running") {
            return Some(Evidence::SimNotRunning { message });
        }
        return Some(Evidence::ErrorDialog { message });
    }
    None
}

/// Whether BATC is connected, as far as its own log says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// BATC reported the simulator's reply in this run, and nothing since says
    /// the connection ended. Not a liveness check: the caller still owns
    /// whether the process and the simulator are up.
    Connected { detail: String },
    /// Connected in this run, then closed.
    Disconnected { detail: String },
    /// BATC said, in this run, that it could not find the simulator, and has
    /// not connected since.
    Failed { reason: String },
    /// No evidence this adapter can vouch for.
    Unverified(Why),
}

/// Why the connection is not verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// No `Player.log` in the directory.
    NoLog,
    /// The log exists but nothing has been written to it since the watch began.
    NoNewLines,
    /// The directory or log could not be read.
    Unreadable(String),
    /// This run is writing its log but has not opened SimConnect.
    Starting,
    /// The SimConnect client was created; the simulator has not answered.
    Opening,
    /// This run closed before it ever connected.
    ClosedBeforeConnecting,
    /// BATC's updater found a newer version and nothing has connected since.
    /// In every observed case the instance then exited and a new one started.
    UpdateOffered { current: String, latest: String },
    /// A connection marker was seen, but no version line in this run.
    VersionUnknown,
    /// A version this adapter has no evidence for.
    UnsupportedVersion(String),
    /// Nothing connected within the caller's timeout, measured from the start
    /// of the watch or of the latest run, whichever is later.
    TimedOut { waited: Duration },
}

impl Connection {
    pub fn is_connected(&self) -> bool {
        matches!(self, Connection::Connected { .. })
    }

    /// A line for the UI.
    pub fn describe(&self) -> String {
        match self {
            Connection::Connected { .. } => "connected".to_string(),
            Connection::Disconnected { detail } => format!("disconnected \u{2014} {detail}"),
            Connection::Failed { reason } => format!("not connected \u{2014} {reason}"),
            Connection::Unverified(why) => match why {
                Why::NoLog | Why::NoNewLines => "connection unverified".to_string(),
                Why::Unreadable(e) => format!("connection unverified \u{2014} log unreadable: {e}"),
                Why::Starting => "starting".to_string(),
                Why::Opening => "connecting to the simulator".to_string(),
                Why::ClosedBeforeConnecting => "closed before connecting".to_string(),
                Why::UpdateOffered { current, latest } => {
                    format!("updating {current} \u{2192} {latest}")
                }
                Why::VersionUnknown => {
                    "connection unverified \u{2014} version not reported".to_string()
                }
                Why::UnsupportedVersion(v) => {
                    format!("connection unverified \u{2014} version {v} not recognised")
                }
                Why::TimedOut { waited } => format!(
                    "connection unverified \u{2014} nothing after {}s",
                    waited.as_secs()
                ),
            },
        }
    }
}

/// Everything one poll found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub connection: Connection,
    /// 0 while the log that existed when the watch began is still the current
    /// one; one more each time a new run's log (or a [`Watch::rearm`]) replaces
    /// the evidence. Lets the caller tie evidence to a process restart.
    pub generation: u32,
    /// The latest run's install, from `Mono path[0]`.
    pub install_dir: Option<String>,
    /// The latest run's version, as BATC's updater printed it.
    pub version: Option<String>,
    /// Error dialogs BATC showed in the latest run, as written.
    pub errors: Vec<String>,
    /// Everything recognised in the latest run, with line numbers.
    pub evidence: Vec<Found>,
}

impl Status {
    fn bare(connection: Connection, generation: u32) -> Self {
        Status {
            connection,
            generation,
            install_dir: None,
            version: None,
            errors: Vec::new(),
            evidence: Vec::new(),
        }
    }
}

/// Judge one run's evidence. Only what follows the last `Started` counts: a
/// second `Mono path[0]` in one file is a second process.
pub fn judge(found: &[Found]) -> Status {
    let run = match found
        .iter()
        .rposition(|f| matches!(f.evidence, Evidence::Started { .. }))
    {
        Some(start) => &found[start..],
        None => found,
    };

    enum State {
        Starting,
        Opening,
        Connected(String),
        Disconnected(String),
        Failed(String),
        ClosedEarly,
    }

    let mut state = State::Starting;
    let mut install_dir = None;
    let mut version: Option<(String, String)> = None;
    let mut errors = Vec::new();

    for f in run {
        match &f.evidence {
            Evidence::Started { install_dir: dir } => install_dir = Some(dir.clone()),
            Evidence::Version { current, latest } => {
                version = Some((current.clone(), latest.clone()));
            }
            Evidence::BridgeOpened => {
                if !matches!(state, State::Connected(_)) {
                    state = State::Opening;
                }
            }
            Evidence::Connected { detail } => state = State::Connected(detail.clone()),
            Evidence::Closed { detail } => {
                state = match state {
                    State::Connected(_) | State::Disconnected(_) => {
                        State::Disconnected(detail.clone())
                    }
                    State::Failed(reason) => State::Failed(reason),
                    _ => State::ClosedEarly,
                };
            }
            Evidence::QuitAllowed => {
                state = match state {
                    State::Connected(_) => State::Disconnected("BATC is closing".to_string()),
                    keep @ (State::Disconnected(_) | State::Failed(_)) => keep,
                    _ => State::ClosedEarly,
                };
            }
            Evidence::SimNotRunning { message } => {
                errors.push(message.clone());
                state = match state {
                    // Never observed after a connection; read as a loss
                    // rather than a failure to connect.
                    State::Connected(_) | State::Disconnected(_) => {
                        State::Disconnected(message.clone())
                    }
                    _ => State::Failed(message.clone()),
                };
            }
            Evidence::ErrorDialog { message } => errors.push(message.clone()),
        }
    }

    let unsupported = version
        .as_ref()
        .filter(|(current, _)| !SUPPORTED_VERSIONS.contains(&current.as_str()))
        .map(|(current, _)| current.clone());

    let connection = match state {
        State::Connected(_) | State::Disconnected(_) | State::Failed(_) if version.is_none() => {
            Connection::Unverified(Why::VersionUnknown)
        }
        State::Connected(_) | State::Disconnected(_) | State::Failed(_)
            if unsupported.is_some() =>
        {
            Connection::Unverified(Why::UnsupportedVersion(
                unsupported.clone().unwrap_or_default(),
            ))
        }
        State::Connected(detail) => Connection::Connected { detail },
        State::Disconnected(detail) => Connection::Disconnected { detail },
        State::Failed(reason) => Connection::Failed { reason },
        other => match &version {
            Some((current, latest)) if current != latest => {
                Connection::Unverified(Why::UpdateOffered {
                    current: current.clone(),
                    latest: latest.clone(),
                })
            }
            _ if unsupported.is_some() => {
                Connection::Unverified(Why::UnsupportedVersion(unsupported.unwrap_or_default()))
            }
            _ => Connection::Unverified(match other {
                State::Opening => Why::Opening,
                State::ClosedEarly => Why::ClosedBeforeConnecting,
                _ => Why::Starting,
            }),
        },
    };

    Status {
        connection,
        generation: 0,
        install_dir,
        version: version.map(|(current, _)| current),
        errors,
        evidence: run.to_vec(),
    }
}

/// Which file, and where in it the watch's evidence starts.
#[derive(Debug, Clone)]
struct Generation {
    /// Device and inode. Unity rotates by renaming, so a new run is a new inode.
    id: (u64, u64),
    /// Byte offset: everything before it was written before the watch.
    from: u64,
    /// Lines wholly before `from`, so line numbers stay true to the file.
    lines_before: usize,
    /// The bytes just before `from`, to notice an in-place rewrite.
    tail: Vec<u8>,
}

/// What was there when the watch (re)began.
#[derive(Debug, Clone)]
enum Baseline {
    /// A log to read past.
    At(Generation),
    /// No log: whatever appears is new.
    Missing,
    /// It could not be read, so nothing tells an old file from a new one —
    /// except a file born after this moment.
    Unknown(SystemTime),
}

/// One BATC launch being watched.
#[derive(Debug, Clone)]
pub struct Watch {
    dir: PathBuf,
    baseline: Baseline,
    generation: u32,
    /// When the watch, or the latest run, began — for the timeout.
    since: Instant,
    timeout: Option<Duration>,
}

impl Watch {
    /// Start watching `dir` (see [`log_dir`]). Call it **before** launching or
    /// adopting BATC: everything already in the log is taken to belong to an
    /// earlier run.
    pub fn begin(dir: &Path) -> Self {
        Watch {
            dir: dir.to_path_buf(),
            baseline: snapshot(&dir.join(PLAYER_LOG)),
            generation: 0,
            since: Instant::now(),
            timeout: None,
        }
    }

    /// Report [`Why::TimedOut`] once this long has passed with no connection.
    /// No default: how long BATC takes to connect has not been measured.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The instance being watched exited and another is expected (an update
    /// respawn). Everything written so far is the old instance's. Call it when
    /// the exit is seen, not after the new instance appears, or its first
    /// lines are skipped — though a respawn normally rotates the log anyway,
    /// and a rotated-in log is read from its start.
    pub fn rearm(&mut self) {
        self.baseline = snapshot(&self.dir.join(PLAYER_LOG));
        self.generation += 1;
        self.since = Instant::now();
    }

    /// Read what has been written since the watch began.
    pub fn poll(&mut self) -> Status {
        self.poll_at(Instant::now())
    }

    /// [`Watch::poll`] with the clock supplied, for the timeout.
    pub fn poll_at(&mut self, now: Instant) -> Status {
        let path = self.dir.join(PLAYER_LOG);
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return self.waiting(Why::NoLog, now);
            }
            Err(e) => return self.bare(Connection::Unverified(Why::Unreadable(e.to_string()))),
        };
        let id = (meta.dev(), meta.ino());

        let read = match &self.baseline {
            Baseline::At(g) if g.id == id => read_generation(&path, g),
            Baseline::Unknown(at) if !born_after(&meta, *at) => {
                return self.bare(Connection::Unverified(Why::Unreadable(
                    "the log could not be read when the watch began, so its lines cannot be \
                     told from an earlier run's"
                        .to_string(),
                )));
            }
            _ => Ok(None),
        };

        let (text, lines_before) = match read {
            Ok(Some(found)) => found,
            Ok(None) => {
                // A different file, or the same one rewritten: a new run.
                self.baseline = Baseline::At(Generation {
                    id,
                    from: 0,
                    lines_before: 0,
                    tail: Vec::new(),
                });
                self.generation += 1;
                self.since = now;
                match read_from(&path, 0) {
                    Ok(text) => (text, 0),
                    Err(e) => {
                        return self.bare(Connection::Unverified(Why::Unreadable(e.to_string())));
                    }
                }
            }
            Err(e) => return self.bare(Connection::Unverified(Why::Unreadable(e.to_string()))),
        };

        let complete = complete_lines(&text);
        if complete.trim().is_empty() {
            return self.waiting(Why::NoNewLines, now);
        }

        let mut status = judge(&parse(complete, lines_before + 1));
        status.generation = self.generation;
        if let Connection::Unverified(Why::Starting | Why::Opening) = status.connection {
            if let Some(timed_out) = self.timed_out(now) {
                status.connection = Connection::Unverified(timed_out);
            }
        }
        status
    }

    fn waiting(&self, why: Why, now: Instant) -> Status {
        let why = self.timed_out(now).unwrap_or(why);
        self.bare(Connection::Unverified(why))
    }

    fn timed_out(&self, now: Instant) -> Option<Why> {
        let waited = now.saturating_duration_since(self.since);
        self.timeout
            .filter(|t| waited >= *t)
            .map(|_| Why::TimedOut { waited })
    }

    fn bare(&self, connection: Connection) -> Status {
        Status::bare(connection, self.generation)
    }
}

/// Where the log is now, so only what follows counts.
fn snapshot(path: &Path) -> Baseline {
    let now = SystemTime::now();
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Baseline::Missing,
        Err(_) => return Baseline::Unknown(now),
    };
    let Ok(bytes) = fs::read(path) else {
        return Baseline::Unknown(now);
    };
    let from = bytes.len() as u64;
    let tail_start = bytes.len().saturating_sub(FINGERPRINT as usize);
    Baseline::At(Generation {
        id: (meta.dev(), meta.ino()),
        from,
        lines_before: bytes.iter().filter(|b| **b == b'\n').count(),
        tail: bytes[tail_start..].to_vec(),
    })
}

/// Text past the generation's offset, with the line count before it — or
/// `None` if the file is no longer the one the offset belongs to: shorter than
/// it, or different just before it.
///
/// A line that was half-written when the watch began is the old run's, so
/// the text starts after its end.
fn read_generation(path: &Path, g: &Generation) -> io::Result<Option<(String, usize)>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < g.from {
        return Ok(None);
    }
    let tail_len = g.tail.len() as u64;
    file.seek(SeekFrom::Start(g.from - tail_len))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.get(..g.tail.len()) != Some(g.tail.as_slice()) {
        return Ok(None);
    }
    let mut rest = &bytes[g.tail.len()..];
    let mut lines_before = g.lines_before;
    let mid_line = g.tail.last().is_some_and(|b| *b != b'\n');
    if mid_line {
        match rest.iter().position(|b| *b == b'\n') {
            Some(end) => {
                rest = &rest[end + 1..];
                lines_before += 1;
            }
            None => rest = &[],
        }
    }
    Ok(Some((
        String::from_utf8_lossy(rest).into_owned(),
        lines_before,
    )))
}

fn read_from(path: &Path, from: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Drop a last line still being written: `current: 1.10.4.Experim` would read
/// as an unknown version for one poll.
fn complete_lines(text: &str) -> &str {
    match text.rfind('\n') {
        Some(end) => &text[..end],
        None => "",
    }
}

/// Was this file created after `at`? Unknown birth time counts as no.
fn born_after(meta: &Metadata, at: SystemTime) -> bool {
    meta.created().is_ok_and(|born| born > at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pid 252944: connected, flight-plan error dialog, closed by
    /// the person. 24 lines copied from the real `Player.log`.
    const CLOSED: &str =
        include_str!("../tests/fixtures/batc/player-1.10.4-connected-then-closed.log");
    /// pid 66858: copied while it was connected and running.
    const CONNECTED: &str = include_str!("../tests/fixtures/batc/player-1.10.4-connected.log");
    /// Companion prefix: a 1.8.21.EA run whose updater found
    /// 1.10.2 and quit to install it. Two version lines; the second counts.
    const UPDATE_QUIT: &str = include_str!("../tests/fixtures/batc/player-1.8.21-update-quit.log");
    /// Companion prefix: a 1.10.2 run that never found the
    /// simulator, said so three times, and was closed.
    const SIM_NOT_RUNNING: &str =
        include_str!("../tests/fixtures/batc/player-1.10.2-sim-not-running.log");
    /// Unity's shutdown memory report: `Failed count: 927`.
    const NOISE: &str = include_str!("../tests/fixtures/batc/noise-memory-stats.log");

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hangar-batc-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, text: &str) {
        fs::write(dir.join(PLAYER_LOG), text).unwrap();
    }

    fn append(dir: &Path, text: &str) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(dir.join(PLAYER_LOG))
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// Unity's rotation: the old log is renamed, a new one is begun.
    fn rotate(dir: &Path, new_text: &str) {
        fs::rename(dir.join(PLAYER_LOG), dir.join(PREVIOUS_LOG)).unwrap();
        write(dir, new_text);
    }

    /// The lines of a fixture up to and including the first containing `upto`.
    fn until(text: &str, upto: &str) -> String {
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            out.push_str(line);
            if line.contains(upto) {
                break;
            }
        }
        out
    }

    #[test]
    fn the_real_lines_are_recognised_with_their_line_numbers() {
        let found = parse(CLOSED, 1);
        let kinds: Vec<_> = found.iter().map(|f| (f.line, &f.evidence)).collect();
        assert_eq!(
            kinds[0],
            (
                1,
                &Evidence::Started {
                    install_dir: "C:/BeyondATC".to_string()
                }
            )
        );
        assert_eq!(
            kinds[1],
            (
                4,
                &Evidence::Version {
                    current: "1.10.4.Experimental".to_string(),
                    latest: "1.10.4.Experimental".to_string()
                }
            )
        );
        assert!(found.iter().any(|f| f.evidence
            == Evidence::Connected {
                detail: "app='SunRise' version=12.2.282174.999 -> Msfs2024".to_string()
            }));
        assert_eq!(
            found
                .iter()
                .filter(|f| f.evidence == Evidence::BridgeOpened)
                .count(),
            2
        );
        // The line endings are CRLF; none survives into the text.
        assert!(found.iter().all(|f| !f.text.contains('\r')));
    }

    #[test]
    fn the_word_failed_in_unitys_memory_report_is_not_evidence() {
        assert!(parse(NOISE, 1).is_empty());
    }

    #[test]
    fn bare_carriage_returns_are_stepped_over() {
        let found = parse(
            "<RI> Initializing input.\r\r\n[Simconnect] SimConnect DLL client closed\r\r\n",
            1,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
    }

    #[test]
    fn a_two_line_error_dialog_is_kept_whole() {
        let status = judge(&parse(CLOSED, 1));
        assert_eq!(
            status.errors,
            vec![
                "Failed to read MSFS flight plan: PLN file has no destination airport.".to_string()
            ]
        );
    }

    #[test]
    fn a_connected_session_is_connected() {
        let dir = scratch("connected");
        let mut watch = Watch::begin(&dir);
        write(&dir, CONNECTED);

        let status = watch.poll();
        assert!(status.connection.is_connected(), "{status:?}");
        assert_eq!(status.version.as_deref(), Some("1.10.4.Experimental"));
        assert_eq!(status.install_dir.as_deref(), Some("C:/BeyondATC"));
        assert_eq!(status.generation, 1);
        assert_eq!(status.connection.describe(), "connected");
    }

    #[test]
    fn a_flight_plan_error_does_not_undo_a_connection() {
        let status = judge(&parse(&until(CLOSED, "destination airport"), 1));
        assert!(status.connection.is_connected(), "{status:?}");
        assert_eq!(status.errors.len(), 1);
    }

    #[test]
    fn a_run_that_never_connects_stays_unverified_and_then_times_out() {
        let dir = scratch("never");
        let mut watch = Watch::begin(&dir).with_timeout(Duration::from_secs(90));
        write(&dir, &until(CONNECTED, "Detected Microsoft"));

        let start = Instant::now();
        let status = watch.poll_at(start);
        assert_eq!(status.connection, Connection::Unverified(Why::Starting));

        let later = start + Duration::from_secs(91);
        match watch.poll_at(later).connection {
            Connection::Unverified(Why::TimedOut { waited }) => {
                assert!(waited >= Duration::from_secs(90));
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn an_opened_client_without_an_answer_is_not_a_connection() {
        let status = judge(&parse(&until(CONNECTED, "CAMERA STATE -1"), 1));
        assert_eq!(status.connection, Connection::Unverified(Why::Opening));
    }

    #[test]
    fn closing_before_connecting_is_not_a_disconnect() {
        let mut text = until(CONNECTED, "Flight Simulator Build");
        text.push_str(
            "[QuitGuard] wantsToQuit | programStarted=True | approved=True | allowQuit=True\r\n",
        );
        let status = judge(&parse(&text, 1));
        assert_eq!(
            status.connection,
            Connection::Unverified(Why::ClosedBeforeConnecting)
        );
    }

    #[test]
    fn a_quit_prompt_that_was_not_approved_changes_nothing() {
        let mut text = CONNECTED.to_string();
        text.push_str(
            "[QuitGuard] wantsToQuit | programStarted=True | approved=False | allowQuit=False\r\n",
        );
        assert!(judge(&parse(&text, 1)).connection.is_connected());
    }

    #[test]
    fn an_update_run_reads_as_updating_not_failed() {
        let status = judge(&parse(UPDATE_QUIT, 1));
        assert_eq!(
            status.connection,
            Connection::Unverified(Why::UpdateOffered {
                current: "1.8.21.EA".to_string(),
                latest: "1.10.2.Experimental".to_string(),
            })
        );
        assert_eq!(status.install_dir.as_deref(), Some("C:/BeyondATC"));
    }

    #[test]
    fn the_install_that_ran_is_named() {
        // The shape a stale companion copy writes when run in the
        // simulator's session; it is what tells the two installs apart.
        let line = "Mono path[0] = 'Z:/home/user/.local/share/msfs-manager/companion-prefix/pfx/drive_c/BeyondATC/BeyondATC_Data/Managed'";
        assert_eq!(
            parse(line, 1)[0].evidence,
            Evidence::Started {
                install_dir:
                    "Z:/home/user/.local/share/msfs-manager/companion-prefix/pfx/drive_c/BeyondATC"
                        .to_string()
            }
        );
    }

    #[test]
    fn the_real_sim_not_running_session_names_its_version_and_its_errors() {
        let status = judge(&parse(SIM_NOT_RUNNING, 1));
        // 1.10.2: the dialog is recorded but not interpreted.
        assert_eq!(
            status.connection,
            Connection::Unverified(Why::UnsupportedVersion("1.10.2.Experimental".to_string()))
        );
        let not_running = "Flight Simulator is not running! Please start the simulator before starting BeyondATC.";
        assert_eq!(
            status.errors.iter().filter(|e| *e == not_running).count(),
            3
        );
        assert!(status
            .errors
            .iter()
            .any(|e| e == "Must input a valid Simbrief ID!"));
    }

    #[test]
    fn sim_not_running_on_a_supported_version_is_a_failure() {
        // The 1.10.2 session with its version line changed: the dialog has
        // not been observed on 1.10.4.
        let text = SIM_NOT_RUNNING.replace(
            "Latest version: 1.10.2.Experimental, current: 1.10.2.Experimental",
            "Latest version: 1.10.4.Experimental, current: 1.10.4.Experimental",
        );
        assert_eq!(
            judge(&parse(&text, 1)).connection,
            Connection::Failed {
                reason: "Flight Simulator is not running! Please start the simulator before starting BeyondATC.".to_string()
            }
        );
    }

    #[test]
    fn a_connection_after_sim_not_running_is_connected() {
        let mut text = until(CONNECTED, "Flight Simulator Build");
        text.push_str("ERROR DIALOG: Flight Simulator is not running! Please start the simulator before starting BeyondATC.\r\n");
        text.push_str(&CONNECTED[until(CONNECTED, "Flight Simulator Build").len()..]);
        assert!(judge(&parse(&text, 1)).connection.is_connected());
    }

    #[test]
    fn a_quit_with_nothing_started_reads_as_closing_without_approval() {
        // `approved=False | allowQuit=True`, the last line of the real
        // sim-not-running session.
        let quit = SIM_NOT_RUNNING.lines().last().unwrap();
        assert!(quit.contains("approved=False"));
        let mut text = until(CONNECTED, "Flight Simulator Build");
        text.push_str(quit);
        text.push('\n');
        assert_eq!(
            judge(&parse(&text, 1)).connection,
            Connection::Unverified(Why::ClosedBeforeConnecting)
        );
    }

    #[test]
    fn a_disconnect_after_connecting_is_reported() {
        let dir = scratch("disconnect");
        let mut watch = Watch::begin(&dir);
        let connected = until(CLOSED, "facility definitions");
        write(&dir, &connected);
        assert!(watch.poll().connection.is_connected());

        append(&dir, &CLOSED[connected.len()..]);
        let status = watch.poll();
        assert_eq!(
            status.connection,
            Connection::Disconnected {
                detail: "SimConnect client closed".to_string()
            }
        );
        // Same file, same run.
        assert_eq!(status.generation, 1);
    }

    #[test]
    fn the_quit_guard_alone_already_reads_as_closing() {
        let status = judge(&parse(&until(CLOSED, "approved=True"), 1));
        assert_eq!(
            status.connection,
            Connection::Disconnected {
                detail: "BATC is closing".to_string()
            }
        );
    }

    #[test]
    fn a_simulator_quit_after_connecting_is_a_disconnect() {
        // The token was recorded from a rotated-away 1.10.4 log; its full line
        // shape is not on disk, so only the token is matched.
        let mut text = CONNECTED.to_string();
        text.push_str("SIMCONNECT_RECV_QUIT\r\n");
        assert_eq!(
            judge(&parse(&text, 1)).connection,
            Connection::Disconnected {
                detail: "the simulator quit".to_string()
            }
        );
    }

    #[test]
    fn lines_from_before_the_watch_do_not_count() {
        let dir = scratch("stale");
        write(&dir, CLOSED);
        let mut watch = Watch::begin(&dir);

        assert_eq!(
            watch.poll().connection,
            Connection::Unverified(Why::NoNewLines)
        );

        // The old instance is still writing (its shutdown report): still
        // nothing that says anything about a connection.
        append(&dir, NOISE);
        let status = watch.poll();
        assert_eq!(status.connection, Connection::Unverified(Why::Starting));
        assert_eq!(status.generation, 0);
        assert!(status.evidence.is_empty());
    }

    #[test]
    fn a_connected_run_before_the_watch_cannot_certify_the_next_launch() {
        let dir = scratch("stale-connected");
        write(&dir, CONNECTED);
        let mut watch = Watch::begin(&dir);
        assert!(!watch.poll().connection.is_connected());
    }

    #[test]
    fn a_rotation_between_begin_and_poll_reads_the_new_run_from_its_start() {
        let dir = scratch("rotate");
        write(&dir, CLOSED);
        let mut watch = Watch::begin(&dir);

        rotate(&dir, CONNECTED);
        let status = watch.poll();
        assert!(status.connection.is_connected(), "{status:?}");
        assert_eq!(status.generation, 1);
        assert_eq!(status.evidence[0].line, 1);
    }

    #[test]
    fn a_rotation_drops_the_previous_runs_connection() {
        let dir = scratch("rotate-drop");
        let mut watch = Watch::begin(&dir);
        write(&dir, CONNECTED);
        assert!(watch.poll().connection.is_connected());

        // An update respawn: a new instance, which has not connected yet.
        rotate(&dir, &until(CONNECTED, "[Updater]"));
        let status = watch.poll();
        assert_eq!(status.connection, Connection::Unverified(Why::Starting));
        assert_eq!(status.generation, 2);
    }

    #[test]
    fn an_update_and_respawn_ends_connected_on_the_new_run() {
        let dir = scratch("respawn");
        let mut watch = Watch::begin(&dir);
        write(&dir, UPDATE_QUIT);
        assert!(matches!(
            watch.poll().connection,
            Connection::Unverified(Why::UpdateOffered { .. })
        ));

        rotate(&dir, CONNECTED);
        let status = watch.poll();
        assert!(status.connection.is_connected());
        assert_eq!(status.install_dir.as_deref(), Some("C:/BeyondATC"));
    }

    #[test]
    fn a_log_rewritten_in_place_is_a_new_run() {
        let dir = scratch("rewrite");
        write(&dir, CLOSED);
        let mut watch = Watch::begin(&dir);

        // Same inode, truncated and rewritten to beyond the old length with a
        // run that has not connected: the old run's OPEN must not leak in.
        let mut text = until(CONNECTED, "[Updater]");
        while text.len() <= CLOSED.len() {
            text.push_str(NOISE);
        }
        write(&dir, &text);
        let status = watch.poll();
        assert_eq!(status.connection, Connection::Unverified(Why::Starting));
        assert_eq!(status.generation, 1);
    }

    #[test]
    fn a_truncated_log_is_read_whole() {
        let dir = scratch("truncate");
        write(&dir, CLOSED);
        let mut watch = Watch::begin(&dir);
        write(&dir, CONNECTED);
        assert!(CONNECTED.len() < CLOSED.len());
        assert!(watch.poll().connection.is_connected());
    }

    #[test]
    fn a_second_start_line_in_one_file_is_a_second_process() {
        let text = format!("{CONNECTED}{}", until(CONNECTED, "[Updater]"));
        assert_eq!(
            judge(&parse(&text, 1)).connection,
            Connection::Unverified(Why::Starting)
        );
    }

    #[test]
    fn a_half_written_line_is_left_for_the_next_poll() {
        let dir = scratch("partial");
        let mut watch = Watch::begin(&dir);
        let head = until(CONNECTED, "Initialize engine");
        write(
            &dir,
            &format!("{head}[Updater] Latest version: 1.10.4.Experimental, current: 1.10.4.Exper"),
        );
        let status = watch.poll();
        assert_eq!(status.version, None);
        assert_eq!(status.connection, Connection::Unverified(Why::Starting));
    }

    #[test]
    fn a_line_being_written_when_the_watch_began_is_the_old_runs() {
        let dir = scratch("midline");
        write(&dir, &format!("{CLOSED}[SimConnectBridge] SimConnect OP"));
        let mut watch = Watch::begin(&dir);
        append(
            &dir,
            "EN: app='SunRise' version=12.2.282174.999 -> Msfs2024\r\n",
        );
        let status = watch.poll();
        assert!(!status.connection.is_connected(), "{status:?}");
        assert!(status.evidence.is_empty());
    }

    #[test]
    fn line_numbers_stay_true_to_the_file_after_an_offset() {
        let dir = scratch("numbers");
        let head = until(CONNECTED, "Flight Simulator Build");
        write(&dir, &head);
        let mut watch = Watch::begin(&dir);
        append(&dir, &CONNECTED[head.len()..]);
        let status = watch.poll();
        let open = status
            .evidence
            .iter()
            .find(|f| matches!(f.evidence, Evidence::Connected { .. }))
            .unwrap();
        // Line 11 of the fixture, as `parse` from the top would number it.
        assert_eq!(open.line, 11);
        assert_eq!(parse(CONNECTED, 1)[4].line, 11);
    }

    #[test]
    fn an_unrecognised_version_is_named_not_trusted() {
        let text = CONNECTED.replace("1.10.4.Experimental", "1.11.0.Experimental");
        let status = judge(&parse(&text, 1));
        assert_eq!(
            status.connection,
            Connection::Unverified(Why::UnsupportedVersion("1.11.0.Experimental".to_string()))
        );
        assert!(status.connection.describe().contains("1.11.0.Experimental"));
    }

    #[test]
    fn a_connection_with_no_version_line_is_not_trusted() {
        let text: String = CONNECTED
            .split_inclusive('\n')
            .filter(|l| !l.starts_with("[Updater]"))
            .collect();
        assert_eq!(
            judge(&parse(&text, 1)).connection,
            Connection::Unverified(Why::VersionUnknown)
        );
    }

    #[test]
    fn no_log_at_all_is_unverified() {
        let dir = scratch("nolog");
        let mut watch = Watch::begin(&dir);
        assert_eq!(watch.poll().connection, Connection::Unverified(Why::NoLog));

        let missing = dir.join("not-there");
        let mut watch = Watch::begin(&missing);
        assert_eq!(watch.poll().connection, Connection::Unverified(Why::NoLog));
    }

    #[test]
    fn an_unreadable_directory_is_unverified_not_absent() {
        // A file where the directory should be: every read fails with
        // something other than "not found".
        let dir = scratch("unreadable");
        let not_a_dir = dir.join("BeyondATC");
        fs::write(&not_a_dir, "").unwrap();
        let mut watch = Watch::begin(&not_a_dir);
        assert!(matches!(
            watch.poll().connection,
            Connection::Unverified(Why::Unreadable(_))
        ));
    }

    #[test]
    fn a_log_unreadable_at_begin_cannot_certify_its_old_lines_later() {
        let dir = scratch("unknown-baseline");
        write(&dir, CONNECTED);
        let mut watch = Watch::begin(&dir);
        // As if the first read had failed.
        watch.baseline = Baseline::Unknown(SystemTime::now() + Duration::from_secs(3600));
        assert!(matches!(
            watch.poll().connection,
            Connection::Unverified(Why::Unreadable(_))
        ));
    }

    #[test]
    fn rearm_discards_the_instance_that_left() {
        let dir = scratch("rearm");
        let mut watch = Watch::begin(&dir);
        write(&dir, CONNECTED);
        assert!(watch.poll().connection.is_connected());

        watch.rearm();
        let status = watch.poll();
        assert_eq!(status.connection, Connection::Unverified(Why::NoNewLines));
        assert_eq!(status.generation, 2);
    }

    #[test]
    fn the_log_directory_is_under_the_prefix() {
        let pfx = Path::new("/x/compatdata/2537590/pfx");
        assert_eq!(
            log_dir(pfx),
            PathBuf::from(
                "/x/compatdata/2537590/pfx/drive_c/users/steamuser/AppData/LocalLow/Skirmish Mode Games, Inc/BeyondATC"
            )
        );
    }
}
