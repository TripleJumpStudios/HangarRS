//! Launching and stopping companion apps from the UI.
//!
//! The UI is synchronous and these operations are not, so an action spawns a
//! task and reports back through shared state. Two things are tracked per app:
//! whether an action is in flight (so the button cannot be pressed twice) and
//! the result of the last one.

use crate::journal::Journal;
use hangar_catalog::Item;
use hangar_runtime::observe::{Expected, Identity, Observation};
use hangar_runtime::resolve::{self, Decision, Resolution};
use hangar_runtime::respawn::{Event, Tracker};
use hangar_runtime::runner::{LaunchRequest, Launched, RunMode};
use hangar_runtime::{launch_with, proc, ProtonEnv};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shared state for in-flight and completed app actions.
#[derive(Clone, Default)]
pub struct AppActions {
    /// Where an action was pressed, for the launch log. Flight rows
    /// set "Flight" around the shared controls; empty means Hangar or Map.
    pub source: &'static str,
    busy: Arc<Mutex<BTreeSet<String>>>,
    pub sessions: hangar_runtime::sim_session::Runtime,
    result: Arc<Mutex<Option<(String, bool)>>>,
    /// Companion and native apps started from a row, followed across a
    /// self-update restart the way Flight follows its own. Keyed by the
    /// `/proc` match key; the sim-session runtime follows its own apps.
    followed: Arc<Mutex<Followed>>,
}

#[derive(Default)]
struct Followed {
    apps: BTreeMap<String, (String, Tracker)>,
    observed_at: Option<Instant>,
}

impl AppActions {
    pub fn live() -> Self {
        Self {
            sessions: hangar_runtime::sim_session::Runtime::live(),
            ..Self::default()
        }
    }

    fn source(&self) -> &'static str {
        if self.source.is_empty() {
            "Hangar/Map"
        } else {
            self.source
        }
    }

    pub fn is_busy(&self, label: &str) -> bool {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(label)
    }

    pub fn take_result(&self) -> Option<(String, bool)> {
        self.result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear_result(&self) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Follow an app this row just started (tests; the row uses `follow_as`).
    #[cfg(test)]
    pub fn follow(&self, label: &str, key: &str, launched: Instant) {
        self.followed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apps
            .insert(
                key.to_owned(),
                (label.to_owned(), Tracker::new(key, launched)),
            );
    }

    /// Follow an app this row just started, knowing the pid its launch
    /// returned: breaks a tie when another copy is already running.
    pub fn follow_as(&self, label: &str, key: &str, launched: Instant, pid: u32) {
        let mut tracker = Tracker::new(key, launched);
        tracker.launched_as(pid);
        self.followed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apps
            .insert(key.to_owned(), (label.to_owned(), tracker));
    }

    /// The tracker of an app a row started, for Stop and Restart by identity.
    pub fn followed_tracker(&self, key: &str) -> Option<Tracker> {
        self.followed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apps
            .get(key)
            .map(|(_, t)| t.clone())
    }

    /// The instance that left to update and how long it has been away, for
    /// an app a row started.
    pub fn away(&self, key: &str, now: Instant) -> Option<(Identity, Duration)> {
        let followed = self.followed.lock().unwrap_or_else(|e| e.into_inner());
        let (_, tracker) = followed.apps.get(key)?;
        match tracker.phase() {
            hangar_runtime::respawn::Phase::Updating { was, since } => {
                Some((*was, now.saturating_duration_since(*since)))
            }
            _ => None,
        }
    }

    /// Away restarting after an update, whoever started it: a row here, or
    /// the sim-session runtime.
    pub fn is_updating(&self, key: &str) -> bool {
        self.away(key, Instant::now()).is_some() || self.sessions.updating(key).is_some()
    }

    /// One observation for every followed app, at most every
    /// [`crate::flight::OBSERVE_EVERY`]. Reads the process table only through
    /// `observer`, so a test or an inert app reads nothing.
    pub fn follow_apps(
        &self,
        observer: &crate::flight_details::Observer,
        journal: &Journal,
        now: Instant,
    ) {
        let keys: Vec<String> = {
            let mut followed = self.followed.lock().unwrap_or_else(|e| e.into_inner());
            if followed.apps.is_empty()
                || followed.observed_at.is_some_and(|at| {
                    now.saturating_duration_since(at) < crate::flight::OBSERVE_EVERY
                })
            {
                return;
            }
            followed.observed_at = Some(now);
            followed.apps.keys().cloned().collect()
        };
        let observation = observer.observe(&keys);
        self.take_observation(&observation, journal, now);
    }

    pub(crate) fn take_observation(
        &self,
        observation: &Observation,
        journal: &Journal,
        now: Instant,
    ) {
        let mut followed = self.followed.lock().unwrap_or_else(|e| e.into_inner());
        for (key, (label, tracker)) in &mut followed.apps {
            if let Some(at) = self.sessions.closing_since(key) {
                tracker.closing(at);
            }
            match tracker.observe(observation.found(key), now) {
                Event::Exited(was) => {
                    tracing::info!(target: "hangar_launch", source = "Hangar/Map", app = %label, pid = was.pid, "exited soon after launch; waiting for an update restart");
                    journal.note(
                        label,
                        format!("pid {} exited soon after launch; waiting for it to restart after an update", was.pid),
                    );
                }
                Event::Respawned { was, now, .. } => {
                    tracing::info!(target: "hangar_launch", source = "Hangar/Map", app = %label, was = was.pid, pid = now.pid, "update restart adopted");
                    journal.note(
                        label,
                        format!("restarted after an update as pid {}", now.pid),
                    );
                }
                Event::GaveUp(was) => journal.note(
                    label,
                    format!(
                        "pid {} did not come back within {} min",
                        was.pid,
                        hangar_runtime::respawn::RESPAWN_WAIT.as_secs() / 60
                    ),
                ),
                _ => {}
            }
        }
        followed
            .apps
            .retain(|_, (_, t)| *t.phase() != hangar_runtime::respawn::Phase::Exited);
    }

    fn start(&self, label: &str) {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(label.to_owned());
    }

    fn finish(&self, label: &str, message: String, ok: bool) {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(label);
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some((message, ok));
    }
}

