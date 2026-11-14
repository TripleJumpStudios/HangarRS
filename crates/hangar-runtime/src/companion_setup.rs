//! Setting up a companion prefix from nothing.
//!
//! A new user has no companion prefix to run Project Airports, PilotEdge or
//! Little Navmap in. Setting one up is one explicit action rather than a side
//! effect of launching an app:
//!
//! 1. create the directory;
//! 2. initialise `pfx/` with the sim's own Proton (the toolchain
//!    [`ProtonEnv::companion`] shares), by running `wineboot` through it;
//! 3. add the sim detection link ([`crate::simlink`]);
//! 4. write `SimConnect.cfg` into both places clients look
//!    ([`crate::env::ensure_simconnect_cfg`]).
//!
//! Every step is skipped when its result is already there, so a rerun on a
//! ready prefix changes nothing and [`Outcome::changed`] says so.
//!
//! # An existing link is never re-aimed
//!
//! The link is made by [`crate::simlink::repair`], which never re-aims a
//! symlink that points somewhere else: somebody aimed it there, and a real
//! directory or a re-aimed link is left alone and reported.
//!
//! # Refusals
//!
//! It refuses while the sim is running, and it refuses a target that is, or is
//! inside, or contains, the sim's own compat-data directory — initialising
//! there would run `wineboot` against the prefix the sim lives in.
//!
//! # The initialiser is injected
//!
//! Starting Proton is the one step that reaches outside the process, so it is
//! an [`Initialise`] passed in. [`proton_wineboot`] is the real one; tests pass
//! a closure that creates the files Proton would.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::env::{self, ProtonEnv};
use crate::simlink;

/// A file Proton writes into a prefix it has initialised.
const SYSTEM_REG: &str = "system.reg";

/// Proton's own record of which version set the prefix up, next to `pfx/`.
const PROTON_VERSION: &str = "version";

/// How long `wineboot` may take before it is given up on. A first run copies
/// Proton's default prefix and boots Wine once; that is tens of seconds.
const INIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Runs Proton once against a companion environment so it creates `pfx/`.
///
/// Returns a short line for the journal on success.
pub type Initialise = dyn Fn(&ProtonEnv) -> Result<String, String> + Send + Sync;

/// Where the prefix stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixState {
    /// The directory does not exist.
    Missing,
    /// The directory exists but Proton has not initialised `pfx/`.
    NotInitialised,
    /// `pfx/system.reg` is there.
    Ready,
}

impl PrefixState {
    pub fn label(self) -> &'static str {
        match self {
            PrefixState::Missing => "missing",
            PrefixState::NotInitialised => "not initialised",
            PrefixState::Ready => "ready",
        }
    }
}

/// Everything the Prefix tab shows about one companion prefix. A pure read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The compat-data directory, the parent of `pfx/`.
    pub dir: PathBuf,
    pub state: PrefixState,
    /// Proton's `version` file, when there is one.
    pub proton_version: Option<String>,
    pub link: simlink::Check,
    /// Each `SimConnect.cfg` location and whether a file is there.
    pub cfg: Vec<(PathBuf, bool)>,
}

impl Status {
    pub fn cfg_present(&self) -> usize {
        self.cfg.iter().filter(|(_, there)| *there).count()
    }

    /// [`set_up`] would change something.
    pub fn needs_work(&self) -> bool {
        self.state != PrefixState::Ready
            || self.link.can_repair()
            || self.cfg_present() < self.cfg.len()
    }
}

/// Look at a companion prefix.
pub fn status(dir: &Path, msfs_prefix: &Path) -> Status {
    let pfx = dir.join("pfx");
    let state = if pfx.join(SYSTEM_REG).is_file() {
        PrefixState::Ready
    } else if dir.is_dir() {
        PrefixState::NotInitialised
    } else {
        PrefixState::Missing
    };
    let proton_version = std::fs::read_to_string(dir.join(PROTON_VERSION))
        .ok()
        .map(|v| v.lines().next().unwrap_or_default().trim().to_string())
        .filter(|v| !v.is_empty());
    Status {
        dir: dir.to_path_buf(),
        state,
        proton_version,
        link: simlink::check(&pfx, msfs_prefix),
        cfg: env::simconnect_cfg_paths(&pfx)
            .into_iter()
            .map(|p| {
                let there = p.exists();
                (p, there)
            })
            .collect(),
    }
}

