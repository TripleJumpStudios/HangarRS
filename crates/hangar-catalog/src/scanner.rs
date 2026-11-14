//! Scanning the Community folders, package libraries and `EXE.xml` into items.
//!
//! Synchronous and unremarkable on purpose: real manifests are a few hundred
//! bytes, so `read_to_string` beats `mmap` and a library's worth takes
//! microseconds. Parallelise only with a measurement to point at.

use crate::item::{Item, ItemKind, ItemState};
use hangar_msfs::{ExeXml, ManifestJson, MsfsEnv};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a scan found, plus anything that went wrong while finding it.
#[derive(Debug, Default)]
pub struct ScanResult {
    pub items: Vec<Item>,
    /// Directories that looked like packages but could not be read.
    ///
    /// Surfaced rather than skipped: a package that vanishes from the library
    /// because its manifest has a stray comma is the worst kind of bug to
    /// diagnose from the UI.
    pub problems: Vec<Problem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub reason: String,
}

/// Scan everything the environment knows about.
///
/// Read-only. Nothing here creates, moves or deletes anything.
pub fn scan(env: &MsfsEnv, libraries: &[PathBuf]) -> ScanResult {
    let mut result = ScanResult::default();
    // Keyed by name so a package present in both a library and the Community
    // folder is reported once, with the Community folder's state winning —
    // that is the copy the sim actually loads.
    let mut packages: BTreeMap<String, Item> = BTreeMap::new();

    for library in libraries {
        for item in scan_dir(library, None, &mut result.problems) {
            packages.entry(item.name.clone()).or_insert(item);
        }
    }

    for community in &env.communities {
        for item in scan_dir(&community.path, Some(&community.path), &mut result.problems) {
            packages.insert(item.name.clone(), item);
        }
    }

    result.items.extend(packages.into_values());
    result.items.extend(scan_exe_xml(env, &mut result.problems));
    // Companion apps and native tools from HangarRS' config, falling back to
    // the previous application's until a migration has been run. Without
    // these the Apps section is empty of the things it exists to show.
    result.items.extend(crate::apps::configured_apps());
    result
}

/// Scan one directory of packages.
///
/// `community` is `Some` when the directory being scanned *is* a Community
/// folder, which is what separates a resident package from an available one.
pub fn scan_dir(dir: &Path, community: Option<&Path>, problems: &mut Vec<Problem>) -> Vec<Item> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        // `symlink_metadata` first: a dangling link has no `metadata()` at all,
        // and it is precisely the case worth reporting.
        let Ok(link_meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let is_symlink = link_meta.file_type().is_symlink();
        let target = is_symlink.then(|| std::fs::read_link(&path).unwrap_or_default());

        if is_symlink && !path.exists() {
            items.push(broken_link(
                name,
                path,
                target.unwrap_or_default(),
                community,
            ));
            continue;
        }
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("manifest.json");
        if !manifest_path.is_file() {
            continue; // not a package; not a problem either
        }

        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => match ManifestJson::parse(&text) {
                Ok(m) => m,
                Err(e) => {
                    problems.push(Problem {
                        path: manifest_path,
                        reason: format!("manifest is not valid JSON: {e}"),
                    });
                    continue;
                }
            },
            Err(e) => {
                problems.push(Problem {
                    path: manifest_path,
                    reason: format!("could not read manifest: {e}"),
                });
                continue;
            }
        };

        let state = match (community, is_symlink) {
            (Some(_), true) => ItemState::Linked {
                target: target.clone().unwrap_or_default(),
            },
            (Some(_), false) => ItemState::Resident,
            (None, _) => ItemState::Available,
        };

        // Measured, never the manifest's `total_package_size`: only some
        // manifests carry it and those report the download rather than what
        // landed, so trusting it gives one column two meanings and a dash for
        // the rest. See `hangar_core::disk::dir_bytes`.
        let size_bytes = hangar_core::disk::dir_bytes(&path);

        items.push(Item {
            name,
            kind: ItemKind::Package,
            state,
            title: manifest.title.clone(),
            creator: manifest.creator.clone(),
            version: manifest.package_version.clone(),
            content_type: Some(manifest.content_type()),
            size_bytes,
            path,
            community: community.map(Path::to_path_buf),
            args: Vec::new(),
        });
    }

    items
}

fn broken_link(name: String, path: PathBuf, target: PathBuf, community: Option<&Path>) -> Item {
    Item {
        name,
        kind: ItemKind::Package,
        state: ItemState::Dangling { target },
        title: String::new(),
        creator: String::new(),
        version: String::new(),
        content_type: None,
        size_bytes: None,
        path,
        community: community.map(Path::to_path_buf),
        args: Vec::new(),
    }
}

