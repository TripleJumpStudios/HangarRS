//! The sim as a process, and how to start one.
//!
//! # Starting it is a hand-off, not a launch
//!
//! `steam://rungameid/2537590` is the same URL a person clicks in Steam. Opening
//! it asks Steam to start the game and returns immediately — **there is no child
//! process, no PID and no exit code**, because Steam owns what it started.
//!
//! That is deliberate rather than a limitation. HangarRS must never become
//! something the sim needs in order to launch: close it, uninstall it, and
//! Steam starts MSFS exactly as before. Spawning the executable directly would
//! bypass Steam's own Proton setup and put this application in the middle of a
//! path that works fine without it.
//!
//! The consequence is that *started* and *ready* are separate observations,
//! minutes apart: a `/proc` scan for the first, a SimConnect connection for the
//! second.
//!
//! # Finding it
//!
//! `argv[0]` only, like everything else here. `comm` truncates the sim to
//! `FlightSimulator2` and `/proc/PID/exe` is the Wine preloader. A loose
//! substring search over whole command lines is worse still: it matches the
//! shell running the search, which is precisely the failure the rule exists for.

use crate::proc::{self, AppProcesses};
use std::process::Command;

/// MSFS 2024's Steam App ID.
pub const MSFS_2024_APP_ID: &str = "2537590";

/// The executable's file name, which is what `argv[0]` ends with.
pub const MSFS_2024_EXE: &str = "FlightSimulator2024.exe";

/// `steam://rungameid/2537590`.
pub fn run_url(app_id: &str) -> String {
    format!("steam://rungameid/{app_id}")
}

/// The sim's processes, if it is up.
pub fn processes() -> AppProcesses {
    proc::find_app(MSFS_2024_EXE)
}

/// Whether a sim process exists.
///
/// **Not the same as ready.** The window can be up, the process running and
/// SimConnect still refusing connections for several more minutes.
pub fn is_running() -> bool {
    processes().is_running()
}

/// How the sim was asked to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    /// The URL that was opened.
    pub url: String,
    /// The command used to open it.
    pub opener: String,
}

/// Ask Steam to start the sim.
///
/// Returns as soon as the URL has been handed off. Nothing is waited for and
/// nothing is tracked — [`is_running`] is how you find out it worked.
///
/// `xdg-open` first, because that is exactly what clicking the link does. If
/// the desktop has no handler registered, the `steam` binary is asked directly
/// rather than failing: a missing MIME association is a desktop configuration
/// problem, not a reason to refuse to start a flight.
pub fn launch(app_id: &str) -> std::io::Result<Handoff> {
    let url = run_url(app_id);

    for opener in ["xdg-open", "steam"] {
        match Command::new(opener)
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_child) => {
                // Deliberately not waited on. The opener exits immediately and
                // Steam does the work; reaping it would tell us nothing about
                // the sim.
                tracing::info!(%url, opener, "asked Steam to start the sim");
                return Ok(Handoff {
                    url,
                    opener: opener.to_string(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "neither xdg-open nor steam is on PATH, so the sim cannot be started \
         the way a person would start it",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_is_the_one_a_person_would_click() {
        assert_eq!(run_url(MSFS_2024_APP_ID), "steam://rungameid/2537590");
    }

    #[test]
    fn the_needle_is_a_file_name_not_a_path() {
        // `proc::find_app` matches the end of argv[0], so a bare file name
        // covers the sim however Steam and Proton spell the path to it.
        assert!(!MSFS_2024_EXE.contains('/'));
        assert!(!MSFS_2024_EXE.contains('\\'));
        assert!(MSFS_2024_EXE.ends_with(".exe"));
    }

    #[test]
    fn scanning_for_the_sim_does_not_match_this_test_process() {
        // The bug this guards: a loose search over whole command lines matches
        // whatever is doing the searching. Running the real scan here is a live
        // check that it does not, since this binary's command line contains
        // the crate name and the test filter.
        let found = processes();
        assert!(
            !found.is_running() || found.main_pid() != Some(std::process::id()),
            "the scan matched itself"
        );
    }
}
