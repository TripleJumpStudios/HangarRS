//! Where an app is actually running, compared with where its route puts it.
//!
//! Observation and a pure comparison, nothing else. Nothing here launches,
//! adopts, enters a namespace or changes a launch decision. The Flight details
//! view reads the result.
//!
//! Two rules hold throughout:
//!
//! * **Read only allowlisted environment fields.** `WINEPREFIX` is the one
//!   variable taken from a process environment; every other entry is
//!   discarded as it is parsed and nothing else is retained.
//! * **Missing or unreadable evidence stays unknown.** It is never rounded to
//!   a match, and several candidates are never treated as one.
//!
//! The reader takes a `/proc` root, so a scratch tree stands in for the real
//! one in tests — the same injection `sim_session` uses.

use crate::proc;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The real process table.
pub const PROC: &str = "/proc";

/// Same bound `sim_session` reads with: a process's environment is untrusted.
const MAX_PROC: u64 = 2 * 1024 * 1024;

/// A PID plus its kernel start time, so a recycled PID is never the same process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub pid: u32,
    pub start: u64,
}

/// A namespace, identified the way the kernel does: device and inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Namespace {
    dev: u64,
    ino: u64,
}

impl Namespace {
    /// A namespace for fixtures; real ones come only from `observe`.
    #[doc(hidden)]
    pub fn fixture(ino: u64) -> Self {
        Self { dev: 0, ino }
    }
}

/// One piece of evidence, which may be absent or unreadable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fact<T> {
    Known(T),
    /// Read successfully, and not there.
    Missing,
    /// Could not be read; nothing can be concluded.
    Unreadable,
}

/// One process whose `argv[0]` names the executable exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub identity: Identity,
    /// A Chromium helper (`--type=`), not the instance in charge.
    pub helper: bool,
    /// `WINEPREFIX`, canonicalised where the path resolves.
    pub prefix: Fact<PathBuf>,
    pub user_ns: Fact<Namespace>,
    pub mount_ns: Fact<Namespace>,
}

/// Everything found for one match key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Exact `argv[0]` basename matches, lowest PID first.
    pub exact: Vec<Process>,
    /// Basenames that merely end with the key — `MyBATC.exe` for `BATC.exe`.
    /// The monitor's suffix rule would count these; this view does not.
    pub similar: Vec<String>,
}

impl Found {
    pub fn mains(&self) -> impl Iterator<Item = &Process> {
        self.exact.iter().filter(|p| !p.helper)
    }
}

/// One immutable read of the process table, for a fixed set of keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub taken: SystemTime,
    /// HangarRS's own namespaces: the host side of every comparison.
    pub host_user: Fact<Namespace>,
    pub host_mount: Fact<Namespace>,
    /// Per match key. The simulator's key is always present.
    pub apps: BTreeMap<String, Found>,
}

impl Observation {
    /// An observation in which nothing is running, for inert states.
    pub fn empty(keys: &[String]) -> Self {
        let mut apps: BTreeMap<_, _> = keys.iter().map(|k| (k.clone(), Found::default())).collect();
        apps.entry(crate::sim::MSFS_2024_EXE.into()).or_default();
        Self {
            taken: SystemTime::now(),
            host_user: Fact::Unreadable,
            host_mount: Fact::Unreadable,
            apps,
        }
    }

