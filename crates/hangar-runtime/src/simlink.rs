//! The one link every companion app's sim detection depends on.
//!
//! A companion app running in the companion prefix looks for the sim where a
//! Windows application would: `%APPDATA%\Microsoft Flight Simulator 2024`. In
//! its own prefix that directory does not exist, because the sim is installed
//! in a different prefix entirely. What makes it work is a symlink:
//!
//! ```text
//! companion-prefix/pfx/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024
//!   -> …/compatdata/2537590/pfx/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024
//! ```
//!
//! Every companion app's sim detection runs through it: Orbx Central writes
//! `rootPath` and `UserCfg.opt` paths that resolve through it, and Just
//! Flight's installer records the same path in the prefix registry under
//! `Software\justflight\msfs24`.
//!
//! A companion prefix adopted from `MSFS_Linux_Manager` already has it. One
//! built fresh would not, and every companion app in it would silently stop
//! finding the sim — which is why `companion_setup` adds it.
//!
//! # Reading is free; writing is not
//!
//! [`check`] is a pure read and can run every frame. [`repair`] creates a
//! symlink inside a live prefix, so it is a caller's explicit act behind its
//! own confirmation — the same shape as `ensure_simconnect_cfg`, and for the
//! same reason.
//!
//! # What repair will never do
//!
//! **Replace a real directory.** If something has put actual files at that
//! path in the companion prefix, they are a companion app's saved state and
//! removing them to install a symlink would delete data to fix a convenience.
//! [`State::RealDirectory`] reports it, says how much is in there, and
//! [`repair`] refuses. The same goes for a symlink pointing somewhere else:
//! somebody aimed it deliberately, and re-aiming it is their call.
//!
//! Only [`State::Missing`] is repaired, and only when the sim's own directory
//! exists to point at — a link to a directory that is not there is a link that
//! answers "the sim is not installed", which is worse than no link at all
//! because it looks deliberate.
//!
//! # This does not make HangarRS required to fly
//!
//! The link outlives the process that made it, needs no cleanup, and is the
//! same one a person would create with `ln -s`. Delete HangarRS and every
//! companion app keeps finding the sim exactly as it does now.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Where a Windows application looks for the sim's data, inside a prefix.
///
/// `steamuser` is Proton's fixed user name, and the same literal is already
/// relied on across `hangar-msfs` and `hangar-install`.
pub const RELATIVE: &str =
    "drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024";

/// What is at the link's path in the companion prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// A symlink resolving to the sim prefix's own directory. Nothing to do.
    Linked,
    /// A symlink resolving somewhere else, with where. Never re-aimed:
    /// somebody pointed it there.
    Elsewhere { target: PathBuf },
    /// A symlink whose target does not exist. Reported rather than repaired,
    /// because replacing it would discard whatever it was aimed at.
    Dangling { target: PathBuf },
    /// A real directory with files in it. Never replaced.
    RealDirectory { entries: usize },
    /// Nothing there — the one repairable case.
    Missing,
    /// Could not be inspected, with why. Never read as either good or bad.
    Unreadable(String),
}

/// The link, the two ends of it, and what is actually there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Where the link goes, inside the companion prefix.
    pub link: PathBuf,
    /// What it should point at, inside the MSFS prefix.
    pub want: PathBuf,
    /// Whether that target exists. A link to a directory that is not there
    /// tells every companion app the sim is not installed.
    pub target_exists: bool,
    pub state: State,
}

impl Check {
    /// Companion apps will find the sim through this.
    pub fn is_good(&self) -> bool {
        self.state == State::Linked && self.target_exists
    }

    /// [`repair`] would do something, and would be allowed to.
    pub fn can_repair(&self) -> bool {
        self.state == State::Missing && self.target_exists
    }

    /// One line for a report or a UI, saying what is true and what follows.
    pub fn why(&self) -> String {
        match &self.state {
            State::Linked if self.target_exists => {
                "companion apps resolve the sim's AppData through this link".into()
            }
            // The sim's own AppData path is ~125 characters on a default
            // install, so naming it would make the line a paragraph (the
            // prefix tab's one-sentence rule). The folder is named instead.
            State::Linked => "the link is there but the sim's AppData folder is not — companion \
                              apps will read this as the sim not being installed"
                .into(),
            State::Elsewhere { target } => format!(
                "points at {} rather than the sim prefix — left alone, because somebody \
                 aimed it there",
                target.display()
            ),
            State::Dangling { target } => format!(
                "points at {}, which does not exist — left alone rather than replaced",
                target.display()
            ),
            State::RealDirectory { entries } => format!(
                "a real directory with {entries} entr{} in it, not a link — never replaced, \
                 because those are some application's saved state",
                if *entries == 1 { "y" } else { "ies" }
            ),
            State::Missing if self.target_exists => {
                "missing — no companion app in this prefix can find the sim".into()
            }
            State::Missing => "missing, and the sim's AppData folder does not exist either, \
                               so there is nothing to point at"
                .into(),
            State::Unreadable(why) => format!("could not be inspected: {why}"),
        }
    }
}

