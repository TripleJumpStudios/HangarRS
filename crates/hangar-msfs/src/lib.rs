//! MSFS 2024 specifics — the seam that keeps every other crate sim-agnostic.
//!
//! `UserCfg.opt`, the Community folders and package manifests live here, so
//! adding X-Plane later is a new crate rather than a refactor of the existing
//! ones. `hangar-steam` deliberately knows nothing about any of it.
//!
//! # Read-only
//!
//! Nothing in this crate writes to the prefix or the Community folder.

pub mod env;
pub mod exe_xml;
pub mod manifest;
pub mod xml_encoding;

pub use env::{CommunityFolder, Generation, MsfsEnv};
pub use exe_xml::{ExeXml, LaunchAddon};
pub use manifest::{ContentType, ManifestJson};

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MsfsError {
    #[error(transparent)]
    Steam(#[from] hangar_steam::SteamError),

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is malformed: {reason}")]
    Malformed { path: PathBuf, reason: String },

    #[error("no Community folder found under {packages_root}")]
    NoCommunityFolder { packages_root: PathBuf },
}

pub type Result<T> = std::result::Result<T, MsfsError>;