/// The launch request for a catalog item.
pub fn request_for(item: &Item) -> LaunchRequest {
    LaunchRequest {
        label: item.name.clone(),
        exe: item.path.clone(),
        args: item.args.clone(),
        // Companion apps are Windows binaries in a prefix; native tools are not.
        mode: match item.kind {
            hangar_catalog::ItemKind::SimSessionApp => RunMode::SimSession,
            hangar_catalog::ItemKind::NativeTool => RunMode::Native,
            _ => RunMode::Prefix,
        },
    }
}

/// Start an app, adopting an instance that is already running.
pub fn spawn_launch(
    actions: &AppActions,
    env: ProtonEnv,
    item: &Item,
    journal: &Journal,
    wake: impl Fn() + Send + 'static,
) {
    if !actions.sessions.is_live() {
        journal.note(&item.name, "App actions are unavailable in this session.");
        return;
    }

    let request = request_for(item);
    tracing::info!(target: "hangar_launch", source = actions.source(), app = %request.label, mode = ?request.mode, prefix = %env.wineprefix.display(), "launch requested");
    let label = request.label.clone();
    actions.start(&label);
    let actions = actions.clone();
    let journal = journal.clone();
    // `runner::spawn` drops a live process's reader handles on purpose; the
    // sink is the only way its output is kept.
    let sink = journal.sink(&label);

    tokio::spawn(async move {
        let key = request.match_key();
        let resolution = resolve_launch(
            &request,
            &env,
            &hangar_runtime::observe::observe(
                std::path::Path::new(hangar_runtime::observe::PROC),
                std::slice::from_ref(&key),
            ),
            actions.away(&key, Instant::now()),
        );
        journal.note(&label, attempt_line(&resolution));
        tracing::info!(target: "hangar_launch", source = actions.source(), app = %label, attempt = resolution.attempt, decision = ?resolution.decision, "launch resolved");
        if let Some(refused) = refusal(&label, resolution.decision) {
            record(&journal, &label, &refused, false);
            actions.finish(&label, refused, false);
            wake();
            return;
        }
        journal.note(&label, format!("launching {}", request.exe.display()));
        let launched = if request.mode == RunMode::SimSession {
            actions.sessions.launch(&env, &request, Some(sink)).await
        } else {
            launch_with(&env, &request, Some(sink)).await
        };
        let (message, ok) = match launched {
            Launched::Adopted(app) => (
                adopted_message(&app.label, app.pid, resolution.decision),
                true,
            ),
            Launched::Started(app) => {
                if request.mode != RunMode::SimSession {
                    actions.follow_as(&label, &key, Instant::now(), app.pid);
                }
                (format!("{} started as pid {}", app.label, app.pid), true)
            }
            Launched::Failed { outcome, log } => {
                for line in &log {
                    journal.output(&label, line);
                }
                (describe_failure(&label, &outcome, &log), false)
            }
            Launched::SpawnError(e) => (format!("{label} could not start: {e}"), false),
        };
        tracing::info!(target: "hangar_launch", app = %label, ok, result = %message, "app action result");
        record(&journal, &label, &message, ok);
        actions.finish(&label, message, ok);
        wake();
    });
}

