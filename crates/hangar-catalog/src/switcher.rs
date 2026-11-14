//! Enabling and disabling packages by moving them.
//!
//! # Why moves rather than symlinks
//!
//! Installed packages are usually real directories inside the Community
//! folder, and whether MSFS 2024 under Proton follows symlinks correctly has
//! never been verified.
//!
//! Moving sidesteps the question. A rename within one filesystem is atomic and instant
//! whatever the directory weighs, and the 4.2 GB Bonanza moves as fast as a
//! 7 KB airport. Nothing is ever copied, no symlink is ever created, and the
//! existing layout is left exactly as it is.
//!
//! # Where disabled packages go
//!
//! ```text
//! Packages/
//!   Community2024/                  ← the sim reads this
//!   .hangar-disabled/
//!     Community2024/                ← and ignores this
//! ```
//!
//! Hidden, obviously ours, a sibling of the Community folder so the rename
//! stays on one filesystem, and deliberately **not** named after any folder MSFS
//! looks for. A name beginning `Community…` risks being picked up by anything
//! that prefix-matches, and `Community2024Backup` already exists beside it.
//!
//! # Safety
//!
//! Every rule here exists because the target is a live Community folder holding
//! many gigabytes of content that took a long time to install:
//!
//! * Never overwrite. A destination that already exists is an error, never a
//!   silent replacement.
//! * Never cross a filesystem. `EXDEV` is reported, not quietly turned into a
//!   multi-gigabyte copy.
//! * Never touch what is not a package, unless explicitly told to.
//!
//! Not a safety rule, but worth knowing: switching while MSFS is running is
//! harmless and ineffective. The sim reads the Community folder once at startup,
//! so a package moved out mid-session stays loaded until the sim restarts.
//! The Profiles tab says so while the sim is up, and still lets the switch happen.

use crate::item::ItemState;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Directory name holding disabled packages, one subdirectory per Community
/// folder.
pub const DISABLED_DIR: &str = ".hangar-disabled";

#[derive(Debug, Error)]
pub enum SwitchError {
    #[error("{name}: nothing at {path}")]
    Missing { name: String, path: PathBuf },

    #[error("{name}: {path} already exists — refusing to overwrite it")]
    DestinationExists { name: String, path: PathBuf },

