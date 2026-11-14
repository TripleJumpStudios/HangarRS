//! The apps HangarRS was told about, rather than the ones it found.
//!
//! Everything else in the library comes from a filesystem scan. These do not:
//! a companion app is a `.exe` somewhere in a Wine prefix with nothing to mark
//! it out from the several hundred others in there, so which ones are *yours*
//! is a fact only a config file holds.
//!
//! # Which file answers
//!
//! HangarRS' own config, once there is one. Before that — on a machine that has
//! run `MSFS_Linux_Manager` and not yet migrated — the previous application's,
//! read-only, so the Apps section is not empty of the things it exists to show.
//!
//! The two failure modes are deliberately not the same:
//!
//! * **No config at all** means HangarRS has never been configured, so falling
//!   back to the previous application's list is the helpful thing.
//! * **A config that will not parse** means it *has* been, and falling back
//!   there would quietly show apps from a file the person migrated away from.
//!   Their edit would appear to have had no effect whatsoever, which is a much
//!   worse half-hour than an empty list and a warning naming the line.

use crate::item::{Item, ItemKind, ItemState};
use hangar_core::config::{AppEntry, AppKind, Config, Load};

/// Every enabled app from whichever config answers.
pub fn configured_apps() -> Vec<Item> {
    match Config::load() {
        Load::Present(config) => config.enabled_apps().map(item).collect(),
        Load::Missing => crate::legacy::legacy_apps(),
        Load::Malformed { path, reason } => {
            tracing::warn!(
                path = %path.display(),
                "no apps: the config could not be read ({reason})"
            );
            Vec::new()
        }
    }
}

/// One configured app as a library item.
pub fn item(entry: &AppEntry) -> Item {
    Item {
        // The executable's presence is the honest thing to report. Whether it
        // is *running* is `hangar-runtime`'s answer, not the catalog's.
        state: if entry.exe.is_file() {
            ItemState::Ready
        } else {
            ItemState::MissingExecutable
        },
        name: entry.name.clone(),
        title: entry.name.clone(),
        kind: match entry.kind {
            AppKind::Companion => ItemKind::CompanionApp,
            AppKind::SimSession => ItemKind::SimSessionApp,
            AppKind::Native => ItemKind::NativeTool,
        },
        creator: String::new(),
        version: String::new(),
        content_type: None,
        size_bytes: None,
        path: entry.exe.clone(),
        community: None,
        args: entry.args.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, kind: AppKind) -> AppEntry {
        AppEntry {
            name: name.into(),
            exe: PathBuf::from("/nonexistent").join(name),
            args: vec!["--no-sandbox".into()],
            kind,
            uninstaller: None,
            enabled: true,
        }
    }

    /// The correction the item model exists to make: a native tool is not a
    /// Wine app, and the two are launched differently.
    #[test]
    fn the_kind_decides_how_it_will_be_run() {
        assert_eq!(
            item(&entry("BeyondATC", AppKind::SimSession)).kind,
            ItemKind::SimSessionApp
        );
        assert_eq!(
            item(&entry("Project Airports", AppKind::Companion)).kind,
            ItemKind::CompanionApp
        );
        assert_eq!(
            item(&entry("LittleNavMap", AppKind::Native)).kind,
            ItemKind::NativeTool
        );
    }

    /// Losing these would change how an app runs, with nothing to show for it.
    #[test]
    fn launch_arguments_come_across() {
        assert_eq!(item(&entry("X", AppKind::Companion)).args, ["--no-sandbox"]);
    }

    /// Flagged, not hidden: an app that quietly disappears from the list is
    /// harder to diagnose than one marked broken.
    #[test]
    fn a_missing_executable_is_reported_rather_than_dropped() {
        let item = item(&entry("Gone", AppKind::Companion));
        assert_eq!(item.state, ItemState::MissingExecutable);
        assert!(item.state.is_broken());
    }

    #[test]
    fn a_present_executable_reads_as_ready() {
        let dir = std::env::temp_dir().join(format!("hangar-apps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("present.exe");
        std::fs::write(&exe, b"MZ").unwrap();

        let mut e = entry("Present", AppKind::Companion);
        e.exe = exe;
        assert_eq!(item(&e).state, ItemState::Ready);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