/// Look at the link. A pure read — safe to call every frame.
pub fn check(companion_prefix: &Path, msfs_prefix: &Path) -> Check {
    let link = companion_prefix.join(RELATIVE);
    let want = msfs_prefix.join(RELATIVE);
    let target_exists = want.is_dir();

    // `symlink_metadata` rather than `metadata`: the question is what is *at*
    // this path, and `metadata` follows the link and answers about the far
    // end. A dangling link would read as Missing and get replaced.
    let state = match fs::symlink_metadata(&link) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::Missing,
        Err(e) => State::Unreadable(e.to_string()),
        Ok(meta) if meta.file_type().is_symlink() => match fs::canonicalize(&link) {
            // Compared canonically at both ends: the recorded link target is
            // an absolute path through `/mnt/steam/…`, and the prefix handed
            // in may have reached the same directory by another route.
            Ok(resolved) => match fs::canonicalize(&want) {
                Ok(wanted) if resolved == wanted => State::Linked,
                Ok(_) => State::Elsewhere { target: resolved },
                Err(_) => State::Elsewhere { target: resolved },
            },
            Err(_) => State::Dangling {
                target: fs::read_link(&link).unwrap_or_else(|_| PathBuf::from("?")),
            },
        },
        Ok(meta) if meta.is_dir() => State::RealDirectory {
            entries: fs::read_dir(&link).map(|d| d.count()).unwrap_or(0),
        },
        Ok(_) => State::Unreadable("a file sits where the link should be".into()),
    };

    Check {
        link,
        want,
        target_exists,
        state,
    }
}

/// Why a repair was not performed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    #[error("{path} is already a link to the sim prefix")]
    AlreadyLinked { path: PathBuf },

    #[error(
        "{path} is a real directory with {entries} entries in it — removing it to \
         install a link would delete an application's saved state"
    )]
    RealDirectory { path: PathBuf, entries: usize },

    #[error("{path} already points at {target} — re-aiming somebody else's link is their call")]
    PointsElsewhere { path: PathBuf, target: PathBuf },

    #[error("{path} could not be inspected: {why}")]
    Unreadable { path: PathBuf, why: String },

    #[error("there is no {path} to point at — the sim's own AppData directory is missing")]
    NoTarget { path: PathBuf },
}

