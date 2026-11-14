//! The item taxonomy — what the library holds, and how each kind behaves.
//!
//! Calling everything an "App" is the mistake this avoids: a one-shot `.exe`
//! would get PID tracking and teardown it does not need and miss the completion
//! verification it does, and in-sim `EXE.xml` add-ons would have no model.

use hangar_msfs::ContentType;
use std::path::PathBuf;

/// What kind of thing this is, and therefore how it is managed.
///
/// Five library kinds. An **installer is deliberately not a library item**:
/// it runs once, produces an `InstallRecord`, and disappears. Modelling it here
/// would put one-shot installers into an app runner whose job is to launch and
/// track long-lived processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKind {
    /// A Community folder with a `manifest.json`. Switched by profiles.
    Package,
    /// Registers a `<Launch.Addon>` in `EXE.xml`.
    ///
    /// Managed by its `<Disabled>` flag and **never** by an app runner: MSFS
    /// spawns these itself, so launching one separately starts a conflicting
    /// second copy.
    SimAddonExe,
    /// A Windows `.exe` with a lifecycle inside a prefix.
    CompanionApp,
    /// Installed independently, launched inside the running simulator session.
    SimSessionApp,
    /// A native Linux binary — LittleNavMap, HeadtrackRS, `efbconnect`, the ATC
    /// agent.
    NativeTool,
}

impl ItemKind {
    /// Every kind, in the order the Hangar tab offers them.
    pub const ALL: [ItemKind; 5] = [
        ItemKind::Package,
        ItemKind::SimAddonExe,
        ItemKind::CompanionApp,
        ItemKind::SimSessionApp,
        ItemKind::NativeTool,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Package => "Package",
            ItemKind::SimAddonExe => "In-sim add-on",
            ItemKind::CompanionApp => "Companion app",
            ItemKind::SimSessionApp => "Sim session app",
            ItemKind::NativeTool => "Native tool",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            ItemKind::Package => "Packages",
            ItemKind::SimAddonExe => "In-sim add-ons",
            ItemKind::CompanionApp => "Companion apps",
            ItemKind::SimSessionApp => "Sim session apps",
            ItemKind::NativeTool => "Native tools",
        }
    }
}

/// Where an item stands, and whether a profile can do anything about it.
///
/// The distinction between [`ItemState::Linked`] and [`ItemState::Resident`] is
/// the one that matters, and it is not cosmetic. Packages are usually
/// Resident — real directories inside the Community folder, not symlinks. A
/// profile cannot switch those off without physically moving them, so
/// reporting them as "enabled" alongside symlinked packages would promise a
/// switch that does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState {
    /// A symlink in the Community folder pointing at a library. Switchable.
    Linked { target: PathBuf },
    /// A real directory **inside** the Community folder.
    ///
    /// The sim loads it, and it stays loaded: nothing short of moving the
    /// directory turns it off. Installed by the marketplace, by a vendor
    /// installer, or by hand.
    Resident,
    /// In a package library, not currently enabled.
    Available,
    /// A symlink whose target no longer exists. The sim will not load it.
    Dangling { target: PathBuf },
    /// Registered in `EXE.xml` and enabled — the sim will start it.
    SimEnabled,
    /// Registered in `EXE.xml` with `<Disabled>True</Disabled>`.
    SimDisabled,
    /// A configured app whose executable is present.
    ///
    /// Not "running" — that is `hangar-runtime`'s answer. This says only that
    /// it could be launched.
    Ready,
    /// A configured app whose executable is no longer there.
    MissingExecutable,
}

impl ItemState {
    /// Whether the sim will actually load this.
    pub fn is_active(&self) -> bool {
        match self {
            ItemState::Linked { .. } | ItemState::Resident | ItemState::SimEnabled => true,
            // An app being launchable is not the same as it running; only
            // `hangar-runtime`'s process watcher can tell the difference.
            ItemState::Available
            | ItemState::Dangling { .. }
            | ItemState::SimDisabled
            | ItemState::Ready
            | ItemState::MissingExecutable => false,
        }
    }

    /// Whether a profile switch can currently change this item's state.
    ///
    /// `false` for [`ItemState::Resident`] — that is the honest answer, and the
    /// reason the Hangar tab says so rather than offering a toggle that would
    /// silently do nothing.
    pub fn is_switchable(&self) -> bool {
        matches!(
            self,
            ItemState::Linked { .. } | ItemState::Available | ItemState::Dangling { .. }
        )
    }

    /// Whether something is wrong with it.
    pub fn is_broken(&self) -> bool {
        matches!(
            self,
            ItemState::Dangling { .. } | ItemState::MissingExecutable
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            ItemState::Linked { .. } => "Enabled",
            ItemState::Resident => "Installed",
            ItemState::Available => "Available",
            ItemState::Dangling { .. } => "Broken link",
            ItemState::SimEnabled => "Enabled",
            ItemState::SimDisabled => "Disabled",
            ItemState::Ready => "Ready",
            ItemState::MissingExecutable => "Missing",
        }
    }
}