    pub fn found(&self, key: &str) -> Option<&Found> {
        self.apps.get(key)
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

/// The identity of `pid` under `root` now: `None` for a zombie, an exited or
/// an unreadable process. Compare it with a remembered [`Identity`] to tell
/// the same process from a recycled pid.
pub fn read_identity(root: &Path, pid: u32) -> Option<Identity> {
    identity(root, pid)
}

/// Start time from `stat`; `None` for a zombie, an exited or unreadable process.
fn identity(root: &Path, pid: u32) -> Option<Identity> {
    let stat = fs::read_to_string(root.join(pid.to_string()).join("stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
    if fields.first().is_none_or(|s| matches!(*s, "Z" | "X")) {
        return None;
    }
    Some(Identity {
        pid,
        start: fields.get(19)?.parse().ok()?,
    })
}

fn namespace(path: &Path) -> Fact<Namespace> {
    match fs::metadata(path) {
        Ok(m) => Fact::Known(Namespace {
            dev: m.dev(),
            ino: m.ino(),
        }),
        Err(_) => Fact::Unreadable,
    }
}

/// `WINEPREFIX` and nothing else. Other entries are skipped unparsed.
fn wineprefix(raw: &[u8]) -> Fact<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    raw.split(|b| *b == 0)
        .find_map(|entry| entry.strip_prefix(b"WINEPREFIX="))
        .map_or(Fact::Missing, |value| {
            let path = Path::new(std::ffi::OsStr::from_bytes(value));
            Fact::Known(canonical(path))
        })
}

/// Canonical where it resolves; otherwise as written, so it can still be shown.
pub fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// Read the process table once for `keys` plus the simulator.
///
/// Read-only: `cmdline`, `stat`, the `ns/` links and — for exact matches only
/// — `environ`, bounded, from which only `WINEPREFIX` is kept.
pub fn observe(root: &Path, keys: &[String]) -> Observation {
    let mut out = Observation::empty(keys);
    out.taken = SystemTime::now();
    out.host_user = namespace(&root.join("self/ns/user"));
    out.host_mount = namespace(&root.join("self/ns/mnt"));
    let own = (root == Path::new(PROC)).then(std::process::id);
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    let mut pids: Vec<u32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|pid| Some(*pid) != own)
        .collect();
    pids.sort_unstable();
    for pid in pids {
        let dir = root.join(pid.to_string());
        let Ok(raw) = read_bounded(&dir.join("cmdline")) else {
            continue;
        };
        let args = proc::split_cmdline(&raw);
        let Some(argv0) = args.first() else {
            continue;
        };
        let name = basename(argv0);
        for (key, found) in out.apps.iter_mut() {
            if name.eq_ignore_ascii_case(key) {
                let Some(identity) = identity(root, pid) else {
                    continue;
                };
                found.exact.push(Process {
                    identity,
                    helper: args.iter().any(|a| a.starts_with("--type=")),
                    prefix: read_bounded(&dir.join("environ"))
                        .map_or(Fact::Unreadable, |raw| wineprefix(&raw)),
                    user_ns: namespace(&dir.join("ns/user")),
                    mount_ns: namespace(&dir.join("ns/mnt")),
                });
            } else if argv0.ends_with(key.as_str()) && !found.similar.iter().any(|s| s == name) {
                found.similar.push(name.to_owned());
            }
        }
    }
    out
}

/// How a route places an app. Mirrors `AppKind` plus the simulator itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// In the companion prefix, started from the host.
    Companion,
    /// Inside the running simulator's prefix and namespaces.
    SimSession,
    /// A Linux program; no prefix.
    Native,
    /// The simulator, in its own prefix.
    Simulator,
}

/// What the configured route requires. Built from settings, not from a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub route: Route,
    pub key: String,
    /// The prefix the route uses, canonicalised; `None` when it is not known.
    pub prefix: Option<PathBuf>,
}

impl Expected {
    pub fn new(route: Route, key: impl Into<String>, prefix: Option<&Path>) -> Self {
        Self {
            route,
            key: key.into(),
            prefix: match route {
                Route::Native => None,
                _ => prefix.map(canonical),
            },
        }
    }
}

/// Why a running app could not be checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// Its environment or namespaces could not be read.
    Unreadable,
    /// HangarRS does not know which prefix the route uses.
    PrefixUnknown,
    /// A session app is running and no simulator is.
    NoSimulator,
    /// More than one simulator instance; no single session to compare with.
    SimulatorAmbiguous,
}

