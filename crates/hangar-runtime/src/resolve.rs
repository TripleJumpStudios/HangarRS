//! What to do about one app launch, decided from evidence before anything runs.
//!
//! Flight and the Hangar rows ask the same question here — launch it, adopt
//! the instance already running, wait, or refuse — and the existing executors
//! carry the answer out: `runner::launch_with` for the companion and native
//! routes, `sim_session::Runtime` for the simulator's session.
//!
//! Pure: it takes an [`Observation`] and returns a [`Resolution`]. Nothing here
//! reads `/proc`, launches, adopts or changes a route.
//!
//! # The sim-session route keeps its own executor
//!
//! BeyondATC's executor already resolves more strictly than this module could
//! from an observation: it validates the prefix, the user and mount
//! namespaces, pins their descriptors and the PID/start time, and refuses an
//! instance from another session. So for [`Route::SimSession`] the decision is
//! [`Decision::Delegate`] — the pinned executor decides — unless the app is
//! away updating, which is the one thing this module knows and it does not.
//!
//! # What does not count as adoption
//!
//! Exactly one instance where the route puts it is [`Decision::Adopt`]. Several
//! instances, or one whose environment could not be read, is
//! [`Decision::AdoptUnverified`]: no second copy is started (starting another
//! copy beside an unknown one is worse), but the row says the environment is unverified rather than
//! claiming a match. A definite mismatch — another prefix, another session —
//! is refused with the reason.

use crate::observe::{self, Comparison, Expected, Gap, Identity, Observation, Route, Verdict};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Why a launch was refused. Each one is something a person can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Already running in a different Wine prefix.
    WrongPrefix,
    /// Already running outside the simulator's session.
    OtherSession,
}

impl Refusal {
    pub fn explain(self, app: &str) -> String {
        match self {
            Self::WrongPrefix => {
                format!("{app} is already running in another Wine prefix; close it there first")
            }
            Self::OtherSession => {
                format!("{app} is already running outside MSFS's session; close it first")
            }
        }
    }
}

/// The typed answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing is running: start it through the route's executor.
    Launch,
    /// Exactly one instance, where the route puts it.
    Adopt(Identity),
    /// Running, but not certified: several instances, or evidence that could
    /// not be read. No second copy is started.
    AdoptUnverified(Verdict),
    /// It left to update and has not come back yet: wait for it.
    WaitForUpdate { was: Identity, away: Duration },
    /// A definite mismatch.
    Refuse(Refusal),
    /// The sim-session executor resolves this route itself (see module docs).
    Delegate,
}

impl Decision {
    /// Whether the executor should be called at all.
    pub fn proceeds(self) -> bool {
        matches!(
            self,
            Self::Launch | Self::Adopt(_) | Self::AdoptUnverified(_) | Self::Delegate
        )
    }

    /// One line for Activity and the launch log.
    pub fn describe(self) -> String {
        match self {
            Self::Launch => "not running; launching".into(),
            Self::Adopt(id) => format!("running where expected; adopting pid {}", id.pid),
            Self::AdoptUnverified(verdict) => format!(
                "running, environment unverified ({}); not starting a second copy",
                verdict.explain().trim_end_matches('.')
            ),
            Self::WaitForUpdate { was, away } => format!(
                "pid {} is restarting after an update ({}s); waiting for it",
                was.pid,
                away.as_secs()
            ),
            Self::Refuse(r) => match r {
                Refusal::WrongPrefix => "refused: running in another Wine prefix".into(),
                Refusal::OtherSession => "refused: running outside MSFS's session".into(),
            },
            Self::Delegate => "the session executor checks prefix and namespaces".into(),
        }
    }
}

/// A decision, the evidence it came from, and the attempt it belongs to.
#[derive(Clone, Debug)]
pub struct Resolution {
    /// Ties every Activity line of one launch attempt together.
    pub attempt: u64,
    pub route: Route,
    pub decision: Decision,
    pub comparison: Comparison,
}

