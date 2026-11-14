//! An immutable view of the library, published wholesale.
//!
//! **Snapshot, don't clone per frame**: rebuilding an index
//! every repaint from a full clone is the cost this avoids. A scan produces one
//! `CatalogSnapshot`, the actor publishes it behind an `Arc`, and the GUI swaps
//! a pointer.

use crate::item::{Item, ItemKind, ItemState};
use crate::scanner::{Problem, ScanResult};
use crate::section::Section;
use std::collections::BTreeMap;
use std::time::SystemTime;

/// Everything the library holds, as of one scan.
#[derive(Debug, Default)]
pub struct CatalogSnapshot {
    items: Vec<Item>,
    problems: Vec<Problem>,
    scanned_at: Option<SystemTime>,
}

impl CatalogSnapshot {
    pub fn from_scan(scan: ScanResult) -> Self {
        let mut items = scan.items;
        // Sorted once, here, so no consumer has to and every view agrees.
        items.sort_by(|a, b| {
            a.kind.cmp(&b.kind).then_with(|| {
                a.display_title()
                    .to_lowercase()
                    .cmp(&b.display_title().to_lowercase())
            })
        });
        Self {
            items,
            problems: scan.problems,
            scanned_at: Some(SystemTime::now()),
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    pub fn scanned_at(&self) -> Option<SystemTime> {
        self.scanned_at
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many items of each kind.
    pub fn counts_by_kind(&self) -> BTreeMap<ItemKind, usize> {
        let mut counts = BTreeMap::new();
        for item in &self.items {
            *counts.entry(item.kind).or_default() += 1;
        }
        counts
    }

    /// How many items each section holds **after applying `search`**.
    ///
    /// Search-aware on purpose: typing "project" and seeing Apps go to 1 and
    /// Scenery to 2 tells you instantly where the matches are, without having
    /// to click through every section to find out.
    pub fn counts_by_section(&self, search: &str) -> BTreeMap<Section, usize> {
        let mut counts = BTreeMap::new();
        for item in self.items.iter().filter(|i| i.matches(search)) {
            *counts.entry(Section::of(item)).or_default() += 1;
        }
        counts
    }

    /// Items in one section, or all of them.
    pub fn in_section(&self, section: Option<Section>) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(move |i| section.is_none_or(|s| Section::of(i) == s))
    }

    /// Filtered view: section, then free-text search.
    pub fn section_view(&self, section: Option<Section>, search: &str) -> Vec<&Item> {
        self.in_section(section)
            .filter(|i| i.matches(search))
            .collect()
    }

    /// Items of one kind, or all of them.
    pub fn of_kind(&self, kind: Option<ItemKind>) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(move |i| kind.is_none_or(|k| i.kind == k))
    }

    /// Filtered view: kind, then free-text search.
    pub fn view(&self, kind: Option<ItemKind>, search: &str) -> Vec<&Item> {
        self.of_kind(kind).filter(|i| i.matches(search)).collect()
    }

    /// How many items the sim will actually load.
    pub fn active_count(&self) -> usize {
        self.items.iter().filter(|i| i.state.is_active()).count()
    }

    /// Items a profile could switch.
    pub fn switchable_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.state.is_switchable())
            .count()
    }

    /// Items that are active but which no profile can switch off.
    ///
    /// Worth its own accessor because it is the honest answer to "why did my
    /// profile not change anything": on a library of resident packages, the
    /// answer is all of them.
    pub fn resident_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.state == ItemState::Resident)
            .count()
    }

    pub fn broken_count(&self) -> usize {
        self.items.iter().filter(|i| i.state.is_broken()).count()
    }

    /// Total measured size, and how many items could be measured.
    ///
    /// Both numbers, because a total over a library where some directories
    /// could not be read is a number that invites the wrong conclusion. See
    /// [`crate::Item::size_bytes`] for why it is measured, not declared.
    pub fn measured_size(&self) -> (u64, usize) {
        let mut total = 0;
        let mut declared = 0;
        for item in &self.items {
            if let Some(bytes) = item.size_bytes {
                total += bytes;
                declared += 1;
            }
        }
        (total, declared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn item(name: &str, kind: ItemKind, state: ItemState, title: &str) -> Item {
        Item {
            name: name.into(),
            kind,
            state,
            title: title.into(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: PathBuf::from(name),
            community: None,
            args: Vec::new(),
        }
    }

    fn snapshot(items: Vec<Item>) -> CatalogSnapshot {
        CatalogSnapshot::from_scan(ScanResult {
            items,
            problems: Vec::new(),
        })
    }

    #[test]
    fn items_sort_by_kind_then_title() {
        let s = snapshot(vec![
            item("z", ItemKind::NativeTool, ItemState::Available, "Zulu"),
            item("b", ItemKind::Package, ItemState::Resident, "beta"),
            item("a", ItemKind::Package, ItemState::Resident, "Alpha"),
        ]);
        let titles: Vec<_> = s.items().iter().map(|i| i.display_title()).collect();
        assert_eq!(
            titles,
            ["Alpha", "beta", "Zulu"],
            "case-insensitive within kind"
        );
    }

    #[test]
    fn counts_group_by_kind() {
        let s = snapshot(vec![
            item("a", ItemKind::Package, ItemState::Resident, "a"),
            item("b", ItemKind::Package, ItemState::Available, "b"),
            item("c", ItemKind::SimAddonExe, ItemState::SimEnabled, "c"),
        ]);
        let counts = s.counts_by_kind();
        assert_eq!(counts[&ItemKind::Package], 2);
        assert_eq!(counts[&ItemKind::SimAddonExe], 1);
        assert_eq!(counts.get(&ItemKind::NativeTool), None);
    }

    /// The number that explains why a profile switch would do nothing.
    #[test]
    fn resident_items_count_as_active_but_not_switchable() {
        let s = snapshot(vec![
            item("a", ItemKind::Package, ItemState::Resident, "a"),
            item("b", ItemKind::Package, ItemState::Resident, "b"),
            item(
                "c",
                ItemKind::Package,
                ItemState::Linked {
                    target: "/lib/c".into(),
                },
                "c",
            ),
        ]);
        assert_eq!(s.active_count(), 3);
        assert_eq!(s.resident_count(), 2);
        assert_eq!(s.switchable_count(), 1);
    }

    #[test]
    fn broken_links_are_counted() {
        let s = snapshot(vec![
            item(
                "a",
                ItemKind::Package,
                ItemState::Dangling {
                    target: "/x".into(),
                },
                "a",
            ),
            item("b", ItemKind::Package, ItemState::Resident, "b"),
        ]);
        assert_eq!(s.broken_count(), 1);
        assert_eq!(s.active_count(), 1);
    }

    #[test]
    fn the_view_filters_by_kind_then_search() {
        let mut pkg = item("koak", ItemKind::Package, ItemState::Resident, "Oakland");
        pkg.creator = "bullfrogsim".into();
        let s = snapshot(vec![
            pkg,
            item(
                "tds",
                ItemKind::SimAddonExe,
                ItemState::SimEnabled,
                "TDS GTNXi",
            ),
        ]);

        assert_eq!(s.view(None, "").len(), 2);
        assert_eq!(s.view(Some(ItemKind::Package), "").len(), 1);
        assert_eq!(s.view(None, "oakland").len(), 1);
        assert_eq!(s.view(None, "bullfrog").len(), 1);
        assert_eq!(s.view(Some(ItemKind::SimAddonExe), "oakland").len(), 0);
        assert_eq!(s.view(None, "nothing here").len(), 0);
    }

    /// Reporting a total without saying how many items it covers invites the
    /// wrong conclusion when half the manifests omit the field.
    #[test]
    fn measured_size_reports_its_own_coverage() {
        let mut a = item("a", ItemKind::Package, ItemState::Resident, "a");
        a.size_bytes = Some(1000);
        let b = item("b", ItemKind::Package, ItemState::Resident, "b");
        let s = snapshot(vec![a, b]);
        assert_eq!(s.measured_size(), (1000, 1));
    }

    #[test]
    fn an_empty_snapshot_is_coherent() {
        let s = CatalogSnapshot::default();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.active_count(), 0);
        assert_eq!(s.measured_size(), (0, 0));
        assert!(s.scanned_at().is_none());
    }
}