/// Decide a launch with `hangar_runtime::resolve`. Pure.
pub fn resolve_launch(
    request: &LaunchRequest,
    env: &ProtonEnv,
    observation: &Observation,
    away: Option<(Identity, Duration)>,
) -> Resolution {
    let route = resolve::route_of(request.mode);
    let expected = Expected::new(route, request.match_key(), Some(&env.wineprefix));
    resolve::resolve(&expected, observation, away)
}

/// The Activity line that ties one launch attempt together.
pub fn attempt_line(resolution: &Resolution) -> String {
    format!(
        "attempt {} ({:?} route): {}",
        resolution.attempt,
        resolution.route,
        resolution.decision.describe()
    )
}

/// Why this launch does not go ahead, when it does not.
pub fn refusal(label: &str, decision: Decision) -> Option<String> {
    match decision {
        Decision::Refuse(why) => Some(why.explain(label)),
        Decision::WaitForUpdate { was, .. } => Some(format!(
            "{label} is restarting after an update (pid {} left); wait for it to come back",
            was.pid
        )),
        _ => None,
    }
}

/// An adopted instance, said as strongly as the evidence allows.
pub fn adopted_message(label: &str, pid: u32, decision: Decision) -> String {
    match decision {
        Decision::AdoptUnverified(_) => {
            format!("{label} was already running as pid {pid}; environment unverified")
        }
        _ => format!("{label} was already running as pid {pid}"),
    }
}

/// Stop an app and start it again.
///
/// The way back to a window that was closed but left the app running. Nothing
/// else works: the app's own focus IPC does not cross Wine, there is no tray
/// icon, and the window was destroyed rather than hidden.
pub fn spawn_restart(
    actions: &AppActions,
    env: ProtonEnv,
    item: &Item,
    journal: &Journal,
    wake: impl Fn() + Send + 'static,
) {
    if !actions.sessions.is_live() {
        journal.note(&item.name, "App actions are unavailable in this session.");
        return;
    }

    let request = request_for(item);
    tracing::info!(target: "hangar_launch", source = actions.source(), app = %request.label, mode = ?request.mode, prefix = %env.wineprefix.display(), "restart requested");
    let label = request.label.clone();
    let tracker = actions.followed_tracker(&request.match_key());
    actions.start(&label);
    let actions = actions.clone();
    let journal = journal.clone();
    let sink = journal.sink(&label);

    tokio::spawn(async move {
        journal.note(&label, "restarting");
        let launched = if request.mode == RunMode::SimSession {
            actions.sessions.restart(&env, &request, Some(sink)).await
        } else {
            hangar_runtime::runner::restart_followed(&env, &request, tracker.as_ref(), Some(sink))
                .await
        };
        let (message, ok) = match launched {
            Launched::Started(app) => (format!("{} restarted as pid {}", app.label, app.pid), true),
            // A restart that adopts means the old process never exited.
            Launched::Adopted(app) => (
                format!("{} did not exit; still pid {}", app.label, app.pid),
                false,
            ),
            Launched::Failed { outcome, log } => {
                for line in &log {
                    journal.output(&label, line);
                }
                (describe_failure(&label, &outcome, &log), false)
            }
            Launched::SpawnError(e) => (format!("{label} could not restart: {e}"), false),
        };
        tracing::info!(target: "hangar_launch", app = %label, ok, result = %message, "app action result");
        record(&journal, &label, &message, ok);
        actions.finish(&label, message, ok);
        wake();
    });
}