/// Which route a launch request takes.
pub fn route_of(mode: crate::runner::RunMode) -> Route {
    match mode {
        crate::runner::RunMode::Prefix => Route::Companion,
        crate::runner::RunMode::SimSession => Route::SimSession,
        crate::runner::RunMode::Native => Route::Native,
    }
}

static ATTEMPT: AtomicU64 = AtomicU64::new(1);

/// A fresh attempt number, unique for this process.
pub fn next_attempt() -> u64 {
    ATTEMPT.fetch_add(1, Ordering::Relaxed)
}

/// Decide one launch.
///
/// `updating` is the self-update tracker's answer for this app: the instance
/// that left and how long it has been away (see [`crate::respawn`]).
pub fn resolve(
    expected: &Expected,
    obs: &Observation,
    updating: Option<(Identity, Duration)>,
) -> Resolution {
    let comparison = observe::compare(expected, obs, None);
    let decision = match (updating, expected.route) {
        (Some((was, away)), _) => Decision::WaitForUpdate { was, away },
        (None, Route::SimSession) => Decision::Delegate,
        (None, _) => match comparison.verdict {
            Verdict::NotRunning => Decision::Launch,
            Verdict::Matches => match comparison.instance {
                Some(id) => Decision::Adopt(id),
                None => Decision::AdoptUnverified(Verdict::Unverified(Gap::Unreadable)),
            },
            // Only a prefix that was read and differs is a mismatch. An
            // instance with no WINEPREFIX at all may be one Wine spawned
            // itself (a self-update relaunch); that is unknown, not wrong.
            Verdict::WrongPrefix if instance_prefix_known(obs, expected, &comparison) => {
                Decision::Refuse(Refusal::WrongPrefix)
            }
            Verdict::WrongPrefix => Decision::AdoptUnverified(Verdict::Unverified(Gap::Unreadable)),
            Verdict::OtherSession => Decision::Refuse(Refusal::OtherSession),
            v @ (Verdict::Ambiguous(_) | Verdict::Unverified(_)) => Decision::AdoptUnverified(v),
        },
    };
    Resolution {
        attempt: next_attempt(),
        route: expected.route,
        decision,
        comparison,
    }
}

