//! The Install tab — the seven-stage pipeline, with a window on it.
//!
//! Drop a Windows installer on the page. Stages 1 to 3 run immediately and
//! **read only**: what built it, who made it, which vendor rules it has to run
//! under, and whether the machine is in a state to run it. Stage 4 onwards is
//! behind a confirm.
//!
//! # Why the reading half is separated from the running half
//!
//! Because **most known failure modes exit 0**: an installer that ran in the
//! wrong prefix, was torn down mid-unpack by `proton run`, or hit a purchase
//! check that could not spawn its helper looks, from outside, like one that
//! worked. Inspection and observation establish what happened.
//!
//! # Nothing here answers a dialog
//!
//! InstallShield's MAX_PATH prompt offers Abort, Retry and Ignore, and **Abort
//! rolls back the whole install**. Nothing in this tab may press anything
//! inside a vendor's installer.
//!
//! # A draw must not be able to start an installer
//!
//! [`InstallState::inert`] is what `HangarApp::new` gets, and [`arm`] refuses
//! on it. Only [`InstallState::live`] can run one: a multi-gigabyte write into
//! a live Proton prefix.
//!
//! # Unchecked is not a pass
//!
//! The gate answers each check three ways: Pass with evidence, Refuse with a
//! typed error, or Unchecked with why it could not judge. [`gate_rows`] paints
//! three different things; an Unchecked drawn as a tick is a gate that
//! silently stopped gating.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hangar_install::gate::{self, Outcome, Report, Request};
use hangar_install::run::{self, Invocation};
use hangar_install::shim;
use hangar_install::snapshot::{Change, Diff, Places, Snapshot};
use hangar_install::vendor::{self, Prefix, Produces, Profile, Table};
use hangar_install::{classify, inspect, record, verify, Inspection, InstallRecord};
use hangar_runtime::ProtonEnv;
use hangar_ui as ui;
use hangar_ui::ConsoleLine;

use crate::app::HangarApp;

/// The environment an install runs against, owned.
///
/// Copied out of `Discovery` rather than borrowed because analysis and the run
/// itself happen off the UI thread, and neither may hold a reference into
/// application state that a rescan can replace underneath it.
#[derive(Clone)]
pub struct Target {
    pub msfs_prefix: PathBuf,
    pub community: PathBuf,
    pub exe_xml: PathBuf,
    pub companion_prefix: Option<PathBuf>,
    /// Proton pointed at the MSFS prefix.
    pub proton_msfs: Option<ProtonEnv>,
    /// Proton pointed at the companion prefix.
    pub proton_companion: Option<ProtonEnv>,
}

impl Target {
    /// Everything an install needs, from what discovery found.
    ///
    /// `None` when MSFS was not found: there is no prefix to install into and
    /// no Community folder to watch, so there is nothing honest to offer.
    pub fn of(app: &HangarApp) -> Option<Target> {
        let msfs = app.discovery.msfs()?;
        let steam = app.discovery.steam()?;
        let base = ProtonEnv::for_msfs(msfs, steam);
        // The configured companion prefix, resolved once in `HangarApp::new`.
        let companion_dir = app.proton.as_ref().map(|p| p.compat_data.clone());
        Some(Target {
            msfs_prefix: msfs.install.prefix.clone(),
            community: msfs.community.clone(),
            exe_xml: msfs.exe_xml(),
            companion_prefix: companion_dir
                .as_ref()
                .map(|d| d.join("pfx"))
                .filter(|p| p.exists()),
            proton_companion: base
                .clone()
                .zip(companion_dir.as_ref())
                .map(|(base, dir)| base.companion(dir)),
            proton_msfs: base,
        })
    }

    /// The prefix a profile's install lands in.
    fn prefix_for(&self, profile: &Profile) -> Option<PathBuf> {
        match profile.prefix {
            Prefix::Msfs => Some(self.msfs_prefix.clone()),
            Prefix::Companion => self.companion_prefix.clone(),
        }
    }

    fn proton_for(&self, profile: &Profile) -> Option<ProtonEnv> {
        match profile.prefix {
            Prefix::Msfs => self.proton_msfs.clone(),
            Prefix::Companion => self.proton_companion.clone(),
        }
    }
}

/// Stages 1 to 3, plus the command stage 4 would use. All read-only.
pub struct Analysis {
    pub path: PathBuf,
    /// Stage 1, or why the file could not be read at all.
    pub inspection: Result<Inspection, String>,
    /// Stage 2. `None` means no vendor rule matched, which is a refusal to
    /// guess rather than a gap — a wrong key runs an installer under another
    /// vendor's rules.
    pub profile: Option<Profile>,
    /// Stage 3, when there was a profile to gate.
    pub gate: Option<Report>,
    /// The exact command, or why one could not be planned.
    pub invocation: Option<Result<Invocation, String>>,
    /// Strings that mean failure whatever the exit code says.
    pub signatures: Vec<String>,
    pub target_prefix: Option<PathBuf>,
    /// Whether the sim was running when this was analysed.
    pub sim_running: bool,
    /// Whether an install into that prefix can reach what the sim reads.
    ///
    /// `None` is "nobody could tell", which the gate reports as Unchecked. A
    /// wrong `false` would let an installer write the sim's `EXE.xml` from the
    /// supposedly separate companion prefix.
    pub reaches_sim_content: Option<bool>,
}

impl Analysis {
    /// Whether stage 4 may be offered at all.
    pub fn runnable(&self) -> bool {
        self.gate.as_ref().is_some_and(Report::is_clear)
            && matches!(self.invocation, Some(Ok(_)))
            && self.target_prefix.is_some()
    }
}

/// Run stages 1 to 3 against one file. Blocking; writes nothing.
pub fn analyse(path: &Path, target: &Target) -> Analysis {
    let inspection = inspect::inspect(path).map_err(|e| e.to_string());

    let Ok(inspection) = &inspection else {
        return Analysis {
            path: path.to_path_buf(),
            inspection,
            profile: None,
            gate: None,
            invocation: None,
            signatures: Vec::new(),
            target_prefix: None,
            sim_running: false,
            reaches_sim_content: None,
        };
    };

    // Stage 2. Keyed on the engine, so an engine nothing pointed at matches
    // nothing rather than being filed under a guess.
    let table = Table::load();
    let profile = table
        .match_on(
            inspection.engine,
            inspection.identity.vendor.as_deref().unwrap_or(""),
            inspection.identity.product.as_deref().unwrap_or(""),
        )
        .map(|m| m.profile.clone());

    let Some(profile) = profile else {
        return Analysis {
            path: path.to_path_buf(),
            inspection: Ok(inspection.clone()),
            profile: None,
            gate: None,
            invocation: None,
            signatures: Vec::new(),
            target_prefix: None,
            sim_running: hangar_runtime::sim::is_running(),
            reaches_sim_content: None,
        };
    };

    let target_prefix = target.prefix_for(&profile);
    // The filesystem question, not the vendor table's: a companion prefix can
    // symlink the sim's AppData directory, so an installer "confined" to it can
    // write Community and `EXE.xml`.
    let reaches_sim_content = target_prefix
        .as_deref()
        .and_then(|p| gate::reaches_sim_content(p, &target.msfs_prefix));
    let sim_running = hangar_runtime::sim::is_running();

    // Stage 3.
    let report = gate::run(Request {
        profile: &profile,
        msfs_prefix: &target.msfs_prefix,
        companion_prefix: target.companion_prefix.as_deref(),
        payload_bytes: profile.payload_estimate(inspection),
        sim_running,
        reaches_sim_content,
    });

    // Stage 4's planning half runs nothing. Planning failures are shown before
    // an action is offered; the exact command belongs to diagnostics and history.
    let invocation = target
        .proton_for(&profile)
        .map(|proton| Invocation::plan(&profile, &proton, path, None).map_err(|e| e.to_string()));
    let invocation = Some(invocation.unwrap_or_else(|| {
        Err(format!(
            "no compatibility tool is configured for the {} prefix",
            profile.prefix.label()
        ))
    }));

    let signatures = Invocation::signatures(&profile);

    Analysis {
        path: path.to_path_buf(),
        inspection: Ok(inspection.clone()),
        profile: Some(profile),
        gate: Some(report),
        invocation,
        signatures,
        target_prefix,
        sim_running,
        reaches_sim_content,
    }
}

// ── running one ──────────────────────────────────────────────────────────────

/// What the whole pipeline made of a run, rendered to strings.
///
/// Flattened inside the task rather than kept as `hangar-install` types, so the
/// draw path holds no borrows into a completed job and the panel stays a
/// panel.
#[derive(Default)]
pub struct RunReport {
    pub elapsed: Duration,
    pub exit_code: Option<i32>,
    pub verdict: String,
    pub objected: bool,
    pub changes: Vec<(String, String)>,
    pub landed: Vec<(String, String)>,
    pub concerns: Vec<String>,
    pub verified: Option<String>,
    pub filed: Option<String>,
    pub error: Option<String>,
    /// Catalog identity to open from the result, when a recognizable item landed.
    pub catalog_name: Option<String>,
    /// The library entry this install earned, for the GUI thread to add.
    ///
    /// **Published rather than written here.** `config.toml` belongs to
    /// `HangarApp`, which rewrites it whole; a background task saving its own
    /// copy would drop whatever the person changed while the install ran. The
    /// draw applies it, once.
    ///
    /// `None` when the install produced no application: a package, a runtime,
    /// or nothing at all.
    pub register: Option<hangar_core::config::AppEntry>,
}

