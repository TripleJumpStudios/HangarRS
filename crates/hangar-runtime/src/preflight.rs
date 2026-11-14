//! One click from cold to flying.
//!
//! Every flight starts the same way: launch the sim, then bring up the
//! companion apps, in an order that has to be right, with waits between them
//! that have to be long enough. A **preflight** is that sequence, saved and
//! named.
//!
//! # Preconditions, not a sorted list
//!
//! The usual version of this is a shell script with `sleep 90` in it. It works
//! until the machine is busy, or a shader cache rebuild adds four minutes — and
//! when it fails it fails silently, starting a companion app against a sim that
//! is not there.
//!
//! So a step does not wait for a *duration*, it waits for a [`Condition`]. The
//! order falls out of the conditions rather than being typed in: "third in the
//! list" rots the moment a fourth app is added, while "needs the sim accepting
//! SimConnect" stays true forever and is something the application can actually
//! observe.
//!
//! # HangarRS is never in the way
//!
//! A preflight presses the buttons a person could press. The sim is started
//! with the same `steam://rungameid/` URL you would click, so the launch path
//! does not run *through* this application — it watches and reacts. Close
//! HangarRS, or uninstall it, and Steam starts the sim exactly as before.
//!
//! **No step may be something only HangarRS can undo.** The package-profile
//! step is the one to watch, because moving directories is the only part of a
//! sequence that leaves state behind.
//!
//! # This module decides; it does not act
//!
//! [`Run`] is a state machine. It says what should happen next and whether it
//! is still waiting; the caller performs the action and reports back. That
//! keeps launching, filesystem work and SimConnect out of here, which is what
//! makes the ordering testable without a sim, a stick or a Steam client.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// What has to be true before a step runs.
///
/// Deliberately a small closed set. Anything an application cannot *observe*
/// is not a condition, it is a guess with a nicer name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Condition {
    /// Run immediately.
    #[default]
    Nothing,
    /// A process matching the sim's `argv[0]` exists.
    ///
    /// Separate from [`Condition::SimConnectReady`] because `steam://` hands
    /// off: there is no child process, no PID and no exit code, so *started*
    /// and *ready* are two different observations minutes apart.
    SimStarted,
    /// SimConnect accepts a connection. This is the one that matters for
    /// anything that talks to the sim.
    SimConnectReady,
    /// Telemetry is arriving, not merely connected.
    TelemetryFlowing,
    /// Every app started before this step has finished its startup work.
    ///
    /// Only usable when each of those apps has a readiness probe — see
    /// [`crate::readiness`]. Project Airports says `sync.done` in its own log,
    /// so the gate opens itself rather than waiting on a person.
    ///
    /// Never times out. A sync can be a gigabyte and a half, and the manual
    /// override is always there, so giving up would only ever be wrong.
    AppsReady,
    /// A person says go.
    ///
    /// The honest condition for something nothing can observe. A fixed delay
    /// would be the `sleep 90` problem wearing a different hat, so the sequence
    /// stops and waits to be told.
    Manual,
}

impl Condition {
    /// Whether this waits for a person rather than for the machine.
    pub fn is_manual(self) -> bool {
        self == Condition::Manual
    }

    /// Whether a person can push past this one.
    ///
    /// True for both gates that hold the sim back: a stalled sync must not be
    /// a dead end, and 90 `sync.start` lines against 74 `sync.done` in the real
    /// logs says stalls happen.
    pub fn is_overridable(self) -> bool {
        matches!(self, Condition::Manual | Condition::AppsReady)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Condition::Nothing => "immediately",
            Condition::SimStarted => "the sim process to appear",
            Condition::SimConnectReady => "SimConnect to answer",
            Condition::TelemetryFlowing => "telemetry to flow",
            Condition::AppsReady => "the apps before it to finish",
            Condition::Manual => "you to say go",
        }
    }

    /// A sensible ceiling for waiting on this.
    ///
    /// The sim takes minutes from a cold start, and longer after an update
    /// rebuilds shaders. These are generous on purpose: a preflight that gives
    /// up early is worse than one that is still visibly waiting, because the
    /// second can be watched and cancelled.
    pub fn default_timeout(self) -> Duration {
        match self {
            Condition::Nothing => Duration::from_secs(0),
            Condition::SimStarted => Duration::from_secs(120),
            Condition::SimConnectReady => Duration::from_secs(600),
            Condition::TelemetryFlowing => Duration::from_secs(120),
            // Someone may walk away mid-sync and come back to it. Giving up on
            // a person is the one timeout that has no defensible length, and a
            // sync that stalls has a Continue button beside it.
            Condition::AppsReady | Condition::Manual => Duration::MAX,
        }
    }
}