/// The comparison's conclusion. Only `Matches` is a positive statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    NotRunning,
    Matches,
    WrongPrefix,
    /// A sim-session app outside the simulator's namespaces.
    OtherSession,
    /// This many instances in charge; none is certified.
    Ambiguous(usize),
    Unverified(Gap),
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotRunning => "Not running",
            Self::Matches => "Matches",
            Self::WrongPrefix => "Wrong prefix",
            Self::OtherSession => "Other session",
            Self::Ambiguous(_) => "Several instances",
            Self::Unverified(_) => "Unverified",
        }
    }

    /// One sentence for the details view.
    pub fn explain(self) -> &'static str {
        match self {
            Self::NotRunning => "No instance is running.",
            Self::Matches => "The running instance is where its launch route puts it.",
            Self::WrongPrefix => "The running instance uses a different Wine prefix.",
            Self::OtherSession => "The running instance is outside the simulator's session.",
            Self::Ambiguous(_) => "More than one instance is running; none is treated as this app.",
            Self::Unverified(Gap::Unreadable) => "Its environment could not be read.",
            Self::Unverified(Gap::PrefixUnknown) => "The expected prefix is not known here.",
            Self::Unverified(Gap::NoSimulator) => {
                "MSFS is not running, so its session cannot be compared."
            }
            Self::Unverified(Gap::SimulatorAmbiguous) => "More than one MSFS instance is running.",
        }
    }
}

/// How this check relates to the previous one for the same app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Continuity {
    /// No earlier check, or nothing then and nothing now.
    First,
    Same,
    /// A different process is now the instance.
    Replaced {
        was: Identity,
    },
    /// The PID is the same but it is a different process.
    PidReused {
        was: Identity,
    },
    /// The earlier instance is gone and nothing replaced it.
    Gone {
        was: Identity,
    },
}

/// One line of the expected/observed table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub fact: &'static str,
    pub expected: String,
    pub observed: String,
    /// `None` where the route sets no requirement or the evidence is missing.
    pub agrees: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    pub verdict: Verdict,
    /// The single instance in charge, when there is exactly one.
    pub instance: Option<Identity>,
    pub continuity: Continuity,
    pub rows: Vec<Row>,
    pub helpers: usize,
    pub similar: Vec<String>,
    pub taken: SystemTime,
}

fn show_prefix(fact: &Fact<PathBuf>) -> String {
    match fact {
        Fact::Known(p) => p.display().to_string(),
        Fact::Missing => "Not set".into(),
        Fact::Unreadable => "Unreadable".into(),
    }
}

fn session_name(p: &Process, obs: &Observation, sim: Option<&Process>) -> (String, Fact<bool>) {
    let same = |a: &Fact<Namespace>, b: &Fact<Namespace>| match (a, b) {
        (Fact::Known(a), Fact::Known(b)) => Fact::Known(a == b),
        _ => Fact::Unreadable,
    };
    let in_sim =
        sim.map(
            |s| match (same(&p.user_ns, &s.user_ns), same(&p.mount_ns, &s.mount_ns)) {
                (Fact::Known(u), Fact::Known(m)) => Fact::Known(u && m),
                _ => Fact::Unreadable,
            },
        );
    let on_host = match (
        same(&p.user_ns, &obs.host_user),
        same(&p.mount_ns, &obs.host_mount),
    ) {
        (Fact::Known(u), Fact::Known(m)) => Fact::Known(u && m),
        _ => Fact::Unreadable,
    };
    let name = match (&in_sim, &on_host) {
        (Some(Fact::Known(true)), Fact::Known(true)) => "Host (shared with MSFS)",
        (Some(Fact::Known(true)), _) => "MSFS session",
        (_, Fact::Known(true)) => "Host",
        (Some(Fact::Unreadable), _) | (None, Fact::Unreadable) => "Unreadable",
        _ => "Other session",
    };
    (name.into(), in_sim.unwrap_or(Fact::Missing))
}