/// Read `EXE.xml` into [`ItemKind::SimAddonExe`] items.
fn scan_exe_xml(env: &MsfsEnv, problems: &mut Vec<Problem>) -> Vec<Item> {
    let path = env.exe_xml();
    let doc = match ExeXml::read(&path) {
        Ok(doc) => doc,
        Err(e) => {
            problems.push(Problem {
                path,
                reason: e.to_string(),
            });
            return Vec::new();
        }
    };

    doc.addons
        .iter()
        .map(|addon| {
            // The document-level flag disables every entry at once, so an
            // add-on is only really enabled when both flags allow it.
            let enabled = !doc.disabled && !addon.disabled;
            Item {
                name: addon.name.clone(),
                kind: ItemKind::SimAddonExe,
                state: if enabled {
                    ItemState::SimEnabled
                } else {
                    ItemState::SimDisabled
                },
                title: addon.name.clone(),
                creator: String::new(),
                version: String::new(),
                content_type: None,
                size_bytes: None,
                path: addon
                    .resolve_in_prefix(&env.install.prefix)
                    .unwrap_or_else(|| PathBuf::from(&addon.path)),
                community: None,
                args: Vec::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-catalog-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn package(dir: &Path, name: &str, content_type: &str) -> PathBuf {
        let pkg = dir.join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("manifest.json"),
            format!(
                r#"{{"title":"{name} title","creator":"someone","package_version":"1.0.0",
                    "content_type":"{content_type}","dependencies":[]}}"#
            ),
        )
        .unwrap();
        pkg
    }

    /// A real directory inside Community is Resident, not Linked.
    #[test]
    fn a_directory_in_community_is_resident() {
        let community = scratch();
        package(&community, "a2a-aircraft-pa24", "AIRCRAFT");

        let mut problems = Vec::new();
        let items = scan_dir(&community, Some(&community), &mut problems);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, ItemState::Resident);
        assert!(items[0].state.is_active());
        assert!(!items[0].state.is_switchable());
        assert!(problems.is_empty());
    }

    #[test]
    fn a_symlink_into_community_is_linked() {
        let library = scratch();
        let community = scratch();
        let target = package(&library, "koak", "SCENERY");
        std::os::unix::fs::symlink(&target, community.join("koak")).unwrap();

        let mut problems = Vec::new();
        let items = scan_dir(&community, Some(&community), &mut problems);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, ItemState::Linked { target });
        assert!(items[0].state.is_switchable());
    }

    #[test]
    fn a_package_in_a_library_is_available() {
        let library = scratch();
        package(&library, "spare-scenery", "SCENERY");

        let mut problems = Vec::new();
        let items = scan_dir(&library, None, &mut problems);
        assert_eq!(items[0].state, ItemState::Available);
        assert!(!items[0].state.is_active());
    }

    /// A dangling symlink has no metadata to stat, so it has to be detected
    /// before anything tries to read a manifest through it.
    #[test]
    fn a_dangling_symlink_is_reported_not_skipped() {
        let community = scratch();
        std::os::unix::fs::symlink("/nonexistent/target", community.join("gone")).unwrap();

        let mut problems = Vec::new();
        let items = scan_dir(&community, Some(&community), &mut problems);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "gone");
        assert!(items[0].state.is_broken());
        assert!(!items[0].state.is_active());
    }

    /// A directory with no manifest is simply not a package. A directory whose
    /// manifest is corrupt is a *problem* — it must not silently vanish.
    #[test]
    fn a_broken_manifest_is_a_problem_and_a_plain_directory_is_not() {
        let community = scratch();
        std::fs::create_dir_all(community.join("not-a-package")).unwrap();
        let broken = community.join("broken-pkg");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("manifest.json"), "{ not json").unwrap();
        package(&community, "good-pkg", "SCENERY");

        let mut problems = Vec::new();
        let items = scan_dir(&community, Some(&community), &mut problems);

        assert_eq!(items.len(), 1, "only the good package becomes an item");
        assert_eq!(items[0].name, "good-pkg");
        assert_eq!(problems.len(), 1, "the broken one is reported");
        assert!(problems[0].path.ends_with("broken-pkg/manifest.json"));
    }

    #[test]
    fn manifest_fields_carry_into_the_item() {
        let community = scratch();
        package(&community, "koak", "SCENERY");

        let mut problems = Vec::new();
        let items = scan_dir(&community, Some(&community), &mut problems);
        let i = &items[0];
        assert_eq!(i.title, "koak title");
        assert_eq!(i.creator, "someone");
        assert_eq!(i.version, "1.0.0");
        assert_eq!(i.content_type, Some(hangar_msfs::ContentType::Scenery));
        // Measured off the disk, not read out of the manifest — which does not
        // declare a size here.
        assert_eq!(
            i.size_bytes,
            Some(
                std::fs::metadata(i.path.join("manifest.json"))
                    .unwrap()
                    .len()
            ),
            "the package's only file is its manifest"
        );
    }

    /// A package in both a library and the Community folder is one item, in the
    /// state the Community folder gives it — that is the copy the sim loads.
    #[test]
    fn a_community_copy_wins_over_a_library_copy() {
        let library = scratch();
        let community = scratch();
        package(&library, "shared", "SCENERY");
        package(&community, "shared", "SCENERY");

        let mut merged: BTreeMap<String, Item> = BTreeMap::new();
        let mut problems = Vec::new();
        for item in scan_dir(&library, None, &mut problems) {
            merged.entry(item.name.clone()).or_insert(item);
        }
        for item in scan_dir(&community, Some(&community), &mut problems) {
            merged.insert(item.name.clone(), item);
        }

        assert_eq!(merged.len(), 1);
        assert_eq!(merged["shared"].state, ItemState::Resident);
    }

    #[test]
    fn an_unreadable_directory_yields_nothing_rather_than_failing() {
        let mut problems = Vec::new();
        let items = scan_dir(Path::new("/nonexistent/dir"), None, &mut problems);
        assert!(items.is_empty());
        assert!(problems.is_empty());
    }
}