/// What a run did. Every field empty or false is a no-op rerun.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub created_dir: bool,
    /// The initialiser ran, with its line.
    pub initialised: Option<String>,
    pub linked: Option<PathBuf>,
    /// Why the link was left as it is, when it is not linked and was not made.
    pub link_note: Option<String>,
    pub cfg_written: Vec<PathBuf>,
}

impl Outcome {
    pub fn changed(&self) -> bool {
        self.created_dir
            || self.initialised.is_some()
            || self.linked.is_some()
            || !self.cfg_written.is_empty()
    }

    /// One line for the tab and the journal.
    pub fn summary(&self) -> String {
        if !self.changed() {
            return match &self.link_note {
                Some(note) => format!("Already set up; nothing changed. Link: {note}"),
                None => "Already set up; nothing changed.".into(),
            };
        }
        let mut parts = Vec::new();
        if self.created_dir {
            parts.push("created the directory".to_string());
        }
        if self.initialised.is_some() {
            parts.push("initialised the prefix".to_string());
        }
        if self.linked.is_some() {
            parts.push("linked the sim's data".to_string());
        }
        if !self.cfg_written.is_empty() {
            parts.push(format!(
                "wrote {} SimConnect.cfg file{}",
                self.cfg_written.len(),
                if self.cfg_written.len() == 1 { "" } else { "s" }
            ));
        }
        let mut line = parts.join(", ");
        if let Some(first) = line.get(..1) {
            line = first.to_uppercase() + &line[1..];
        }
        match &self.link_note {
            Some(note) => format!("{line}. Link: {note}"),
            None => format!("{line}."),
        }
    }
}

#[derive(Debug, Error)]
pub enum SetupError {
    #[error("the sim is running; close it before setting up the companion prefix")]
    SimRunning,

    #[error("{dir} is the sim's own prefix, or overlaps it")]
    SimPrefix { dir: PathBuf },

    #[error("could not create {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Proton could not initialise the prefix: {0}")]
    Initialise(String),

    #[error("Proton finished but {path} is not there")]
    NotCreated { path: PathBuf },

    #[error("the sim detection link could not be made: {0}")]
    Link(String),
}

/// Set a companion prefix up, or finish setting one up.
///
/// `msfs` is the sim's own environment; the companion one is derived from it
/// so the toolchain is the same. `init` only runs when `pfx/` is not ready.
pub fn set_up(
    dir: &Path,
    msfs: &ProtonEnv,
    sim_running: bool,
    init: &Initialise,
) -> Result<Outcome, SetupError> {
    if sim_running {
        return Err(SetupError::SimRunning);
    }
    if overlaps(dir, &msfs.compat_data) || overlaps(dir, &msfs.wineprefix) {
        return Err(SetupError::SimPrefix {
            dir: dir.to_path_buf(),
        });
    }

    let mut outcome = Outcome::default();
    let before = status(dir, &msfs.wineprefix);

    if before.state == PrefixState::Missing {
        std::fs::create_dir_all(dir).map_err(|source| SetupError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        outcome.created_dir = true;
    }

    let companion = msfs.companion(dir);
    if before.state != PrefixState::Ready {
        let line = init(&companion).map_err(SetupError::Initialise)?;
        let reg = companion.wineprefix.join(SYSTEM_REG);
        if !reg.is_file() {
            return Err(SetupError::NotCreated { path: reg });
        }
        outcome.initialised = Some(line);
    }

    // Checked after initialising: Wine creates the user directory the link
    // sits in, and a link made first would be in Proton's way.
    let link = simlink::check(&companion.wineprefix, &msfs.wineprefix);
    if link.can_repair() {
        let made = simlink::repair(&link).map_err(|e| SetupError::Link(e.to_string()))?;
        outcome.linked = Some(made);
    } else if !link.is_good() {
        outcome.link_note = Some(link.why());
    }

    outcome.cfg_written =
        env::ensure_simconnect_cfg(&companion.wineprefix).map_err(|source| SetupError::Io {
            path: companion.wineprefix.clone(),
            source,
        })?;

    Ok(outcome)
}

/// The real initialiser: `proton run wineboot` in the companion environment.
///
/// Proton's `run` verb calls its own `setup_prefix()` first, which copies its
/// default prefix into `pfx/` and writes `version` beside it; `wineboot` then
/// boots Wine in it once. Output is captured and the last line of it is
/// returned on failure, because a person needs more than an exit code.
pub fn proton_wineboot(env: &ProtonEnv) -> Result<String, String> {
    let (program, mut args) = env.invocation();
    args.push("wineboot".into());
    let mut child = Command::new(&program)
        .args(&args)
        .envs(env.vars())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", PathBuf::from(&program).display()))?;

    // Drain stderr on its own thread so a chatty Wine cannot fill the pipe
    // and stall the child while this waits on it.
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = stderr {
            use std::io::Read;
            let _ = pipe.read_to_string(&mut text);
        }
        text
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > INIT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "wineboot did not finish within {} s",
                    INIT_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let text = reader.join().unwrap_or_default();
    if status.success() {
        Ok(format!(
            "wineboot finished in {:.0} s",
            started.elapsed().as_secs_f32()
        ))
    } else {
        let last = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output");
        Err(format!("wineboot exited with {status}: {last}"))
    }
}

/// `a` is `b`, inside it, or contains it — compared through symlinks where
/// the path exists, and lexically below the deepest part that does.
fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (resolve(a), resolve(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Canonicalise the deepest existing ancestor and append the rest, so a
/// target that does not exist yet is still compared by where it would be.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return normalise(&out);
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return normalise(path),
        }
    }
}