/// An install in flight, or one that has finished.
pub struct Job {
    pub file: PathBuf,
    pub prefix: PathBuf,
    pub started: Instant,
    pub console: Arc<Mutex<Vec<ConsoleLine>>>,
    pub report: Arc<Mutex<Option<RunReport>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Job {
    pub fn finished(&self) -> bool {
        self.report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn lines(&self) -> Vec<ConsoleLine> {
        self.console
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for Job {
    /// Dropping a `JoinHandle` detaches rather than cancels, and a detached
    /// installer would go on writing into a prefix with nobody watching.
    ///
    /// Aborting mid-install leaves a half-written prefix, which is why the tab
    /// keeps a finished job on screen rather than clearing it — this fires only
    /// when the application itself is going away.
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// How many console lines a job keeps.
///
/// A large InstallShield title writes tens of thousands; the interesting lines
/// are the last ones.
const CONSOLE_LIMIT: usize = 20_000;

/// One package's stage 6 result: name, what was found, and whether it is
/// complete.
pub type VerifyRow = (String, String, bool);

#[derive(Default)]
enum PickerState {
    #[default]
    Idle,
    Open,
    Ready(Option<PathBuf>),
}

/// Everything the Install tab remembers.
pub struct InstallState {
    /// False for [`InstallState::inert`]. See the module note.
    live: bool,
    picker: Arc<Mutex<PickerState>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// A path typed rather than dropped.
    pub typed: String,
    pub selection_error: Option<String>,
    /// The file being looked at.
    pub selected: Option<PathBuf>,
    /// Stages 1–3, once they have run.
    pub analysis: Arc<Mutex<Option<Analysis>>>,
    /// Set while the analysis task is out.
    pub analysing: Arc<Mutex<Option<PathBuf>>>,
    /// The file a Run click is armed against. Cleared by anything else.
    pub confirm: Option<PathBuf>,
    pub job: Option<Job>,
    pub follow: bool,
    /// Technical output is secondary to the completed result.
    pub show_output: bool,
    /// Stage 7's records, newest first. Read on demand.
    pub history: Vec<InstallRecord>,
    pub history_read: bool,
    /// Set by whichever task just filed a record, cleared by the next draw.
    ///
    /// **The list is read once, not per frame** (`list_default` opens every
    /// record), so an install filed during a session sets this to be shown.
    /// Both writers set this; `foot_actions` takes it.
    pub history_stale: Arc<Mutex<bool>>,
    /// Stage 6 over the whole library.
    pub verification: Arc<Mutex<Option<Vec<VerifyRow>>>>,
    pub verifying: Arc<Mutex<bool>>,
    /// The two foot actions, each expanding in place.
    pub show_verify: bool,
    pub show_history: bool,
}

impl InstallState {
    /// Only an explicit action in a live window may open a system dialog.
    fn pick(&self, folder: bool) {
        if !self.live {
            return;
        }
        let mut state = self.picker.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*state, PickerState::Idle) {
            return;
        }
        *state = PickerState::Open;
        drop(state);
        let picker = Arc::clone(&self.picker);
        let wake = Arc::clone(&self.wake);
        std::thread::spawn(move || {
            let dialog =
                rfd::FileDialog::new().set_title("Choose an installer, archive or package folder");
            let path = if folder {
                dialog.pick_folder()
            } else {
                dialog.pick_file()
            };
            *picker.lock().unwrap_or_else(|e| e.into_inner()) = PickerState::Ready(path);
            wake();
        });
    }

    fn picked(&self) -> Option<PathBuf> {
        let mut state = self.picker.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, PickerState::Ready(_)) {
            if let PickerState::Ready(path) = std::mem::take(&mut *state) {
                return path;
            }
        }
        None
    }

    /// A state that can never run an installer. What `HangarApp::new` gets.
    pub fn inert() -> Self {
        Self {
            live: false,
            picker: Arc::new(Mutex::new(PickerState::Idle)),
            wake: Arc::new(|| {}),
            typed: String::new(),
            selection_error: None,
            selected: None,
            analysis: Arc::new(Mutex::new(None)),
            analysing: Arc::new(Mutex::new(None)),
            confirm: None,
            job: None,
            follow: true,
            show_output: false,
            history: Vec::new(),
            history_read: false,
            history_stale: Arc::new(Mutex::new(false)),
            verification: Arc::new(Mutex::new(None)),
            verifying: Arc::new(Mutex::new(false)),
            show_verify: false,
            show_history: true,
        }
    }

    /// A state that can. Built by `main.rs`, once there is a window.
    pub fn live(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            live: true,
            wake: Arc::new(wake),
            ..Self::inert()
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Look at a file: clears the last answer and starts stages 1–3.
    ///
    /// Read-only whatever the state, so an inert state runs it too — the whole
    /// reading half of this tab has to be renderable by a test.
    pub fn select(&mut self, path: PathBuf, target: Option<Target>) {
        self.confirm = None;
        self.selected = Some(path.clone());
        *self.analysis.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let Some(target) = target else {
            return;
        };
        *self.analysing.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.clone());

        let analysis = Arc::clone(&self.analysis);
        let analysing = Arc::clone(&self.analysing);
        let wake = Arc::clone(&self.wake);
        // Blocking: it walks a PE's resource tree (some run to hundreds of MB),
        // reads the prefix registry and calls `statvfs`.
        tokio::task::spawn_blocking(move || {
            let done = analyse(&path, &target);
            *analysis.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
            *analysing.lock().unwrap_or_else(|e| e.into_inner()) = None;
            wake();
        });
    }

    /// Forget what was being looked at, without touching a running job.
    ///
    /// A job keeps its card: an install that is still running is not something
    /// dropping another file should hide.
    pub fn forget(&mut self) {
        self.confirm = None;
        self.selected = None;
        *self.analysis.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn is_analysing(&self) -> bool {
        self.analysing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

/// Add a finished install's application to the library, once.
///
/// Taken out of the report rather than read from it, so a draw that runs sixty
/// times a second registers it exactly once and a person who then removes the
/// entry by hand does not have it put back on the next frame.
///
/// Skipped when the same executable is already listed. Matching on the path
/// and not the name is deliberate: a reinstall of the same app must update
/// nothing and add nothing, while two apps that happen to share a display name
/// are still two apps.
fn register_finished_install(app: &mut HangarApp) {
    let Some(job) = app.install.job.as_ref() else {
        return;
    };
    let entry = {
        let mut report = job.report.lock().unwrap_or_else(|e| e.into_inner());
        match report.as_mut() {
            Some(report) => report.register.take(),
            None => None,
        }
    };
    let Some(entry) = entry else { return };

    if app.config.apps.iter().any(|a| a.exe == entry.exe) {
        tracing::info!("{} is already in the library", entry.exe.display());
        return;
    }

    app.journal
        .done("Install", format!("added {} to the library", entry.name));
    app.config.apps.push(entry);
    app.save_config();
    // The Hangar's Apps section comes from the config; `draw` consumes the
    // flag at the top of the next frame.
    app.rescan_requested = true;
}

/// The library entry an install earned, if it earned one.
///
/// Three things have to be true, and none of them is "the run exited 0":
/// the vendor profile said this produces an application, stage 5 found one,
/// and the executable it picked exists. Packages and runtimes return `None`:
/// the library finds those by scanning.
///
/// A companion app is one `.exe` among hundreds in a prefix with nothing on
/// disk marking it out, so only the config can record it.
fn entry_for(
    classified: &classify::Report,
    inspection: &Inspection,
    run_prefix: &Path,
    produces: Produces,
) -> Option<hangar_core::config::AppEntry> {
    if produces != Produces::CompanionApp {
        return None;
    }
    let (exe, also) = classified.landed.iter().find_map(|item| match item {
        classify::Landed::CompanionApp { exe, also } => Some((exe, also)),
        _ => None,
    })?;

    // Stage 5's paths are relative to `drive_c`, because that is the only
    // frame in which two prefixes' contents are comparable. An `AppEntry` is
    // launched, so it needs the real one.
    let root = run_prefix.join("drive_c");
    let full = root.join(exe);
    if !full.is_file() {
        tracing::warn!("not registering {}: it is not there", full.display());
        return None;
    }

    Some(hangar_core::config::AppEntry {
        name: display_name(exe, inspection),
        exe: full,
        args: Vec::new(),
        kind: hangar_core::config::AppKind::Companion,
        // Recorded now: it cannot be found again once the app is gone. Matched
        // with stage 5's rule.
        uninstaller: also
            .iter()
            .find(|p| {
                p.file_stem()
                    .map(|s| classify::is_uninstaller(&s.to_string_lossy().to_ascii_lowercase()))
                    .unwrap_or(false)
            })
            .map(|p| root.join(p)),
        enabled: true,
    })
}

/// What to call it in the library.
///
/// The executable's own stem, because an installer's product string carries a
/// version (`Orbx Central 4.5.48`), and a name that changes on every update
/// detaches the app's saved settings.
///
/// The product string is the fallback when the stem is an internal name
/// (`PilotEdge.PilotClient.exe`).
fn display_name(exe: &Path, inspection: &Inspection) -> String {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !stem.is_empty() && !stem.contains('.') {
        return stem;
    }
    inspection
        .identity
        .product
        .clone()
        .map(|p| strip_version(&p))
        .filter(|p| !p.is_empty())
        .unwrap_or(stem)
}

/// `Orbx Central 4.5.48` becomes `Orbx Central`.
///
/// Only a trailing token of digits and dots with at least one dot, so
/// `MSFS 2024` and `FSUIPC7` keep theirs.
fn strip_version(product: &str) -> String {
    let trimmed = product.trim();
    match trimmed.rsplit_once(' ') {
        Some((head, last))
            if !last.is_empty()
                && last.contains('.')
                && last.chars().all(|c| c.is_ascii_digit() || c == '.') =>
        {
            head.trim().to_string()
        }
        _ => trimmed.to_string(),
    }
}

/// Start the install. **The only place in the application that runs one.**
///
/// Refused unless the state is live, the gate is clear, and a command could be
/// planned. Refused with a log line rather than a panic, because the caller is
/// a click handler and there is nothing useful to unwind to.
///
/// Output goes to the card's own console and to the application's
/// [`Journal`]: the card is dismissible, the journal is the record.
fn arm(
    state: &mut InstallState,
    analysis: &Analysis,
    target: &Target,
    journal: &crate::journal::Journal,
    busy: Option<&str>,
) {
    if !state.live {
        tracing::warn!("an inert install state was asked to run an installer; refused");
        return;
    }
    // `HangarApp::community_busy`, read at the click: nothing else may be
    // changing Community or running in the prefix while an installer starts.
    if let Some(why) = busy {
        state.selection_error = Some(why.to_string());
        return;
    }
    if !analysis.runnable() {
        tracing::warn!("the gate is not clear, so nothing was run");
        return;
    }
    let (Some(Ok(invocation)), Some(prefix), Some(profile), Ok(inspection)) = (
        analysis.invocation.as_ref(),
        analysis.target_prefix.clone(),
        analysis.profile.clone(),
        analysis.inspection.as_ref(),
    ) else {
        return;
    };

    let console: Arc<Mutex<Vec<ConsoleLine>>> = Arc::new(Mutex::new(vec![
        ConsoleLine::note(format!("running {}", analysis.path.display())),
        ConsoleLine::note(format!("in {}", prefix.display())),
        ConsoleLine::note(invocation.command_line()),
        ConsoleLine::note(format!("cwd {}", invocation.cwd.display())),
        ConsoleLine::note(format!(
            "watching the console for {} strings that mean failure",
            analysis.signatures.len()
        )),
    ]));
    let report: Arc<Mutex<Option<RunReport>>> = Arc::new(Mutex::new(None));

    let invocation = invocation.clone();
    let signatures = analysis.signatures.clone();
    let inspection = inspection.clone();
    let target = target.clone();
    let msfs_prefix = target.msfs_prefix.clone();
    let community = target.community.clone();
    let exe_xml = target.exe_xml.clone();
    let out = Arc::clone(&console);
    let done = Arc::clone(&report);
    let history_stale = Arc::clone(&state.history_stale);
    let wake = Arc::clone(&state.wake);
    let run_prefix = prefix.clone();
    let powershell_policy = profile.powershell;
    let journal = journal.clone();
    let source = analysis
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "installer".to_string());

    let handle = tokio::spawn(async move {
        let places = Places {
            community: Some(community.as_path()),
            prefix: Some(&run_prefix),
            exe_xml: Some(&exe_xml),
        };
        // Before. Not observed is never unchanged: a place that could not be
        // read stays Unreadable, so a permissions error cannot read as a failed
        // install and vice versa.
        let before = Snapshot::take(places);

        journal.note(&source, format!("running in {}", run_prefix.display()));
        journal.note(&source, invocation.command_line());

        // Repair anything a killed run left parked *before* parking again, so
        // a prefix is fixed by the next attempt whether or not the process
        // that broke it ever came back. Then park for this run: the guard is
        // held for the whole `run` and restores when it drops, including on a
        // panic or a cancelled future.
        for restored in shim::restore_stray(&run_prefix) {
            let note = format!("put back {} from an earlier run", restored.display());
            push(&out, ConsoleLine::note(note.clone()));
            journal.note(&source, note);
        }
        let parked = match powershell_policy {
            vendor::PowerShell::Keep => shim::Parked::default(),
            vendor::PowerShell::Park => match shim::park(&run_prefix) {
                Ok(parked) => {
                    for path in parked.paths() {
                        let note = format!(
                            "moved Wine's PowerShell stub aside for this run: {}",
                            path.display()
                        );
                        push(&out, ConsoleLine::note(note.clone()));
                        journal.note(&source, note);
                    }
                    parked
                }
                Err(e) => {
                    // Not fatal. The installer may not consult PowerShell at
                    // all, and refusing here would block installs that worked
                    // before this check existed.
                    let note = format!("could not move Wine's PowerShell stub aside: {e}");
                    push(&out, ConsoleLine::fail(note.clone()));
                    journal.fail(&source, note);
                    shim::Parked::default()
                }
            },
        };
        let shimmed: Vec<String> = parked
            .paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        let sink: run::OnLine = {
            let out = Arc::clone(&out);
            let wake = Arc::clone(&wake);
            let journal = journal.clone();
            let source = source.clone();
            Arc::new(move |line: &str| {
                push(&out, ConsoleLine::out(line));
                journal.output(&source, line);
                wake();
            })
        };

        let ran = match run::run(&invocation, &signatures, Some(sink)).await {
            Ok(ran) => ran,
            Err(e) => {
                push(&out, ConsoleLine::fail(e.to_string()));
                journal.fail(&source, e.to_string());
                *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(RunReport {
                    error: Some(e.to_string()),
                    verdict: "it could not be started at all".into(),
                    objected: true,
                    ..RunReport::default()
                });
                wake();
                return;
            }
        };

        let after = Snapshot::take(places);
        let diff = Diff::between(&before, &after);
        let verdict = run::judge(&ran);

        for hit in &ran.hits {
            let text = format!("[{}] {}", hit.signature, hit.line);
            push(&out, ConsoleLine::fail(text.clone()));
            journal.fail(&source, text);
        }

        let classified = classify::classify(&diff, &verdict, profile.produces, &msfs_prefix);

        // Stage 6, when a package landed.
        let verified = classified.landed.iter().find_map(|item| match item {
            classify::Landed::Package { name } => verify::verify_package(&community.join(name))
                .ok()
                .map(|report| record::Verified::of(name, &report)),
            _ => None,
        });

        let entry = record::InstallRecord::of(
            &inspection,
            &profile,
            &invocation,
            &ran,
            &verdict,
            &classified,
            verified.clone(),
            shimmed,
        );
        let filed = record::file_default(&entry);
        if filed.is_ok() {
            *history_stale.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }

        push(
            &out,
            match &filed {
                Ok(path) => ConsoleLine::done(format!("filed as {}", path.display())),
                Err(e) => ConsoleLine::fail(format!("could not file this install: {e}")),
            },
        );
        match &filed {
            Ok(path) => journal.done(&source, format!("filed as {}", path.display())),
            Err(e) => journal.fail(&source, format!("could not file this install: {e}")),
        }

        let register = entry_for(&classified, &inspection, &run_prefix, profile.produces);
        let catalog_name = register
            .as_ref()
            .map(|entry| entry.name.clone())
            .or_else(|| classified.landed.first().map(classify::Landed::name));
        *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(RunReport {
            elapsed: ran.after,
            exit_code: ran.code,
            verdict: describe_verdict(&verdict),
            objected: verdict.objected(),
            changes: describe_changes(&diff),
            landed: classified
                .landed
                .iter()
                .map(|item| (item.kind().label().to_string(), describe_landed(item)))
                .collect(),
            concerns: classified.concerns.iter().map(describe_concern).collect(),
            verified: verified.map(|v| {
                format!(
                    "{}: {}/{} present at the right size, {} orphan",
                    v.package, v.verified, v.declared, v.orphans
                )
            }),
            filed: Some(match filed {
                Ok(path) => path.display().to_string(),
                Err(e) => e.to_string(),
            }),
            error: None,
            catalog_name,
            register,
        });
        wake();
    });

    state.confirm = None;
    state.job = Some(Job {
        file: analysis.path.clone(),
        prefix,
        started: Instant::now(),
        console,
        report,
        handle,
    });
}

fn push(console: &Arc<Mutex<Vec<ConsoleLine>>>, line: ConsoleLine) {
    let mut lines = console.lock().unwrap_or_else(|e| e.into_inner());
    if lines.len() >= CONSOLE_LIMIT {
        lines.remove(0);
    }
    lines.push(line);
}

// ── the tab ──────────────────────────────────────────────────────────────────

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    register_finished_install(app);

    if app.discovery.msfs().is_none() {
        ui::group(ui, None, |ui| {
            ui.label(
                egui::RichText::new("MSFS was not found, so there is nowhere to install to.")
                    .color(ui::theme::DIM),
            );
        });
        return;
    }

    if app.install.job.is_some() {
        job_card(app, ui);
        return;
    }

    if app.drop_install.source.is_some() {
        crate::drop_install::draw(app, ui);
        picker_card(app, ui);
        ui.add_space(ui::ROW_GAP);
        foot_actions(app, ui);
        return;
    }

    let analysis = Arc::clone(&app.install.analysis);
    let held = analysis.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(analysis) = held.as_ref() {
        ui.add_space(ui::ROW_GAP);
        subject_card(ui, analysis);
        // Borrow ends before the run half, which needs `app` mutably.
        let path = analysis.path.clone();
        let runnable = analysis.runnable();
        let prefix = analysis.target_prefix.clone();
        let reaches = analysis.reaches_sim_content;
        let produces = analysis.profile.as_ref().map(|p| p.produces);
        drop(held);
        ui.add_space(ui::ROW_GAP);
        run_card(app, ui, &path, runnable, prefix, reaches, produces);
        ui.add_space(ui::ROW_GAP);
        picker_card(app, ui);
    } else {
        drop(held);
        if app.install.is_analysing() {
            ui.add_space(ui::ROW_GAP);
            ui::group(ui, None, |ui| {
                ui::activity(ui, "Reading installer…");
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Nothing is being run.")
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::DIM),
                );
            });
        }
        picker_card(app, ui);
    }
    if let Some(error) = &app.install.selection_error {
        ui.label(egui::RichText::new(error).color(ui::theme::ORANGE));
    }

    ui.add_space(ui::ROW_GAP);
    foot_actions(app, ui);
}

/// Pick up a file dropped anywhere on the window.
///
/// Switch to Install for review. A drop never approves an installation.
///
/// **Two pipelines, decided by what was dropped.** An `.exe` goes to the seven
/// stages, because a Windows installer will not say what it is about to do.
/// Anything else (a folder, a `.zip`, a `.7z`) goes to
/// [`crate::drop_install`], because a scenery download hides nothing.
pub fn take_dropped_file(app: &mut HangarApp, ui: &egui::Ui) {
    let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect()
    });
    if dropped.len() > 1 {
        app.active_tab = crate::tab::Tab::Install;
        app.install.selection_error = Some("Choose one archive or folder at a time.".into());
        return;
    }
    let Some(path) = app.install.picked().or_else(|| dropped.into_iter().next()) else {
        return;
    };

