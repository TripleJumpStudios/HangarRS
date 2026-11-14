//! Stage 4's observations — the three places an installer can land, read
//! before and after it runs.
//!
//! **The diff is the evidence, not the exit code.** Four of the five known
//! failure modes return 0: a wrong prefix, a stub torn down mid-unpack by
//! `proton run`, a purchase check that could not spawn its helper. All of them
//! look from outside exactly like success. What separates them is that nothing
//! arrived, and that is only visible by having looked first.
//!
//! Three places, because an installer can produce three different things:
//!
//! | Place | What landing there means |
//! |---|---|
//! | `Community` | a package — a directory with a `manifest.json` |
//! | the prefix's `.exe` set | an application with a lifecycle |
//! | `EXE.xml` | an add-on the sim spawns itself |
//!
//! # Not observed is not unchanged
//!
//! Every observation is a [`Seen`], and a place that could not be read stays
//! [`Seen::Unreadable`] rather than becoming an empty set. The difference
//! matters: an empty set diffed against an empty set says "nothing landed",
//! which is precisely the report a failed install produces. A place nobody
//! could read has to say so, or a permissions error reads as a failed
//! install and a failed install reads as a permissions error.
//!
//! # Symlinks are not followed
//!
//! A Wine prefix links its own directories about — `My Documents` →
//! `./Documents`, `Application Data` → `./AppData/Roaming` — so following
//! them counts the same executable several times and reports files as
//! "arriving" that were only ever reached by a second name. Worse, a prefix is
//! free to link outside itself, and a walk that follows would leave the prefix
//! entirely.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hangar_msfs::ExeXml;

/// How deep a prefix walk will go before it stops and says so.
///
/// A guard against a pathological tree rather than a working limit. Hitting it
/// is recorded in [`Snapshot::notes`]: a walk that quietly truncated would
/// report the files it never reached as missing.
const MAX_DEPTH: usize = 16;

/// Directory names that hold **sim content rather than applications**, skipped
/// wherever they appear in a prefix.
///
/// Not only an optimisation. The sim keeps its packages *inside* the prefix,
/// at `drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator
/// 2024`, and that tree is both the largest thing in the prefix and deeper
/// than [`MAX_DEPTH`] in many places. Three reasons it does not belong in the
/// executable set:
///
/// * it is content, not applications, so an `.exe` arriving in it is a package
///   file rather than something with a lifecycle;
/// * it is already observed, as `Community` and as `EXE.xml`, so walking it
///   here would report the same arrival twice;
/// * it contains a mirror of its own path (`$PC$/USERS/STEAMUSER/APPDATA/…`),
///   which is what drives the depth past sixteen.
const CONTENT_DIRS: &[&str] = &[
    "Microsoft Flight Simulator 2024",
    "Microsoft Flight Simulator",
];

/// One observation, or why it could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen<T> {
    Read(T),
    /// Nobody could look. Never treated as empty.
    Unreadable(String),
}

impl<T> Seen<T> {
    pub fn read(&self) -> Option<&T> {
        match self {
            Seen::Read(v) => Some(v),
            Seen::Unreadable(_) => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Seen::Read(_) => None,
            Seen::Unreadable(why) => Some(why),
        }
    }
}

/// A directory in `Community`.
///
/// Carries whether it holds a `manifest.json`, because that is what makes it a
/// package rather than a directory someone left there. A directory that gains
/// a manifest between two snapshots shows up as one removed and one added,
/// which is the honest reading: what it *is* changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub has_manifest: bool,
}

/// A `<Launch.Addon>` entry in `EXE.xml`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Addon {
    pub name: String,
    /// The Windows path exactly as written, because that is what identifies it.
    pub path: String,
}

/// Where to look.
#[derive(Debug, Clone, Copy)]
pub struct Places<'a> {
    /// The Community folder. `None` when the install cannot target one.
    pub community: Option<&'a Path>,
    /// A Proton prefix — the `pfx` directory, with `drive_c` under it.
    pub prefix: Option<&'a Path>,
    /// `EXE.xml`, wherever discovery put it.
    pub exe_xml: Option<&'a Path>,
}

