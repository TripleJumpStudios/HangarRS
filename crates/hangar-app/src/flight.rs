//! Running a preflight from the UI.
//!
//! The sequencer in `hangar-runtime` decides what happens next; this drives it.
//! One step's action is in flight at a time, performed on the async runtime and
//! reported back through shared state, the same way the Apps section launches a
//! single app.
//!
//! # Conditions come from what the app already knows
//!
//! Readiness is **not** a TCP connect. The sim accepts the socket during its
//! loading screen and never completes the handshake, so a port check would
//! start every companion app several minutes early. The status bar's telemetry
//! link has already done a real handshake, so the answer is read off it.

use crate::env::TelemetryLink;
use hangar_runtime::preflight::{Action, Condition, Next, Preflight, Run};
use hangar_runtime::readiness::{self, Readiness, Watch};
use hangar_runtime::respawn::{Event, Tracker, RESPAWN_WAIT};
use hangar_runtime::{sim, LaunchRequest, Launched, ProtonEnv};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the sim has to be gone before the apps started with it are closed.
///
/// Long enough to ride out a crash-and-restart or a quick reload, short enough
/// that a stack of Wine processes is not still sitting there an hour after the
/// flight ended.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

/// How often a running Flight looks for its apps exiting and coming back.
/// The process monitor's cadence; a `/proc` read per frame would be wasteful.
pub const OBSERVE_EVERY: Duration = Duration::from_secs(2);

/// Where one action's result lands when it finishes.
///
/// `Ok` is what to record against the step, `Err` is why it failed. Shared
/// because the action runs on the async runtime and the UI reads it next frame.
type Outcome = Arc<Mutex<Option<Result<String, String>>>>;

/// How the sim gets started.
///
/// Injectable so a test that ticks a `Flight` through its first step cannot
/// open `steam://rungameid/2537590`; the real launcher is reached only through
/// [`Flight::start`].
pub type SimLauncher = fn(&str) -> std::io::Result<sim::Handoff>;

/// Whether the sim's process is up, asked once per tick.
///
/// Injectable so tests never read the real process table and pass whether or
/// not MSFS is running. [`Flight::with_launcher`] starts with a probe that
/// never sees a sim; the real one is [`sim::is_running`], reached only through
/// [`Flight::start`].
pub type SimProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// The one app with a connection adapter so far.
const BATC_EXE: &str = "BeyondATC.exe";

/// Where an app runs relative to the sim.
///
/// An app that **connects** to the sim has to come after it; an app that
/// **prepares content the sim reads at startup** (Project Airports' scenery
/// sync) has to come before, or the flight misses what was updated.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    /// Before the sim. For anything that prepares content.
    Before,
    /// After SimConnect answers. For anything that talks to the sim.
    After,
    /// Not part of this preflight.
    Off,
}

/// Build a preflight from a placement per app.
///
/// Order: everything placed **before**, then the sim, then everything placed
/// **after**. When anything runs before it, the sim step waits for
/// [`Condition::Manual`] unless every such app has a readiness probe: a fixed
/// delay cannot know a sync finished, so the sequence stops and asks.
#[cfg(test)]
pub fn build(placements: &BTreeMap<String, Placement>) -> Preflight {
    build_ordered(placements, &[])
}

/// The same ordered list drives both the editor and the launch sequence.
pub fn build_ordered(placements: &BTreeMap<String, Placement>, order: &[String]) -> Preflight {
    use hangar_runtime::preflight::Step;

    let names = ordered_names(placements, order);
    let before: Vec<&String> = names
        .iter()
        .filter(|name| placements[*name] == Placement::Before)
        .collect();
    let after: Vec<&String> = names
        .iter()
        .filter(|name| placements[*name] == Placement::After)
        .collect();

    let mut preflight = Preflight::new("Before a flight");

    for label in &before {
        preflight = preflight.with(
            Step::now(Action::LaunchApp {
                label: (*label).clone(),
            })
            .optional(),
        );
    }

    let sim_step = Action::LaunchSim {
        app_id: sim::MSFS_2024_APP_ID.to_string(),
    };
    // The gate opens itself only when every app before it can be observed.
    // One unprobed app and nothing can know the sequence is ready, so it asks.
    let all_observable = !before.is_empty()
        && before
            .iter()
            .all(|label| readiness::probe_for(label).is_some());
    preflight = preflight.with(match (before.is_empty(), all_observable) {
        (true, _) => Step::now(sim_step),
        (false, true) => Step::after(sim_step, Condition::AppsReady),
        (false, false) => Step::after(sim_step, Condition::Manual),
    });

    for label in after {
        preflight = preflight.with(
            Step::after(
                Action::LaunchApp {
                    label: label.clone(),
                },
                Condition::SimConnectReady,
            )
            .optional(),
        );
    }

    preflight
}

/// Which apps take part, kept between runs.
///
/// Only choices someone actually made are stored. An app that has never been
/// touched is absent, and picks up its computed default — *before* the sim if it
/// has a readiness probe, *after* otherwise. So installing something new joins
/// the sequence rather than silently arriving switched off, and switching
/// something off stays off.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// App name to placement, for apps whose placement was chosen by hand.
    #[serde(default)]
    pub placement: BTreeMap<String, Placement>,
    /// User order. Older files default to the previous alphabetical order.
    #[serde(default)]
    pub order: Vec<String>,
    /// Absent for ordinary renders/tests; enabled once by main.rs.
    #[serde(skip)]
    save_path: Option<std::path::PathBuf>,
    /// Which configured app each name is, for writing the file by ID.
    #[serde(skip)]
    ids: crate::app_ids::Ids,
    /// What loading made of every stored key (see `app_ids`).
    #[serde(skip)]
    pub report: Vec<String>,
}

fn ordered_names(placements: &BTreeMap<String, Placement>, order: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for name in order.iter().chain(placements.keys()) {
        if placements.contains_key(name) && !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlanEntry {
    App(String),
    Simulator,
}

impl Settings {
    pub fn enable_saving(&mut self) {
        self.save_path = Self::path();
    }
    pub fn plan(&self, placements: &BTreeMap<String, Placement>) -> Vec<PlanEntry> {
        let names = ordered_names(placements, &self.order);
        let apps_at = |at| {
            names
                .iter()
                .filter(move |name| placements[*name] == at)
                .cloned()
                .map(PlanEntry::App)
        };
        apps_at(Placement::Before)
            .chain([PlanEntry::Simulator])
            .chain(apps_at(Placement::After))
            .collect()
    }

    /// Move into a gap in the visible plan, including across the simulator.
    /// Unknown/stale drag payloads are ignored. No I/O here.
    pub fn move_entry(
        &mut self,
        placements: &BTreeMap<String, Placement>,
        entry: &PlanEntry,
        gap: usize,
    ) -> bool {
        let mut plan = self.plan(placements);
        let Some(from) = plan.iter().position(|item| item == entry) else {
            return false;
        };
        if gap > plan.len() {
            return false;
        }
        let to = gap - usize::from(from < gap);
        if from == to {
            return false;
        }
        let moved = plan.remove(from);
        plan.insert(to, moved);
        let mut placement = Placement::Before;
        let mut order = Vec::new();
        for item in plan {
            match item {
                PlanEntry::Simulator => placement = Placement::After,
                PlanEntry::App(name) => {
                    if placements[&name] != placement {
                        self.placement.insert(name.clone(), placement);
                    }
                    order.push(name);
                }
            }
        }
        for name in &self.order {
            if !order.contains(name) {
                order.push(name.clone());
            }
        }
        self.order = order;
        true
    }

    /// `$XDG_CONFIG_HOME/hangar-rs/preflight.toml`.
    pub fn path() -> Option<std::path::PathBuf> {
        hangar_core::paths::config_dir().map(|d| d.join("preflight.toml"))
    }

    /// Read them for the apps configured now, or start empty.
    ///
    /// A file that will not parse is logged and ignored rather than taking the
    /// tab down: the worst case is some switches reading as their defaults,
    /// which is visible and one click to fix. Reading never writes: names in
    /// an older file are translated in memory, and the file is rewritten by
    /// ID only when a choice changes.
    pub fn load_for(apps: &[crate::app_ids::AppRef]) -> Self {
        let Some(path) = Self::path() else {
            return Settings::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Settings::from_stored(crate::app_ids::Stored::default(), apps);
        };
        match toml::from_str(&text) {
            Ok(stored) => Settings::from_stored(stored, apps),
            Err(e) => {
                tracing::warn!(path = %path.display(), "ignoring preflight settings: {e}");
                Settings::default()
            }
        }
    }

    /// The pure half of [`Settings::load_for`].
    pub fn from_stored(stored: crate::app_ids::Stored, apps: &[crate::app_ids::AppRef]) -> Self {
        let loaded = crate::app_ids::load(stored, apps);
        Settings {
            placement: loaded.placement,
            order: loaded.order,
            save_path: None,
            ids: loaded.ids,
            report: loaded.report,
        }
    }

    /// The stable ID a configured app's choices are saved under.
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.ids.id_of(name)
    }

    /// The file this would write.
    pub fn to_stored(&self) -> crate::app_ids::Stored {
        crate::app_ids::store(&self.placement, &self.order, &self.ids)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.save_path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&self.to_stored())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, text)
    }
}