/// Drop `.` and resolve `..` lexically.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "hangar-companion-setup-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A sim prefix with its AppData directory, and a toolchain that does not
    /// exist — nothing here could start Proton even by mistake.
    fn sim(root: &Path) -> ProtonEnv {
        let compat = root.join("compatdata/2537590");
        std::fs::create_dir_all(compat.join("pfx").join(simlink::RELATIVE)).unwrap();
        ProtonEnv {
            wineprefix: compat.join("pfx"),
            proton_script: root.join("no-proton/proton"),
            wine_binary: root.join("no-proton/files/bin/wine"),
            compat_data: compat,
            compat_install: root.join("common/MSFS2024"),
            steam_client: root.join("Steam"),
            is_flatpak: false,
        }
    }

    /// Does what Proton's `setup_prefix` + `wineboot` leave behind, and counts
    /// its calls.
    fn fake_proton(calls: Arc<AtomicU32>) -> Box<Initialise> {
        Box::new(move |env: &ProtonEnv| {
            calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(env.wineprefix.parent(), Some(env.compat_data.as_path()));
            std::fs::create_dir_all(env.wineprefix.join("drive_c/users/steamuser")).unwrap();
            std::fs::write(env.wineprefix.join(SYSTEM_REG), "WINE REGISTRY\n").unwrap();
            std::fs::write(env.compat_data.join(PROTON_VERSION), "10.1000-200\n").unwrap();
            Ok("fake wineboot".into())
        })
    }

    #[test]
    fn a_fresh_target_is_created_initialised_linked_and_configured() {
        let root = Scratch::new("fresh");
        let msfs = sim(&root.0);
        let dir = root.0.join("hangar-rs/companion-prefix");
        assert_eq!(status(&dir, &msfs.wineprefix).state, PrefixState::Missing);

        let calls = Arc::new(AtomicU32::new(0));
        let outcome = set_up(&dir, &msfs, false, &*fake_proton(calls.clone())).unwrap();

        assert!(outcome.created_dir);
        assert_eq!(outcome.initialised.as_deref(), Some("fake wineboot"));
        assert!(outcome.linked.is_some());
        assert_eq!(outcome.cfg_written.len(), 2);
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let after = status(&dir, &msfs.wineprefix);
        assert_eq!(after.state, PrefixState::Ready);
        assert_eq!(after.proton_version.as_deref(), Some("10.1000-200"));
        assert!(after.link.is_good());
        assert_eq!(after.cfg_present(), 2);
        assert!(!after.needs_work());
        for (path, _) in &after.cfg {
            assert_eq!(std::fs::read_to_string(path).unwrap(), env::SIMCONNECT_CFG);
        }
    }

    /// Idempotent: a rerun on a ready prefix starts nothing, writes nothing,
    /// and says so.
    #[test]
    fn a_rerun_on_a_ready_prefix_changes_nothing_and_says_so() {
        let root = Scratch::new("rerun");
        let msfs = sim(&root.0);
        let dir = root.0.join("companion");
        let calls = Arc::new(AtomicU32::new(0));
        set_up(&dir, &msfs, false, &*fake_proton(calls.clone())).unwrap();

        let again = set_up(&dir, &msfs, false, &*fake_proton(calls.clone())).unwrap();
        assert!(!again.changed(), "{again:?}");
        assert_eq!(calls.load(Ordering::Relaxed), 1, "Proton was not run again");
        assert!(again.summary().contains("nothing changed"));
    }

    /// A directory that exists but was never initialised gets Proton and
    /// nothing else redone.
    #[test]
    fn an_uninitialised_directory_is_initialised_not_recreated() {
        let root = Scratch::new("uninit");
        let msfs = sim(&root.0);
        let dir = root.0.join("companion");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            status(&dir, &msfs.wineprefix).state,
            PrefixState::NotInitialised
        );
        let outcome = set_up(&dir, &msfs, false, &*fake_proton(Default::default())).unwrap();
        assert!(!outcome.created_dir);
        assert!(outcome.initialised.is_some());
    }

    /// An existing SimConnect.cfg was tuned by somebody; it stays.
    #[test]
    fn an_existing_cfg_is_never_overwritten() {
        let root = Scratch::new("cfg");
        let msfs = sim(&root.0);
        let dir = root.0.join("companion");
        let home = dir.join("pfx/drive_c/users/steamuser/SimConnect.cfg");
        std::fs::create_dir_all(home.parent().unwrap()).unwrap();
        std::fs::write(&home, "[SimConnect]\nPort=9999\n").unwrap();

        let outcome = set_up(&dir, &msfs, false, &*fake_proton(Default::default())).unwrap();
        assert_eq!(outcome.cfg_written.len(), 1, "only the Documents copy");
        assert!(std::fs::read_to_string(&home).unwrap().contains("9999"));
    }

    /// A real directory where the link goes is application state: left, and
    /// the outcome says why.
    #[test]
    fn a_real_directory_at_the_link_is_left_and_reported() {
        let root = Scratch::new("realdir");
        let msfs = sim(&root.0);
        let dir = root.0.join("companion");
        let real = dir.join("pfx").join(simlink::RELATIVE);
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("UserCfg.opt"), "keep me").unwrap();

        let outcome = set_up(&dir, &msfs, false, &*fake_proton(Default::default())).unwrap();
        assert!(outcome.linked.is_none());
        assert!(outcome.link_note.is_some());
        assert!(real.join("UserCfg.opt").is_file());
        assert!(!std::fs::symlink_metadata(&real)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn it_refuses_while_the_sim_is_running() {
        let root = Scratch::new("running");
        let msfs = sim(&root.0);
        let dir = root.0.join("companion");
        let calls = Arc::new(AtomicU32::new(0));
        let err = set_up(&dir, &msfs, true, &*fake_proton(calls.clone())).unwrap_err();
        assert!(matches!(err, SetupError::SimRunning));
        assert!(!dir.exists(), "nothing was created");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    /// The sim's prefix itself, its `pfx`, a directory inside it, a parent of
    /// it, and the same place reached through a symlink: all refused before
    /// anything is touched.
    #[test]
    fn it_refuses_the_sims_own_prefix_however_it_is_named() {
        let root = Scratch::new("simpfx");
        let msfs = sim(&root.0);
        let alias = root.0.join("alias");
        std::os::unix::fs::symlink(&msfs.compat_data, &alias).unwrap();
        let calls = Arc::new(AtomicU32::new(0));
        for target in [
            msfs.compat_data.clone(),
            msfs.wineprefix.clone(),
            msfs.compat_data.join("companion"),
            root.0.join("compatdata"),
            alias.clone(),
            alias.join("not-yet"),
            root.0.join("elsewhere/../compatdata/2537590"),
        ] {
            let err = set_up(&target, &msfs, false, &*fake_proton(calls.clone())).unwrap_err();
            assert!(
                matches!(err, SetupError::SimPrefix { .. }),
                "{target:?}: {err}"
            );
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(!alias.join("not-yet").exists());
    }

    /// Proton reporting success is not proof; the file has to be there.
    #[test]
    fn an_initialiser_that_creates_nothing_is_an_error() {
        let root = Scratch::new("liar");
        let msfs = sim(&root.0);
        let dir = root.0.join("companion");
        let liar: Box<Initialise> = Box::new(|_| Ok("ok".into()));
        let err = set_up(&dir, &msfs, false, &*liar).unwrap_err();
        assert!(matches!(err, SetupError::NotCreated { .. }), "{err}");

        let failing: Box<Initialise> = Box::new(|_| Err("exit 1".into()));
        let err = set_up(&dir, &msfs, false, &*failing).unwrap_err();
        assert!(err.to_string().contains("exit 1"));
    }

    /// With no sim data to point at, the link is reported, not faked.
    #[test]
    fn no_sim_data_means_no_link_and_a_note() {
        let root = Scratch::new("nodata");
        let msfs = sim(&root.0);
        std::fs::remove_dir_all(msfs.wineprefix.join(simlink::RELATIVE)).unwrap();
        let dir = root.0.join("companion");
        let outcome = set_up(&dir, &msfs, false, &*fake_proton(Default::default())).unwrap();
        assert!(outcome.linked.is_none());
        assert!(outcome.link_note.is_some());
        assert!(!dir.join("pfx").join(simlink::RELATIVE).exists());
    }

    #[test]
    fn the_summary_names_what_happened() {
        let outcome = Outcome {
            created_dir: true,
            initialised: Some("x".into()),
            linked: Some("/l".into()),
            link_note: None,
            cfg_written: vec!["/a".into(), "/b".into()],
        };
        assert_eq!(
            outcome.summary(),
            "Created the directory, initialised the prefix, linked the sim's data, \
             wrote 2 SimConnect.cfg files."
        );
    }

    /// The one real initialisation, opt-in twice over: `--ignored`, and a
    /// target named in `HANGAR_COMPANION_SETUP_LIVE`, which must be under
    /// `/tmp`. Starts the sim's real Proton. Run with the sim closed.
    ///
    /// ```text
    /// HANGAR_COMPANION_SETUP_LIVE=/tmp/hangar-companion-live \
    ///   cargo test -p hangar-runtime --lib -- --ignored --nocapture live_initialisation
    /// ```
    #[test]
    #[ignore = "starts Proton; needs HANGAR_COMPANION_SETUP_LIVE=/tmp/..."]
    fn live_initialisation_into_a_scratch_target() {
        let Some(target) = std::env::var_os("HANGAR_COMPANION_SETUP_LIVE").map(PathBuf::from)
        else {
            panic!("set HANGAR_COMPANION_SETUP_LIVE to a path under /tmp");
        };
        assert!(
            target.starts_with("/tmp/") && !target.exists(),
            "the target must be a new path under /tmp: {}",
            target.display()
        );
        let steam = hangar_steam::SteamEnv::discover().expect("Steam");
        let msfs = hangar_msfs::MsfsEnv::discover(&steam).expect("MSFS");
        let env = ProtonEnv::for_msfs(&msfs, &steam).expect("a compat tool");
        let running = crate::sim::is_running();
        assert!(!running, "close the sim first");

        let outcome = set_up(&target, &env, running, &proton_wineboot).expect("set up");
        println!("{outcome:#?}");
        println!("{}", outcome.summary());
        let again = set_up(&target, &env, false, &proton_wineboot).expect("rerun");
        println!("rerun: {}", again.summary());
        assert!(!again.changed());
        println!("{:#?}", status(&target, &env.wineprefix));
    }

    #[test]
    fn overlap_is_containment_either_way_not_a_shared_prefix_string() {
        assert!(overlaps(Path::new("/a/b"), Path::new("/a/b")));
        assert!(overlaps(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(overlaps(Path::new("/a"), Path::new("/a/b")));
        assert!(!overlaps(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!overlaps(Path::new("/x/companion"), Path::new("/a/b")));
    }
}
