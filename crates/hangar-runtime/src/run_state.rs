//! What "running" means for an app, and how we found out.
//!
//! An adopted PID has no `Child` handle to wait on, so the exit path an owned
//! process uses simply does not exist for it. The distinction is a type here
//! rather than a comment.

use crate::proc::AppProcesses;
use std::time::{Duration, Instant};

/// How we came to be tracking this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// We spawned it and hold a `Child`. Exit arrives from `wait()`, and its
    /// output is on our pipes.
    Owned,
    /// It was already running when we looked. There is no handle and no
    /// output — exit can only come from watching `/proc/[pid]` disappear.
    Adopted,
}

impl Ownership {
    /// Whether a `Child::wait()` can report this process exiting.
    pub fn has_child_handle(self) -> bool {
        matches!(self, Ownership::Owned)
    }

    /// Whether this process's output reaches our console.
    ///
    /// An adopted process's stdout went wherever it was going before we
    /// arrived — usually nowhere. The console will be empty for it, and that is
    /// expected rather than a fault.
    pub fn streams_output(self) -> bool {
        matches!(self, Ownership::Owned)
    }

    pub fn label(self) -> &'static str {
        match self {
            Ownership::Owned => "launched",
            Ownership::Adopted => "adopted",
        }
    }
}

/// A running app HangarRS is tracking.
#[derive(Debug, Clone)]
pub struct RunningApp {
    pub label: String,
    /// The process in charge. Killing this is sufficient; children wind down.
    pub pid: u32,
    pub ownership: Ownership,
    /// When we started or noticed it.
    pub since: Instant,
    /// Helper PIDs, for display only.
    pub helpers: Vec<u32>,
    /// Whether the process has produced any output we could see.
    ///
    /// Only ever true for [`Ownership::Owned`]; used with elapsed time to spot
    /// a launch that died immediately.
    pub produced_output: bool,
}

impl RunningApp {
    pub fn owned(label: impl Into<String>, pid: u32) -> Self {
        Self {
            label: label.into(),
            pid,
            ownership: Ownership::Owned,
            since: Instant::now(),
            helpers: Vec::new(),
            produced_output: false,
        }
    }

    /// Build from a `/proc` scan that found the app already running.
    pub fn adopted(label: impl Into<String>, found: &AppProcesses) -> Option<Self> {
        Some(Self {
            label: label.into(),
            pid: found.main_pid()?,
            ownership: Ownership::Adopted,
            since: Instant::now(),
            helpers: found.helpers.iter().map(|p| p.pid).collect(),
            produced_output: false,
        })
    }

    pub fn uptime(&self) -> Duration {
        self.since.elapsed()
    }

    /// Total processes, main plus helpers.
    pub fn process_count(&self) -> usize {
        1 + self.helpers.len()
    }
}

/// How long a launch has to survive before it counts as having started.
pub const IMMEDIATE_EXIT: Duration = Duration::from_secs(2);

/// Why a launch ended the moment it began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// It is running.
    Started,
    /// It exited straight away without saying anything.
    ImmediateExit {
        after: Duration,
        likely_cause: String,
    },
    /// It exited quickly, but told us why on the way out.
    ExitedWithOutput { after: Duration },
}

/// Judge how a launch went.
///
/// **The exit code is useless here.** An Electron app that loses the
/// single-instance lock calls `app.quit()`, which is a clean exit 0 — identical
/// to a successful run from the outside. Four of the five known installer
/// failure modes also return 0. What separates a failed launch from a working
/// one is that it died almost immediately and produced no output of its own.
///
/// The Wine startup noise (`winevulkan`, `wineopenxr`, and in particular
/// `OpenXR: Unable to get required Vulkan Device extensions size`) is emitted
/// *before* the app's own code runs, so it must not count as output. Callers
/// pass `produced_output = false` for it — see [`is_app_output`].
pub fn judge_launch(
    elapsed: Duration,
    produced_output: bool,
    still_running: bool,
) -> LaunchOutcome {
    if still_running {
        return LaunchOutcome::Started;
    }
    if elapsed >= IMMEDIATE_EXIT {
        return LaunchOutcome::ExitedWithOutput { after: elapsed };
    }
    if produced_output {
        return LaunchOutcome::ExitedWithOutput { after: elapsed };
    }
    LaunchOutcome::ImmediateExit {
        after: elapsed,
        likely_cause: "another instance is probably already running and holding a \
                       single-instance lock — try killing it first"
            .into(),
    }
}

