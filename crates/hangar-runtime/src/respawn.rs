//! An app that exits soon after launch and comes back is updating, not failing.
//!
//! Project Airports can exit four seconds into its launch and come back as a
//! new process, and BeyondATC's updater relaunches it two to three minutes
//! later. Neither comes back through HangarRS. Treating the first exit as
//! "exited" loses the readiness gate and drops the new instance from status,
//! Stop and sim-exit close.
//!
//! # The rule
//!
//! An exit within [`RESPAWN_WINDOW`] of launch starts a bounded wait
//! ([`RESPAWN_WAIT`]). A process with the same executable name, in the **same
//! prefix and user/mount namespaces** as the instance that exited, is the same
//! app restarted: it is adopted as the tracked instance. Name alone is not
//! enough — suffix matching collides, and a second copy in another prefix is a
//! different install.
//!
//! Missing or unreadable evidence never matches, and several candidates are
//! never treated as one: the wait simply continues until it runs out.
//!
//! An exit after HangarRS asked the app to close is final, whenever it comes
//! ([`Tracker::closing`]). BATC asks for confirmation in its own dialog, so a
//! Stop can end in an exit minutes after launch that would otherwise read as an
//! update.
//!
//! Nothing here reads `/proc` or the clock. The caller passes an
//! [`Observation`](crate::observe::Observation) and `now`, so tests drive it
//! with fixtures and instant time.

use crate::observe::{Fact, Found, Identity, Namespace, Process};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How soon after launch an exit counts as a possible update.
///
/// Both observed updates happened in the first three minutes. An app that
/// quits later is taken to have been closed.
pub const RESPAWN_WINDOW: Duration = Duration::from_secs(10 * 60);

/// How long to wait for the restarted instance before calling it exited.
///
/// BeyondATC's updater took 94 s and 119 s, Project Airports
/// 4 s. Twice the slowest, rounded up.
pub const RESPAWN_WAIT: Duration = Duration::from_secs(4 * 60);

/// Where an instance runs. Two processes are the same app only when all three agree.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    prefix: Fact<PathBuf>,
    user_ns: Fact<Namespace>,
    mount_ns: Fact<Namespace>,
}

impl Signature {
    fn of(p: &Process) -> Self {
        Self {
            prefix: p.prefix.clone(),
            user_ns: p.user_ns.clone(),
            mount_ns: p.mount_ns.clone(),
        }
    }

    /// Known and equal on every field; anything unknown is not a match.
    fn matches(&self, p: &Process) -> bool {
        fn same<T: PartialEq>(a: &Fact<T>, b: &Fact<T>) -> bool {
            matches!((a, b), (Fact::Known(x), Fact::Known(y)) if x == y)
        }
        // A prefix the process does not set (native tool) is a fact too.
        let prefix = same(&self.prefix, &p.prefix)
            || (self.prefix == Fact::Missing && p.prefix == Fact::Missing);
        prefix && same(&self.user_ns, &p.user_ns) && same(&self.mount_ns, &p.mount_ns)
    }
}

/// Where the tracked app is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Launched; no instance seen yet.
    Starting,
    Running(Identity),
    /// The instance exited inside the window; waiting for it to come back.
    Updating {
        was: Identity,
        since: Instant,
    },
    /// Gone for good, or never observed in a way that could be followed.
    Exited,
}

/// What changed on one observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    None,
    /// First instance seen after launch.
    Seen(Identity),
    /// The instance exited inside the window; the wait has begun.
    Exited(Identity),
    /// A matching instance replaced the old one. `gap` is whether the exit was
    /// observed first (`Exited` came before) or the PID simply changed.
    Respawned {
        was: Identity,
        now: Identity,
        gap: bool,
    },
    /// The wait ran out with nothing to adopt.
    GaveUp(Identity),
    /// Exited after the window: a normal exit, not an update.
    Ended(Identity),
    /// Exited after HangarRS asked it to close: final, not an update.
    Closed(Identity),
}

/// Follows one launched app across self-update restarts.
#[derive(Clone, Debug)]
pub struct Tracker {
    pub key: String,
    launched: Instant,
    signature: Option<Signature>,
    phase: Phase,
    /// HangarRS asked this launch to close.
    closing: bool,
    /// The pid the launch returned, when the caller passed it on. Breaks a tie
    /// between several first candidates; never required.
    launched_pid: Option<u32>,
}

