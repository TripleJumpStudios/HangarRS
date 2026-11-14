//! Domain types shared by every HangarRS crate: paths, errors, and the
//! identifiers the other crates key off.
//!
//! Nothing here knows about egui. That is the point — no crate below
//! `hangar-app` depends on a GUI toolkit, which is what makes every subsystem
//! testable and reusable on its own.

pub mod config;
pub mod disk;
pub mod error;
pub mod evdev;
pub mod input;
pub mod migrate;
pub mod paths;
pub mod proc;
pub mod time;

pub use error::{Error, Result};
pub use time::Utc;

/// The Steam App ID of Microsoft Flight Simulator 2024.
pub const MSFS2024_APP_ID: &str = "2537590";

/// Directory name of the MSFS 2024 user data inside the Proton prefix.
///
/// The full, spaced product name — used to build the `UserCfg.opt` path. There
/// is deliberately no `Input/` folder beneath it: bindings live in Steam Cloud
/// userdata instead.
pub const MSFS2024_APPDATA_DIR: &str = "Microsoft Flight Simulator 2024";