    accept_path(app, path);
}

pub(crate) fn accept_path(app: &mut HangarApp, path: PathBuf) {
    app.active_tab = crate::tab::Tab::Install;
    if app.community_busy().is_some() || app.drop_install.is_reading() || app.install.is_analysing()
    {
        app.install.selection_error =
            Some("Wait for the current operation to finish, then choose again.".into());
        return;
    }
    app.install.selection_error = None;
    if is_installer(&path) {
        let target = Target::of(app);
        app.drop_install.clear();
        app.install.typed = path.display().to_string();
        app.install.select(path, target);
    } else {
        crate::drop_install::accept(app, path);
    }
}

/// Whether a path is for the seven stages.
///
/// Decided on the extension rather than on the contents: a directory is never
/// an installer whatever is inside it, and reading a 200 MB PE to find out
/// which pipeline to use is work done before anything has been asked for.
///
/// A path that does not exist still routes on its name, because this also
/// decides what a *typed* path opens — and answering "that is not an
/// installer" for a mistyped `setup.exe` would send it to the wrong half.
pub fn is_installer(path: &Path) -> bool {
    !path.is_dir()
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// Where a file comes from.
///
/// With nothing chosen this is the whole tab: a drop target, and the typed
/// path for the sessions that cannot receive a drag. On Wayland winit delivers
/// no `DroppedFile` at all, so the invitation changes rather than lying — see
/// [`crate::env::FileDrops`].
fn picker_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    let drops = app.file_drops;
    // Never true on Wayland either: `hovered_files` comes from the same
    // backends. Read anyway, so the X11 path is not special-cased on a flag.
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let chosen = app.install.selected.is_some() || app.drop_install.source.is_some();

    // The target is a target, so it is the one bordered thing on the page.
    // Once a file is chosen it collapses to the field, because the answer is
    // what the person is now reading.
    let frame = egui::Frame::new()
        .fill(if hovering {
            ui::theme::PANEL_HI
        } else {
            ui::theme::WELL
        })
        .stroke(egui::Stroke::new(
            1.0,
            if hovering {
                ui::theme::ORANGE
            } else {
                ui::theme::STROKE_SOFT
            },
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(18, 12));

    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        if !chosen {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(if hovering {
                        "Drop it here."
                    } else if drops.available() {
                        "Drop an installer, folder or .zip here"
                    } else {
                        "Choose an installer, folder or .zip"
                    })
                    .size(ui::theme::TEXT_MD)
                    .color(ui::theme::TEXT_HI),
                );
                if !drops.available() {
                    ui.add_space(4.0);
                    // Outside any horizontal, so it wraps rather than
                    // extending a row and pushing the card past the page.
                    ui.label(
                        egui::RichText::new("Drag and drop is unavailable in this session.")
                            .size(ui::theme::TEXT_BODY)
                            .color(ui::theme::DIM),
                    );
                }
                ui.add_space(12.0);
            });
        }

        ui.horizontal_wrapped(|ui| {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::FILE,
                "Choose file",
                ui::ButtonKind::Primary,
            )
            .clicked()
            {
                app.install.pick(false);
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::FOLDER,
                "Place folder",
                ui::ButtonKind::Normal,
            )
            .clicked()
            {
                app.install.pick(true);
            }
            if matches!(
                *app.install.picker.lock().unwrap_or_else(|e| e.into_inner()),
                PickerState::Open
            ) {
                ui::state(ui, "Choosing", ui::Tone::Idle);
            }
        });

        ui.add_space(10.0);
        let mut go = false;
        ui.horizontal(|ui| {
            let room = (ui.available_width() - 130.0).max(120.0);
            let response = ui::text_field(ui, &mut app.install.typed, "Or enter a path", room);
            go |= response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui::tool_button(
                ui,
                egui_phosphor::regular::MAGNIFYING_GLASS,
                "Review",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                go = true;
            }
        });

        if go {
            // The same fork a drop takes, so a typed `.zip` opens the drop
            // sheet rather than reporting itself unreadable to the PE inspector.
            let path = PathBuf::from(expand_tilde(app.install.typed.trim()));
            accept_path(app, path);
        }
    });
}

