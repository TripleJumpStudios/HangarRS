//! Typed errors shared across the library crates.
//!
//! Library crates use `thiserror` and return these; `hangar-app` and the actor
//! tasks use `anyhow` and never panic.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{what} not found at {path}")]
    NotFound { what: &'static str, path: PathBuf },

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is malformed: {reason}")]
    Malformed { path: PathBuf, reason: String },

    /// A write was refused before it was attempted.
    ///
    /// Either the target is under the read-only constraint — the live MSFS
    /// prefix, the Community folder, or Steam-managed userdata — and no
    /// explicit approval was given, or writing would destroy something the
    /// caller has no business destroying, as when a migration finds a config
    /// already in place.
    ///
    /// Typed rather than a generic error so a caller cannot mistake it for a
    /// transient I/O failure and retry it.
    #[error("refusing to write {path}: {reason}")]
    WriteRefused { path: PathBuf, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Wrap an I/O error with the path it happened to.
    ///
    /// `std::io::Error` carries no path, so a bare `?` up several layers
    /// produces "No such file or directory" with nothing to act on.
    pub fn read(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Error::Read {
            path: path.into(),
            source,
        }
    }

    /// Wrap an I/O error from a write with its path.
    pub fn write(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Error::Write {
            path: path.into(),
            source,
        }
    }

    /// A parse failure, naming the file and what was wrong with it.
    pub fn malformed(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Error::Malformed {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_carry_the_path_they_happened_to() {
        let e = Error::read("/mnt/steam/nope.vdf")(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No such file or directory",
        ));
        let msg = e.to_string();
        assert!(
            msg.contains("/mnt/steam/nope.vdf"),
            "path missing from: {msg}"
        );
    }

    #[test]
    fn a_refused_write_reads_as_a_refusal_not_a_failure() {
        let e = Error::WriteRefused {
            path: "/mnt/steam/.../Community".into(),
            reason: "live Community folder is read-only until approved".into(),
        };
        assert!(e.to_string().starts_with("refusing to write"));
    }
}