/// Which process a Stop or Restart of one launch means, by identity.
///
/// Resolved from the same evidence and the same rule as [`Tracker::observe`],
/// without changing the tracker: an instance is reached only by its identity
/// (pid plus start time) or as the one update restart in the same prefix and
/// namespaces. Anything else is a refusal, never a guess by match key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// This exact process.
    Instance(Identity),
    /// Nothing of this launch is running.
    NotRunning,
    /// Exited inside the window; its restart has not appeared yet.
    Updating { was: Identity },
    /// Several processes could be it; none is touched.
    Ambiguous(usize),
}

impl Tracker {
    pub fn new(key: impl Into<String>, launched: Instant) -> Self {
        Self {
            key: key.into(),
            launched,
            signature: None,
            phase: Phase::Starting,
            closing: false,
            launched_pid: None,
        }
    }

    /// The pid the launch itself returned. Only breaks a tie between several
    /// first candidates; with one candidate the old rule stands, because a
    /// launch through Proton's script returns the script's pid, not the app's.
    pub fn launched_as(&mut self, pid: u32) {
        self.launched_pid = Some(pid);
    }

    /// HangarRS asked the app to close at `requested`. A request from before
    /// this launch belongs to an earlier one and is ignored.
    pub fn closing(&mut self, requested: Instant) {
        if requested >= self.launched {
            self.closing = true;
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// How long it has been updating, if it is.
    pub fn updating_for(&self, now: Instant) -> Option<Duration> {
        match self.phase {
            Phase::Updating { since, .. } => Some(now.saturating_duration_since(since)),
            _ => None,
        }
    }

    pub fn current(&self) -> Option<Identity> {
        match self.phase {
            Phase::Running(id) => Some(id),
            _ => None,
        }
    }

    /// The first instance of the launch: the only candidate, or among
    /// several, the one with the launch's own pid.
    fn first<'a>(&self, candidates: &[&'a Process]) -> Option<&'a Process> {
        match candidates {
            [only] => Some(*only),
            many => {
                let pid = self.launched_pid?;
                let mut hits = many.iter().filter(|p| p.identity.pid == pid);
                match (hits.next(), hits.next()) {
                    (Some(p), None) => Some(*p),
                    _ => None,
                }
            }
        }
    }

    fn candidates<'a>(&self, found: Option<&'a Found>) -> Vec<&'a Process> {
        let mains: Vec<&Process> = found.map(|f| f.mains().collect()).unwrap_or_default();
        match &self.signature {
            Some(sig) => mains.into_iter().filter(|p| sig.matches(p)).collect(),
            None => mains,
        }
    }

    /// The process a Stop or Restart of this launch should reach now,
    /// without changing the tracker.
    ///
    /// The tracker may be a poll behind: the instance it knows can have
    /// exited and been replaced by its update restart since. The same rule as
    /// [`Tracker::observe`] decides, so the respawned process is reached and a
    /// stale pid or a stranger with the same exe never is.
    pub fn target(&self, found: Option<&Found>, now: Instant) -> Target {
        let candidates = self.candidates(found);
        let respawn = |was: Identity| match candidates[..] {
            [] => Target::Updating { was },
            [only] => Target::Instance(only.identity),
            ref many => Target::Ambiguous(many.len()),
        };
        match self.phase {
            Phase::Starting => match self.first(&candidates) {
                Some(p) => Target::Instance(p.identity),
                None if candidates.is_empty() => Target::NotRunning,
                None => Target::Ambiguous(candidates.len()),
            },
            Phase::Running(id) => {
                let still = found.is_some_and(|f| f.mains().any(|p| p.identity == id));
                if still {
                    Target::Instance(id)
                } else if self.closing
                    || now.saturating_duration_since(self.launched) >= RESPAWN_WINDOW
                {
                    // Closed, or ended normally: whatever runs now is not this launch.
                    Target::NotRunning
                } else {
                    respawn(id)
                }
            }
            Phase::Updating { was, since } => {
                if now.saturating_duration_since(since) >= RESPAWN_WAIT {
                    Target::NotRunning
                } else {
                    respawn(was)
                }
            }
            Phase::Exited => Target::NotRunning,
        }
    }

    /// Take one observation of this app's key.
    pub fn observe(&mut self, found: Option<&Found>, now: Instant) -> Event {
        let candidates = self.candidates(found);
        match self.phase.clone() {
            Phase::Starting => {
                // Several unrelated candidates: never treated as one, unless
                // one carries the launch's own pid.
                if let Some(only) = self.first(&candidates) {
                    self.signature = Some(Signature::of(only));
                    self.phase = Phase::Running(only.identity);
                    return Event::Seen(only.identity);
                }
                Event::None
            }
            Phase::Running(id) => {
                if candidates.iter().any(|p| p.identity == id) {
                    return Event::None;
                }
                if self.closing {
                    self.phase = Phase::Exited;
                    return Event::Closed(id);
                }
                let inside = now.saturating_duration_since(self.launched) < RESPAWN_WINDOW;
                if !inside {
                    self.phase = Phase::Exited;
                    return Event::Ended(id);
                }
                if let [only] = candidates[..] {
                    self.phase = Phase::Running(only.identity);
                    return Event::Respawned {
                        was: id,
                        now: only.identity,
                        gap: false,
                    };
                }
                self.phase = Phase::Updating {
                    was: id,
                    since: now,
                };
                Event::Exited(id)
            }
            Phase::Updating { was, since } => {
                if let [only] = candidates[..] {
                    self.phase = Phase::Running(only.identity);
                    return Event::Respawned {
                        was,
                        now: only.identity,
                        gap: true,
                    };
                }
                if now.saturating_duration_since(since) >= RESPAWN_WAIT {
                    self.phase = Phase::Exited;
                    return Event::GaveUp(was);
                }
                Event::None
            }
            Phase::Exited => Event::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns(ino: u64) -> Fact<Namespace> {
        Fact::Known(Namespace::fixture(ino))
    }

    fn process(pid: u32, prefix: &str, user: u64) -> Process {
        Process {
            identity: Identity {
                pid,
                start: pid as u64 * 10,
            },
            helper: false,
            prefix: Fact::Known(PathBuf::from(prefix)),
            user_ns: ns(user),
            mount_ns: ns(user + 1),
        }
    }

    fn found(ps: &[Process]) -> Found {
        Found {
            exact: ps.to_vec(),
            similar: vec![],
        }
    }

    #[test]
    fn project_airports_l2_flight_1_exit_and_return_is_an_update() {
        // pid 30051 seen 20:16:13, gone 20:16:36, 30402 at 20:16:40.
        let t0 = Instant::now();
        let mut t = Tracker::new("Project Airports.exe", t0);
        let old = process(30051, "/companion/pfx", 100);
        let new = process(30402, "/companion/pfx", 100);
        assert_eq!(
            t.observe(Some(&found(std::slice::from_ref(&old))), t0),
            Event::Seen(old.identity)
        );
        let gone = t0 + Duration::from_secs(23);
        assert_eq!(
            t.observe(Some(&found(&[])), gone),
            Event::Exited(old.identity)
        );
        assert_eq!(
            t.updating_for(gone + Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            t.observe(
                Some(&found(std::slice::from_ref(&new))),
                gone + Duration::from_secs(4)
            ),
            Event::Respawned {
                was: old.identity,
                now: new.identity,
                gap: true
            }
        );
        assert_eq!(t.current(), Some(new.identity));
    }

    #[test]
    fn a_pid_change_between_scans_is_a_respawn_without_a_gap() {
        let t0 = Instant::now();
        let mut t = Tracker::new("BeyondATC.exe", t0);
        let old = process(38377, "/sim/pfx", 7);
        let new = process(39513, "/sim/pfx", 7);
        t.observe(Some(&found(std::slice::from_ref(&old))), t0);
        assert_eq!(
            t.observe(
                Some(&found(std::slice::from_ref(&new))),
                t0 + Duration::from_secs(90)
            ),
            Event::Respawned {
                was: old.identity,
                now: new.identity,
                gap: false
            }
        );
    }

    #[test]
    fn another_prefix_or_namespace_is_never_adopted() {
        let t0 = Instant::now();
        let mut t = Tracker::new("BeyondATC.exe", t0);
        t.observe(Some(&found(&[process(1, "/sim/pfx", 7)])), t0);
        t.observe(Some(&found(&[])), t0 + Duration::from_secs(40));
        // The companion copy, and a same-prefix copy in the host namespaces.
        let other_prefix = process(2, "/companion/pfx", 7);
        let other_ns = process(3, "/sim/pfx", 50);
        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            t.observe(Some(&found(&[other_prefix, other_ns])), later),
            Event::None
        );
        assert!(matches!(t.phase(), Phase::Updating { .. }));
    }

    #[test]
    fn unreadable_evidence_and_two_candidates_do_not_match() {
        let t0 = Instant::now();
        let mut t = Tracker::new("App.exe", t0);
        t.observe(Some(&found(&[process(1, "/p", 7)])), t0);
        t.observe(None, t0 + Duration::from_secs(1));
        let mut unreadable = process(2, "/p", 7);
        unreadable.user_ns = Fact::Unreadable;
        assert_eq!(
            t.observe(Some(&found(&[unreadable])), t0 + Duration::from_secs(2)),
            Event::None
        );
        let two = [process(3, "/p", 7), process(4, "/p", 7)];
        assert_eq!(
            t.observe(Some(&found(&two)), t0 + Duration::from_secs(3)),
            Event::None
        );
        // Helpers never count as the instance.
        let mut helper = process(5, "/p", 7);
        helper.helper = true;
        assert_eq!(
            t.observe(Some(&found(&[helper])), t0 + Duration::from_secs(4)),
            Event::None
        );
    }

    #[test]
    fn the_wait_is_bounded() {
        let t0 = Instant::now();
        let mut t = Tracker::new("App.exe", t0);
        let p = process(1, "/p", 7);
        t.observe(Some(&found(std::slice::from_ref(&p))), t0);
        let gone = t0 + Duration::from_secs(30);
        t.observe(Some(&found(&[])), gone);
        assert_eq!(
            t.observe(None, gone + RESPAWN_WAIT - Duration::from_secs(1)),
            Event::None
        );
        assert_eq!(
            t.observe(None, gone + RESPAWN_WAIT),
            Event::GaveUp(p.identity)
        );
        assert_eq!(*t.phase(), Phase::Exited);
        // A later instance is not picked up after giving up.
        assert_eq!(
            t.observe(
                Some(&found(&[process(9, "/p", 7)])),
                gone + RESPAWN_WAIT * 2
            ),
            Event::None
        );
    }

    #[test]
    fn an_exit_after_the_window_is_a_normal_exit() {
        let t0 = Instant::now();
        let mut t = Tracker::new("App.exe", t0);
        let p = process(1, "/p", 7);
        t.observe(Some(&found(std::slice::from_ref(&p))), t0);
        assert_eq!(
            t.observe(Some(&found(&[])), t0 + RESPAWN_WINDOW),
            Event::Ended(p.identity)
        );
        assert_eq!(t.updating_for(t0 + RESPAWN_WINDOW), None);
    }

    #[test]
    fn an_exit_after_a_close_request_is_final_inside_the_window() {
        // BATC as observed: launched 05:17:58, Stop 05:26:37, Confirm in its own
        // dialog, exit 05:27:20 (9 min 22 s). Then nothing comes back.
        let t0 = Instant::now();
        let mut t = Tracker::new("BeyondATC.exe", t0);
        let p = process(252944, "/sim/pfx", 7);
        t.observe(Some(&found(std::slice::from_ref(&p))), t0);
        let stop = t0 + Duration::from_secs(8 * 60 + 39);
        t.closing(stop);
        // The dialog is up: still running, still followed.
        assert_eq!(
            t.observe(
                Some(&found(std::slice::from_ref(&p))),
                stop + Duration::from_secs(15)
            ),
            Event::None
        );
        let exit = t0 + Duration::from_secs(9 * 60 + 22);
        assert_eq!(
            t.observe(Some(&found(&[])), exit),
            Event::Closed(p.identity)
        );
        assert_eq!(*t.phase(), Phase::Exited);
        assert_eq!(t.updating_for(exit + Duration::from_secs(24)), None);
        // A same-session instance afterwards is not adopted into this launch.
        assert_eq!(
            t.observe(
                Some(&found(&[process(9, "/sim/pfx", 7)])),
                exit + Duration::from_secs(90)
            ),
            Event::None
        );
    }

    #[test]
    fn a_close_request_from_before_the_launch_does_not_count() {
        let t0 = Instant::now();
        let earlier = t0;
        let mut t = Tracker::new("App.exe", t0 + Duration::from_secs(1));
        t.closing(earlier);
        let p = process(1, "/p", 7);
        t.observe(
            Some(&found(std::slice::from_ref(&p))),
            t0 + Duration::from_secs(2),
        );
        assert_eq!(
            t.observe(Some(&found(&[])), t0 + Duration::from_secs(30)),
            Event::Exited(p.identity)
        );
    }

    #[test]
    fn the_bounds_cover_the_observed_updates() {
        assert!(RESPAWN_WAIT >= Duration::from_secs(119 * 2));
        assert!(RESPAWN_WINDOW >= Duration::from_secs(3 * 60));
    }
}