/// A preflight for someone who has none saved: the sim, then every app.
///
/// The Preflight tab builds from placements instead; this is the shape those
/// placements default to, and what the tests lean on.
#[cfg(test)]
pub fn suggested(app_labels: impl IntoIterator<Item = String>) -> Preflight {
    build(
        &app_labels
            .into_iter()
            .map(|label| (label, Placement::After))
            .collect(),
    )
}

struct StartedApp {
    label: String,
    key: String,
    session: Option<hangar_runtime::sim_session::Token>,
}

/// A preflight being run from the UI.
pub struct Flight {
    run: Run,
    /// The result of the action currently in flight, once it lands.
    pending: Option<Outcome>,
    requests: BTreeMap<String, LaunchRequest>,
    proton: Option<ProtonEnv>,
    launch_sim: SimLauncher,
    sim_probe: SimProbe,
    sim_proton: Option<ProtonEnv>,
    sessions: hangar_runtime::sim_session::Runtime,
    /// Set by [`Flight::proceed`], consumed by the gated step.
    proceed: bool,
    /// One per app started before the sim that can be observed.
    watches: Vec<Watch>,
    /// Apps **this flight started**, as label and `/proc` match key.
    ///
    /// Only ones it started: an adopted app was open before Run was pressed,
    /// and the preflight tidies up after itself, not after the person.
    ///
    /// Shared because a launch finishes on the async runtime.
    started: Arc<Mutex<Vec<StartedApp>>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    /// Whether the sim was ever seen running during this flight.
    ///
    /// Without it, "the sim is not running" at the start would look exactly
    /// like "the sim has exited" and tear everything down immediately.
    sim_seen: bool,
    /// When the sim was first noticed missing after having been seen.
    sim_gone_since: Option<Instant>,
    /// What the teardown did, once it has happened.
    torn_down: Option<Vec<String>>,
    /// Where the prefix keeps app data, for starting a watch.
    roaming: Option<std::path::PathBuf>,
    /// Where every step and every app's output is recorded.
    ///
    /// A default journal only writes into a buffer, so it needs no injection
    /// point.
    journal: crate::journal::Journal,
    /// Reads the process table to follow self-update restarts. Inert unless
    /// the app's live observer is passed in.
    observer: crate::flight_details::Observer,
    /// One per app this flight launched, by label (see `hangar_runtime::respawn`).
    trackers: BTreeMap<String, Tracker>,
    observed_at: Option<Instant>,
    /// The MSFS instance first seen while following this flight's apps.
    sim_instance: Option<hangar_runtime::observe::Identity>,
    /// Apps a Hangar row started, so a step never launches a second copy
    /// beside one that is away updating.
    rows: Option<crate::apps::AppActions>,
    /// BeyondATC's own log, watched from just before this flight launched it
    /// (`hangar_runtime::batc_ready`), and its last reading.
    batc: Option<(String, hangar_runtime::batc_ready::Watch)>,
    connections: BTreeMap<String, hangar_runtime::batc_ready::Connection>,
    connections_at: Option<Instant>,
    /// Set once a different MSFS instance replaces it: earlier, then now.
    sim_replaced: Option<(
        hangar_runtime::observe::Identity,
        hangar_runtime::observe::Identity,
    )>,
}

impl Flight {
    pub fn start(
        preflight: Preflight,
        requests: BTreeMap<String, LaunchRequest>,
        proton: Option<ProtonEnv>,
    ) -> Self {
        Flight::with_launcher(preflight, requests, proton, sim::launch)
            .with_sim_probe(Arc::new(sim::is_running))
    }

    /// A flight that starts the sim some other way.
    ///
    /// Tests pass a stub. Nothing else should need this.
    pub fn with_launcher(
        preflight: Preflight,
        requests: BTreeMap<String, LaunchRequest>,
        proton: Option<ProtonEnv>,
        launch_sim: SimLauncher,
    ) -> Self {
        let proton_roaming = proton
            .as_ref()
            .map(|p| p.wineprefix.join("drive_c/users/steamuser/AppData/Roaming"));
        Flight {
            run: Run::start(preflight),
            pending: None,
            requests,
            proton,
            launch_sim,
            sim_probe: Arc::new(|| false),
            sim_proton: None,
            sessions: hangar_runtime::sim_session::Runtime::default(),
            proceed: false,
            watches: Vec::new(),
            started: Arc::new(Mutex::new(Vec::new())),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sim_seen: false,
            sim_gone_since: None,
            torn_down: None,
            roaming: proton_roaming.clone(),
            journal: crate::journal::Journal::default(),
            observer: crate::flight_details::Observer::default(),
            trackers: BTreeMap::new(),
            observed_at: None,
            sim_instance: None,
            rows: None,
            batc: None,
            connections: BTreeMap::new(),
            connections_at: None,
            sim_replaced: None,
        }
    }

    pub fn with_sessions(
        mut self,
        env: Option<ProtonEnv>,
        sessions: hangar_runtime::sim_session::Runtime,
    ) -> Self {
        self.sim_proton = env;
        self.sessions = sessions;
        self
    }

    /// Consult the row launches' self-update tracking before launching.
    pub fn with_rows(mut self, actions: &crate::apps::AppActions) -> Self {
        self.rows = Some(actions.clone());
        self
    }

    /// Ask this probe whether the sim is up, instead of never seeing it.
    pub fn with_sim_probe(mut self, probe: SimProbe) -> Self {
        self.sim_probe = probe;
        self
    }