/// Stop an app, whoever started it.
pub fn spawn_stop(
    actions: &AppActions,
    env: Option<ProtonEnv>,
    item: &Item,
    journal: &Journal,
    wake: impl Fn() + Send + 'static,
) {
    if !actions.sessions.is_live() {
        journal.note(&item.name, "App actions are unavailable in this session.");
        return;
    }

    let label = item.name.clone();
    let request = request_for(item);
    tracing::info!(target: "hangar_launch", source = actions.source(), app = %request.label, mode = ?request.mode, "stop requested");
    let key = proc::match_key(&item.path);
    let tracker = actions.followed_tracker(&key);
    // Before the close is sent, so its exit is never read as an update.
    actions.sessions.close_requested(&key);
    actions.start(&label);
    let actions = actions.clone();
    let journal = journal.clone();

    tokio::spawn(async move {
        journal.note(&label, format!("stopping {key}"));
        let (message, ok) = if request.mode == RunMode::SimSession {
            let sink = journal.sink(&label);
            match actions
                .sessions
                .stop(env.as_ref(), &request, Some(sink))
                .await
            {
                Ok(()) => (format!("{label}: closed"), true),
                Err(e) => (format!("{label}: {e}"), false),
            }
        } else {
            // A Windows app is asked to close first, like its close button, so
            // it can shut down cleanly; a Linux one exits cleanly on SIGTERM.
            let outcome = match (&request.mode, env.as_ref()) {
                (RunMode::Prefix, Some(env)) => {
                    hangar_runtime::kill::stop_app(Some(env), &key, tracker.as_ref()).await
                }
                _ => hangar_runtime::kill::stop_app(None, &key, tracker.as_ref()).await,
            };
            (
                format!("{label}: {}", outcome.summary()),
                outcome.is_stopped(),
            )
        };
        tracing::info!(target: "hangar_launch", app = %label, ok, result = %message, "app action result");
        record(&journal, &label, &message, ok);
        actions.finish(&label, message, ok);
        wake();
    });
}

/// The outcome of an action, in the scrollback beside the app's own output.
///
/// A failed launch's collected log is written by the caller first, so an app
/// that failed and printed why is not recorded as having said nothing.
fn record(journal: &Journal, label: &str, message: &str, ok: bool) {
    if ok {
        journal.done(label, message.to_owned());
    } else {
        journal.fail(label, message.to_owned());
    }
}