    #[error(
        "{name}: the disabled store is on a different filesystem from the Community folder, \
         so this would be a copy rather than a move"
    )]
    CrossDevice { name: String },

    #[error("{name}: {path} is not a package (no manifest.json)")]
    NotAPackage { name: String, path: PathBuf },

    #[error("{name}: {source}")]
    Io {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

/// Where a Community folder's disabled packages live.
#[derive(Debug, Clone)]
pub struct Switcher {
    community: PathBuf,
    disabled_root: PathBuf,
    /// Move directories that carry no `manifest.json`.
    ///
    /// Off by default: the Community folder can hold things that are not
    /// packages, and moving one of those is a good way to lose something.
    allow_non_packages: bool,
}

impl Switcher {
    /// Build a switcher for one Community folder.
    ///
    /// The disabled store is a sibling of the Community folder, which is what
    /// keeps every rename on one filesystem.
    pub fn new(community: impl Into<PathBuf>) -> Self {
        let community = community.into();
        let name = community
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("Community"));
        let disabled_root = community
            .parent()
            .unwrap_or(Path::new("."))
            .join(DISABLED_DIR)
            .join(name);
        Self {
            community,
            disabled_root,
            allow_non_packages: false,
        }
    }

    /// Permit moving directories without a `manifest.json`.
    pub fn allowing_non_packages(mut self) -> Self {
        self.allow_non_packages = true;
        self
    }

    pub fn community(&self) -> &Path {
        &self.community
    }

    pub fn disabled_root(&self) -> &Path {
        &self.disabled_root
    }

    /// Names currently sitting in the disabled store.
    pub fn disabled_names(&self) -> BTreeSet<String> {
        read_names(&self.disabled_root)
    }

    /// Names currently in the Community folder.
    pub fn enabled_names(&self) -> BTreeSet<String> {
        read_names(&self.community)
    }

    /// Move a package out of the Community folder.
    pub fn disable(&self, name: &str) -> Result<(), SwitchError> {
        let from = self.community.join(name);
        let to = self.disabled_root.join(name);
        self.check_source(name, &from)?;
        std::fs::create_dir_all(&self.disabled_root).map_err(|source| SwitchError::Io {
            name: name.to_owned(),
            source,
        })?;
        self.move_dir(name, &from, &to)
    }

    /// Move a package back into the Community folder.
    pub fn enable(&self, name: &str) -> Result<(), SwitchError> {
        let from = self.disabled_root.join(name);
        let to = self.community.join(name);
        self.check_source(name, &from)?;
        self.move_dir(name, &from, &to)
    }

    /// The source must exist, and be a package unless that check is waived.
    ///
    /// `symlink_metadata` rather than `exists`, so a symlink someone else
    /// created is still movable — renaming a symlink moves the link itself,
    /// which is exactly the right behaviour.
    fn check_source(&self, name: &str, from: &Path) -> Result<(), SwitchError> {
        if from.symlink_metadata().is_err() {
            return Err(SwitchError::Missing {
                name: name.to_owned(),
                path: from.to_path_buf(),
            });
        }
        let is_symlink = from
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if !self.allow_non_packages && !is_symlink && !from.join("manifest.json").is_file() {
            return Err(SwitchError::NotAPackage {
                name: name.to_owned(),
                path: from.to_path_buf(),
            });
        }
        Ok(())
    }

    fn move_dir(&self, name: &str, from: &Path, to: &Path) -> Result<(), SwitchError> {
        if to.symlink_metadata().is_ok() {
            return Err(SwitchError::DestinationExists {
                name: name.to_owned(),
                path: to.to_path_buf(),
            });
        }
        std::fs::rename(from, to).map_err(|source| {
            // EXDEV means the disabled store landed on another filesystem.
            // Falling back to a copy would turn an "instant" switch into
            // minutes of I/O on a 4 GB package, so it is reported instead.
            if source.raw_os_error() == Some(18) {
                SwitchError::CrossDevice {
                    name: name.to_owned(),
                }
            } else {
                SwitchError::Io {
                    name: name.to_owned(),
                    source,
                }
            }
        })
    }
}

/// What a switch did, and what it could not do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SwitchReport {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl SwitchReport {
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }

    pub fn touched(&self) -> usize {
        self.enabled.len() + self.disabled.len()
    }

    pub fn summary(&self) -> String {
        let base = format!("+{} \u{2212}{}", self.enabled.len(), self.disabled.len());
        if self.failed.is_empty() {
            base
        } else {
            format!("{base}, {} failed", self.failed.len())
        }
    }
}

/// Switch from one set of enabled packages to another, touching only the delta.
///
/// **Never wipe and regenerate.** The delta is `current XOR target`, and
/// everything in both sets is left completely alone — which is what keeps the
/// Community folder from ever being momentarily empty, and what makes switching
/// between two similar profiles nearly free.
///
/// Disables run before enables so a name being freed cannot collide with a name
/// being taken.
pub fn apply_diff(
    switcher: &Switcher,
    current: &BTreeSet<String>,
    target: &BTreeSet<String>,
) -> SwitchReport {
    let mut report = SwitchReport::default();

    for name in current.difference(target) {
        match switcher.disable(name) {
            Ok(()) => report.disabled.push(name.clone()),
            Err(e) => report.failed.push((name.clone(), e.to_string())),
        }
    }
    for name in target.difference(current) {
        match switcher.enable(name) {
            Ok(()) => report.enabled.push(name.clone()),
            Err(e) => report.failed.push((name.clone(), e.to_string())),
        }
    }

    report
}

/// The delta a switch would apply, without applying it.
///
/// Shown before a write so the count is never a surprise.
pub fn plan_diff(
    current: &BTreeSet<String>,
    target: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    (
        target.difference(current).cloned().collect(),
        current.difference(target).cloned().collect(),
    )
}

/// Whether an item can be switched by this engine.
pub fn is_switchable(state: &ItemState) -> bool {
    state.is_switchable() || *state == ItemState::Resident
}