/// The chosen file and any checks that need attention.
fn subject_card(ui: &mut egui::Ui, analysis: &Analysis) {
    ui::group(ui, None, |ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    analysis
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| analysis.path.display().to_string()),
                )
                .font(egui::FontId::new(ui::theme::TEXT_LG, ui::theme::semibold()))
                .color(ui::theme::TEXT_HI),
            )
            .truncate(),
        );
        ui.add_space(6.0);

        if let Ok(inspection) = &analysis.inspection {
            ui::kv_row(ui, "Made by", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(inspection.identity.describe())
                            .size(ui::theme::TEXT_BODY)
                            .color(ui::theme::TEXT),
                    )
                    .truncate(),
                );
            });
            ui::kv_row(ui, "Size", |ui| {
                mono(ui, &hangar_core::disk::human(inspection.size));
            });
        }
        // Read and Matched: whichever first wants a person.
        let concern = match &analysis.inspection {
            Err(e) => Some((e.clone(), ui::theme::ERR)),
            Ok(i) if !i.looks_like_installer() => Some((
                "This file was not recognized as an installer.".to_string(),
                ui::theme::WARN,
            )),
            Ok(i) if i.packed.is_some() => Some((
                format!(
                    "This installer uses {} compression and could not be fully checked.",
                    i.packed.as_deref().unwrap_or_default()
                ),
                ui::theme::WARN,
            )),
            Ok(_) if analysis.profile.is_none() => Some((
                "This installer is not supported yet.".to_string(),
                ui::theme::WARN,
            )),
            Ok(_) => None,
        };
        if let Some((line, colour)) = concern {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(line)
                    .size(ui::theme::TEXT_BODY)
                    .color(colour),
            );
        }

        if let Some(Err(error)) = &analysis.invocation {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(error.to_string())
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::ERR),
            );
        }

        // The gate always reports in full when it is not clear. A refusal that
        // is not on screen is a gate that has quietly stopped gating.
        if let Some(report) = &analysis.gate {
            let unchecked = report.unchecked();
            let refused = report
                .outcomes
                .iter()
                .any(|c| matches!(c.outcome, Outcome::Refuse(_)));
            if refused || !unchecked.is_empty() || analysis.sim_running {
                ui.add_space(8.0);
                gate_rows(ui, report);
            }
            if analysis.sim_running {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "The sim is running, so a package could be loaded half-written.",
                    )
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::WARN),
                );
            }
            if !unchecked.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} check{} could not be completed.",
                        unchecked.len(),
                        if unchecked.len() == 1 { "" } else { "s" },
                    ))
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::WARN),
                );
            }
            if analysis.reaches_sim_content == Some(true) {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "This prefix can write the sim's Community folder and EXE.xml.",
                    )
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::WARN),
                );
            }
        }
    });
}