fn instance_prefix_known(obs: &Observation, expected: &Expected, c: &Comparison) -> bool {
    let Some(id) = c.instance else { return false };
    obs.found(&expected.key)
        .and_then(|f| f.mains().find(|p| p.identity == id))
        .is_some_and(|p| matches!(p.prefix, crate::observe::Fact::Known(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::{Fact, Found, Namespace, Process};
    use std::path::{Path, PathBuf};

    const KEY: &str = "Project Airports.exe";

    fn process(pid: u32, prefix: Fact<PathBuf>) -> Process {
        Process {
            identity: Identity { pid, start: 1000 },
            helper: false,
            prefix,
            user_ns: Fact::Known(Namespace::fixture(1)),
            mount_ns: Fact::Known(Namespace::fixture(2)),
        }
    }

    fn observation(found: Vec<Process>) -> Observation {
        let mut obs = Observation::empty(&[KEY.to_string()]);
        obs.host_user = Fact::Known(Namespace::fixture(1));
        obs.host_mount = Fact::Known(Namespace::fixture(2));
        obs.apps.insert(
            KEY.to_string(),
            Found {
                exact: found,
                ..Found::default()
            },
        );
        obs
    }

    fn companion() -> Expected {
        Expected::new(
            Route::Companion,
            KEY,
            Some(Path::new("/scratch/companion/pfx")),
        )
    }

    fn known(p: &str) -> Fact<PathBuf> {
        Fact::Known(PathBuf::from(p))
    }

    #[test]
    fn nothing_running_launches() {
        let r = resolve(&companion(), &observation(vec![]), None);
        assert_eq!(r.decision, Decision::Launch);
        assert!(r.decision.proceeds());
    }

    #[test]
    fn one_instance_where_the_route_puts_it_is_adopted() {
        let obs = observation(vec![process(41, known("/scratch/companion/pfx"))]);
        let r = resolve(&companion(), &obs, None);
        assert_eq!(
            r.decision,
            Decision::Adopt(Identity {
                pid: 41,
                start: 1000
            })
        );
    }

    #[test]
    fn an_instance_in_another_prefix_is_refused_with_the_reason() {
        let obs = observation(vec![process(41, known("/scratch/sim/pfx"))]);
        let r = resolve(&companion(), &obs, None);
        assert_eq!(r.decision, Decision::Refuse(Refusal::WrongPrefix));
        assert!(!r.decision.proceeds());
        assert!(Refusal::WrongPrefix
            .explain("Project Airports")
            .contains("another Wine prefix"));
    }

    #[test]
    fn an_instance_with_no_prefix_set_is_unverified_not_refused() {
        let obs = observation(vec![process(41, Fact::Missing)]);
        let r = resolve(&companion(), &obs, None);
        assert_eq!(
            r.comparison.verdict,
            Verdict::WrongPrefix,
            "the view says so"
        );
        assert!(matches!(r.decision, Decision::AdoptUnverified(_)), "{r:?}");
    }

    #[test]
    fn several_instances_or_unreadable_evidence_are_never_a_match() {
        let two = observation(vec![
            process(41, known("/scratch/companion/pfx")),
            process(42, known("/scratch/companion/pfx")),
        ]);
        let r = resolve(&companion(), &two, None);
        assert_eq!(r.decision, Decision::AdoptUnverified(Verdict::Ambiguous(2)));
        assert!(r.decision.proceeds(), "no second copy, but no refusal");

        let unreadable = observation(vec![process(41, Fact::Unreadable)]);
        let r = resolve(&companion(), &unreadable, None);
        assert!(matches!(r.decision, Decision::AdoptUnverified(_)), "{r:?}");
    }

    #[test]
    fn a_prefix_route_with_no_known_prefix_does_not_certify() {
        let expected = Expected::new(Route::Companion, KEY, None);
        let obs = observation(vec![process(41, known("/scratch/companion/pfx"))]);
        let r = resolve(&expected, &obs, None);
        assert!(matches!(r.decision, Decision::AdoptUnverified(_)), "{r:?}");
    }

    #[test]
    fn an_app_away_updating_is_waited_for_on_every_route() {
        let was = Identity {
            pid: 41,
            start: 1000,
        };
        for route in [Route::Companion, Route::SimSession, Route::Native] {
            let expected = Expected::new(route, KEY, Some(Path::new("/scratch/companion/pfx")));
            let r = resolve(
                &expected,
                &observation(vec![]),
                Some((was, Duration::from_secs(30))),
            );
            assert_eq!(
                r.decision,
                Decision::WaitForUpdate {
                    was,
                    away: Duration::from_secs(30)
                }
            );
            assert!(!r.decision.proceeds());
        }
    }

    #[test]
    fn the_sim_session_route_is_left_to_its_pinned_executor() {
        let expected = Expected::new(Route::SimSession, "BeyondATC.exe", Some(Path::new("/s")));
        // Even a definite-looking mismatch: the executor's own check is stricter.
        let obs = observation(vec![process(41, known("/elsewhere"))]);
        assert_eq!(resolve(&expected, &obs, None).decision, Decision::Delegate);
    }

    #[test]
    fn a_native_tool_needs_no_prefix() {
        let expected = Expected::new(Route::Native, KEY, None);
        let obs = observation(vec![process(41, Fact::Missing)]);
        let r = resolve(&expected, &obs, None);
        assert!(matches!(r.decision, Decision::Adopt(_)), "{r:?}");
    }

    #[test]
    fn attempts_are_numbered_apart() {
        let a = resolve(&companion(), &observation(vec![]), None).attempt;
        let b = resolve(&companion(), &observation(vec![]), None).attempt;
        assert_ne!(a, b);
    }
}