/// Directory names directly inside `dir`, ignoring the ones we manage.
fn read_names(dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.path()
                .symlink_metadata()
                .map(|m| m.is_dir() || m.file_type().is_symlink())
                .unwrap_or(false)
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != DISABLED_DIR)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-switch-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A Packages tree shaped like the real one.
    fn packages_tree(names: &[&str]) -> (PathBuf, Switcher) {
        let root = scratch();
        let community = root.join("Community2024");
        std::fs::create_dir_all(&community).unwrap();
        for name in names {
            let pkg = community.join(name);
            std::fs::create_dir_all(&pkg).unwrap();
            std::fs::write(pkg.join("manifest.json"), r#"{"title":"x"}"#).unwrap();
            std::fs::write(pkg.join("payload.bin"), vec![0u8; 64]).unwrap();
        }
        let switcher = Switcher::new(&community);
        (root, switcher)
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_disabled_store_is_a_hidden_sibling_of_community() {
        let (root, s) = packages_tree(&[]);
        assert_eq!(
            s.disabled_root(),
            root.join(".hangar-disabled").join("Community2024")
        );
        // Same parent, so the rename never crosses a filesystem.
        assert_eq!(
            s.disabled_root().parent().unwrap().parent(),
            s.community().parent()
        );
        // And not a name MSFS looks for.
        assert!(!DISABLED_DIR.starts_with("Community"));
    }

    #[test]
    fn disabling_moves_the_package_out_and_enabling_moves_it_back() {
        let (_root, s) = packages_tree(&["koak", "bfg-aca-scout"]);

        s.disable("koak").unwrap();
        assert!(!s.community().join("koak").exists());
        assert!(s.disabled_root().join("koak/manifest.json").is_file());
        assert!(
            s.disabled_root().join("koak/payload.bin").is_file(),
            "contents move with the directory"
        );
        assert_eq!(s.enabled_names(), set(&["bfg-aca-scout"]));
        assert_eq!(s.disabled_names(), set(&["koak"]));

        s.enable("koak").unwrap();
        assert!(s.community().join("koak/manifest.json").is_file());
        assert!(s.disabled_names().is_empty());
        assert_eq!(s.enabled_names(), set(&["bfg-aca-scout", "koak"]));
    }

    /// The rule that protects installed content.
    #[test]
    fn a_switch_never_overwrites_an_existing_destination() {
        let (_root, s) = packages_tree(&["koak"]);
        // A stale copy is already sitting in the disabled store.
        std::fs::create_dir_all(s.disabled_root().join("koak")).unwrap();
        std::fs::write(s.disabled_root().join("koak/important.txt"), b"keep me").unwrap();

        let err = s.disable("koak").unwrap_err();
        assert!(
            matches!(err, SwitchError::DestinationExists { .. }),
            "{err}"
        );
        assert!(
            s.community().join("koak/manifest.json").is_file(),
            "the original must be left where it was"
        );
        assert_eq!(
            std::fs::read(s.disabled_root().join("koak/important.txt")).unwrap(),
            b"keep me",
            "and the existing copy untouched"
        );
    }

    /// The Community folder can hold things that are not packages.
    #[test]
    fn non_packages_are_refused_unless_explicitly_allowed() {
        let (_root, s) = packages_tree(&[]);
        std::fs::create_dir_all(s.community().join("some-notes")).unwrap();

        let err = s.disable("some-notes").unwrap_err();
        assert!(matches!(err, SwitchError::NotAPackage { .. }), "{err}");
        assert!(s.community().join("some-notes").is_dir());

        let permissive = Switcher::new(s.community()).allowing_non_packages();
        permissive.disable("some-notes").unwrap();
        assert!(!s.community().join("some-notes").exists());
    }

    #[test]
    fn switching_something_absent_is_an_error_not_a_silent_success() {
        let (_root, s) = packages_tree(&[]);
        assert!(matches!(
            s.disable("never-installed").unwrap_err(),
            SwitchError::Missing { .. }
        ));
        assert!(matches!(
            s.enable("never-installed").unwrap_err(),
            SwitchError::Missing { .. }
        ));
    }

    /// A symlink someone else made must move as a link, not be followed.
    #[test]
    fn a_symlink_moves_as_a_link() {
        let (root, s) = packages_tree(&[]);
        let real = root.join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("manifest.json"), r#"{"title":"x"}"#).unwrap();
        std::os::unix::fs::symlink(&real, s.community().join("linked-pkg")).unwrap();

        s.disable("linked-pkg").unwrap();
        let moved = s.disabled_root().join("linked-pkg");
        assert!(moved.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(
            real.join("manifest.json").is_file(),
            "the target is untouched"
        );
    }

    /// **The diff primitive.** Only the delta moves; anything in both sets is
    /// never touched, so the Community folder is never momentarily empty.
    #[test]
    fn apply_diff_touches_only_the_delta() {
        let (_root, s) = packages_tree(&["keep-a", "keep-b", "drop-me"]);
        s.disable("was-off").ok(); // not present; ignored
        let stash = s.disabled_root().join("add-me");
        std::fs::create_dir_all(&stash).unwrap();
        std::fs::write(stash.join("manifest.json"), r#"{"title":"x"}"#).unwrap();

        let keep_a_before = std::fs::metadata(s.community().join("keep-a")).unwrap();

        let current = set(&["keep-a", "keep-b", "drop-me"]);
        let target = set(&["keep-a", "keep-b", "add-me"]);
        let report = apply_diff(&s, &current, &target);

        assert!(report.is_clean(), "{:?}", report.failed);
        assert_eq!(report.disabled, ["drop-me"]);
        assert_eq!(report.enabled, ["add-me"]);
        assert_eq!(report.touched(), 2, "only the delta");

        assert_eq!(s.enabled_names(), set(&["keep-a", "keep-b", "add-me"]));
        assert_eq!(s.disabled_names(), set(&["drop-me"]));

        // Untouched means untouched: same inode, not moved out and back.
        let keep_a_after = std::fs::metadata(s.community().join("keep-a")).unwrap();
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&keep_a_before),
            std::os::unix::fs::MetadataExt::ino(&keep_a_after)
        );
    }

    /// Switching to the same set must do nothing at all.
    #[test]
    fn switching_to_the_same_profile_is_a_no_op() {
        let (_root, s) = packages_tree(&["a", "b"]);
        let same = set(&["a", "b"]);
        let report = apply_diff(&s, &same, &same);
        assert_eq!(report, SwitchReport::default());
        assert_eq!(report.touched(), 0);
        assert_eq!(s.enabled_names(), same);
    }

    /// One failure must not abandon the rest of the switch.
    #[test]
    fn a_failure_is_reported_without_stopping_the_others() {
        let (_root, s) = packages_tree(&["good-one", "not-a-pkg-marker"]);
        // Make one entry unswitchable by removing its manifest.
        std::fs::remove_file(s.community().join("not-a-pkg-marker/manifest.json")).unwrap();

        let current = set(&["good-one", "not-a-pkg-marker"]);
        let target = BTreeSet::new();
        let report = apply_diff(&s, &current, &target);

        assert_eq!(report.disabled, ["good-one"]);
        assert_eq!(report.failed.len(), 1);
        assert!(!report.is_clean());
        assert!(report.summary().contains("failed"));
    }

    #[test]
    fn a_plan_describes_the_switch_without_performing_it() {
        let (_root, s) = packages_tree(&["a", "b"]);
        let (on, off) = plan_diff(&set(&["a", "b"]), &set(&["b", "c"]));
        assert_eq!(on, ["c"]);
        assert_eq!(off, ["a"]);
        assert_eq!(s.enabled_names(), set(&["a", "b"]), "nothing moved");
    }

    /// The disabled store must never be mistaken for a package.
    #[test]
    fn the_disabled_store_is_not_listed_as_enabled() {
        let (_root, s) = packages_tree(&["a"]);
        s.disable("a").unwrap();
        std::fs::create_dir_all(s.community().parent().unwrap().join(DISABLED_DIR)).unwrap();
        assert!(!s.enabled_names().contains(DISABLED_DIR));
    }

    /// Resident packages are exactly what this engine exists to switch.
    #[test]
    fn resident_items_are_switchable_by_this_engine() {
        assert!(is_switchable(&ItemState::Resident));
        assert!(is_switchable(&ItemState::Available));
        assert!(is_switchable(&ItemState::Linked {
            target: PathBuf::from("/x")
        }));
        assert!(!is_switchable(&ItemState::SimEnabled));
    }
}