/// What the three places held at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub community: Seen<BTreeSet<Package>>,
    /// Every `.exe` under `drive_c`, relative to it. Relative because an
    /// absolute path carries the prefix, and the same install compared across
    /// two prefixes would then differ in every entry.
    pub executables: Seen<BTreeSet<PathBuf>>,
    pub addons: Seen<BTreeSet<Addon>>,
    /// Anything that qualifies the reading — a directory that could not be
    /// entered, a walk that hit the depth limit.
    pub notes: Vec<String>,
}

impl Snapshot {
    /// Read all three places. Reads only; writes nothing, starts nothing.
    pub fn take(places: Places<'_>) -> Self {
        let mut notes = Vec::new();

        let community = match places.community {
            Some(dir) => read_community(dir, &mut notes),
            None => Seen::Unreadable("no Community folder was given".into()),
        };

        let executables = match places.prefix {
            Some(prefix) => read_executables(prefix, &mut notes),
            None => Seen::Unreadable("no prefix was given".into()),
        };

        let addons = match places.exe_xml {
            Some(path) => read_addons(path),
            None => Seen::Unreadable("no EXE.xml path was given".into()),
        };

        Snapshot {
            community,
            executables,
            addons,
            notes,
        }
    }
}

fn read_community(dir: &Path, notes: &mut Vec<String>) -> Seen<BTreeSet<Package>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return Seen::Unreadable(format!("{}: {e}", dir.display())),
    };

    let mut found = BTreeSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                notes.push(format!("skipped an entry in {}: {e}", dir.display()));
                continue;
            }
        };
        // `file_type` does not follow the link, so a linked-in package is seen
        // as the link it is rather than as whatever it points at.
        let Ok(kind) = entry.file_type() else {
            notes.push(format!("could not type {}", entry.path().display()));
            continue;
        };
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // The disabled-package sibling is HangarRS's own bookkeeping, not
        // something an installer produced.
        if name.starts_with('.') {
            continue;
        }
        found.insert(Package {
            has_manifest: entry.path().join("manifest.json").is_file(),
            name,
        });
    }

    Seen::Read(found)
}

fn read_executables(prefix: &Path, notes: &mut Vec<String>) -> Seen<BTreeSet<PathBuf>> {
    let root = prefix.join("drive_c");
    if !root.is_dir() {
        return Seen::Unreadable(format!("{} has no drive_c", prefix.display()));
    }

    let mut found = BTreeSet::new();
    walk(&root, &root, 0, &mut found, notes);
    Seen::Read(found)
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    found: &mut BTreeSet<PathBuf>,
    notes: &mut Vec<String>,
) {
    if depth > MAX_DEPTH {
        notes.push(format!(
            "stopped at {} — deeper than {MAX_DEPTH} levels",
            dir.display()
        ));
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            notes.push(format!("could not read {}: {e}", dir.display()));
            return;
        }
    };

    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            if is_content_dir(&entry.file_name()) {
                continue;
            }
            walk(root, &path, depth + 1, found, notes);
        } else if is_exe(&path) {
            if let Ok(rel) = path.strip_prefix(root) {
                found.insert(rel.to_path_buf());
            }
        }
    }
}

fn is_content_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|n| CONTENT_DIRS.iter().any(|c| n.eq_ignore_ascii_case(c)))
}

/// Windows does not care about the case of an extension, so neither can this.
fn is_exe(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

fn read_addons(path: &Path) -> Seen<BTreeSet<Addon>> {
    match ExeXml::read(path) {
        Ok(xml) => Seen::Read(
            xml.addons
                .into_iter()
                .map(|a| Addon {
                    name: a.name,
                    path: a.path,
                })
                .collect(),
        ),
        Err(e) => Seen::Unreadable(format!("{}: {e}", path.display())),
    }
}

/// What changed in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change<T: Ord> {
    Delta {
        added: BTreeSet<T>,
        removed: BTreeSet<T>,
    },
    /// One of the two snapshots could not see this place, so nothing about it
    /// can be claimed.
    Unknown(String),
}