/// Report unsuccessful checks with their full reasons; unknown and refused
/// remain distinct, while successful checks need no extra row.
fn gate_rows(ui: &mut egui::Ui, report: &Report) {
    for checked in &report.outcomes {
        let (tone, detail) = match &checked.outcome {
            Outcome::Pass(_) => continue,
            Outcome::Unchecked(why) => (ui::Tone::Warn, why.clone()),
            Outcome::Refuse(refusal) => (ui::Tone::Err, refusal.to_string()),
        };
        ui::state(ui, &checked.label, tone);
        // A reason to stop must be readable without hovering over clipped text.
        ui.label(
            egui::RichText::new(detail)
                .size(ui::theme::TEXT_BODY)
                .color(tone.color()),
        );
        ui.add_space(8.0);
    }
}

/// Installation destination, confirmation, and the guarded action.
fn run_card(
    app: &mut HangarApp,
    ui: &mut egui::Ui,
    path: &Path,
    runnable: bool,
    prefix: Option<PathBuf>,
    reaches: Option<bool>,
    produces: Option<Produces>,
) {
    ui::group(ui, Some("Installation"), |ui| {
        if let Some(prefix) = &prefix {
            ui::kv_row(ui, "Writes into", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(prefix.display().to_string())
                            .monospace()
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::WARN),
                    )
                    .truncate(),
                );
            });
        }

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Answer prompts in the installer window; Abort at an InstallShield MAX_PATH warning undoes the entire install.",
            )
            .size(ui::theme::TEXT_BODY)
            .color(ui::theme::DIM),
        );

        ui.add_space(10.0);

        let armed = app.install.confirm.as_deref() == Some(path);
        let busy = app.install.job.as_ref().is_some_and(|j| !j.finished());

        if !runnable {
            ui::state(ui, "refused", ui::Tone::Err);
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Resolve the issues above before installing.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            return;
        }

        if busy {
            ui::state(ui, "running", ui::Tone::Accent);
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Another installation is still running.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            return;
        }

        if !armed {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::DOWNLOAD_SIMPLE,
                "Install\u{2026}",
                ui::ButtonKind::Primary,
            )
            .clicked()
            {
                app.install.confirm = Some(path.to_path_buf());
            }
            return;
        }

        // The confirm. It names the file and the prefix, because those are the
        // two facts that decide what a mistake here costs.
        ui.add_space(4.0);
        ui::heading(ui, "Confirm");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!(
                "Run {} and write into {}?",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                prefix
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "\u{2014}".into())
            ))
            .size(ui::theme::TEXT_MD)
            .color(ui::theme::TEXT_HI),
        );
        ui.add_space(3.0);
        ui.label(
            egui::RichText::new(path.display().to_string())
                .monospace()
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );

        if reaches == Some(true) {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "This can also change your Community packages and simulator startup apps.",
                )
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::WARN),
            );
        }
        if produces == Some(Produces::Package) {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("This should land a package in the Community folder.")
                    .size(ui::theme::TEXT_SM)
                    .color(ui::theme::DIM),
            );
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Still drawn and clickable on an inert state: `arm` is the guard,
            // and a guard nothing can reach is a guard no test exercises.
            if ui::tool_button(
                ui,
                egui_phosphor::regular::PLAY,
                "Run the installer",
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                if let Some(target) = Target::of(app) {
                    let analysis = Arc::clone(&app.install.analysis);
                    let held = analysis.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(analysis) = held.as_ref() {
                        // Cloned out from under the lock so `arm` can take the
                        // state mutably.
                        let analysis_path = analysis.path.clone();
                        if analysis_path == path {
                            let snapshot = analysis;
                            let journal = app.journal.clone();
                            let busy = app.community_busy();
                            arm_with(&mut app.install, snapshot, &target, &journal, busy);
                        }
                    }
                }
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::X,
                "Cancel",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.install.confirm = None;
            }
            if !app.install.is_live() {
                ui::state(ui, "not started", ui::Tone::Idle);
            }
        });
    });
}

/// `arm`, reachable while the analysis lock is held.
///
/// Split out only because the confirm button needs the analysis and the state
/// at once, and the analysis lives behind the state's own `Arc`.
fn arm_with(
    state: &mut InstallState,
    analysis: &Analysis,
    target: &Target,
    journal: &crate::journal::Journal,
    busy: Option<&str>,
) {
    // Cleared now so the same path is not installed twice by accident.
    state.typed.clear();
    arm(state, analysis, target, journal, busy);
}

/// The live console, and the report once it is done.
fn job_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    let Some(job) = &app.install.job else {
        return;
    };
    let lines = job.lines();
    let finished = job.finished();
    let elapsed = job.started.elapsed();
    let file = job
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = job.prefix.display().to_string();

    ui::group(
        ui,
        Some(if finished { "Result" } else { "Installing" }),
        |ui| {
            if finished {
                ui::state(ui, "finished", ui::Tone::Idle);
            } else {
                ui::activity(ui, "Install in progress…");
            }
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&file)
                        .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                        .color(ui::theme::TEXT_HI),
                )
                .truncate(),
            );
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("into {prefix}"))
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIM),
                )
                .truncate(),
            );
            ui.add_space(8.0);

            if !finished || app.install.show_output {
                ui::console_header(ui, lines.len(), &mut app.install.follow, |ui| {
                    ui.label(
                        egui::RichText::new(format!("{:.0}s", elapsed.as_secs_f32()))
                            .monospace()
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIM),
                    );
                });
                ui::console_view(
                    ui,
                    "install_console",
                    220.0,
                    &mut app.install.follow,
                    &lines,
                );
            }

            if !finished {
                // Nothing else repaints while an installer is quiet, and a stopped
                // clock reads as a hang.
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                return;
            }

            let held = app
                .install
                .job
                .as_ref()
                .map(|j| j.report.lock().unwrap_or_else(|e| e.into_inner()).is_some());
            if held != Some(true) {
                return;
            }
            let report = app.install.job.as_ref().unwrap();
            let report = report.report.lock().unwrap_or_else(|e| e.into_inner());
            let Some(report) = report.as_ref() else {
                return;
            };

            if report.error.is_none() && report.concerns.is_empty() && !report.landed.is_empty() {
                ui::state(ui, "Items found", ui::Tone::Ok);
            } else if report.error.is_some() || report.objected || !report.concerns.is_empty() {
                ui::state(ui, "Needs attention", ui::Tone::Warn);
            } else {
                ui::state(ui, "Installer closed · check the result", ui::Tone::Idle);
            }
            for (_, item) in &report.landed {
                ui.label(
                    egui::RichText::new(item)
                        .size(ui::theme::TEXT_MD)
                        .color(ui::theme::TEXT_HI),
                );
            }
            for concern in &report.concerns {
                ui.label(
                    egui::RichText::new(concern)
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::WARN),
                );
            }

            ui.add_space(10.0);
            ui::divider(ui);
            ui.add_space(8.0);

            ui::kv_row(ui, "Installer", |ui| {
                ui::state(
                    ui,
                    if report.objected {
                        "reported a problem"
                    } else {
                        "closed"
                    },
                    if report.objected {
                        ui::Tone::Err
                    } else {
                        ui::Tone::Idle
                    },
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(report.verdict.as_str())
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIM),
                    )
                    .truncate(),
                );
            });
            if let Some(error) = &report.error {
                ui::kv_row(ui, "Could not start", |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error.as_str())
                                .size(ui::theme::TEXT_SM)
                                .color(ui::theme::ERR),
                        )
                        .truncate(),
                    );
                });
            }
            ui::kv_row(ui, "Ran for", |ui| {
                mono(
                    ui,
                    &format!(
                        "{:.1}s, exit {}",
                        report.elapsed.as_secs_f32(),
                        report
                            .exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".into())
                    ),
                );
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "The installer closing does not confirm that every file was installed.",
                )
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
            );

            ui.add_space(8.0);
            ui::heading(ui, "What changed");
            ui.add_space(3.0);
            for (place, detail) in &report.changes {
                ui::kv_row(ui, place, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(detail.as_str())
                                .size(ui::theme::TEXT_SM)
                                .color(ui::theme::TEXT),
                        )
                        .truncate(),
                    );
                });
            }

            if !report.landed.is_empty() {
                ui.add_space(8.0);
                ui::heading(ui, "Installed items");
                ui.add_space(3.0);
                for (kind, detail) in &report.landed {
                    ui::kv_row(ui, kind, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(detail.as_str())
                                    .size(ui::theme::TEXT_SM)
                                    .color(ui::theme::TEXT),
                            )
                            .truncate(),
                        );
                    });
                }
            }

            if let Some(verified) = &report.verified {
                ui.add_space(8.0);
                ui::kv_row(ui, "Verified", |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(verified.as_str())
                                .size(ui::theme::TEXT_SM)
                                .color(ui::theme::TEXT),
                        )
                        .truncate(),
                    );
                });
            }

            if !report.concerns.is_empty() {
                ui.add_space(8.0);
                ui::heading(ui, "Concerns");
                ui.add_space(3.0);
                for concern in &report.concerns {
                    ui.label(
                        egui::RichText::new(format!("\u{2014}  {concern}"))
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::WARN),
                    );
                }
            }

            if let Some(filed) = &report.filed {
                ui.add_space(8.0);
                ui::kv_row(ui, "Filed as", |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(filed.as_str())
                                .monospace()
                                .size(ui::theme::TEXT_SM)
                                .color(ui::theme::DIM),
                        )
                        .truncate(),
                    );
                });
            }
        },
    );

    if finished {
        ui.horizontal_wrapped(|ui| {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::ARROW_RIGHT,
                "View in Hangar",
                ui::ButtonKind::Normal,
            )
            .clicked()
            {
                app.hangar_selected = app.install.job.as_ref().and_then(|job| {
                    job.report
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .and_then(|report| report.catalog_name.clone())
                });
                app.active_tab = crate::tab::Tab::Hangar;
                app.hangar_section = None;
                app.rescan_requested = true;
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::PLUS,
                "Install another",
                ui::ButtonKind::Primary,
            )
            .clicked()
            {
                app.install.job = None;
                app.install.selected = None;
                *app.install
                    .analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::LIST_BULLETS,
                "Activity",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.console_source = app
                    .install
                    .selected
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy().into_owned());
                app.console_search.clear();
                app.console_events_only = false;
                app.active_tab = crate::tab::Tab::Console;
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::TERMINAL,
                "Technical output",
                if app.install.show_output {
                    ui::ButtonKind::GhostActive
                } else {
                    ui::ButtonKind::Ghost
                },
            )
            .clicked()
            {
                app.install.show_output = !app.install.show_output;
            }
        });
    }
}