/// Turn a failed launch into something worth reading.
fn describe_failure(
    label: &str,
    outcome: &hangar_runtime::LaunchOutcome,
    log: &[String],
) -> String {
    use hangar_runtime::LaunchOutcome;
    match outcome {
        LaunchOutcome::ImmediateExit {
            after,
            likely_cause,
        } => format!(
            "{label} exited after {:.1}s without starting — {likely_cause}",
            after.as_secs_f32()
        ),
        LaunchOutcome::ExitedWithOutput { after } => {
            let last = log.iter().rev().find(|l| !l.trim().is_empty());
            match last {
                Some(line) => format!(
                    "{label} exited after {:.1}s: {}",
                    after.as_secs_f32(),
                    line.trim()
                ),
                None => format!("{label} exited after {:.1}s", after.as_secs_f32()),
            }
        }
        LaunchOutcome::Started => format!("{label} started"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_catalog::{ItemKind, ItemState};
    use std::path::PathBuf;

    fn item(kind: ItemKind, args: &[&str]) -> Item {
        Item {
            name: "Project Airports".into(),
            kind,
            state: ItemState::Ready,
            title: "Project Airports".into(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: PathBuf::from("/prefix/Programs/@pa/Project Airports.exe"),
            community: None,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    const KEY: &str = "Project Airports.exe";

    fn seen(pids: &[(u32, &str)]) -> Observation {
        use hangar_runtime::observe::{Fact, Found, Namespace, Process};
        let mut obs = Observation::empty(&[KEY.into()]);
        obs.host_user = Fact::Known(Namespace::fixture(1));
        obs.host_mount = Fact::Known(Namespace::fixture(2));
        obs.apps.insert(
            KEY.into(),
            Found {
                exact: pids
                    .iter()
                    .map(|&(pid, prefix)| Process {
                        identity: Identity {
                            pid,
                            start: u64::from(pid),
                        },
                        helper: false,
                        prefix: Fact::Known(prefix.into()),
                        user_ns: Fact::Known(Namespace::fixture(1)),
                        mount_ns: Fact::Known(Namespace::fixture(2)),
                    })
                    .collect(),
                similar: vec![],
            },
        );
        obs
    }

    fn env(prefix: &str) -> ProtonEnv {
        ProtonEnv {
            wineprefix: prefix.into(),
            proton_script: "/scratch/proton".into(),
            wine_binary: "/scratch/wine".into(),
            compat_data: "/scratch".into(),
            compat_install: "/scratch".into(),
            steam_client: "/scratch".into(),
            is_flatpak: false,
        }
    }

    /// A row's launch is resolved first. Nothing running launches; one
    /// instance where it belongs is adopted; one in another prefix is refused
    /// with a reason a person can act on, and nothing is started.
    #[test]
    fn a_rows_launch_is_resolved_before_anything_runs() {
        let request = request_for(&item(ItemKind::CompanionApp, &[]));
        let env = env("/scratch/companion/pfx");

        let r = resolve_launch(&request, &env, &seen(&[]), None);
        assert_eq!(r.decision, Decision::Launch);
        assert!(refusal("Project Airports", r.decision).is_none());
        assert!(attempt_line(&r).starts_with(&format!("attempt {} (Companion route)", r.attempt)));

        let r = resolve_launch(
            &request,
            &env,
            &seen(&[(41, "/scratch/companion/pfx")]),
            None,
        );
        assert!(matches!(r.decision, Decision::Adopt(_)));
        assert_eq!(
            adopted_message("Project Airports", 41, r.decision),
            "Project Airports was already running as pid 41"
        );

        let r = resolve_launch(&request, &env, &seen(&[(41, "/scratch/sim/pfx")]), None);
        let why = refusal("Project Airports", r.decision).expect("refused");
        assert!(why.contains("another Wine prefix"), "{why}");

        let r = resolve_launch(
            &request,
            &env,
            &seen(&[
                (41, "/scratch/companion/pfx"),
                (42, "/scratch/companion/pfx"),
            ]),
            None,
        );
        assert!(
            refusal("Project Airports", r.decision).is_none(),
            "no refusal"
        );
        assert!(adopted_message("Project Airports", 41, r.decision).contains("unverified"));
    }

    /// A companion app started from a row is followed across its
    /// self-update restart: away, the row says Updating and a second launch is
    /// refused; back, it is the same launch again.
    #[test]
    fn a_companion_app_started_from_a_row_is_followed_through_an_update() {
        let actions = AppActions::default();
        let journal = Journal::default();
        let t0 = Instant::now();
        actions.follow("Project Airports", KEY, t0);
        actions.take_observation(&seen(&[(41, "/scratch/companion/pfx")]), &journal, t0);
        assert!(!actions.is_updating(KEY));

        let t1 = t0 + Duration::from_secs(20);
        actions.take_observation(&seen(&[]), &journal, t1);
        assert!(actions.is_updating(KEY), "left inside the window");
        let away = actions.away(KEY, t1 + Duration::from_secs(5));
        assert_eq!(away.map(|(was, _)| was.pid), Some(41));

        let request = request_for(&item(ItemKind::CompanionApp, &[]));
        let r = resolve_launch(&request, &env("/scratch/companion/pfx"), &seen(&[]), away);
        let why = refusal("Project Airports", r.decision).expect("waits");
        assert!(why.contains("restarting after an update"), "{why}");

        let t2 = t1 + Duration::from_secs(30);
        actions.take_observation(&seen(&[(57, "/scratch/companion/pfx")]), &journal, t2);
        assert!(!actions.is_updating(KEY), "back as pid 57");
        assert!(journal
            .view(Some("Project Airports"), "")
            .iter()
            .any(|line| line.text.contains("restarted after an update as pid 57")));
    }

    /// A Stop from a row makes the exit that follows final, not an update.
    #[test]
    fn a_stopped_companion_app_is_not_waited_for() {
        let actions = AppActions::default();
        let journal = Journal::default();
        let t0 = Instant::now();
        actions.follow("Project Airports", KEY, t0);
        actions.take_observation(&seen(&[(41, "/scratch/companion/pfx")]), &journal, t0);
        actions.sessions.close_requested(KEY);
        actions.take_observation(&seen(&[]), &journal, t0 + Duration::from_secs(5));
        assert!(!actions.is_updating(KEY));
        assert!(actions.away(KEY, t0 + Duration::from_secs(6)).is_none());
    }

    #[test]
    fn a_companion_app_runs_in_the_prefix_with_its_arguments() {
        let request = request_for(&item(ItemKind::CompanionApp, &["--no-sandbox"]));
        assert_eq!(request.mode, RunMode::Prefix);
        assert_eq!(request.args, ["--no-sandbox"]);
        assert_eq!(request.match_key(), "Project Airports.exe");
    }

    #[test]
    fn a_native_tool_runs_directly() {
        assert_eq!(
            request_for(&item(ItemKind::NativeTool, &[])).mode,
            RunMode::Native
        );
    }

    #[test]
    fn a_sim_session_item_selects_the_session_runner() {
        assert_eq!(
            request_for(&item(ItemKind::SimSessionApp, &[])).mode,
            RunMode::SimSession
        );
    }

    #[test]
    fn the_launch_log_names_where_an_action_was_pressed() {
        let mut actions = AppActions::default();
        assert_eq!(actions.source(), "Hangar/Map");
        actions.source = "Flight";
        assert_eq!(actions.source(), "Flight");
        assert_eq!(
            actions.clone().source(),
            "Flight",
            "the spawned task logs the same source"
        );
    }

    #[test]
    fn busy_tracks_one_app_without_blocking_others() {
        let actions = AppActions::default();
        assert!(!actions.is_busy("A"));
        actions.start("A");
        assert!(actions.is_busy("A"));
        assert!(!actions.is_busy("B"));
        actions.finish("A", "done".into(), true);
        assert!(!actions.is_busy("A"));
        assert_eq!(actions.take_result(), Some(("done".into(), true)));
        actions.clear_result();
        assert!(actions.take_result().is_none());
    }

    /// The message for the exit-0 failure has to name the likely cause, since
    /// the exit status says nothing.
    #[test]
    fn an_immediate_exit_names_the_single_instance_lock() {
        let outcome = hangar_runtime::LaunchOutcome::ImmediateExit {
            after: std::time::Duration::from_millis(300),
            likely_cause: "another instance is probably already running and holding a \
                           single-instance lock — try killing it first"
                .into(),
        };
        let message = describe_failure("Project Airports", &outcome, &[]);
        assert!(message.contains("single-instance lock"), "{message}");
        assert!(message.contains("0.3s"), "{message}");
    }

    #[test]
    fn an_exit_with_output_quotes_the_last_line() {
        let outcome = hangar_runtime::LaunchOutcome::ExitedWithOutput {
            after: std::time::Duration::from_secs(4),
        };
        let log = vec!["starting".into(), "fatal: no display".into(), "  ".into()];
        let message = describe_failure("App", &outcome, &log);
        assert!(message.contains("fatal: no display"), "{message}");
    }
}