impl<T: Ord> Change<T> {
    pub fn added(&self) -> Option<&BTreeSet<T>> {
        match self {
            Change::Delta { added, .. } => Some(added),
            Change::Unknown(_) => None,
        }
    }

    pub fn removed(&self) -> Option<&BTreeSet<T>> {
        match self {
            Change::Delta { removed, .. } => Some(removed),
            Change::Unknown(_) => None,
        }
    }

    /// Observed, and nothing moved. An unknown place is not quiet — it is
    /// unknown, so this is false for it.
    pub fn is_quiet(&self) -> bool {
        match self {
            Change::Delta { added, removed } => added.is_empty() && removed.is_empty(),
            Change::Unknown(_) => false,
        }
    }

    fn unknown_reason(&self) -> Option<&str> {
        match self {
            Change::Delta { .. } => None,
            Change::Unknown(why) => Some(why),
        }
    }
}

/// The difference between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub community: Change<Package>,
    pub executables: Change<PathBuf>,
    pub addons: Change<Addon>,
    /// Notes from either snapshot, so a qualified reading stays qualified.
    pub notes: Vec<String>,
}

impl Diff {
    pub fn between(before: &Snapshot, after: &Snapshot) -> Self {
        let mut notes = before.notes.clone();
        for note in &after.notes {
            if !notes.contains(note) {
                notes.push(note.clone());
            }
        }

        Diff {
            community: change(&before.community, &after.community),
            executables: change(&before.executables, &after.executables),
            addons: change(&before.addons, &after.addons),
            notes,
        }
    }

    /// Nothing arrived anywhere that could be observed.
    ///
    /// Not the same as "the install failed" — that is stage 5's call, and a
    /// runtime installs into places none of these three watch. It is the
    /// question stage 5 starts from.
    pub fn nothing_arrived(&self) -> bool {
        self.community.added().is_none_or(BTreeSet::is_empty)
            && self.executables.added().is_none_or(BTreeSet::is_empty)
            && self.addons.added().is_none_or(BTreeSet::is_empty)
    }

    /// The places that could not be compared, and why.
    pub fn unknown(&self) -> Vec<(&'static str, &str)> {
        [
            ("Community", self.community.unknown_reason()),
            ("prefix executables", self.executables.unknown_reason()),
            ("EXE.xml", self.addons.unknown_reason()),
        ]
        .into_iter()
        .filter_map(|(place, why)| why.map(|w| (place, w)))
        .collect()
    }
}