/// The two actions about the library rather than this file; closed by default.
fn foot_actions(app: &mut HangarApp, ui: &mut egui::Ui) {
    refresh_history(&mut app.install);

    ui.horizontal(|ui| {
        if ui::tool_button(
            ui,
            egui_phosphor::regular::CHECK_CIRCLE,
            "Check installed packages",
            if app.install.show_verify {
                ui::ButtonKind::GhostActive
            } else {
                ui::ButtonKind::Ghost
            },
        )
        .clicked()
        {
            app.install.show_verify = !app.install.show_verify;
            // Expanding it is the request; the button inside is for a re-run.
            let done = app
                .install
                .verification
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if app.install.show_verify && !done {
                start_verify(app);
            }
        }
        if ui::tool_button(
            ui,
            egui_phosphor::regular::CLOCK_COUNTER_CLOCKWISE,
            &format!("Recent installs ({})", app.install.history.len()),
            if app.install.show_history {
                ui::ButtonKind::GhostActive
            } else {
                ui::ButtonKind::Ghost
            },
        )
        .clicked()
        {
            app.install.show_history = !app.install.show_history;
        }
    });

    if app.install.show_verify {
        ui.add_space(ui::ROW_GAP);
        verify_card(app, ui);
    }
    if app.install.show_history {
        ui.add_space(ui::ROW_GAP);
        history_card(app, ui);
    }
}

/// Load once and refresh after either installer path files a new record.
pub(crate) fn refresh_history(state: &mut InstallState) {
    let stale = {
        let mut flag = state
            .history_stale
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *flag, false)
    };
    if !state.history_read || stale {
        state.history = record::list_default();
        state.history_read = true;
    }
}

/// Stage 6 over the whole installed library.
fn verify_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    let busy = *app
        .install
        .verifying
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let results = app
        .install
        .verification
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();

    ui::group(ui, Some("Installed packages"), |ui| {
        if busy {
            ui::activity(ui, "Checking packages…");
        }
        if ui::tool_button_enabled(
            ui,
            !busy,
            egui_phosphor::regular::CHECK_CIRCLE,
            if busy {
                "Checking\u{2026}"
            } else if results.is_some() {
                "Check again"
            } else {
                "Check the library"
            },
            ui::ButtonKind::Normal,
        )
        .clicked()
        {
            start_verify(app);
        }

        let Some(results) = results else {
            return;
        };
        ui.add_space(10.0);
        let complete = results.iter().filter(|(_, _, ok)| *ok).count();
        ui::kv_row(ui, "Complete", |ui| {
            mono(ui, &format!("{complete} of {}", results.len()));
        });
        ui.add_space(4.0);
        for (name, detail, ok) in &results {
            ui::kv_row(ui, name, |ui| {
                ui::state(
                    ui,
                    if *ok { "complete" } else { "findings" },
                    if *ok { ui::Tone::Ok } else { ui::Tone::Warn },
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(detail.as_str())
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIM),
                    )
                    .truncate(),
                );
            });
        }
    });
}

fn start_verify(app: &mut HangarApp) {
    let Some(msfs) = app.discovery.msfs() else {
        return;
    };
    let community = msfs.community.clone();
    *app.install
        .verifying
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = true;
    let out = Arc::clone(&app.install.verification);
    let busy = Arc::clone(&app.install.verifying);
    let wake = Arc::clone(&app.install.wake);

    tokio::task::spawn_blocking(move || {
        let mut rows: Vec<VerifyRow> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&community) {
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for dir in dirs {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match verify::verify_package(&dir) {
                    Ok(report) => {
                        let ok = report.is_complete();
                        let detail = if ok {
                            format!("{} files", report.declared)
                        } else {
                            format!(
                                "{}/{} present, {} missing, {} wrong size, {} orphan{}",
                                report.verified(),
                                report.declared,
                                report.missing.len(),
                                report.wrong_size.len(),
                                report.orphans.len(),
                                if report.case_collisions.is_empty() {
                                    String::new()
                                } else {
                                    format!(", {} case collision", report.case_collisions.len())
                                }
                            )
                        };
                        rows.push((name, detail, ok));
                    }
                    Err(e) => rows.push((name, e.to_string(), false)),
                }
            }
        }
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(rows);
        *busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        wake();
    });
}

/// Stage 7: what this application has installed, newest first.
fn history_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    ui::group(ui, Some("Recent installs"), |ui| {
        if app.install.history.is_empty() {
            ui.label(
                egui::RichText::new("Nothing has been installed through HangarRS yet.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            return;
        }

        for entry in &app.install.history {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [100.0, 24.0],
                    egui::Label::new(egui::RichText::new(when(&entry.at)).color(ui::theme::DIM))
                        .halign(egui::Align::Min),
                );
                let room = (ui.available_width() - 130.0).max(40.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(room, 24.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_width(room);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(landed_line(entry))
                                    .size(ui::theme::TEXT_SM)
                                    .color(ui::theme::TEXT),
                            )
                            .truncate(),
                        )
                        .on_hover_text(landed_line(entry));
                    },
                );
                let clean = entry.is_clean();
                ui::state(
                    ui,
                    if clean { "ok" } else { "look at this" },
                    if clean { ui::Tone::Ok } else { ui::Tone::Warn },
                )
                .on_hover_text(if entry.concerns.is_empty() {
                    entry.verdict.clone()
                } else {
                    entry.concerns.join("\n")
                });
            });
        }
    });
}

/// A filed stamp as a person reads a date.
///
/// Records are named like `2026-08-31T23-11-17Z`, which sorts but reads badly.
pub(crate) fn when(stamp: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Anything that is not the shape we file is shown as filed, rather than
    // mangled into a date that is not one.
    let Some((date, rest)) = stamp.split_once('T') else {
        return stamp.to_owned();
    };
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = rest.trim_end_matches('Z').split('-').collect();
    let (Some(month), Some(day)) = (d.get(1), d.get(2)) else {
        return stamp.to_owned();
    };
    let Some(name) = month
        .parse::<usize>()
        .ok()
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)))
    else {
        return stamp.to_owned();
    };
    match (t.first(), t.get(1)) {
        (Some(h), Some(m)) => format!("{} {name} {h}:{m}", day.trim_start_matches('0')),
        _ => format!("{} {name}", day.trim_start_matches('0')),
    }
}

/// What an install put on the machine, in a person's words.
fn landed_line(entry: &InstallRecord) -> String {
    let what = if entry.landed.is_empty() {
        "added nothing".to_owned()
    } else {
        format!(
            "added {}",
            entry
                .landed
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let verified = match &entry.verified {
        Some(v) => format!(" \u{00b7} {}/{} verified", v.verified, v.declared),
        None => String::new(),
    };
    format!(
        "{}  \u{00b7}  {what}{verified}",
        entry.product.as_deref().unwrap_or("?")
    )
}

// ── flattening ───────────────────────────────────────────────────────────────

fn describe_verdict(verdict: &run::Verdict) -> String {
    use run::Verdict;
    match verdict {
        Verdict::BadConsole(hits) => format!(
            "the console said it failed, whatever the exit code was ({} line{})",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" }
        ),
        Verdict::Vanished { after } => format!(
            "gone in {:.1}s without saying anything \u{2014} a stub torn down mid-unpack looks \
             exactly like this",
            after.as_secs_f32()
        ),
        Verdict::Killed { .. } => "killed by a signal rather than exiting".into(),
        Verdict::ExitCode(code) => format!("it exited {code}"),
        Verdict::NoComplaint => "nothing in the run said it failed".into(),
    }
}

fn describe_changes(diff: &Diff) -> Vec<(String, String)> {
    fn one<T: Ord>(change: &Change<T>, show: impl Fn(&T) -> String) -> String {
        match change {
            Change::Unknown(why) => format!("could not be compared \u{2014} {why}"),
            Change::Delta { added, removed } if added.is_empty() && removed.is_empty() => {
                "unchanged".into()
            }
            Change::Delta { added, removed } => {
                let mut parts: Vec<String> =
                    added.iter().map(|i| format!("+{}", show(i))).collect();
                parts.extend(removed.iter().map(|i| format!("-{}", show(i))));
                parts.join(", ")
            }
        }
    }
    vec![
        ("Community".into(), one(&diff.community, |p| p.name.clone())),
        (
            "prefix .exe".into(),
            one(&diff.executables, |p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string())
            }),
        ),
        (
            "EXE.xml".into(),
            one(&diff.addons, |a| format!("{} \u{2192} {}", a.name, a.path)),
        ),
    ]
}