/// Whether a console line came from the app rather than from Wine starting up.
///
/// Wine emits a good deal before handing control to the program, and counting
/// any of it as "the app said something" would mask exactly the failure this is
/// trying to catch.
pub fn is_app_output(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() {
        return false;
    }
    const WINE_NOISE: &[&str] = &[
        "fixme:",
        "err:winevulkan",
        "err:wineopenxr",
        "wine:",
        "OpenXR:",
        "MESA-INTEL:",
        "ATTENTION: default value of option",
    ];
    !WINE_NOISE.iter().any(|prefix| l.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::ProcInfo;

    fn found(main: Option<u32>, helpers: &[u32]) -> AppProcesses {
        AppProcesses {
            main: main.map(|pid| ProcInfo {
                pid,
                cmdline: "app.exe".into(),
                args: vec!["app.exe".into()],
            }),
            helpers: helpers
                .iter()
                .map(|pid| ProcInfo {
                    pid: *pid,
                    cmdline: "app.exe --type=renderer".into(),
                    args: vec!["app.exe".into(), "--type=renderer".into()],
                })
                .collect(),
        }
    }

    /// An adopted PID has no child handle, so nothing may try to `wait()` on it.
    #[test]
    fn an_adopted_process_has_no_child_handle_and_no_output() {
        let app =
            RunningApp::adopted("Project Airports", &found(Some(53513), &[53536, 53591])).unwrap();
        assert_eq!(app.ownership, Ownership::Adopted);
        assert!(!app.ownership.has_child_handle());
        assert!(!app.ownership.streams_output());
        assert_eq!(app.pid, 53513);
        assert_eq!(app.process_count(), 3);
    }

    #[test]
    fn a_launched_process_has_both() {
        let app = RunningApp::owned("BeyondATC", 1234);
        assert!(app.ownership.has_child_handle());
        assert!(app.ownership.streams_output());
        assert_eq!(app.process_count(), 1);
    }

    #[test]
    fn adopting_nothing_yields_nothing() {
        assert!(RunningApp::adopted("X", &found(None, &[])).is_none());
        assert!(
            RunningApp::adopted("X", &found(None, &[53591])).is_none(),
            "helpers alone are not an adoptable app"
        );
    }

    /// **The failure that exits 0.** A launch that dies in under two seconds
    /// with nothing to say is the single-instance lock, every time.
    #[test]
    fn an_instant_silent_exit_is_reported_as_a_failure() {
        let outcome = judge_launch(Duration::from_millis(400), false, false);
        match outcome {
            LaunchOutcome::ImmediateExit {
                after,
                likely_cause,
            } => {
                assert!(after < IMMEDIATE_EXIT);
                assert!(likely_cause.contains("single-instance lock"));
            }
            other => panic!("expected an immediate exit, got {other:?}"),
        }
    }

    #[test]
    fn a_process_still_running_has_started() {
        assert_eq!(
            judge_launch(Duration::from_millis(10), false, true),
            LaunchOutcome::Started
        );
    }

    /// Dying quickly but *saying* something is a different problem, and gets a
    /// different message — the console has the reason in it.
    #[test]
    fn a_quick_exit_that_said_something_is_not_the_lock() {
        let outcome = judge_launch(Duration::from_millis(400), true, false);
        assert!(matches!(outcome, LaunchOutcome::ExitedWithOutput { .. }));
    }

    #[test]
    fn a_long_run_that_ends_is_just_an_exit() {
        let outcome = judge_launch(Duration::from_secs(3600), false, false);
        assert!(matches!(outcome, LaunchOutcome::ExitedWithOutput { .. }));
    }

    /// Wine's startup chatter must not count as the app having spoken —
    /// otherwise the immediate-exit check never fires and a failed launch looks
    /// like a successful one.
    #[test]
    fn wine_startup_noise_is_not_app_output() {
        let noise = [
            "fixme:winediag:loader_init wine-staging 9.0",
            "err:winevulkan:wine_vk_init Failed to load vulkan",
            "err:wineopenxr:wine_xrEnumerateInstanceExtensionProperties",
            "OpenXR: Unable to get required Vulkan Device extensions size",
            "wine: using kernel write watches",
            "   ",
            "",
        ];
        for line in noise {
            assert!(!is_app_output(line), "should be ignored: {line:?}");
        }
    }

    #[test]
    fn the_apps_own_output_counts() {
        for line in [
            "[main] starting up",
            "Project Airports 0.1.33",
            "second_instance.focus",
        ] {
            assert!(is_app_output(line), "should count: {line:?}");
        }
    }

    /// A launch that produced only Wine noise is still a silent one.
    #[test]
    fn a_launch_producing_only_wine_noise_is_still_an_immediate_exit() {
        let lines = [
            "fixme:winediag:loader_init",
            "OpenXR: Unable to get required Vulkan Device extensions size",
        ];
        let produced = lines.iter().any(|l| is_app_output(l));
        assert!(!produced);
        assert!(matches!(
            judge_launch(Duration::from_millis(300), produced, false),
            LaunchOutcome::ImmediateExit { .. }
        ));
    }
}