/// Compare one app's requirement with an observation. Pure: no I/O.
///
/// `previous` is the instance identity from the last check of this app, so a
/// recycled PID or a replaced process is reported rather than carried over.
pub fn compare(expected: &Expected, obs: &Observation, previous: Option<Identity>) -> Comparison {
    let empty = Found::default();
    let found = obs.found(&expected.key).unwrap_or(&empty);
    let mains: Vec<&Process> = found.mains().collect();
    let sims: Vec<&Process> = obs
        .found(crate::sim::MSFS_2024_EXE)
        .map(|f| f.mains().collect())
        .unwrap_or_default();
    let instance = (mains.len() == 1).then(|| mains[0]);
    let continuity = match (previous, instance.map(|p| p.identity)) {
        (None, _) => Continuity::First,
        (Some(was), Some(now)) if was == now => Continuity::Same,
        (Some(was), Some(now)) if was.pid == now.pid => Continuity::PidReused { was },
        (Some(was), Some(_)) => Continuity::Replaced { was },
        (Some(was), None) if mains.iter().any(|p| p.identity.pid == was.pid) => {
            Continuity::PidReused { was }
        }
        (Some(was), None) => Continuity::Gone { was },
    };

    let mut rows = vec![Row {
        fact: "Instance",
        expected: "One".into(),
        observed: match mains.len() {
            0 => "None".into(),
            1 => format!("PID {}", mains[0].identity.pid),
            n => format!("{n} found"),
        },
        agrees: (!mains.is_empty()).then_some(mains.len() == 1),
    }];

    let expected_prefix = match (expected.route, &expected.prefix) {
        (Route::Native, _) => "Not used".to_owned(),
        (_, Some(p)) => p.display().to_string(),
        (_, None) => "Unknown".into(),
    };
    let prefix_ok = instance.and_then(|p| match (&expected.prefix, &p.prefix) {
        _ if expected.route == Route::Native => None,
        (Some(want), Fact::Known(got)) => Some(want == got),
        (Some(_), Fact::Missing) => Some(false),
        _ => None,
    });
    rows.push(Row {
        fact: "Prefix",
        expected: expected_prefix,
        observed: instance.map_or_else(|| "—".into(), |p| show_prefix(&p.prefix)),
        agrees: prefix_ok,
    });

    let sim = (sims.len() == 1).then(|| sims[0]);
    let (session_observed, in_sim) = instance.map_or_else(
        || ("—".to_owned(), Fact::Missing),
        |p| session_name(p, obs, sim),
    );
    rows.push(Row {
        fact: "Session",
        expected: if expected.route == Route::SimSession {
            "MSFS session".into()
        } else {
            "Not required".into()
        },
        observed: session_observed,
        agrees: match (expected.route, &in_sim) {
            (Route::SimSession, Fact::Known(b)) => Some(*b),
            _ => None,
        },
    });

    let verdict = if mains.is_empty() {
        Verdict::NotRunning
    } else if mains.len() > 1 {
        Verdict::Ambiguous(mains.len())
    } else {
        let p = mains[0];
        match (expected.route, &expected.prefix, &p.prefix) {
            (Route::Native, ..) => Verdict::Matches,
            (_, None, _) => Verdict::Unverified(Gap::PrefixUnknown),
            (_, _, Fact::Unreadable) => Verdict::Unverified(Gap::Unreadable),
            _ if prefix_ok == Some(false) => Verdict::WrongPrefix,
            (Route::SimSession, ..) => match sims.len() {
                0 => Verdict::Unverified(Gap::NoSimulator),
                1 => match in_sim {
                    Fact::Known(true) => Verdict::Matches,
                    Fact::Known(false) => Verdict::OtherSession,
                    _ => Verdict::Unverified(Gap::Unreadable),
                },
                _ => Verdict::Unverified(Gap::SimulatorAmbiguous),
            },
            _ => Verdict::Matches,
        }
    };

    Comparison {
        verdict,
        instance: instance.map(|p| p.identity),
        continuity,
        rows,
        helpers: found.exact.iter().filter(|p| p.helper).count(),
        similar: found.similar.clone(),
        taken: obs.taken,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SIM: &str = crate::sim::MSFS_2024_EXE;

    /// A scratch `/proc`. Namespace files are plain files: two processes share
    /// a namespace by hard-linking the same file, which is what dev/ino see.
    struct Scratch {
        dir: PathBuf,
        root: PathBuf,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "hangar-observe-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = dir.join("proc");
            fs::create_dir_all(root.join("self/ns")).unwrap();
            fs::create_dir_all(dir.join("ns")).unwrap();
            for ns in ["host-user", "host-mnt", "sim-user", "sim-mnt"] {
                fs::write(dir.join("ns").join(ns), ns).unwrap();
            }
            fs::hard_link(dir.join("ns/host-user"), root.join("self/ns/user")).unwrap();
            fs::hard_link(dir.join("ns/host-mnt"), root.join("self/ns/mnt")).unwrap();
            for p in ["sim/pfx", "companion/pfx"] {
                fs::create_dir_all(dir.join(p)).unwrap();
            }
            Self { dir, root }
        }

        fn pfx(&self, which: &str) -> PathBuf {
            self.dir.join(which).join("pfx")
        }

        /// `env`: `Some(None)` = no WINEPREFIX, `None` = unreadable environ.
        fn process(
            &self,
            pid: u32,
            start: u64,
            args: &[&str],
            env: Option<Option<&Path>>,
            sandbox: &str,
        ) {
            let dir = self.root.join(pid.to_string());
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("ns")).unwrap();
            let mut cmdline = Vec::new();
            for a in args {
                cmdline.extend_from_slice(a.as_bytes());
                cmdline.push(0);
            }
            fs::write(dir.join("cmdline"), cmdline).unwrap();
            let mut stat = format!("{pid} (x) S");
            for i in 1..=19 {
                stat.push_str(&format!(" {}", if i == 19 { start } else { 0 }));
            }
            fs::write(dir.join("stat"), stat).unwrap();
            match env {
                // A directory where a file belongs: the read fails, as EACCES would.
                None => fs::create_dir(dir.join("environ")).unwrap(),
                Some(prefix) => {
                    let mut raw = b"HOME=/home/x\0SECRET_TOKEN=do-not-keep\0".to_vec();
                    if let Some(p) = prefix {
                        raw.extend_from_slice(format!("WINEPREFIX={}\0", p.display()).as_bytes());
                    }
                    fs::write(dir.join("environ"), raw).unwrap();
                }
            }
            fs::hard_link(
                self.dir.join(format!("ns/{sandbox}-user")),
                dir.join("ns/user"),
            )
            .unwrap();
            fs::hard_link(
                self.dir.join(format!("ns/{sandbox}-mnt")),
                dir.join("ns/mnt"),
            )
            .unwrap();
        }

        fn sim(&self) {
            self.process(
                100,
                5,
                &[r"Z:\msfs\FlightSimulator2024.exe"],
                Some(Some(&self.pfx("sim"))),
                "sim",
            );
        }

        fn observe(&self, keys: &[&str]) -> Observation {
            observe(
                &self.root,
                &keys.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
            )
        }

        fn batc(&self) -> Expected {
            Expected::new(Route::SimSession, "BATC.exe", Some(&self.pfx("sim")))
        }

        fn pa(&self) -> Expected {
            Expected::new(Route::Companion, "PA.exe", Some(&self.pfx("companion")))
        }
    }

    #[test]
    fn a_session_app_in_the_simulators_prefix_and_namespaces_matches() {
        let s = Scratch::new();
        s.sim();
        s.process(
            200,
            9,
            &[r"C:\BATC\BATC.exe"],
            Some(Some(&s.pfx("sim"))),
            "sim",
        );
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::Matches);
        assert_eq!(c.instance, Some(Identity { pid: 200, start: 9 }));
        assert!(
            c.rows.iter().all(|r| r.agrees != Some(false)),
            "{:?}",
            c.rows
        );
    }

    #[test]
    fn a_companion_app_on_the_host_in_its_prefix_matches() {
        let s = Scratch::new();
        s.process(
            300,
            1,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        let c = compare(&s.pa(), &s.observe(&["PA.exe"]), None);
        assert_eq!(c.verdict, Verdict::Matches);
        assert_eq!(c.rows[2].observed, "Host");
        assert_eq!(
            c.rows[2].agrees, None,
            "companion routes set no session requirement"
        );
    }

    #[test]
    fn wrong_prefix_is_reported_not_adopted() {
        let s = Scratch::new();
        s.sim();
        // BATC started in the companion prefix by hand: same name, wrong place.
        s.process(
            200,
            9,
            &[r"C:\BATC\BATC.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::WrongPrefix);
        assert_eq!(c.rows[1].agrees, Some(false));
        // A missing WINEPREFIX where one is required is also not a match.
        s.process(200, 9, &[r"C:\BATC\BATC.exe"], Some(None), "sim");
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::WrongPrefix);
        assert_eq!(c.rows[1].observed, "Not set");
    }

    #[test]
    fn right_prefix_in_another_session_is_other_session() {
        let s = Scratch::new();
        s.sim();
        // Sim prefix, but HangarRS's own namespaces.
        s.process(
            200,
            9,
            &[r"C:\BATC\BATC.exe"],
            Some(Some(&s.pfx("sim"))),
            "host",
        );
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::OtherSession);
        assert_eq!(c.rows[2].agrees, Some(false));
    }

    #[test]
    fn a_suffix_collision_is_listed_and_never_counted() {
        let s = Scratch::new();
        s.sim();
        s.process(
            400,
            1,
            &[r"C:\Other\MyBATC.exe"],
            Some(Some(&s.pfx("sim"))),
            "sim",
        );
        let obs = s.observe(&["BATC.exe"]);
        // The monitor's suffix rule would call this BATC.
        assert!(proc::ProcInfo {
            pid: 400,
            cmdline: String::new(),
            args: vec![r"C:\Other\MyBATC.exe".into()]
        }
        .runs("BATC.exe"));
        let c = compare(&s.batc(), &obs, None);
        assert_eq!(c.verdict, Verdict::NotRunning);
        assert_eq!(c.similar, ["MyBATC.exe"]);
    }

    #[test]
    fn several_instances_are_ambiguous_even_when_each_would_match() {
        let s = Scratch::new();
        s.sim();
        s.process(
            200,
            9,
            &[r"C:\BATC\BATC.exe"],
            Some(Some(&s.pfx("sim"))),
            "sim",
        );
        s.process(
            201,
            10,
            &[r"C:\BATC\BATC.exe"],
            Some(Some(&s.pfx("sim"))),
            "sim",
        );
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::Ambiguous(2));
        assert_eq!(c.instance, None);
        assert_eq!(c.rows[0].observed, "2 found");
        // Two simulators leave a session app unverifiable, not matched.
        s.process(
            201,
            10,
            &[r"C:\x\FlightSimulator2024.exe"],
            Some(Some(&s.pfx("sim"))),
            "sim",
        );
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::Unverified(Gap::SimulatorAmbiguous));
    }

    #[test]
    fn an_unreadable_environment_stays_unknown() {
        let s = Scratch::new();
        s.sim();
        s.process(200, 9, &[r"C:\BATC\BATC.exe"], None, "sim");
        let obs = s.observe(&["BATC.exe"]);
        assert_eq!(
            obs.found("BATC.exe").unwrap().exact[0].prefix,
            Fact::Unreadable
        );
        let c = compare(&s.batc(), &obs, None);
        assert_eq!(c.verdict, Verdict::Unverified(Gap::Unreadable));
        assert_eq!(c.rows[1].agrees, None);
        assert_eq!(c.rows[1].observed, "Unreadable");
    }

    #[test]
    fn helper_processes_are_counted_apart_from_the_instance() {
        let s = Scratch::new();
        s.process(
            300,
            1,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        for pid in [301, 302] {
            // A renderer in the wrong prefix must not affect the verdict.
            s.process(
                pid,
                2,
                &[r"C:\PA\PA.exe", "--type=renderer"],
                Some(Some(&s.pfx("sim"))),
                "sim",
            );
        }
        let c = compare(&s.pa(), &s.observe(&["PA.exe"]), None);
        assert_eq!(c.verdict, Verdict::Matches);
        assert_eq!(c.helpers, 2);
        assert_eq!(c.instance.unwrap().pid, 300);
    }

    #[test]
    fn a_recycled_pid_is_a_different_process() {
        let s = Scratch::new();
        s.process(
            300,
            1,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        let first = compare(&s.pa(), &s.observe(&["PA.exe"]), None);
        assert_eq!(first.continuity, Continuity::First);
        let was = first.instance.unwrap();
        assert_eq!(
            compare(&s.pa(), &s.observe(&["PA.exe"]), Some(was)).continuity,
            Continuity::Same
        );
        // Same PID, new start time: the old evidence does not carry over.
        s.process(
            300,
            77,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        let c = compare(&s.pa(), &s.observe(&["PA.exe"]), Some(was));
        assert_eq!(c.continuity, Continuity::PidReused { was });
        // A different PID is a replacement; nothing at all is gone.
        s.process(300, 77, &["/usr/bin/bash"], Some(None), "host");
        s.process(
            310,
            80,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        assert_eq!(
            compare(&s.pa(), &s.observe(&["PA.exe"]), Some(was)).continuity,
            Continuity::Replaced { was }
        );
        fs::remove_dir_all(s.root.join("310")).unwrap();
        assert_eq!(
            compare(&s.pa(), &s.observe(&["PA.exe"]), Some(was)).continuity,
            Continuity::Gone { was }
        );
    }

    #[test]
    fn a_zombie_is_not_an_instance() {
        let s = Scratch::new();
        s.process(
            300,
            1,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        fs::write(
            s.root.join("300/stat"),
            "300 (PA.exe) Z 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1",
        )
        .unwrap();
        assert_eq!(
            compare(&s.pa(), &s.observe(&["PA.exe"]), None).verdict,
            Verdict::NotRunning
        );
    }

    #[test]
    fn a_session_app_with_no_simulator_is_unverified() {
        let s = Scratch::new();
        s.process(
            200,
            9,
            &[r"C:\BATC\BATC.exe"],
            Some(Some(&s.pfx("sim"))),
            "sim",
        );
        let c = compare(&s.batc(), &s.observe(&["BATC.exe"]), None);
        assert_eq!(c.verdict, Verdict::Unverified(Gap::NoSimulator));
    }

    #[test]
    fn an_unknown_expected_prefix_never_matches_and_native_needs_none() {
        let s = Scratch::new();
        s.process(
            300,
            1,
            &[r"C:\PA\PA.exe"],
            Some(Some(&s.pfx("companion"))),
            "host",
        );
        let obs = s.observe(&["PA.exe", "littlenavmap"]);
        let unknown = Expected::new(Route::Companion, "PA.exe", None);
        assert_eq!(
            compare(&unknown, &obs, None).verdict,
            Verdict::Unverified(Gap::PrefixUnknown)
        );
        s.process(500, 1, &["/opt/lnm/littlenavmap"], Some(None), "host");
        let native = Expected::new(Route::Native, "littlenavmap", Some(&s.pfx("sim")));
        assert_eq!(native.prefix, None);
        let c = compare(&native, &s.observe(&["littlenavmap"]), None);
        assert_eq!(c.verdict, Verdict::Matches);
        assert_eq!(c.rows[1].expected, "Not used");
    }

    #[test]
    fn only_wineprefix_is_kept_from_an_environment() {
        let s = Scratch::new();
        s.sim();
        let obs = s.observe(&[]);
        let text = format!("{obs:?}");
        assert!(
            !text.contains("SECRET_TOKEN")
                && !text.contains("do-not-keep")
                && !text.contains("HOME")
        );
        assert_eq!(
            obs.found(SIM).unwrap().exact[0].prefix,
            Fact::Known(canonical(&s.pfx("sim")))
        );
    }

    #[test]
    fn the_simulator_is_compared_against_its_own_prefix() {
        let s = Scratch::new();
        s.sim();
        let expected = Expected::new(Route::Simulator, SIM, Some(&s.pfx("sim")));
        assert_eq!(
            compare(&expected, &s.observe(&[]), None).verdict,
            Verdict::Matches
        );
        let elsewhere = Expected::new(Route::Simulator, SIM, Some(&s.pfx("companion")));
        assert_eq!(
            compare(&elsewhere, &s.observe(&[]), None).verdict,
            Verdict::WrongPrefix
        );
    }

    #[test]
    fn a_missing_proc_root_observes_nothing() {
        let obs = observe(Path::new("/nonexistent/proc"), &["PA.exe".into()]);
        assert!(obs.apps.values().all(|f| f.exact.is_empty()));
        assert_eq!(obs.host_user, Fact::Unreadable);
    }
}