fn describe_landed(item: &classify::Landed) -> String {
    match item {
        classify::Landed::Package { name } => name.clone(),
        classify::Landed::CompanionApp { exe, also } => {
            if also.is_empty() {
                exe.display().to_string()
            } else {
                format!("{} (and {} more)", exe.display(), also.len())
            }
        }
        classify::Landed::SimAddon {
            name,
            win_path,
            resolved,
        } => match resolved {
            Some(p) => format!("{name}: {win_path} \u{2192} {}", p.display()),
            None => format!("{name}: {win_path} \u{2014} does NOT resolve in the MSFS prefix"),
        },
    }
}

fn describe_concern(concern: &classify::Concern) -> String {
    use classify::Concern;
    match concern {
        Concern::NothingArrived { expected } => format!(
            "nothing arrived, and this profile expects {}",
            expected.label()
        ),
        Concern::RunObjected { detail } => format!("the run objected: {detail}"),
        Concern::AddonPathUnreachable { name, win_path } => {
            format!("{name} registered {win_path}, which the sim's prefix cannot follow")
        }
        Concern::Unobserved { place, why } => format!("{place}: {why}"),
        Concern::Removed { place, what } => {
            format!(
                "{place}: {} removed \u{2014} {}",
                what.len(),
                what.join(", ")
            )
        }
    }
}

// ── small helpers ────────────────────────────────────────────────────────────

fn mono(ui: &mut egui::Ui, s: &str) {
    ui.label(
        egui::RichText::new(s)
            .monospace()
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::TEXT),
    );
}