    /// Follow this flight's apps through the given process-table reader.
    pub fn with_observer(mut self, observer: crate::flight_details::Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Record this flight into the application's shared scrollback.
    pub fn with_journal(mut self, journal: &crate::journal::Journal) -> Self {
        self.journal = journal.clone();
        self
    }

    pub fn run(&self) -> &Run {
        &self.run
    }

    pub fn is_finished(&self) -> bool {
        self.run.is_finished() && self.pending.is_none()
    }

    /// Satisfy a manual gate — the person has said go.
    pub fn proceed(&mut self) {
        self.proceed = true;
    }

    /// Stop the sequence **and** close the apps it started.
    ///
    /// The sequencer rolls nothing back on its own (a half-run preflight is a
    /// normal state), but Cancel is a person asking for it to be undone.
    ///
    /// The sim is left alone: Steam owns what it started, and killing a sim
    /// mid-load is not something a Cancel button should do quietly.
    pub fn cancel(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.run.cancel();
        self.stop_started("cancelled");
    }

    /// Apps this flight started, by label.
    pub fn started_apps(&self) -> Vec<String> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|app| app.label.clone())
            .collect()
    }

    /// What the teardown closed, once it has run.
    pub fn torn_down(&self) -> Option<&[String]> {
        self.torn_down.as_deref()
    }

    /// Whether this flight is now watching for the sim to exit.
    pub fn is_watching_for_shutdown(&self) -> bool {
        self.run.is_finished()
            && self.sim_seen
            && self.torn_down.is_none()
            && !self.started_apps().is_empty()
    }

    /// How long the sim has been gone, if it has.
    pub fn sim_gone_for(&self) -> Option<Duration> {
        self.sim_gone_since.map(|since| since.elapsed())
    }

    /// Close everything this flight started.
    fn stop_started(&mut self, why: &str) {
        let apps: Vec<StartedApp> =
            std::mem::take(&mut *self.started.lock().unwrap_or_else(|e| e.into_inner()));
        if apps.is_empty() {
            return;
        }

        let labels: Vec<String> = apps.iter().map(|app| app.label.clone()).collect();
        tracing::info!(?labels, why, "preflight closing what it started");
        self.journal.note(
            "Preflight",
            format!("closing what it started \u{2014} {why}"),
        );
        for StartedApp {
            label,
            key,
            session,
        } in apps
        {
            // Before the close is sent, so its exit is never read as an update.
            self.sessions.close_requested(&key);
            let journal = self.journal.clone();
            let sessions = self.sessions.clone();
            // A Windows app in the companion prefix is asked to close first.
            let companion = self
                .requests
                .get(&label)
                .filter(|r| r.mode == hangar_runtime::RunMode::Prefix)
                .and(self.proton.clone());
            let tracker = self.trackers.get(&label).cloned();
            tokio::spawn(async move {
                if let Some(token) = session {
                    let sink = journal.sink(&label);
                    match sessions.stop_started(&token, Some(sink)).await {
                        Ok(()) => journal.done(&label, "closed"),
                        Err(e) => journal.fail(&label, e.to_string()),
                    }
                    return;
                }
                let outcome =
                    hangar_runtime::kill::stop_app(companion.as_ref(), &key, tracker.as_ref())
                        .await;
                tracing::info!(%label, "{}", outcome.summary());
                journal.note(&label, outcome.summary());
            });
        }
        self.torn_down = Some(labels);
    }

    /// Advance one frame's worth.
    ///
    /// `wake` is called when an action finishes, so the UI repaints without
    /// polling at full speed.
    pub fn tick(&mut self, sim: &TelemetryLink, wake: impl Fn() + Send + Clone + 'static) {
        self.watch_for_shutdown();
        self.follow_apps(Instant::now());
        self.read_connections(Instant::now());

        // Collect a finished action before asking what comes next.
        if let Some(slot) = &self.pending {
            let outcome = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            match outcome {
                Some(Ok(message)) => {
                    self.pending = None;
                    self.run.acted(message);
                }
                Some(Err(reason)) => {
                    self.pending = None;
                    // Nothing launched, so there is nothing to follow.
                    if let Some(Action::LaunchApp { label }) = self
                        .run
                        .states()
                        .iter()
                        .position(|s| matches!(s, hangar_runtime::preflight::State::Acting))
                        .map(|i| &self.run.preflight().steps[i].action)
                    {
                        self.trackers.remove(label);
                    }
                    self.run.failed(reason);
                }
                // Still running.
                None => return,
            }
        }

        let connected = sim.status().is_connected();
        let streaming = sim.is_streaming();
        let proceed = self.proceed;
        // Continue satisfies both gates, so a stalled sync is not a dead end.
        // An app that left to update is not ready, whatever its log said
        // before it went: the gate waits for the instance that comes back.
        let apps_ready = proceed
            || self
                .watches
                .iter()
                .all(|w| w.poll().is_ready() && self.updating_for(&w.label).is_none());
        // Asked only when a step waits on it: it is a /proc scan.
        let sim_up = &self.sim_probe;
        let next = self.run.tick(Instant::now(), |condition| match condition {
            Condition::Nothing => true,
            Condition::SimStarted => sim_up(),
            Condition::SimConnectReady => connected,
            Condition::TelemetryFlowing => streaming,
            Condition::AppsReady => apps_ready,
            Condition::Manual => proceed,
        });

        if let Next::Do { step } = next {
            // Consumed, so a later manual gate asks again rather than being
            // waved through by a press meant for the first one.
            self.proceed = false;
            let action = self.run.preflight().steps[step].action.clone();
            self.perform(action, wake);
        }
    }

    /// Look for this flight's apps exiting and coming back (self-update).
    ///
    /// The restarted instance becomes the one this flight follows: its status,
    /// and the readiness gate, which starts over on it. Stop and the
    /// sim-exit close already reach it — by name for a companion app, by
    /// launch for a sim-session app, whose runtime adopts the restart itself.
    pub(crate) fn follow_apps(&mut self, now: Instant) {
        if self.trackers.is_empty()
            || self
                .observed_at
                .is_some_and(|at| now.saturating_duration_since(at) < OBSERVE_EVERY)
        {
            return;
        }
        self.observed_at = Some(now);
        // A close HangarRS asked for — this flight, a row's Stop, or the
        // sim-session runtime — makes the exit that follows final.
        for tracker in self.trackers.values_mut() {
            if let Some(at) = self.sessions.closing_since(&tracker.key) {
                tracker.closing(at);
            }
        }
        let keys: Vec<String> = self.trackers.values().map(|t| t.key.clone()).collect();
        let observation = self.observer.observe(&keys);
        self.note_sim_instance(&observation);
        let mut events = Vec::new();
        for (label, tracker) in &mut self.trackers {
            let event = tracker.observe(observation.found(&tracker.key), now);
            if event != Event::None {
                events.push((label.clone(), event));
            }
        }
        for (label, event) in events {
            let watch = self.watches.iter_mut().find(|w| w.label == label);
            match event {
                Event::Exited(was) => {
                    if let Some(watch) = watch {
                        watch.rearm();
                    }
                    tracing::info!(target: "hangar_launch", source = "Flight", app = %label, pid = was.pid, "exited soon after launch; waiting for an update restart");
                    self.journal.note(
                        &label,
                        format!("pid {} exited soon after launch; waiting for it to restart after an update", was.pid),
                    );
                }
                Event::Respawned { was, now, gap } => {
                    // A new instance writes a new run's log; old evidence is void.
                    if let Some((batc, w)) = &mut self.batc {
                        if *batc == label {
                            w.rearm();
                            self.connections.remove(&label);
                        }
                    }
                    if let Some(watch) = watch {
                        if gap {
                            watch.restart_clock();
                        } else {
                            watch.rearm();
                        }
                    }
                    tracing::info!(target: "hangar_launch", source = "Flight", app = %label, was = was.pid, pid = now.pid, "update restart adopted");
                    self.journal.note(
                        &label,
                        format!("restarted after an update as pid {}", now.pid),
                    );
                }
                Event::GaveUp(was) => {
                    self.journal.note(
                        &label,
                        format!(
                            "pid {} did not come back within {} min",
                            was.pid,
                            RESPAWN_WAIT.as_secs() / 60
                        ),
                    );
                }
                Event::None | Event::Seen(_) | Event::Ended(_) | Event::Closed(_) => {}
            }
        }
    }

    /// A simulator restart invalidates session evidence: apps
    /// this flight started belong to the first instance and are not quietly
    /// attached to the new one. Recorded once; nothing is closed or moved.
    fn note_sim_instance(&mut self, observation: &hangar_runtime::observe::Observation) {
        let sims: Vec<_> = observation
            .found(sim::MSFS_2024_EXE)
            .map(|f| f.mains().map(|p| p.identity).collect())
            .unwrap_or_default();
        let [now] = sims[..] else { return };
        match self.sim_instance {
            None => self.sim_instance = Some(now),
            Some(was) if was != now && self.sim_replaced.is_none() => {
                self.sim_replaced = Some((was, now));
                tracing::info!(target: "hangar_launch", source = "Flight", was = was.pid, pid = now.pid, "simulator replaced");
                self.journal.note(
                    "Preflight",
                    format!(
                        "MSFS restarted (pid {} → {}); apps from the first start stay with it",
                        was.pid, now.pid
                    ),
                );
            }
            Some(_) => {}
        }
    }

    /// The MSFS instance this flight's apps were started with, and the one
    /// that replaced it, once that has happened.
    pub fn sim_replaced(
        &self,
    ) -> Option<(
        hangar_runtime::observe::Identity,
        hangar_runtime::observe::Identity,
    )> {
        self.sim_replaced
    }

    /// Read each connection adapter, at most every [`OBSERVE_EVERY`].
    fn read_connections(&mut self, now: Instant) {
        let Some((label, watch)) = &mut self.batc else {
            return;
        };
        if self
            .connections_at
            .is_some_and(|at| now.saturating_duration_since(at) < OBSERVE_EVERY)
        {
            return;
        }
        self.connections_at = Some(now);
        let status = watch.poll_at(now);
        let previous = self
            .connections
            .insert(label.clone(), status.connection.clone());
        if previous.as_ref() != Some(&status.connection) {
            tracing::info!(target: "hangar_launch", source = "Flight", app = %label, connection = %status.connection.describe(), generation = status.generation, "connection evidence");
            self.journal.note(
                label,
                format!("connection: {}", status.connection.describe()),
            );
        }
    }

    /// An app's own connection evidence, when an adapter has some.
    /// `None` means no adapter, or nothing fresh: the row says unverified.
    pub fn connection_of(&self, label: &str) -> Option<String> {
        use hangar_runtime::batc_ready::Connection;
        match self.connections.get(label)? {
            Connection::Unverified(_) => None,
            known => Some(known.describe()),
        }
    }

    /// How long an app of this flight has been away updating, if it is.
    pub fn updating_for(&self, label: &str) -> Option<Duration> {
        self.trackers
            .get(label)
            .and_then(|t| t.updating_for(Instant::now()))
    }

    /// The flight is over: MSFS came and went, and nothing it ran is left.
    ///
    /// The caller then returns to the launch list rather than staying
    /// on "Launch complete". An app still restarting after an update is not
    /// down.
    pub fn is_over(&self, sim_running: bool, apps_running: bool) -> bool {
        self.is_finished()
            && self.sim_seen
            && !sim_running
            && !apps_running
            && !self
                .trackers
                .keys()
                .any(|label| self.updating_for(label).is_some())
    }

    #[cfg(test)]
    pub fn saw_sim(&mut self) {
        self.sim_seen = true;
    }

    /// Follow an app as if this flight had just launched it.
    #[cfg(test)]
    pub(crate) fn follow_for_test(&mut self, label: &str, key: &str, watch: Option<Watch>) {
        self.trackers
            .insert(label.into(), Tracker::new(key, Instant::now()));
        self.watches.extend(watch);
    }

    /// Once the flight is done, close its apps when the sim goes away.
    ///
    /// Only after the sim has actually been seen running. Otherwise "not
    /// running yet" and "has exited" are indistinguishable, and the first tick
    /// would tear down everything it had just started.
    fn watch_for_shutdown(&mut self) {
        if self.torn_down.is_some() {
            return;
        }

        if (self.sim_probe)() {
            self.sim_seen = true;
            self.sim_gone_since = None;
            return;
        }
        if !self.sim_seen || !self.run.is_finished() {
            return;
        }

        match self.sim_gone_since {
            None => self.sim_gone_since = Some(Instant::now()),
            Some(since) if since.elapsed() >= SHUTDOWN_GRACE => {
                self.stop_started("the sim exited");
            }
            Some(_) => {}
        }
    }

    /// Start one action. Its result lands in `pending`.
    fn perform(&mut self, action: Action, wake: impl Fn() + Send + Clone + 'static) {
        let slot: Outcome = Arc::new(Mutex::new(None));
        self.pending = Some(Arc::clone(&slot));

        match action {
            Action::LaunchSim { app_id } => {
                // Synchronous and immediate: it opens a URL and returns. There
                // is no child to wait on — Steam owns what it starts.
                self.journal
                    .note("Preflight", format!("launching the sim, app id {app_id}"));
                let outcome = match (self.launch_sim)(&app_id) {
                    Ok(handoff) => {
                        self.journal.done("Preflight", handoff.url.clone());
                        Ok(format!("asked Steam to start it ({})", handoff.opener))
                    }
                    Err(e) => {
                        self.journal.fail("Preflight", e.to_string());
                        Err(e.to_string())
                    }
                };
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                wake();
            }

            Action::LaunchApp { label } => {
                // Begin watching before the app runs, so the offset excludes
                // everything the previous run wrote.
                if let (Some(probe), Some(roaming)) =
                    (readiness::probe_for(&label), self.roaming.as_deref())
                {
                    self.watches.push(Watch::begin(&label, probe, roaming));
                }

                let Some(request) = self.requests.get(&label).cloned() else {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Err(format!("{label} is no longer in the library")));
                    return;
                };
                self.trackers.insert(
                    label.clone(),
                    Tracker::new(request.match_key(), Instant::now()),
                );
                // BATC's connection evidence: watched from before the launch,
                // so nothing an earlier run wrote can certify this one. A
                // machine read, so it follows the observer's live switch.
                if request.mode == hangar_runtime::RunMode::SimSession
                    && request.match_key().eq_ignore_ascii_case(BATC_EXE)
                    && self.observer.is_live()
                {
                    if let Some(sim) = &self.sim_proton {
                        let dir = hangar_runtime::batc_ready::log_dir(&sim.wineprefix);
                        self.batc = Some((
                            label.clone(),
                            hangar_runtime::batc_ready::Watch::begin(&dir),
                        ));
                    }
                }
                let Some(env) = (if request.mode == hangar_runtime::RunMode::SimSession {
                    self.sim_proton.clone()
                } else {
                    self.proton.clone()
                }) else {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Err("no Proton environment".to_string()));
                    return;
                };

                // Decide before anything runs, from one read of the
                // process table through the injected observer.
                let key = request.match_key();
                let resolution = crate::apps::resolve_launch(
                    &request,
                    &env,
                    &self.observer.observe(std::slice::from_ref(&key)),
                    self.rows
                        .as_ref()
                        .and_then(|rows| rows.away(&key, Instant::now())),
                );
                self.journal
                    .note(&label, crate::apps::attempt_line(&resolution));
                tracing::info!(target: "hangar_launch", source = "Flight", app = %label, attempt = resolution.attempt, decision = ?resolution.decision, "launch resolved");
                if let Some(refused) = crate::apps::refusal(&label, resolution.decision) {
                    self.journal.fail(&label, refused.clone());
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(refused));
                    wake();
                    return;
                }
                let decision = resolution.decision;

                let sessions = self.sessions.clone();
                let started = Arc::clone(&self.started);
                let cancelled = Arc::clone(&self.cancelled);
                let journal = self.journal.clone();
                let sink = journal.sink(&label);
                tracing::info!(target: "hangar_launch", source = "Flight", app = %label, mode = ?request.mode, prefix = %env.wineprefix.display(), "app launch requested");
                journal.note(&label, format!("launching {}", request.exe.display()));
                tokio::spawn(async move {
                    let launched = if request.mode == hangar_runtime::RunMode::SimSession {
                        sessions.launch(&env, &request, Some(sink)).await
                    } else {
                        hangar_runtime::launch_with(&env, &request, Some(sink)).await
                    };
                    let outcome = match launched {
                        Launched::Adopted(app) => Ok(match decision {
                            hangar_runtime::resolve::Decision::AdoptUnverified(_) => format!(
                                "already running, adopted pid {}; environment unverified",
                                app.pid
                            ),
                            _ => format!("already running, adopted pid {}", app.pid),
                        }),
                        Launched::Started(app) => {
                            let session = sessions.owned_token(&request, app.pid);
                            let cancelled_during_launch = {
                                let mut started = started.lock().unwrap_or_else(|e| e.into_inner());
                                let cancelled = cancelled.load(std::sync::atomic::Ordering::SeqCst);
                                if !cancelled
                                    && (request.mode != hangar_runtime::RunMode::SimSession
                                        || session.is_some())
                                {
                                    started.push(StartedApp {
                                        label: label.clone(),
                                        key: key.clone(),
                                        session: session.clone(),
                                    });
                                }
                                cancelled
                            };
                            if cancelled_during_launch {
                                if let Some(token) = session {
                                    sessions
                                        .stop_started(&token, Some(journal.sink(&label)))
                                        .await
                                        .map_err(|e| e.to_string())
                                        .map(|()| "cancelled; app closed".to_string())
                                } else if request.mode == hangar_runtime::RunMode::SimSession {
                                    Ok("cancelled; original app already exited".into())
                                } else {
                                    let result = hangar_runtime::kill_app(&key).await;
                                    if result.is_stopped() {
                                        Ok(result.summary())
                                    } else {
                                        Err(result.summary())
                                    }
                                }
                            } else {
                                Ok(format!("started as pid {}", app.pid))
                            }
                        }
                        Launched::Failed { outcome, log } => {
                            for line in &log {
                                journal.output(&label, line);
                            }
                            Err(format!("did not survive: {outcome:?}"))
                        }
                        Launched::SpawnError(e) => Err(e),
                    };
                    tracing::info!(target: "hangar_launch", app = %label, result = ?outcome, "flight app result");
                    match &outcome {
                        Ok(message) => journal.done(&label, message.clone()),
                        Err(message) => journal.fail(&label, message.clone()),
                    }
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                    wake();
                });
            }

            Action::ApplyProfile { profile } => {
                // Not done from a sequence: moving directories leaves state
                // behind if it dies halfway, so it stays behind the
                // Community-folder write gate.
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(format!(
                    "applying profile {profile:?} from a preflight is not enabled yet"
                )));
                wake();
            }
        }
    }

    /// What the UI should say about a step.
    #[cfg(test)]
    pub fn state_of(&self, step: usize) -> &hangar_runtime::preflight::State {
        &self.run.states()[step]
    }

    /// What each observable app is doing, for the UI.
    pub fn app_readiness(&self) -> Vec<(String, Readiness)> {
        self.watches
            .iter()
            .map(|w| {
                let readiness = match self.updating_for(&w.label) {
                    Some(_) => Readiness::Working {
                        detail: "updating \u{2014} waiting for it to restart".into(),
                    },
                    None => w.poll(),
                };
                (w.label.clone(), readiness)
            })
            .collect()
    }

    /// The step currently being waited on, and what for.
    pub fn waiting_on(&self) -> Option<(usize, Condition, std::time::Duration)> {
        let mut probe = self.run.clone();
        match probe.tick(Instant::now(), |_| false) {
            Next::Waiting {
                step,
                condition,
                waited,
                ..
            } => Some((step, condition, waited)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_runtime::preflight::State;

    /// A launcher that starts nothing.
    ///
    /// **Every test that ticks a `Flight` must use this.** The real one opens
    /// `steam://rungameid/2537590`.
    fn stub(app_id: &str) -> std::io::Result<sim::Handoff> {
        Ok(sim::Handoff {
            url: format!("steam://rungameid/{app_id}"),
            opener: "stub".into(),
        })
    }

    fn placements(pairs: &[(&str, Placement)]) -> BTreeMap<String, Placement> {
        pairs
            .iter()
            .map(|(label, p)| ((*label).to_string(), *p))
            .collect()
    }

    #[test]
    fn shipping_a_probe_for_an_app_means_it_belongs_before_the_sim() {
        // The two have to agree: a probe exists because the app does startup
        // work the sim must wait for. One without the other is a bug.
        for (label, _) in hangar_runtime::readiness::PROBES
            .iter()
            .map(|p| (p.matches, p))
        {
            let preflight = build(&placements(&[(label, Placement::Before)]));
            assert!(
                matches!(preflight.steps[0].action, Action::LaunchApp { .. }),
                "{label} should be able to run before the sim"
            );
            assert_eq!(sim_gate(&preflight), Condition::AppsReady, "for {label}");
        }
    }

    #[test]
    fn only_hand_made_choices_are_stored() {
        // An app nobody has touched must not be written out as "off", or
        // installing something new would silently leave it out of the sequence.
        let settings = Settings::default();
        assert!(settings.placement.is_empty());
        let text = toml::to_string_pretty(&settings).expect("renders");
        assert!(!text.contains("BeyondATC"), "{text}");
    }

    #[test]
    fn dragged_order_survives_reload_and_is_the_order_that_runs() {
        let original = placements(&[
            ("Zulu", Placement::After),
            ("Alpha", Placement::After),
            ("Project Airports", Placement::Before),
            ("Off", Placement::Off),
        ]);
        let mut settings = Settings::default();
        assert!(settings.move_entry(&original, &PlanEntry::App("Zulu".into()), 2));
        let saved = toml::to_string(&settings).unwrap();
        let restored: Settings = toml::from_str(&saved).unwrap();
        let plan = restored.plan(&original);
        assert_eq!(
            plan,
            vec![
                PlanEntry::App("Project Airports".into()),
                PlanEntry::Simulator,
                PlanEntry::App("Zulu".into()),
                PlanEntry::App("Alpha".into())
            ]
        );
        let run = build_ordered(&original, &restored.order);
        let executed: Vec<_> = run
            .steps
            .iter()
            .map(|step| match &step.action {
                Action::LaunchApp { label } => PlanEntry::App(label.clone()),
                Action::LaunchSim { .. } => PlanEntry::Simulator,
                _ => panic!("unexpected action"),
            })
            .collect();
        assert_eq!(plan, executed);
        assert_eq!(sim_gate(&run), Condition::AppsReady);
        assert_eq!(run.steps[2].wait_for, Condition::SimConnectReady);
    }

    #[test]
    fn dragging_the_simulator_changes_placements_without_freezing_defaults() {
        let mut effective = placements(&[("Alpha", Placement::After), ("Zulu", Placement::After)]);
        let mut settings = Settings::default();
        assert!(settings.move_entry(&effective, &PlanEntry::Simulator, 2));
        assert_eq!(
            settings.placement,
            placements(&[("Alpha", Placement::Before)])
        );
        effective.extend(settings.placement.clone());
        assert_eq!(
            settings.plan(&effective),
            vec![
                PlanEntry::App("Alpha".into()),
                PlanEntry::Simulator,
                PlanEntry::App("Zulu".into())
            ]
        );
        assert!(settings.move_entry(&effective, &PlanEntry::App("Alpha".into()), 3));
        effective.extend(settings.placement.clone());
        assert_eq!(
            settings.plan(&effective),
            vec![
                PlanEntry::Simulator,
                PlanEntry::App("Zulu".into()),
                PlanEntry::App("Alpha".into())
            ]
        );
    }

    #[test]
    fn old_settings_new_apps_duplicates_and_stale_drags_are_safe() {
        let old: Settings = toml::from_str("[placement]\nAlpha = 'after'\n").unwrap();
        assert!(old.order.is_empty());
        let choices = placements(&[
            ("Alpha", Placement::After),
            ("Zulu", Placement::After),
            ("Off", Placement::Off),
        ]);
        let mut settings = Settings {
            order: vec!["Zulu".into(), "Removed".into(), "Zulu".into(), "Off".into()],
            ..old
        };
        assert_eq!(
            settings.plan(&choices),
            vec![
                PlanEntry::Simulator,
                PlanEntry::App("Zulu".into()),
                PlanEntry::App("Alpha".into())
            ]
        );
        let before = settings.clone();
        assert!(!settings.move_entry(&choices, &PlanEntry::App("Removed".into()), 0));
        assert!(!settings.move_entry(&choices, &PlanEntry::Simulator, 99));
        assert!(!settings.move_entry(&choices, &PlanEntry::Simulator, 1));
        assert_eq!(before, settings);
        assert!(settings.save_path.is_none());
        settings.save().expect("inert settings perform no write");
    }

    #[test]
    fn the_settings_file_is_something_a_person_could_edit() {
        let mut settings = Settings::default();
        settings
            .placement
            .insert("BeyondATC".into(), Placement::Off);
        settings
            .placement
            .insert("Project Airports".into(), Placement::Before);

        let text = toml::to_string_pretty(&settings).expect("renders");
        // TOML quotes a key with a space in it, which is fine and still
        // hand-editable.
        assert!(text.contains(r#"BeyondATC = "off""#), "{text}");
        assert!(text.contains(r#""Project Airports" = "before""#), "{text}");

        let back: Settings = toml::from_str(&text).expect("parses");
        assert_eq!(back, settings);
    }

    #[test]
    fn a_settings_file_that_will_not_parse_does_not_take_the_tab_down() {
        // Worst case is switches reading as their defaults, which is visible
        // and one click to fix.
        assert!(toml::from_str::<Settings>("this is not = = toml").is_err());
        assert_eq!(Settings::default().placement.len(), 0);
    }

    #[test]
    fn shipping_a_probe_for_an_app_means_it_can_gate_the_sim() {
        // The two have to agree: a probe exists because the app does startup
        // work the sim must wait for. One without the other is a bug.
        for probe in hangar_runtime::readiness::PROBES {
            let preflight = build(&placements(&[(probe.matches, Placement::Before)]));
            assert!(
                matches!(preflight.steps[0].action, Action::LaunchApp { .. }),
                "{} should be able to run before the sim",
                probe.matches
            );
            assert_eq!(
                sim_gate(&preflight),
                Condition::AppsReady,
                "and open the gate for {}",
                probe.matches
            );
        }
    }

    #[test]
    fn an_app_that_prepares_content_runs_before_the_sim() {
        // Project Airports syncs updated scenery, which the sim only sees if
        // the sync happened first.
        let preflight = build(&placements(&[
            ("Project Airports", Placement::Before),
            ("BeyondATC", Placement::After),
        ]));

        assert_eq!(preflight.len(), 3);
        assert_eq!(
            preflight.steps[0].action,
            Action::LaunchApp {
                label: "Project Airports".into()
            }
        );
        assert!(matches!(
            preflight.steps[1].action,
            Action::LaunchSim { .. }
        ));
        assert_eq!(
            preflight.steps[2].action,
            Action::LaunchApp {
                label: "BeyondATC".into()
            }
        );
    }

    fn sim_gate(preflight: &Preflight) -> Condition {
        preflight
            .steps
            .iter()
            .find(|s| matches!(s.action, Action::LaunchSim { .. }))
            .expect("a sim step")
            .wait_for
    }

    #[test]
    fn an_observable_app_opens_the_gate_by_itself() {
        // Project Airports writes `sync.done` to its own log, so nothing has to
        // be guessed and nobody has to press anything.
        let preflight = build(&placements(&[("Project Airports", Placement::Before)]));
        assert_eq!(sim_gate(&preflight), Condition::AppsReady);
    }

    #[test]
    fn one_app_that_cannot_be_observed_makes_the_whole_gate_manual() {
        // Readiness is only knowable for apps with a probe. Mixing one that has
        // none means the sequence cannot know it is ready, so it asks.
        let preflight = build(&placements(&[
            ("Project Airports", Placement::Before),
            ("Some Other Tool", Placement::Before),
        ]));
        assert_eq!(sim_gate(&preflight), Condition::Manual);
    }

    #[test]
    fn with_nothing_before_it_the_sim_starts_straight_away() {
        // No gate to ask about, so nothing should stop and wait for a click.
        let preflight = build(&placements(&[("BeyondATC", Placement::After)]));
        assert_eq!(preflight.steps[0].wait_for, Condition::Nothing);
        assert!(matches!(
            preflight.steps[0].action,
            Action::LaunchSim { .. }
        ));
    }

    #[test]
    fn an_app_switched_off_is_not_in_the_sequence() {
        let preflight = build(&placements(&[
            ("BeyondATC", Placement::Off),
            ("PilotEdge", Placement::After),
        ]));
        assert_eq!(preflight.len(), 2);
        assert!(!preflight
            .steps
            .iter()
            .any(|s| s.action.label() == "BeyondATC"));
    }

    #[test]
    fn every_companion_app_is_optional() {
        let preflight = build(&placements(&[
            ("Project Airports", Placement::Before),
            ("BeyondATC", Placement::After),
        ]));
        for step in &preflight.steps {
            match step.action {
                Action::LaunchApp { .. } => {
                    assert!(step.optional, "one app failing must not abandon the flight")
                }
                _ => assert!(!step.optional, "the sim is not optional"),
            }
        }
    }

    #[test]
    fn a_suggested_preflight_launches_the_sim_before_anything_else() {
        let preflight = suggested(["BeyondATC".to_string(), "PilotEdge".to_string()]);
        assert_eq!(preflight.len(), 3);
        assert!(matches!(
            preflight.steps[0].action,
            Action::LaunchSim { .. }
        ));
        assert_eq!(preflight.steps[0].wait_for, Condition::Nothing);
    }

    #[test]
    fn a_preflight_with_no_apps_is_still_a_preflight() {
        let preflight = suggested([]);
        assert_eq!(preflight.len(), 1);
        assert!(!preflight.is_empty());
    }

    #[test]
    fn ticking_a_flight_never_reaches_the_real_launcher() {
        let preflight = suggested(["BeyondATC".to_string()]);
        let mut flight = Flight::with_launcher(preflight, BTreeMap::new(), None, stub);
        let sim = TelemetryLink::idle();

        // One tick performs the action; the next collects its result. Until
        // then the step is `Acting`, which is what stops a second launch being
        // fired while the first is still in flight.
        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(0), State::Acting));

        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(0), State::Done(_)));
        assert!(!flight.is_finished(), "the app step is still to come");

        // Step two waits for SimConnect, which an idle link never reports, so
        // nothing is launched and nothing hangs.
        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(1), State::Waiting));
    }

    #[test]
    fn a_gate_holds_until_told_and_then_goes_once() {
        // An unprobed app, so the gate is the manual one.
        let preflight = build(&placements(&[("Some Other Tool", Placement::Before)]));
        let mut flight = Flight::with_launcher(preflight, BTreeMap::new(), None, stub);
        let sim = TelemetryLink::idle();

        // The app step fails — there is no request for it — which is optional,
        // so the sequence carries on to the gated sim step.
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(0), State::Failed(_)));

        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(1), State::Waiting), "gated");

        flight.proceed();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(1), State::Done(_)));
    }

    #[test]
    fn an_app_with_no_proton_environment_fails_rather_than_hanging() {
        let preflight = Preflight::new("just an app").with(hangar_runtime::preflight::Step::now(
            Action::LaunchApp {
                label: "BeyondATC".into(),
            },
        ));
        let mut flight = Flight::with_launcher(preflight, BTreeMap::new(), None, stub);
        let sim = TelemetryLink::idle();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(matches!(flight.state_of(0), State::Failed(_)));
    }

    /// An app a Hangar row started and that is away updating is waited
    /// for, never launched a second time beside it. No runtime exists here,
    /// so reaching a launch would panic rather than start anything.
    #[test]
    fn a_step_does_not_launch_beside_a_row_app_that_is_updating() {
        use respawn_fixture::{with, KEY};
        let request = LaunchRequest {
            label: "Project Airports".into(),
            exe: format!("/nonexistent/{KEY}").into(),
            args: vec![],
            mode: hangar_runtime::RunMode::Prefix,
        };
        let rows = crate::apps::AppActions::default();
        let journal = crate::journal::Journal::default();
        let t0 = Instant::now();
        rows.follow("Project Airports", KEY, t0);
        rows.take_observation(&with(&[41]), &journal, t0);
        rows.take_observation(&with(&[]), &journal, t0 + Duration::from_secs(5));
        assert!(rows.is_updating(KEY));

        let preflight =
            Preflight::new("one app").with(hangar_runtime::Step::now(Action::LaunchApp {
                label: request.label.clone(),
            }));
        let env = ProtonEnv {
            wineprefix: "/companion/pfx".into(),
            proton_script: "/nonexistent/proton".into(),
            wine_binary: "/nonexistent/wine".into(),
            compat_data: "/nonexistent".into(),
            compat_install: "/nonexistent".into(),
            steam_client: "/nonexistent".into(),
            is_flatpak: false,
        };
        let mut flight = Flight::with_launcher(
            preflight,
            BTreeMap::from([(request.label.clone(), request)]),
            Some(env),
            stub,
        )
        .with_rows(&rows);
        let sim = TelemetryLink::idle();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        match flight.state_of(0) {
            State::Failed(why) => assert!(why.contains("restarting after an update"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn sim_session_preflight_dispatch_uses_the_inert_session_capability() {
        let request = LaunchRequest {
            label: "BeyondATC".into(),
            exe: "/nonexistent/BeyondATC.exe".into(),
            args: vec![],
            mode: hangar_runtime::RunMode::SimSession,
        };
        let preflight =
            Preflight::new("session fixture").with(hangar_runtime::Step::now(Action::LaunchApp {
                label: request.label.clone(),
            }));
        let env = ProtonEnv {
            wineprefix: "/nonexistent/sim/pfx".into(),
            proton_script: "/nonexistent/proton".into(),
            wine_binary: "/nonexistent/wine".into(),
            compat_data: "/nonexistent/sim".into(),
            compat_install: "/nonexistent/game".into(),
            steam_client: "/nonexistent/steam".into(),
            is_flatpak: false,
        };
        // No companion environment: succeeding in reaching the inert refusal
        // proves this path selected the simulator environment.
        let mut flight = Flight::with_launcher(
            preflight,
            BTreeMap::from([(request.label.clone(), request)]),
            None,
            stub,
        )
        .with_sessions(Some(env), hangar_runtime::sim_session::Runtime::default());
        let sim = TelemetryLink::idle();
        flight.tick(&sim, || {});
        for _ in 0..100 {
            tokio::task::yield_now().await;
            flight.tick(&sim, || {});
            if flight.is_finished() {
                break;
            }
        }
        match flight.state_of(0) {
            State::Failed(reason) => assert!(reason.contains("not enabled"), "{reason}"),
            other => panic!("expected inert action refusal: {other:?}"),
        }
        assert!(flight.started_apps().is_empty());
    }

    #[test]
    fn cancelling_marks_the_rest_skipped_and_closes_what_it_started() {
        let preflight = suggested(["BeyondATC".to_string()]);
        let mut flight = Flight::with_launcher(preflight, BTreeMap::new(), None, stub);

        // Nothing was started here, so there is nothing to close — the point is
        // that cancelling reaches for the list rather than ignoring it.
        assert!(flight.started_apps().is_empty());
        flight.cancel();

        assert!(matches!(flight.state_of(1), State::Skipped));
        assert!(flight.run().is_cancelled());
        assert_eq!(flight.torn_down(), None, "nothing had been started");
    }

    #[test]
    fn a_flight_that_never_saw_the_sim_does_not_tear_anything_down() {
        // The trap: on a machine with no sim running, "not started yet" and
        // "has exited" look identical. Tearing down on the first tick would
        // close the apps the flight had just started.
        let preflight = suggested([]);
        let mut flight = Flight::with_launcher(preflight, BTreeMap::new(), None, stub);
        let sim = TelemetryLink::idle();

        for _ in 0..5 {
            flight.tick(&sim, || {});
        }
        assert!(flight.is_finished());
        assert_eq!(flight.torn_down(), None);
        assert!(
            !flight.is_watching_for_shutdown(),
            "there is nothing to watch for"
        );
        assert_eq!(flight.sim_gone_for(), None);
    }

    mod respawn_fixture {
        use hangar_runtime::observe::{Fact, Found, Identity, Namespace, Observation, Process};

        pub const KEY: &str = "Project Airports.exe";

        pub fn with(pids: &[u32]) -> Observation {
            let mut obs = Observation::empty(&[KEY.into()]);
            obs.apps.insert(
                KEY.into(),
                Found {
                    exact: pids
                        .iter()
                        .map(|&pid| Process {
                            identity: Identity {
                                pid,
                                start: pid as u64,
                            },
                            helper: false,
                            prefix: Fact::Known("/companion/pfx".into()),
                            user_ns: Fact::Known(Namespace::fixture(31837)),
                            mount_ns: Fact::Known(Namespace::fixture(31838)),
                        })
                        .collect(),
                    similar: vec![],
                },
            );
            obs
        }
    }

    const PA_START: &str = r#"{"time":"2026-10-10T20:17:05.075Z","level":"info","scope":"main","message":"sync.start","data":{"reason":"build-complete"}}"#;
    const PA_DONE_IDLE: &str = r#"{"time":"2026-10-10T20:17:30.000Z","level":"info","scope":"main","message":"sync.done","data":{"installed":0,"updated":0,"removed":0,"failed":0,"outcome":"ok"}}"#;
    const PA_BOOT: &str = r#"{"time":"2026-10-10T20:16:14.561Z","level":"info","scope":"main","message":"community.detect_startup"}"#;

    fn pa_log(roaming: &std::path::Path, lines: &[&str]) {
        let dir = roaming.join("@project-airports/desktop/logs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("companion-2026-10-10.log"),
            lines.join("\n") + "\n",
        )
        .unwrap();
    }

    /// Project Airports restarts to update while the sim waits on it.
    /// The sim must not start until the instance that came back has synced.
    #[test]
    fn the_sync_gate_waits_for_the_instance_that_comes_back_from_an_update() {
        use respawn_fixture::{with, KEY};
        let roaming =
            std::env::temp_dir().join(format!("hangar-flight-respawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&roaming);
        pa_log(&roaming, &[PA_BOOT]);
        let probe = readiness::probe_for("Project Airports").unwrap();
        let watch = Watch::begin("Project Airports", probe, &roaming);

        let preflight = Preflight::new("gate").with(hangar_runtime::Step::after(
            Action::LaunchSim {
                app_id: "2537590".into(),
            },
            Condition::AppsReady,
        ));
        let observer = crate::flight_details::Observer::given(with(&[30051]));
        let mut flight = Flight::with_launcher(preflight, BTreeMap::new(), None, stub)
            .with_observer(observer.clone());
        flight.follow_for_test("Project Airports", KEY, Some(watch));
        let sim = TelemetryLink::idle();
        let t0 = Instant::now();
        let step = |flight: &mut Flight, at: u64| {
            flight.follow_apps(t0 + Duration::from_secs(at));
            flight.tick(&sim, || {});
        };

        // The old instance speaks and even finishes a sync of its own.
        step(&mut flight, 0);
        pa_log(&roaming, &[PA_BOOT, PA_BOOT, PA_START, PA_DONE_IDLE]);
        // It leaves to update before the gate is consulted again.
        observer.set(with(&[]));
        step(&mut flight, 23);
        assert!(flight.updating_for("Project Airports").is_some());
        assert!(
            matches!(flight.state_of(0), State::Waiting),
            "updating is not ready"
        );
        assert!(flight.app_readiness()[0].1.describe().contains("updating"));

        // The new instance arrives; its window starts over, the old sync is history.
        observer.set(with(&[30402]));
        step(&mut flight, 27);
        assert_eq!(flight.updating_for("Project Airports"), None);
        assert!(matches!(flight.state_of(0), State::Waiting));
        assert!(matches!(
            flight.app_readiness()[0].1,
            Readiness::Starting { .. }
        ));

        // Its own sync begins, then finishes: only now does the sim go.
        pa_log(
            &roaming,
            &[PA_BOOT, PA_BOOT, PA_START, PA_DONE_IDLE, PA_BOOT, PA_START],
        );
        step(&mut flight, 51);
        assert!(matches!(flight.state_of(0), State::Waiting));
        pa_log(
            &roaming,
            &[
                PA_BOOT,
                PA_BOOT,
                PA_START,
                PA_DONE_IDLE,
                PA_BOOT,
                PA_START,
                PA_DONE_IDLE,
            ],
        );
        step(&mut flight, 76);
        step(&mut flight, 78);
        assert!(
            matches!(flight.state_of(0), State::Done(_)),
            "{:?}",
            flight.state_of(0)
        );

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_flight_is_over_only_once_the_sim_and_its_apps_are_down() {
        use respawn_fixture::{with, KEY};
        let observer = crate::flight_details::Observer::given(with(&[1]));
        let mut flight = Flight::with_launcher(suggested([]), BTreeMap::new(), None, stub)
            .with_observer(observer.clone());
        let sim = TelemetryLink::idle();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(flight.is_finished());
        assert!(
            !flight.is_over(false, false),
            "MSFS was never seen: nothing to end"
        );
        flight.saw_sim();
        assert!(!flight.is_over(true, false));
        assert!(!flight.is_over(false, true));
        assert!(flight.is_over(false, false));

        // An app away updating is not down.
        flight.follow_for_test("Project Airports", KEY, None);
        let t0 = Instant::now();
        flight.follow_apps(t0);
        observer.set(with(&[]));
        flight.follow_apps(t0 + OBSERVE_EVERY);
        assert!(!flight.is_over(false, false));
    }

    /// The shutdown watcher asks the injected probe, never `/proc`, so
    /// this holds with MSFS up on the machine running the tests and in CI.
    #[test]
    fn the_shutdown_watcher_sees_the_sim_only_through_its_probe() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut flight = Flight::with_launcher(suggested([]), BTreeMap::new(), None, stub);
        let sim = TelemetryLink::idle();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(flight.is_finished());
        assert!(!flight.sim_seen, "the default probe never sees a sim");

        let up = Arc::new(AtomicBool::new(true));
        let probe = Arc::clone(&up);
        let mut flight = Flight::with_launcher(suggested([]), BTreeMap::new(), None, stub)
            .with_sim_probe(Arc::new(move || probe.load(Ordering::SeqCst)));
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(flight.sim_seen);
        assert!(flight.sim_gone_since.is_none());

        up.store(false, Ordering::SeqCst);
        flight.tick(&sim, || {});
        assert!(
            flight.sim_gone_since.is_some(),
            "the grace period has begun"
        );
        assert_eq!(flight.torn_down(), None, "and has not run out");
    }

    /// MSFS replaced mid-flight is reported once, and nothing is
    /// closed or re-attached because of it.
    #[test]
    fn a_replaced_simulator_is_reported_and_nothing_moves() {
        use hangar_runtime::observe::{Fact, Found, Identity, Namespace, Process};
        use respawn_fixture::{with, KEY};
        let sim_at = |pid: u32| {
            let mut obs = with(&[41]);
            obs.apps.insert(
                sim::MSFS_2024_EXE.into(),
                Found {
                    exact: vec![Process {
                        identity: Identity {
                            pid,
                            start: u64::from(pid),
                        },
                        helper: false,
                        prefix: Fact::Known("/sim/pfx".into()),
                        user_ns: Fact::Known(Namespace::fixture(7)),
                        mount_ns: Fact::Known(Namespace::fixture(8)),
                    }],
                    similar: vec![],
                },
            );
            obs
        };
        let observer = crate::flight_details::Observer::given(sim_at(100));
        let mut flight = Flight::with_launcher(suggested([]), BTreeMap::new(), None, stub)
            .with_observer(observer.clone());
        flight.follow_for_test("Project Airports", KEY, None);
        let t0 = Instant::now();
        flight.follow_apps(t0);
        assert_eq!(flight.sim_replaced(), None);

        observer.set(sim_at(200));
        flight.follow_apps(t0 + OBSERVE_EVERY);
        let (was, now) = flight.sim_replaced().expect("noticed");
        assert_eq!((was.pid, now.pid), (100, 200));
        assert_eq!(flight.torn_down(), None, "nothing closed");
        assert!(flight
            .journal
            .view(Some("Preflight"), "")
            .iter()
            .any(|l| l.text.contains("MSFS restarted (pid 100 → 200)")));

        // Once, not every observation.
        observer.set(sim_at(300));
        flight.follow_apps(t0 + OBSERVE_EVERY * 2);
        assert_eq!(flight.sim_replaced().map(|(_, n)| n.pid), Some(200));
    }

    /// A BATC row says connected only from its own log, written
    /// after the watch began; before that it stays unverified.
    #[test]
    fn a_batc_row_reads_connected_only_from_a_fresh_log() {
        const CONNECTED: &str =
            include_str!("../../hangar-runtime/tests/fixtures/batc/player-1.10.4-connected.log");
        let dir = std::env::temp_dir().join(format!("hangar-flight-batc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A previous run's log: never evidence for this one.
        std::fs::write(dir.join(hangar_runtime::batc_ready::PLAYER_LOG), CONNECTED).unwrap();

        let mut flight = Flight::with_launcher(suggested([]), BTreeMap::new(), None, stub);
        flight.batc = Some((
            "BeyondATC".into(),
            hangar_runtime::batc_ready::Watch::begin(&dir),
        ));
        let t0 = Instant::now();
        flight.read_connections(t0);
        assert_eq!(flight.connection_of("BeyondATC"), None, "stale lines");

        // Unity rotates on start: a new file for the new run.
        let log = dir.join(hangar_runtime::batc_ready::PLAYER_LOG);
        std::fs::rename(&log, dir.join(hangar_runtime::batc_ready::PREVIOUS_LOG)).unwrap();
        std::fs::write(&log, CONNECTED).unwrap();
        flight.read_connections(t0 + OBSERVE_EVERY);
        assert_eq!(
            flight.connection_of("BeyondATC").as_deref(),
            Some("connected")
        );
        assert!(flight
            .journal
            .view(Some("BeyondATC"), "")
            .iter()
            .any(|l| l.text == "connection: connected"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stop, BATC's own confirmation
    /// dialog, Confirm, exit — all inside the respawn window. A close HangarRS
    /// asked for ends the launch; an exit nobody asked for still waits.
    #[test]
    fn an_exit_after_a_stop_is_final_and_an_unrequested_one_still_waits() {
        use respawn_fixture::{with, KEY};
        for requested in [true, false] {
            let observer = crate::flight_details::Observer::given(with(&[252944]));
            let mut flight = Flight::with_launcher(suggested([]), BTreeMap::new(), None, stub)
                .with_observer(observer.clone());
            let sim = TelemetryLink::idle();
            flight.tick(&sim, || {});
            flight.tick(&sim, || {});
            flight.saw_sim();
            flight.follow_for_test("BeyondATC", KEY, None);
            let t0 = Instant::now();
            flight.follow_apps(t0);
            if requested {
                // A row's Stop: recorded in the shared runtime before the close goes.
                flight.sessions.close_requested(KEY);
            }
            // The dialog is up: still running, nothing to report.
            flight.follow_apps(t0 + OBSERVE_EVERY);
            assert_eq!(flight.updating_for("BeyondATC"), None);
            // Confirm, 25 s after the close wait ran out: the process goes.
            observer.set(with(&[]));
            flight.follow_apps(t0 + OBSERVE_EVERY * 2);
            let waiting = !flight
                .journal
                .view(Some("BeyondATC"), "waiting for it to restart")
                .is_empty();
            assert_eq!(flight.updating_for("BeyondATC").is_some(), !requested);
            assert_eq!(waiting, !requested);
            assert_eq!(flight.is_over(false, false), requested);
        }
    }

    #[test]
    fn the_shutdown_grace_is_long_enough_to_ride_out_a_restart() {
        // A sim that crashes and comes back must not lose its companion apps.
        assert!(SHUTDOWN_GRACE >= Duration::from_secs(30));
    }
}