#[derive(Debug, Error)]
pub enum RepairError {
    #[error("refused: {0}")]
    Refused(#[from] Refusal),

    #[error("could not create {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Create the link, when and only when it is missing and has somewhere to
/// point. Returns the path it created.
///
/// Writes into a live prefix, so a caller reaches this from an explicit action
/// rather than from a draw.
pub fn repair(check: &Check) -> Result<PathBuf, RepairError> {
    match &check.state {
        State::Linked => {
            return Err(Refusal::AlreadyLinked {
                path: check.link.clone(),
            }
            .into());
        }
        State::RealDirectory { entries } => {
            return Err(Refusal::RealDirectory {
                path: check.link.clone(),
                entries: *entries,
            }
            .into());
        }
        State::Elsewhere { target } | State::Dangling { target } => {
            return Err(Refusal::PointsElsewhere {
                path: check.link.clone(),
                target: target.clone(),
            }
            .into());
        }
        State::Unreadable(why) => {
            return Err(Refusal::Unreadable {
                path: check.link.clone(),
                why: why.clone(),
            }
            .into());
        }
        State::Missing => {}
    }

    if !check.target_exists {
        return Err(Refusal::NoTarget {
            path: check.want.clone(),
        }
        .into());
    }

    if let Some(parent) = check.link.parent() {
        fs::create_dir_all(parent).map_err(|source| RepairError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::os::unix::fs::symlink(&check.want, &check.link).map_err(|source| RepairError::Io {
        path: check.link.clone(),
        source,
    })?;
    Ok(check.link.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-simlink-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Two prefixes, with the sim's own AppData directory present in the MSFS
    /// one.
    fn prefixes(s: &Scratch) -> (PathBuf, PathBuf) {
        let msfs = s.0.join("msfs/pfx");
        let companion = s.0.join("companion/pfx");
        fs::create_dir_all(msfs.join(RELATIVE)).expect("sim appdata");
        fs::create_dir_all(companion.join(RELATIVE).parent().unwrap()).expect("roaming");
        (msfs, companion)
    }

    #[test]
    fn a_missing_link_is_the_repairable_case() {
        let s = Scratch::new("missing");
        let (msfs, companion) = prefixes(&s);

        let found = check(&companion, &msfs);
        assert_eq!(found.state, State::Missing);
        assert!(found.target_exists);
        assert!(found.can_repair());
        assert!(!found.is_good());

        let made = repair(&found).expect("repaired");
        assert!(fs::symlink_metadata(&made)
            .unwrap()
            .file_type()
            .is_symlink());

        let after = check(&companion, &msfs);
        assert_eq!(after.state, State::Linked);
        assert!(after.is_good());
    }

    /// A correct link already in place. Repairing it is refused rather than
    /// silently redone, so a second click cannot churn the link.
    #[test]
    fn an_existing_link_is_recognised_and_left_alone() {
        let s = Scratch::new("linked");
        let (msfs, companion) = prefixes(&s);
        std::os::unix::fs::symlink(msfs.join(RELATIVE), companion.join(RELATIVE)).unwrap();

        let found = check(&companion, &msfs);
        assert!(found.is_good());
        assert!(!found.can_repair());
        assert!(matches!(
            repair(&found),
            Err(RepairError::Refused(Refusal::AlreadyLinked { .. }))
        ));
    }

    /// The one that would lose data. Never replaced, whatever else is true.
    #[test]
    fn a_real_directory_is_never_replaced() {
        let s = Scratch::new("realdir");
        let (msfs, companion) = prefixes(&s);
        let link = companion.join(RELATIVE);
        fs::create_dir_all(link.join("LocalCache")).unwrap();
        fs::write(link.join("UserCfg.opt"), "[Packages]\n").unwrap();

        let found = check(&companion, &msfs);
        assert_eq!(found.state, State::RealDirectory { entries: 2 });
        assert!(!found.can_repair());
        assert!(matches!(
            repair(&found),
            Err(RepairError::Refused(Refusal::RealDirectory { .. }))
        ));
        assert!(link.join("UserCfg.opt").is_file(), "nothing was removed");
    }

    /// Somebody aimed it deliberately. Re-aiming it is their call, not ours.
    #[test]
    fn a_link_pointing_elsewhere_is_reported_not_reaimed() {
        let s = Scratch::new("elsewhere");
        let (msfs, companion) = prefixes(&s);
        let other = s.0.join("somewhere-else");
        fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, companion.join(RELATIVE)).unwrap();

        let found = check(&companion, &msfs);
        assert!(matches!(found.state, State::Elsewhere { .. }));
        assert!(!found.is_good());
        assert!(matches!(
            repair(&found),
            Err(RepairError::Refused(Refusal::PointsElsewhere { .. }))
        ));
    }

    /// `metadata` follows the link and would report this path as absent, which
    /// would make it look repairable and discard whatever it was aimed at.
    /// `symlink_metadata` is what keeps that from happening.
    #[test]
    fn a_dangling_link_is_not_mistaken_for_a_missing_one() {
        let s = Scratch::new("dangling");
        let (msfs, companion) = prefixes(&s);
        std::os::unix::fs::symlink(s.0.join("never-existed"), companion.join(RELATIVE)).unwrap();

        let found = check(&companion, &msfs);
        assert!(
            matches!(found.state, State::Dangling { .. }),
            "got {:?}",
            found.state
        );
        assert!(!found.can_repair());
    }

    /// A link to a directory that is not there answers "the sim is not
    /// installed", which is worse than no link because it looks deliberate.
    #[test]
    fn nothing_is_linked_to_a_sim_directory_that_is_not_there() {
        let s = Scratch::new("notarget");
        let msfs = s.0.join("msfs/pfx");
        let companion = s.0.join("companion/pfx");
        fs::create_dir_all(companion.join(RELATIVE).parent().unwrap()).unwrap();

        let found = check(&companion, &msfs);
        assert_eq!(found.state, State::Missing);
        assert!(!found.target_exists);
        assert!(!found.can_repair());
        assert!(matches!(
            repair(&found),
            Err(RepairError::Refused(Refusal::NoTarget { .. }))
        ));
    }

    /// Both ends are canonicalised, so a prefix reached by a different route —
    /// a symlinked Steam library, say — still compares equal.
    #[test]
    fn the_comparison_survives_a_differently_routed_prefix() {
        let s = Scratch::new("routed");
        let (msfs, companion) = prefixes(&s);
        std::os::unix::fs::symlink(msfs.join(RELATIVE), companion.join(RELATIVE)).unwrap();

        let alias = s.0.join("msfs-by-another-name");
        std::os::unix::fs::symlink(s.0.join("msfs"), &alias).unwrap();

        assert!(
            check(&companion, &alias.join("pfx")).is_good(),
            "the same directory reached by another path must compare equal"
        );
    }

    /// The prefix tab shows `why` as one sentence (≤ 160 characters, the
    /// app's `no_prose_run_is_longer_than_a_sentence`). With the sim's AppData
    /// missing, the line used to embed that ~125-character path and reach 196:
    /// invisible on a machine with MSFS installed, failing on CI's runner.
    /// Pinned here with a default-install path so it no longer depends on the
    /// host.
    #[test]
    fn the_target_missing_lines_stay_one_sentence_whatever_the_path() {
        let pfx = PathBuf::from("/mnt/steam/SteamLibrary/steamapps/compatdata/2537590/pfx");
        for state in [State::Missing, State::Linked] {
            let found = Check {
                link: pfx.join("companion").join(RELATIVE),
                want: pfx.join(RELATIVE),
                target_exists: false,
                state: state.clone(),
            };
            let why = found.why();
            assert!(why.chars().count() <= 160, "{state:?}: {why}");
            assert!(why.contains("the sim's AppData folder"), "{state:?}: {why}");
        }
    }
}