/// One thing a preflight does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
    /// Apply a package profile by name.
    ///
    /// The only step that leaves state behind if it dies halfway, and the
    /// reason the non-gating rule is written down.
    ApplyProfile { profile: String },
    /// Open `steam://rungameid/<app_id>`.
    LaunchSim { app_id: String },
    /// Launch or adopt a companion app.
    LaunchApp { label: String },
}

impl Action {
    pub fn label(&self) -> &str {
        match self {
            Action::ApplyProfile { profile } => profile,
            Action::LaunchSim { app_id } => app_id,
            Action::LaunchApp { label } => label,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Action::ApplyProfile { profile } => format!("Apply profile {profile:?}"),
            Action::LaunchSim { .. } => "Launch MSFS from Steam".to_string(),
            Action::LaunchApp { label } => format!("Start {label}"),
        }
    }
}

/// A step: something to do, and what has to be true first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    #[serde(flatten)]
    pub action: Action,
    #[serde(default)]
    pub wait_for: Condition,
    /// Stored on disk as whole seconds. `Duration` has no obvious TOML shape
    /// and a hand-editable file should say `timeout_secs = 600`.
    #[serde(rename = "timeout_secs", with = "secs")]
    pub timeout: Duration,
    /// Whether the sequence carries on if this step fails.
    ///
    /// True for a companion app: BeyondATC failing to start is a reason to say
    /// so, not a reason to abandon the flight. False for the sim.
    #[serde(default)]
    pub optional: bool,
}

impl Step {
    pub fn now(action: Action) -> Self {
        Step {
            action,
            wait_for: Condition::Nothing,
            timeout: Duration::from_secs(0),
            optional: false,
        }
    }

    pub fn after(action: Action, condition: Condition) -> Self {
        Step {
            action,
            wait_for: condition,
            timeout: condition.default_timeout(),
            optional: false,
        }
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// A named sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preflight {
    pub name: String,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

/// `Duration` as whole seconds, so the file reads `timeout_secs = 600`.
mod secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

impl Preflight {
    pub fn new(name: impl Into<String>) -> Self {
        Preflight {
            name: name.into(),
            steps: Vec::new(),
        }
    }

    pub fn with(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }
}

/// Where one step got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not reached yet.
    Pending,
    /// Its condition is not met.
    Waiting,
    /// Handed to the caller, which has not reported back.
    Acting,
    Done(String),
    /// Failed, with the reason. The sequence stopped unless it was optional.
    Failed(String),
    /// Skipped because the sequence stopped before reaching it.
    Skipped,
}

impl State {
    pub fn is_finished(&self) -> bool {
        matches!(self, State::Done(_) | State::Failed(_) | State::Skipped)
    }
}

/// What the caller should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Perform this step's action, then call [`Run::acted`].
    Do { step: usize },
    /// Still waiting. Nothing to do but let time pass.
    Waiting {
        step: usize,
        condition: Condition,
        waited: Duration,
        timeout: Duration,
    },
    /// Every step is finished, one way or another.
    Finished,
}

/// A preflight being run.
///
/// Driven by [`Run::tick`], which is given the conditions that currently hold.
/// Nothing here launches, waits or sleeps — time enters only through `now`, so
/// a test can run a ten-minute timeout instantly.
#[derive(Debug, Clone)]
pub struct Run {
    preflight: Preflight,
    states: Vec<State>,
    current: usize,
    /// When the current step started waiting.
    waiting_since: Option<Instant>,
    cancelled: bool,
}

impl Run {
    pub fn start(preflight: Preflight) -> Self {
        let states = vec![State::Pending; preflight.steps.len()];
        Run {
            preflight,
            states,
            current: 0,
            waiting_since: None,
            cancelled: false,
        }
    }