/// `~/Downloads/setup.exe` is what a person types and what a terminal expands.
fn expand_tilde(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    match dirs::home_dir() {
        Some(home) => format!("{}{rest}", home.display()),
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use hangar_install::Engine;
    /// Cleared when the job is armed, not when it finishes, so re-typing the
    /// same path while the result is on screen is not fought over.
    #[test]
    fn arming_an_install_empties_the_path_box() {
        let mut state = InstallState::inert();
        state.typed = "/home/j/Downloads/kpvf-v3.zip".into();
        // What `arm_with` does before handing over to `arm`.
        state.typed.clear();
        assert!(state.typed.is_empty());
    }

    /// A record filed mid-session must show, though the list is read once.
    #[test]
    fn a_freshly_filed_record_makes_the_list_stale() {
        let state = InstallState::inert();
        assert!(
            !*state.history_stale.lock().expect("lock"),
            "nothing has been filed yet"
        );

        // What either writing task does after `file_default` succeeds.
        *state.history_stale.lock().expect("lock") = true;

        // The draw takes it, so the re-read happens once, not every frame.
        let taken = {
            let mut flag = state.history_stale.lock().expect("lock");
            std::mem::replace(&mut *flag, false)
        };
        assert!(taken);
        assert!(
            !*state.history_stale.lock().expect("lock"),
            "taken, not merely read"
        );
    }

    /// The row must not print the timestamp twice (as the key and again at the
    /// head of `InstallRecord::summary`).
    #[test]
    fn a_history_row_says_the_time_once_and_readably() {
        assert_eq!(when("2026-08-31T23-11-17Z"), "31 Aug 23:11");
        assert_eq!(when("2026-01-05T03-04-20Z"), "5 Jan 03:04");
        // Anything not the shape we file is shown as filed rather than
        // mangled into a date that is not one.
        assert_eq!(when("whatever"), "whatever");
        assert_eq!(when("2026-13-01T00-00-00Z"), "2026-13-01T00-00-00Z");
    }

    use super::*;

    #[test]
    fn an_inert_picker_opens_no_system_dialog() {
        let state = InstallState::inert();
        state.pick(false);
        state.pick(true);
        assert!(matches!(*state.picker.lock().unwrap(), PickerState::Idle));
    }

    #[test]
    fn picker_results_are_consumed_once_and_cancel_changes_nothing() {
        let state = InstallState::inert();
        *state.picker.lock().unwrap() =
            PickerState::Ready(Some(PathBuf::from("/scratch/package.zip")));
        assert_eq!(state.picked(), Some(PathBuf::from("/scratch/package.zip")));
        assert_eq!(state.picked(), None);
        *state.picker.lock().unwrap() = PickerState::Ready(None);
        assert_eq!(state.picked(), None);
        assert!(matches!(*state.picker.lock().unwrap(), PickerState::Idle));
    }

    /// A scratch prefix with the executables an install left in it.
    struct Prefix(PathBuf);

    impl Prefix {
        fn new(tag: &str, exes: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-register-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for rel in exes {
                let path = dir.join("drive_c").join(rel);
                std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
                std::fs::write(&path, b"MZ").expect("write");
            }
            Prefix(dir)
        }
    }

    impl Drop for Prefix {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn inspection_named(product: &str) -> Inspection {
        Inspection {
            path: PathBuf::from("/home/j/Downloads/setup.exe"),
            size: 738_456,
            pe: None,
            engine: Engine::Nsis,
            evidence: Vec::new(),
            packed: None,
            siblings: Default::default(),
            identity: hangar_install::Identity {
                product: Some(product.to_string()),
                vendor: Some("Orbx Simulation Systems Pty Ltd".into()),
                version: None,
                source: hangar_install::inspect::Source::VersionResource,
            },
            hits: Vec::new(),
        }
    }

    fn orbx_landed() -> classify::Report {
        classify::Report {
            landed: vec![classify::Landed::CompanionApp {
                exe: PathBuf::from(
                    "users/steamuser/AppData/Local/Programs/orbx-central/Orbx Central.exe",
                ),
                also: vec![PathBuf::from(
                    "users/steamuser/AppData/Local/Programs/orbx-central/Uninstall Orbx Central.exe",
                )],
            }],
            concerns: Vec::new(),
        }
    }

    /// A companion app is the one kind of item a filesystem scan cannot
    /// recognise, so an install that produces one must register it.
    #[test]
    fn a_companion_app_install_earns_a_library_entry() {
        let pfx = Prefix::new(
            "earns",
            &[
                "users/steamuser/AppData/Local/Programs/orbx-central/Orbx Central.exe",
                "users/steamuser/AppData/Local/Programs/orbx-central/Uninstall Orbx Central.exe",
            ],
        );

        let entry = entry_for(
            &orbx_landed(),
            &inspection_named("Orbx Central 4.5.48"),
            &pfx.0,
            Produces::CompanionApp,
        )
        .expect("a companion app earns an entry");

        assert_eq!(
            entry.name, "Orbx Central",
            "not the version, and not texconv"
        );
        assert_eq!(
            entry.exe,
            pfx.0.join(
                "drive_c/users/steamuser/AppData/Local/Programs/orbx-central/Orbx Central.exe"
            )
        );
        assert!(entry.enabled);
        assert_eq!(entry.kind, hangar_core::config::AppKind::Companion);
        // The one path that cannot be found again once the app is gone.
        assert_eq!(
            entry.uninstaller,
            Some(pfx.0.join(
                "drive_c/users/steamuser/AppData/Local/Programs/orbx-central/Uninstall Orbx Central.exe"
            ))
        );
    }

    /// Everything else the library finds by scanning.
    #[test]
    fn a_package_or_a_runtime_earns_nothing() {
        let pfx = Prefix::new("nothing", &["Program Files/dotnet/dotnet.exe"]);
        for produces in [Produces::Package, Produces::Runtime, Produces::SimAddon] {
            assert!(
                entry_for(
                    &orbx_landed(),
                    &inspection_named("Orbx Central 4.5.48"),
                    &pfx.0,
                    produces,
                )
                .is_none(),
                "{produces:?} must not register an app"
            );
        }
    }

    /// An executable stage 5 named but that is not on disk is not registered.
    /// An entry pointing at nothing would draw a launch button that cannot work.
    #[test]
    fn nothing_is_registered_for_an_executable_that_is_not_there() {
        let pfx = Prefix::new("absent", &[]);
        assert!(entry_for(
            &orbx_landed(),
            &inspection_named("Orbx Central 4.5.48"),
            &pfx.0,
            Produces::CompanionApp,
        )
        .is_none());
    }

    /// `AppEntry::name` is the key preflight settings are stored against, so a
    /// name that carries the version detaches them on every update.
    #[test]
    fn a_version_is_not_part_of_the_name() {
        assert_eq!(strip_version("Orbx Central 4.5.48"), "Orbx Central");
        assert_eq!(strip_version("Orbx Central"), "Orbx Central");
        // A number that is part of the name has no dots, or has letters on it.
        assert_eq!(
            strip_version("Microsoft Flight Simulator 2024"),
            "Microsoft Flight Simulator 2024"
        );
        assert_eq!(strip_version("FSUIPC7"), "FSUIPC7");
    }

    /// The stem wins when it reads as a name, and the product string is the
    /// fallback for an internal one like `PilotEdge.PilotClient.exe`.
    #[test]
    fn the_name_comes_from_the_executable_unless_it_is_an_internal_one() {
        let inspection = inspection_named("PilotEdge Pilot Client 1.7.0");
        assert_eq!(
            display_name(Path::new("orbx-central/Orbx Central.exe"), &inspection),
            "Orbx Central"
        );
        assert_eq!(
            display_name(
                Path::new("Pilot Client/PilotEdge.PilotClient.exe"),
                &inspection
            ),
            "PilotEdge Pilot Client"
        );
    }

    /// Keeps a render test from starting an install into a live Proton prefix.
    #[test]
    fn an_inert_state_refuses_to_run_an_installer() {
        let mut state = InstallState::inert();
        assert!(!state.is_live());

        // A clear gate, a planned command, a real prefix — everything an armed
        // confirm would have — and it still runs nothing.
        let analysis = Analysis {
            path: PathBuf::from("/tmp/setup.exe"),
            inspection: Err("not read".into()),
            profile: None,
            gate: Some(Report::default()),
            invocation: None,
            signatures: Vec::new(),
            target_prefix: Some(PathBuf::from("/tmp/pfx")),
            sim_running: false,
            reaches_sim_content: None,
        };
        let target = Target {
            msfs_prefix: PathBuf::from("/tmp/pfx"),
            community: PathBuf::from("/tmp/community"),
            exe_xml: PathBuf::from("/tmp/EXE.xml"),
            companion_prefix: None,
            proton_msfs: None,
            proton_companion: None,
        };
        arm(
            &mut state,
            &analysis,
            &target,
            &crate::journal::Journal::default(),
            None,
        );
        assert!(state.job.is_none(), "an inert state started an installer");
    }

    /// FINAL_REVIEW S4: a live install refuses at `arm` while something else
    /// changes Community — a Remove included. The analysis plans no command,
    /// so even a missing guard could not start anything here; the refusal
    /// line is what tells the two apart.
    #[test]
    fn a_live_install_refuses_while_a_removal_runs() {
        let mut state = InstallState::live(|| {});
        let analysis = Analysis {
            path: PathBuf::from("/tmp/setup.exe"),
            inspection: Err("not read".into()),
            profile: None,
            gate: Some(Report::default()),
            invocation: None,
            signatures: Vec::new(),
            target_prefix: Some(PathBuf::from("/tmp/pfx")),
            sim_running: false,
            reaches_sim_content: None,
        };
        let target = Target {
            msfs_prefix: PathBuf::from("/tmp/pfx"),
            community: PathBuf::from("/tmp/community"),
            exe_xml: PathBuf::from("/tmp/EXE.xml"),
            companion_prefix: None,
            proton_msfs: None,
            proton_companion: None,
        };
        arm(
            &mut state,
            &analysis,
            &target,
            &crate::journal::Journal::default(),
            Some("Wait for the removal that is running."),
        );
        assert!(state.job.is_none());
        assert_eq!(
            state.selection_error.as_deref(),
            Some("Wait for the removal that is running.")
        );
    }

    /// An empty report has no refusals, so `is_clear` is true — and that must
    /// not on its own be enough to run something. Every other precondition has
    /// to hold as well.
    #[test]
    fn a_clear_gate_alone_does_not_make_something_runnable() {
        let analysis = Analysis {
            path: PathBuf::from("/tmp/setup.exe"),
            inspection: Err("not read".into()),
            profile: None,
            gate: Some(Report::default()),
            // No command could be planned.
            invocation: None,
            signatures: Vec::new(),
            target_prefix: Some(PathBuf::from("/tmp/pfx")),
            sim_running: false,
            reaches_sim_content: None,
        };
        assert!(!analysis.runnable());
    }

    /// A refusal must close the run path however everything else looks.
    #[test]
    fn a_refusal_makes_it_unrunnable() {
        let report = Report {
            outcomes: vec![gate::Checked {
                label: "sim".into(),
                outcome: Outcome::Refuse(gate::Refusal::SimRunning),
            }],
        };
        assert!(!report.is_clear());
        let analysis = Analysis {
            path: PathBuf::from("/tmp/setup.exe"),
            inspection: Err("not read".into()),
            profile: None,
            gate: Some(report),
            invocation: None,
            signatures: Vec::new(),
            target_prefix: Some(PathBuf::from("/tmp/pfx")),
            sim_running: true,
            reaches_sim_content: None,
        };
        assert!(!analysis.runnable());
    }

    /// **Unchecked is never silently a pass.** Unjudged checks leave `is_clear`
    /// true, so `gate_rows` must draw them distinctly, not as a tick.
    #[test]
    fn unchecked_is_reported_separately_from_pass() {
        let report = Report {
            outcomes: vec![
                gate::Checked {
                    label: "prefix".into(),
                    outcome: Outcome::Pass("UserCfg.opt found".into()),
                },
                gate::Checked {
                    label: "sim content".into(),
                    outcome: Outcome::Unchecked("no companion prefix to resolve".into()),
                },
            ],
        };
        assert!(report.is_clear(), "unchecked does not refuse");
        assert_eq!(report.unchecked(), vec!["sim content"]);
        assert_eq!(report.refusals().len(), 0);
    }

    #[test]
    fn a_typed_tilde_expands() {
        let home = dirs::home_dir().expect("a home directory");
        assert_eq!(
            expand_tilde("~/Downloads/setup.exe"),
            format!("{}/Downloads/setup.exe", home.display())
        );
        assert_eq!(expand_tilde("/abs/setup.exe"), "/abs/setup.exe");
        assert_eq!(expand_tilde(""), "");
    }

    /// Which pipeline a path opens. Both the drop handler and the typed field
    /// ask this, and they must agree, or a typed `.zip` goes to the PE
    /// inspector and reports itself unreadable.
    #[test]
    fn only_an_exe_goes_to_the_seven_stages() {
        assert!(is_installer(Path::new("/home/j/Downloads/setup.exe")));
        assert!(
            is_installer(Path::new("/home/j/Downloads/Setup.EXE")),
            "case is not meaning"
        );

        for other in [
            "/home/j/Downloads/kpvf-v3.zip",
            "/home/j/Downloads/scenery.7z",
            "/home/j/Downloads/KPVF Placerville v3",
            "/home/j/Downloads/manual.pdf",
        ] {
            assert!(!is_installer(Path::new(other)), "{other}");
        }
    }

    /// A mistyped installer path must still open the installer half, or the
    /// error it gets back is about the wrong pipeline.
    #[test]
    fn a_path_that_does_not_exist_still_routes_on_its_name() {
        assert!(is_installer(Path::new("/nonexistent/setup.exe")));
        assert!(!is_installer(Path::new("/nonexistent/kpvf.zip")));
    }

    /// A directory is never an installer, whatever is inside it — an unzipped
    /// download routinely holds a `setup.exe`.
    #[test]
    fn a_directory_is_never_an_installer() {
        let dir = std::env::temp_dir().join(format!("hangar-route-{}.exe", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert!(!is_installer(&dir), "even named .exe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A drop clears the inspection rather than drawing both pipelines.
    #[test]
    fn forgetting_clears_the_inspection() {
        let mut state = InstallState::inert();
        state.selected = Some(PathBuf::from("/home/j/Downloads/setup.exe"));
        state.confirm = Some(PathBuf::from("/home/j/Downloads/setup.exe"));
        state.forget();
        assert!(state.selected.is_none());
        assert!(
            state.confirm.is_none(),
            "an armed run must not survive a new file"
        );
    }

    /// A verdict is rendered for a person, and "no complaint" must never read
    /// as "it worked".
    #[test]
    fn no_complaint_does_not_claim_success() {
        let text = describe_verdict(&run::Verdict::NoComplaint);
        assert!(text.contains("nothing in the run said it failed"), "{text}");
        assert!(!text.to_lowercase().contains("success"), "{text}");
    }

    #[test]
    fn a_vanished_run_names_what_it_looks_like() {
        let text = describe_verdict(&run::Verdict::Vanished {
            after: Duration::from_millis(900),
        });
        assert!(text.contains("torn down mid-unpack"), "{text}");
    }

    /// The console is bounded; the interesting lines are the last.
    #[test]
    fn the_console_keeps_the_newest_lines() {
        let console: Arc<Mutex<Vec<ConsoleLine>>> = Arc::new(Mutex::new(Vec::new()));
        for n in 0..(CONSOLE_LIMIT + 50) {
            push(&console, ConsoleLine::out(n.to_string()));
        }
        let lines = console.lock().unwrap();
        assert_eq!(lines.len(), CONSOLE_LIMIT);
        assert_eq!(lines.last().unwrap().text, (CONSOLE_LIMIT + 49).to_string());
    }
}
