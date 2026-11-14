//! Proton/Wine orchestration and process lifecycle.
//!
//! `ProtonEnv`, the companion prefix, the runner, the `/proc` watcher, and
//! three features for companion apps: adoption of ones already running,
//! per-app kill, and immediate-exit detection.
//!
//! The rule underneath all three: **match on `cmdline`, never `comm` or `exe`.**
//! Chromium renames its processes to `CrBrowserMain`, and `/proc/PID/exe`
//! resolves to `wine64-preloader` for every Wine process.

pub mod batc_ready;
pub mod companion_setup;
pub mod drift;
pub mod env;
pub mod kill;
pub mod monitor;
pub mod observe;
pub mod preflight;
pub mod readiness;
pub mod resolve;
pub mod respawn;
pub mod run_state;
pub mod runner;
pub mod sim;
pub mod sim_session;
pub mod sim_simconnect;
pub mod simlink;
#[cfg(test)]
mod test_app;

/// Re-exported from `hangar-core`, where it lives so that `hangar-bindings`
/// (the Steam-is-closed gate) and the preflight can use it without a second
/// copy of the `argv[0]` discipline.
pub use hangar_core::proc;

pub use env::ProtonEnv;
pub use kill::{close_then_kill, kill_app, kill_pid, KillOutcome};
pub use monitor::{AppMonitor, RunningSet};
pub use preflight::{Action, Condition, Preflight, Run, Step, Store as PreflightStore};
pub use proc::{find_app, AppProcesses, ProcInfo};
pub use run_state::{judge_launch, LaunchOutcome, Ownership, RunningApp};
pub use runner::{
    already_running, launch, launch_with, restart, restart_with, LaunchRequest, Launched, RunMode,
    Sink,
};
pub use sim::MSFS_2024_APP_ID;
pub use simlink::{check as check_sim_link, repair as repair_sim_link};
