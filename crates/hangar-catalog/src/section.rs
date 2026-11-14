//! How the library is divided for browsing.
//!
//! Sections are not the same thing as [`ItemKind`]. A kind says how something is
//! *managed*; a section says where a person expects to *find* it. Nobody looks
//! for a Cessna under "Package" — they look under Aircraft. And an app you
//! launch belongs with other apps you launch, whether it is a Windows binary in
//! a prefix, a native Linux tool, or something the sim starts from `EXE.xml`.

use crate::item::{Item, ItemKind};
use hangar_msfs::ContentType;

/// A browsable division of the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    /// Things you launch, or that get launched for you.
    ///
    /// Companion apps, native tools, and in-sim `EXE.xml` add-ons together.
    /// They have different lifecycles — which is why [`ItemKind`] keeps them
    /// apart — but they are all "programs", and that is how they are looked for.
    Apps,
    Aircraft,
    Scenery,
    Instruments,
    /// Liveries, sound sets, missions and anything unrecognised.
    ///
    /// A catch-all rather than five sections of one item each: on a real
    /// library these categories have a handful of members between them.
    Other,
}

impl Section {
    /// In nav order.
    pub const ALL: [Section; 5] = [
        Section::Apps,
        Section::Aircraft,
        Section::Scenery,
        Section::Instruments,
        Section::Other,
    ];

    /// Which section an item belongs in.
    pub fn of(item: &Item) -> Section {
        match item.kind {
            ItemKind::CompanionApp
            | ItemKind::SimSessionApp
            | ItemKind::NativeTool
            | ItemKind::SimAddonExe => Section::Apps,
            ItemKind::Package => match &item.content_type {
                Some(ContentType::Aircraft) => Section::Aircraft,
                Some(ContentType::Scenery) => Section::Scenery,
                Some(ContentType::Instruments) => Section::Instruments,
                _ => Section::Other,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Apps => "Apps",
            Section::Aircraft => "Aircraft",
            Section::Scenery => "Scenery",
            Section::Instruments => "Instruments",
            Section::Other => "Other",
        }
    }

    /// What lands here, for the empty state.
    pub fn description(self) -> &'static str {
        match self {
            Section::Apps => "Companion apps, native tools, and add-ons the sim launches itself.",
            Section::Aircraft => "Aircraft packages.",
            Section::Scenery => "Airports, scenery and object libraries.",
            Section::Instruments => "Avionics and instrument packages.",
            Section::Other => "Liveries, sound sets, missions and anything uncategorised.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemState;
    use std::path::PathBuf;

    fn item(kind: ItemKind, content_type: Option<ContentType>) -> Item {
        Item {
            name: "x".into(),
            kind,
            state: ItemState::Resident,
            title: String::new(),
            creator: String::new(),
            version: String::new(),
            content_type,
            size_bytes: None,
            path: PathBuf::new(),
            community: None,
            args: Vec::new(),
        }
    }

    /// The three kinds with different lifecycles still share one section:
    /// they are all programs, and that is how a person looks for them.
    #[test]
    fn everything_launchable_lands_in_apps() {
        for kind in [
            ItemKind::CompanionApp,
            ItemKind::SimSessionApp,
            ItemKind::NativeTool,
            ItemKind::SimAddonExe,
        ] {
            assert_eq!(Section::of(&item(kind, None)), Section::Apps, "{kind:?}");
        }
    }

    #[test]
    fn packages_split_by_content_type() {
        let cases = [
            (ContentType::Aircraft, Section::Aircraft),
            (ContentType::Scenery, Section::Scenery),
            (ContentType::Instruments, Section::Instruments),
            (ContentType::Livery, Section::Other),
            (ContentType::SoundSet, Section::Other),
            (ContentType::Misc, Section::Other),
            (ContentType::Mission, Section::Other),
        ];
        for (content_type, expected) in cases {
            let got = Section::of(&item(ItemKind::Package, Some(content_type.clone())));
            assert_eq!(got, expected, "for {content_type:?}");
        }
    }

    /// An unrecognised content type must land somewhere rather than vanish.
    #[test]
    fn an_unknown_content_type_falls_into_other() {
        let unknown = ContentType::Other("Flight Model Tweak".into());
        assert_eq!(
            Section::of(&item(ItemKind::Package, Some(unknown))),
            Section::Other
        );
        assert_eq!(
            Section::of(&item(ItemKind::Package, None)),
            Section::Other,
            "a package with no declared type is still findable"
        );
    }

    /// Every item must land in exactly one section, or the section counts will
    /// not add up to the library.
    #[test]
    fn every_item_lands_in_exactly_one_section() {
        let all = [
            item(ItemKind::Package, Some(ContentType::Aircraft)),
            item(ItemKind::Package, Some(ContentType::Scenery)),
            item(ItemKind::Package, Some(ContentType::Livery)),
            item(ItemKind::CompanionApp, None),
            item(ItemKind::SimAddonExe, None),
        ];
        for i in &all {
            let section = Section::of(i);
            let matches = Section::ALL.iter().filter(|s| **s == section).count();
            assert_eq!(matches, 1);
        }
    }
}