fn change<T: Ord + Clone>(before: &Seen<BTreeSet<T>>, after: &Seen<BTreeSet<T>>) -> Change<T> {
    match (before, after) {
        (Seen::Read(b), Seen::Read(a)) => Change::Delta {
            added: a.difference(b).cloned().collect(),
            removed: b.difference(a).cloned().collect(),
        },
        (Seen::Unreadable(why), _) => Change::Unknown(format!("before: {why}")),
        (_, Seen::Unreadable(why)) => Change::Unknown(format!("after: {why}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-snapshot-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(&p).expect("mkdir");
            p
        }

        fn file(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(&p, b"x").expect("write");
            p
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pkg(name: &str, has_manifest: bool) -> Package {
        Package {
            name: name.into(),
            has_manifest,
        }
    }

    #[test]
    fn a_community_directory_is_a_package_only_with_a_manifest() {
        let s = Scratch::new("community");
        let community = s.dir("Community");
        s.file("Community/with-manifest/manifest.json");
        s.dir("Community/bare");

        let snap = Snapshot::take(Places {
            community: Some(&community),
            prefix: None,
            exe_xml: None,
        });

        let found = snap.community.read().expect("readable");
        assert!(found.contains(&pkg("with-manifest", true)));
        assert!(found.contains(&pkg("bare", false)));
    }

    #[test]
    fn a_missing_community_folder_is_unreadable_not_empty() {
        let s = Scratch::new("missing");
        let absent = s.0.join("nope");

        let snap = Snapshot::take(Places {
            community: Some(&absent),
            prefix: None,
            exe_xml: None,
        });

        assert!(snap.community.read().is_none());
        assert!(snap.community.reason().is_some());
    }

    #[test]
    fn executables_are_found_at_any_depth_and_any_case() {
        let s = Scratch::new("exes");
        let prefix = s.dir("pfx");
        s.file("pfx/drive_c/Program Files/App/app.exe");
        s.file("pfx/drive_c/windows/system32/TOOL.EXE");
        s.file("pfx/drive_c/Program Files/App/readme.txt");

        let snap = Snapshot::take(Places {
            community: None,
            prefix: Some(&prefix),
            exe_xml: None,
        });

        let found = snap.executables.read().expect("readable");
        assert!(found.contains(&PathBuf::from("Program Files/App/app.exe")));
        assert!(found.contains(&PathBuf::from("windows/system32/TOOL.EXE")));
        assert_eq!(found.len(), 2, "readme.txt is not an executable");
    }

    #[test]
    fn executables_are_relative_to_drive_c() {
        let s = Scratch::new("relative");
        let prefix = s.dir("pfx");
        s.file("pfx/drive_c/a.exe");

        let snap = Snapshot::take(Places {
            community: None,
            prefix: Some(&prefix),
            exe_xml: None,
        });

        let found = snap.executables.read().expect("readable");
        assert_eq!(
            found.iter().next().map(PathBuf::as_path),
            Some(Path::new("a.exe")),
            "an absolute path would carry the prefix into the comparison"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_not_walked_twice() {
        let s = Scratch::new("symlink");
        let prefix = s.dir("pfx");
        s.file("pfx/drive_c/users/steamuser/Documents/thing.exe");
        // Exactly what a Wine prefix does: `My Documents` -> `./Documents`.
        std::os::unix::fs::symlink(
            "./Documents",
            s.0.join("pfx/drive_c/users/steamuser/My Documents"),
        )
        .expect("symlink");

        let snap = Snapshot::take(Places {
            community: None,
            prefix: Some(&prefix),
            exe_xml: None,
        });

        let found = snap.executables.read().expect("readable");
        assert_eq!(
            found.len(),
            1,
            "following the link would count the same file under a second name: {found:?}"
        );
    }

    /// The sim keeps its packages inside the prefix, and that tree is content
    /// rather than applications — already observed as Community, and deep
    /// enough to bury the walk.
    #[test]
    fn the_sims_own_package_tree_is_not_walked() {
        let s = Scratch::new("content");
        let prefix = s.dir("pfx");
        s.file("pfx/drive_c/Program Files/App/app.exe");
        s.file(
            "pfx/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024/Packages/thing/tool.exe",
        );

        let snap = Snapshot::take(Places {
            community: None,
            prefix: Some(&prefix),
            exe_xml: None,
        });

        let found = snap.executables.read().expect("readable");
        assert_eq!(
            found.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            vec![Path::new("Program Files/App/app.exe")],
            "an .exe inside the sim's package tree is a package file"
        );
        assert!(
            snap.notes.is_empty(),
            "and skipping it is what keeps the depth notes empty: {:?}",
            snap.notes
        );
    }

    #[test]
    fn a_prefix_without_drive_c_is_unreadable() {
        let s = Scratch::new("nodrivec");
        let prefix = s.dir("pfx");

        let snap = Snapshot::take(Places {
            community: None,
            prefix: Some(&prefix),
            exe_xml: None,
        });

        assert!(snap.executables.read().is_none());
    }

    #[test]
    fn a_missing_exe_xml_is_no_addons_rather_than_a_failure() {
        let s = Scratch::new("noexexml");
        let absent = s.0.join("EXE.xml");

        let snap = Snapshot::take(Places {
            community: None,
            prefix: None,
            exe_xml: Some(&absent),
        });

        // `ExeXml::read` treats absence as "nothing registered", which is the
        // normal state for most installs.
        assert_eq!(snap.addons.read().map(BTreeSet::len), Some(0));
    }

    #[test]
    fn addons_are_read_from_exe_xml() {
        let s = Scratch::new("exexml");
        let path = s.0.join("EXE.xml");
        std::fs::write(
            &path,
            b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
              <SimBase.Document Type=\"Launch\" version=\"1,0\">\n\
              <Launch.Addon><Name>GTN</Name><Path>C:\\TDS\\gtn.exe</Path></Launch.Addon>\n\
              </SimBase.Document>\n",
        )
        .expect("write");

        let snap = Snapshot::take(Places {
            community: None,
            prefix: None,
            exe_xml: Some(&path),
        });

        let found = snap.addons.read().expect("readable");
        assert_eq!(
            found.iter().next(),
            Some(&Addon {
                name: "GTN".into(),
                path: "C:\\TDS\\gtn.exe".into(),
            })
        );
    }

    fn seen(names: &[(&str, bool)]) -> Seen<BTreeSet<Package>> {
        Seen::Read(names.iter().map(|(n, m)| pkg(n, *m)).collect())
    }

    #[test]
    fn a_diff_names_what_arrived_and_what_left() {
        let before = Snapshot {
            community: seen(&[("old", true)]),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };
        let after = Snapshot {
            community: seen(&[("old", true), ("new", true)]),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };

        let diff = Diff::between(&before, &after);
        assert_eq!(
            diff.community.added(),
            Some(&[pkg("new", true)].into_iter().collect())
        );
        assert_eq!(diff.community.removed().map(BTreeSet::len), Some(0));
        assert!(!diff.nothing_arrived());
    }

    #[test]
    fn a_directory_that_gains_a_manifest_shows_as_a_change() {
        let before = Snapshot {
            community: seen(&[("thing", false)]),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };
        let after = Snapshot {
            community: seen(&[("thing", true)]),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };

        let diff = Diff::between(&before, &after);
        assert_eq!(
            diff.community.added(),
            Some(&[pkg("thing", true)].into_iter().collect()),
            "what it is changed, and that has to be visible"
        );
        assert_eq!(
            diff.community.removed(),
            Some(&[pkg("thing", false)].into_iter().collect())
        );
    }

    #[test]
    fn an_unreadable_place_is_unknown_rather_than_quiet() {
        let before = Snapshot {
            community: Seen::Unreadable("permission denied".into()),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };
        let after = Snapshot {
            community: seen(&[("new", true)]),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };

        let diff = Diff::between(&before, &after);
        assert!(matches!(diff.community, Change::Unknown(_)));
        assert!(
            !diff.community.is_quiet(),
            "unknown must never read as nothing-changed"
        );
        assert_eq!(diff.unknown().len(), 1);
        assert!(diff.unknown()[0].1.contains("permission denied"));
    }

    #[test]
    fn nothing_arrived_is_true_only_when_every_place_agrees() {
        let empty = || Snapshot {
            community: Seen::Read(BTreeSet::new()),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };

        assert!(Diff::between(&empty(), &empty()).nothing_arrived());

        let mut after = empty();
        after.addons = Seen::Read(
            [Addon {
                name: "GTN".into(),
                path: "C:\\gtn.exe".into(),
            }]
            .into_iter()
            .collect(),
        );
        assert!(!Diff::between(&empty(), &after).nothing_arrived());
    }

    #[test]
    fn notes_from_both_snapshots_survive_the_diff() {
        let before = Snapshot {
            community: Seen::Read(BTreeSet::new()),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: vec!["could not read one directory".into()],
        };
        let after = Snapshot {
            community: Seen::Read(BTreeSet::new()),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: vec!["stopped at a deep tree".into()],
        };

        let diff = Diff::between(&before, &after);
        assert_eq!(diff.notes.len(), 2);
    }
}