/// One thing in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable identity: the package directory name, or the `EXE.xml` add-on
    /// name. Also the directory name a switch moves, so it is the profile key.
    pub name: String,
    pub kind: ItemKind,
    pub state: ItemState,
    /// Human title from the manifest, falling back to [`Item::name`].
    pub title: String,
    pub creator: String,
    pub version: String,
    /// Packages only.
    pub content_type: Option<ContentType>,
    /// What it occupies on disk, walked at scan time.
    ///
    /// **Measured, not declared.** `manifest.json`'s `total_package_size` is in
    /// only some manifests and reports the download rather than what landed.
    /// `None` means the directory could not be read, which is a real problem
    /// rather than a quiet omission.
    pub size_bytes: Option<u64>,
    /// Where the content actually lives.
    pub path: PathBuf,
    /// Which Community folder this belongs to, when it is in one.
    pub community: Option<PathBuf>,
    /// Arguments to launch with. Only meaningful for the launchable kinds.
    ///
    /// These apply **only to launches HangarRS makes**. An app that updates and
    /// relaunches itself chooses its own arguments — Project Airports came back
    /// with `--updated` and none of the configured `--no-sandbox` — so an
    /// adopted process may be running with a different set.
    pub args: Vec<String>,
}

impl Item {
    /// Title if the manifest gave one, otherwise the directory name.
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.name
        } else {
            self.title.trim()
        }
    }

    /// Whether this item matches a case-insensitive search across the fields a
    /// person would actually type.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let hay = [
            self.name.as_str(),
            self.title.as_str(),
            self.creator.as_str(),
            self.content_type.as_ref().map_or("", |c| c.label()),
        ];
        hay.iter().any(|h| h.to_lowercase().contains(&needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, state: ItemState) -> Item {
        Item {
            name: name.into(),
            kind: ItemKind::Package,
            state,
            title: String::new(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: PathBuf::from("/x").join(name),
            community: None,
            args: Vec::new(),
        }
    }

    /// Resident packages are loaded, and a profile cannot switch them.
    #[test]
    fn resident_packages_are_active_but_not_switchable() {
        let resident = item("a2a-aircraft-pa24", ItemState::Resident);
        assert!(resident.state.is_active(), "the sim does load it");
        assert!(
            !resident.state.is_switchable(),
            "but a profile cannot turn it off without moving the directory"
        );
    }

    #[test]
    fn linked_packages_are_active_and_switchable() {
        let linked = item(
            "pkg",
            ItemState::Linked {
                target: "/library/pkg".into(),
            },
        );
        assert!(linked.state.is_active());
        assert!(linked.state.is_switchable());
    }

    #[test]
    fn available_packages_are_switchable_but_not_active() {
        let available = item("pkg", ItemState::Available);
        assert!(!available.state.is_active());
        assert!(available.state.is_switchable());
    }

    /// A dangling link is not loaded by the sim, and is worth flagging rather
    /// than quietly reporting as off.
    #[test]
    fn a_dangling_link_is_inactive_and_broken() {
        let dangling = item(
            "pkg",
            ItemState::Dangling {
                target: "/gone".into(),
            },
        );
        assert!(!dangling.state.is_active());
        assert!(dangling.state.is_broken());
        assert!(
            dangling.state.is_switchable(),
            "removing it is still possible"
        );
    }

    #[test]
    fn exe_xml_states_track_the_disabled_flag() {
        assert!(ItemState::SimEnabled.is_active());
        assert!(!ItemState::SimDisabled.is_active());
        // Neither is switchable by a *profile*; they toggle in EXE.xml.
        assert!(!ItemState::SimEnabled.is_switchable());
        assert!(!ItemState::SimDisabled.is_switchable());
    }

    #[test]
    fn a_missing_title_falls_back_to_the_directory_name() {
        let mut i = item("bfg-aca-scout", ItemState::Resident);
        assert_eq!(i.display_title(), "bfg-aca-scout");
        i.title = "  ACA Scout  ".into();
        assert_eq!(i.display_title(), "ACA Scout");
    }

    #[test]
    fn search_covers_name_title_creator_and_type() {
        let mut i = item("koak", ItemState::Resident);
        i.title = "Oakland Intl".into();
        i.creator = "bullfrogsim".into();
        i.content_type = Some(ContentType::Scenery);

        assert!(i.matches(""), "an empty search matches everything");
        assert!(i.matches("koak"));
        assert!(i.matches("OAKLAND"));
        assert!(i.matches("bullfrog"));
        assert!(i.matches("scenery"));
        assert!(!i.matches("aircraft"));
    }

    /// Installer is deliberately absent — it is not a library item.
    #[test]
    fn the_taxonomy_has_five_library_kinds() {
        assert_eq!(ItemKind::ALL.len(), 5);
        assert!(!ItemKind::ALL.iter().any(|k| k.label().contains("Install")));
    }
}