    pub fn preflight(&self) -> &Preflight {
        &self.preflight
    }

    pub fn states(&self) -> &[State] {
        &self.states
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn is_finished(&self) -> bool {
        self.current >= self.preflight.steps.len()
    }

    /// Stop, leaving whatever already started running.
    ///
    /// **Nothing is rolled back.** A half-run preflight is a normal state, not
    /// an error: the sim may be up and one app running, which is visible and
    /// finishable by hand. Tearing that down because a later step was cancelled
    /// would be a surprise, and surprises here cost someone their flight.
    pub fn cancel(&mut self) {
        if self.is_finished() {
            return;
        }
        self.cancelled = true;
        for state in &mut self.states[self.current..] {
            if !state.is_finished() {
                *state = State::Skipped;
            }
        }
        self.current = self.preflight.steps.len();
    }

    /// Decide what happens next.
    ///
    /// `met` answers whether a condition currently holds; `now` is the clock.
    pub fn tick(&mut self, now: Instant, met: impl Fn(Condition) -> bool) -> Next {
        if self.is_finished() {
            return Next::Finished;
        }

        let step = &self.preflight.steps[self.current];

        if step.wait_for == Condition::Nothing || met(step.wait_for) {
            self.waiting_since = None;
            self.states[self.current] = State::Acting;
            return Next::Do { step: self.current };
        }

        let since = *self.waiting_since.get_or_insert(now);
        let waited = now.saturating_duration_since(since);
        self.states[self.current] = State::Waiting;

        // `Duration::MAX` means no timeout at all, which is what a manual gate
        // needs: comparing against it directly avoids ever adding it to an
        // `Instant`, which would panic.
        if step.timeout != Duration::MAX && waited >= step.timeout {
            let reason = format!(
                "gave up after {}s waiting for {}",
                step.timeout.as_secs(),
                step.wait_for.as_str()
            );
            return self.fail_current(reason);
        }

        Next::Waiting {
            step: self.current,
            condition: step.wait_for,
            waited,
            timeout: step.timeout,
        }
    }

    /// Report that the current step's action succeeded.
    pub fn acted(&mut self, outcome: impl Into<String>) {
        if self.is_finished() {
            return;
        }
        self.states[self.current] = State::Done(outcome.into());
        self.current += 1;
        self.waiting_since = None;
    }

    /// Report that the current step's action failed.
    ///
    /// An optional step's failure is recorded and the sequence carries on; a
    /// required one stops it, and everything after is marked skipped rather
    /// than left looking pending.
    pub fn failed(&mut self, reason: impl Into<String>) -> Next {
        self.fail_current(reason.into())
    }

    fn fail_current(&mut self, reason: String) -> Next {
        let optional = self.preflight.steps[self.current].optional;
        self.states[self.current] = State::Failed(reason.clone());
        let failed_at = self.current;
        self.current += 1;
        self.waiting_since = None;

        if optional {
            return if self.is_finished() {
                Next::Finished
            } else {
                Next::Waiting {
                    step: self.current,
                    condition: self.preflight.steps[self.current].wait_for,
                    waited: Duration::ZERO,
                    timeout: self.preflight.steps[self.current].timeout,
                }
            };
        }

        for state in &mut self.states[self.current..] {
            *state = State::Skipped;
        }
        self.current = self.preflight.steps.len();
        tracing::warn!(step = failed_at, %reason, "preflight stopped");
        Next::Finished
    }

    /// A one-line summary for a status bar.
    pub fn summary(&self) -> String {
        let done = self
            .states
            .iter()
            .filter(|s| matches!(s, State::Done(_)))
            .count();
        let failed = self
            .states
            .iter()
            .filter(|s| matches!(s, State::Failed(_)))
            .count();
        let skipped = self.states.iter().filter(|s| **s == State::Skipped).count();

        let mut parts = vec![format!("{done}/{} done", self.preflight.steps.len())];
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped"));
        }
        if self.cancelled {
            parts.push("cancelled".to_string());
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Preflight {
        Preflight::new("Before a flight")
            .with(Step::now(Action::ApplyProfile {
                profile: "VFR".into(),
            }))
            .with(Step::now(Action::LaunchSim {
                app_id: "2537590".into(),
            }))
            .with(
                Step::after(
                    Action::LaunchApp {
                        label: "BeyondATC".into(),
                    },
                    Condition::SimConnectReady,
                )
                .optional(),
            )
    }

    /// Nothing is met, so waiting steps stay waiting.
    fn nothing(_: Condition) -> bool {
        false
    }

    fn everything(_: Condition) -> bool {
        true
    }

    #[test]
    fn steps_with_no_condition_run_straight_away() {
        let mut run = Run::start(sample());
        let now = Instant::now();

        assert_eq!(run.tick(now, nothing), Next::Do { step: 0 });
        run.acted("moved 3 packages");
        assert_eq!(run.tick(now, nothing), Next::Do { step: 1 });
        run.acted("opened steam://rungameid/2537590");
    }

    #[test]
    fn a_step_waits_for_its_condition_rather_than_a_duration() {
        let mut run = Run::start(sample());
        let start = Instant::now();
        run.tick(start, nothing);
        run.acted("ok");
        run.tick(start, nothing);
        run.acted("ok");

        // The sim is not up yet, so BeyondATC does not start. The first tick
        // is what starts this step's clock, so it reports zero waited.
        match run.tick(start, nothing) {
            Next::Waiting {
                step,
                condition,
                waited,
                ..
            } => {
                assert_eq!(step, 2);
                assert_eq!(condition, Condition::SimConnectReady);
                assert_eq!(waited, Duration::ZERO);
            }
            other => panic!("expected to be waiting, got {other:?}"),
        }
        match run.tick(start + Duration::from_secs(90), nothing) {
            Next::Waiting { waited, .. } => assert_eq!(waited, Duration::from_secs(90)),
            other => panic!("expected to be waiting, got {other:?}"),
        }

        // Ninety seconds later than a `sleep 90` would have fired, and it still
        // waits — then goes the moment the sim answers.
        assert_eq!(
            run.tick(start + Duration::from_secs(400), everything),
            Next::Do { step: 2 }
        );
    }

    #[test]
    fn waiting_too_long_stops_rather_than_launching_into_nothing() {
        let mut run = Run::start(sample());
        let start = Instant::now();
        run.tick(start, nothing);
        run.acted("ok");
        run.tick(start, nothing);
        run.acted("ok");

        run.tick(start, nothing);
        let next = run.tick(start + Duration::from_secs(601), nothing);
        assert_eq!(next, Next::Finished);
        assert!(matches!(run.states()[2], State::Failed(_)));
        assert!(run.is_finished());
    }

    #[test]
    fn a_required_step_failing_skips_the_rest_rather_than_leaving_them_pending() {
        let mut run = Run::start(sample());
        let now = Instant::now();
        run.tick(now, nothing);
        run.failed("the Community folder is read-only");

        assert!(matches!(run.states()[0], State::Failed(_)));
        assert_eq!(run.states()[1], State::Skipped);
        assert_eq!(run.states()[2], State::Skipped);
        assert_eq!(run.tick(now, everything), Next::Finished);
        assert_eq!(run.summary(), "0/3 done, 1 failed, 2 skipped");
    }

    #[test]
    fn an_optional_step_failing_does_not_abandon_the_flight() {
        // BeyondATC not starting is worth saying, not worth stopping for.
        let preflight = Preflight::new("two apps")
            .with(
                Step::now(Action::LaunchApp {
                    label: "BeyondATC".into(),
                })
                .optional(),
            )
            .with(Step::now(Action::LaunchApp {
                label: "PilotEdge".into(),
            }));
        let mut run = Run::start(preflight);
        let now = Instant::now();

        run.tick(now, everything);
        run.failed("exited immediately");
        assert!(matches!(run.states()[0], State::Failed(_)));

        assert_eq!(run.tick(now, everything), Next::Do { step: 1 });
        run.acted("started");
        assert_eq!(run.summary(), "1/2 done, 1 failed");
    }

    #[test]
    fn cancelling_leaves_what_already_started_alone() {
        // The sim is up and a profile was applied. Cancelling must not undo
        // either — a half-run preflight is a normal state.
        let mut run = Run::start(sample());
        let now = Instant::now();
        run.tick(now, nothing);
        run.acted("moved 3 packages");
        run.tick(now, nothing);
        run.acted("sim launching");

        run.cancel();

        assert!(run.is_cancelled());
        assert_eq!(run.states()[0], State::Done("moved 3 packages".into()));
        assert_eq!(run.states()[1], State::Done("sim launching".into()));
        assert_eq!(run.states()[2], State::Skipped);
        assert_eq!(run.tick(now, everything), Next::Finished);
        assert!(run.summary().contains("cancelled"));
    }

    #[test]
    fn cancelling_a_finished_run_changes_nothing() {
        let mut run = Run::start(Preflight::new("empty"));
        assert_eq!(run.tick(Instant::now(), everything), Next::Finished);
        run.cancel();
        assert!(!run.is_cancelled(), "there was nothing to cancel");
    }

    #[test]
    fn the_wait_clock_starts_when_the_step_does_not_when_the_run_does() {
        // Otherwise a slow first step eats the second step's timeout.
        let preflight = Preflight::new("late")
            .with(Step::now(Action::LaunchSim {
                app_id: "2537590".into(),
            }))
            .with(Step::after(
                Action::LaunchApp {
                    label: "BeyondATC".into(),
                },
                Condition::SimStarted,
            ));
        let mut run = Run::start(preflight);
        let start = Instant::now();

        run.tick(start, nothing);
        // Five minutes pass before the first step is reported done.
        let later = start + Duration::from_secs(300);
        run.acted("ok");

        match run.tick(later, nothing) {
            Next::Waiting { waited, .. } => assert_eq!(waited, Duration::ZERO),
            other => panic!("expected waiting, got {other:?}"),
        }
        // And the 120s timeout is measured from there, not from `start`.
        match run.tick(later + Duration::from_secs(119), nothing) {
            Next::Waiting { .. } => {}
            other => panic!("should still be waiting, got {other:?}"),
        }
        assert_eq!(
            run.tick(later + Duration::from_secs(121), nothing),
            Next::Finished
        );
    }

    #[test]
    fn a_manual_gate_waits_forever_and_goes_when_told() {
        // A manual gate stops and asks. Giving up on a person has no defensible
        // timeout.
        let preflight = Preflight::new("sync first")
            .with(Step::now(Action::LaunchApp {
                label: "Project Airports".into(),
            }))
            .with(Step::after(
                Action::LaunchSim {
                    app_id: "2537590".into(),
                },
                Condition::Manual,
            ));
        let mut run = Run::start(preflight);
        let start = Instant::now();

        run.tick(start, nothing);
        run.acted("started");

        // A day later it is still waiting, not failed.
        match run.tick(start + Duration::from_secs(86_400), nothing) {
            Next::Waiting { condition, .. } => assert!(condition.is_manual()),
            other => panic!("a manual gate must not time out, got {other:?}"),
        }

        // And goes the moment it is told.
        assert_eq!(
            run.tick(start + Duration::from_secs(86_400), |c| c
                == Condition::Manual),
            Next::Do { step: 1 }
        );
    }

    #[test]
    fn conditions_carry_timeouts_long_enough_for_a_cold_start() {
        // A sim that takes six minutes after a shader rebuild must not trip a
        // timeout that was picked for a warm start.
        assert!(Condition::SimConnectReady.default_timeout() >= Duration::from_secs(300));
        assert_eq!(Condition::Nothing.default_timeout(), Duration::ZERO);
        assert_eq!(Condition::Manual.default_timeout(), Duration::MAX);
    }
}

/// Saved preflights, one TOML file each.
///
/// Mirrors `hangar_catalog::ProfileStore` deliberately: same directory
/// convention, same "one malformed file must not stop the others loading"
/// rule, same hand-editable format. A person who has looked at one has looked
/// at both.
#[derive(Debug, Clone, Default)]
pub struct Store {
    dir: std::path::PathBuf,
    preflights: std::collections::BTreeMap<String, Preflight>,
}

impl Store {
    /// Load every `.toml` in `dir`. A missing directory is an empty store.
    pub fn load(dir: impl Into<std::path::PathBuf>) -> Self {
        let dir = dir.into();
        let mut preflights = std::collections::BTreeMap::new();

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|text| toml::from_str::<Preflight>(&text).map_err(|e| e.to_string()))
                {
                    Ok(p) => {
                        preflights.insert(p.name.clone(), p);
                    }
                    Err(e) => tracing::warn!("ignoring preflight {}: {e}", path.display()),
                }
            }
        }

        Store { dir, preflights }
    }

    /// Load from `$XDG_CONFIG_HOME/hangar-rs/preflights`.
    pub fn load_default() -> Self {
        match hangar_core::paths::preflights_dir() {
            Some(dir) => Self::load(dir),
            None => Store::default(),
        }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn names(&self) -> Vec<&str> {
        self.preflights.keys().map(String::as_str).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Preflight> {
        self.preflights.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.preflights.is_empty()
    }

    pub fn len(&self) -> usize {
        self.preflights.len()
    }

    /// Write one out, replacing any file of the same name.
    pub fn save(&mut self, preflight: Preflight) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(&preflight.name);
        let text = toml::to_string_pretty(&preflight)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(&path, text)?;
        self.preflights.insert(preflight.name.clone(), preflight);
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> std::io::Result<()> {
        let path = self.path_for(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.preflights.remove(name);
        Ok(())
    }

    /// One file per preflight, named from the preflight.
    ///
    /// Anything that is not a letter, digit, dash or underscore becomes a dash,
    /// so a name with a slash in it cannot escape the directory.
    fn path_for(&self, name: &str) -> std::path::PathBuf {
        let stem: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let stem = stem.trim_matches('-');
        let stem = if stem.is_empty() { "preflight" } else { stem };
        self.dir.join(format!("{stem}.toml"))
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;

    fn sample() -> Preflight {
        Preflight::new("Before a flight")
            .with(Step::now(Action::LaunchSim {
                app_id: "2537590".into(),
            }))
            .with(
                Step::after(
                    Action::LaunchApp {
                        label: "BeyondATC".into(),
                    },
                    Condition::SimConnectReady,
                )
                .optional(),
            )
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hangar-preflight-store-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_preflight_survives_a_round_trip_through_toml() {
        let original = sample();
        let text = toml::to_string_pretty(&original).unwrap();
        let back: Preflight = toml::from_str(&text).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn the_file_is_something_a_person_could_edit() {
        let text = toml::to_string_pretty(&sample()).unwrap();
        assert!(text.contains("name = \"Before a flight\""), "{text}");
        assert!(text.contains("action = \"launch-sim\""), "{text}");
        assert!(text.contains("wait_for = \"sim-connect-ready\""), "{text}");
        // Seconds, not a Duration struct.
        assert!(text.contains("timeout_secs = 600"), "{text}");
    }

    #[test]
    fn saving_and_loading_gives_back_what_went_in() {
        let dir = scratch("roundtrip");
        let mut store = Store::load(&dir);
        assert!(store.is_empty());

        store.save(sample()).unwrap();
        let reloaded = Store::load(&dir);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.get("Before a flight"), Some(&sample()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_malformed_file_does_not_hide_the_others() {
        let dir = scratch("malformed");
        let mut store = Store::load(&dir);
        store.save(sample()).unwrap();
        std::fs::write(dir.join("broken.toml"), "this is not toml = [[[").unwrap();

        let reloaded = Store::load(&dir);
        assert_eq!(reloaded.len(), 1, "the good one still loaded");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_cannot_escape_the_directory() {
        let store = Store::load("/tmp/preflights");
        let path = store.path_for("../../etc/passwd");
        assert_eq!(path.parent(), Some(std::path::Path::new("/tmp/preflights")));
        assert!(!path.to_string_lossy().contains(".."));
    }

    #[test]
    fn deleting_removes_the_file_and_the_entry() {
        let dir = scratch("delete");
        let mut store = Store::load(&dir);
        store.save(sample()).unwrap();
        store.delete("Before a flight").unwrap();

        assert!(store.is_empty());
        assert!(Store::load(&dir).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
