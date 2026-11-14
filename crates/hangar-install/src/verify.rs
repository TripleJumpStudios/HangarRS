//! Stage 6 — checking an installed package against `layout.json`.
//!
//! `layout.json` is the sim's own manifest of what a package contains: every
//! file, its size, its timestamp. The sim reads it to decide what to load, so
//! it is ground truth in the only sense that matters — a file the sim does not
//! see listed there might as well not be on disk.
//!
//! Three questions, and they are genuinely different:
//!
//! 1. **Is everything there, at the right size?** Catches an install that ran
//!    out of room, was interrupted, or hit a path the filesystem refused.
//! 2. **Is anything there that should not be?** An orphan is a file from a
//!    previous version that the new one no longer lists. The sim ignores it,
//!    but it is how a package quietly grows and how a downgrade leaves a
//!    working aircraft in an impossible state.
//! 3. **Did this installer actually apply?** The one that needs the cab.
//!
//! # Why question 3 needs opening the cab
//!
//! Because a package that is complete and internally consistent can still be
//! the *wrong version*. Comparing the installed `layout.json` against the one
//! the installer ships answers "is what is on disk what this installer would
//! produce", which no amount of checking the install against itself can.
//!
//! Measured against a real Commander 114 install:
//!
//! ```text
//! shipped in data1.cab   package_version 0.1.0, minimum_game_version 1.8.9,  912 entries
//! installed on disk      package_version 0.1.1, minimum_game_version 1.8.14, 914 entries
//!                        2 files added, 29 files a different size
//! ```
//!
//! On that pair the version strings differ, so a cheaper check would have
//! caught it too. They do not always — a point release can ship the same
//! string — and comparing the layouts costs one `unshield` of one file.
//!
//! # Nothing here repairs anything
//!
//! [`too_long`] reports the paths that will exceed Windows' `MAX_PATH`, and
//! stops there. The repair — extracting those files by basename — writes into
//! Community, and belongs behind the same gate as everything else that does.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use thiserror::Error;

/// The two files in a package directory that `layout.json` never lists.
///
/// Confirmed against the real packages: `bksq-aircraft-bonanzapro` declares
/// 1084 entries and holds 1086 files; `bksq-aircraft-commander114` declares
/// 914 and holds 916: the rule is "content count + 2", and these are the two.
pub const UNDECLARED: &[&str] = &["layout.json", "manifest.json"];

/// Windows' path limit. The sim is a Windows application under Proton, so its
/// paths are Windows paths whatever the host filesystem allows.
pub const MAX_PATH: usize = 260;

/// One file a package declares.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    /// Windows FILETIME. Carried because it is in the file; nothing here
    /// compares it — a copy that preserves content but not timestamps is not
    /// a broken install.
    #[serde(default)]
    pub date: i64,
}

/// A parsed `layout.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Layout {
    pub content: Vec<Entry>,
}

impl Layout {
    pub fn read(path: &Path) -> Result<Layout, VerifyError> {
        let bytes = std::fs::read(path).map_err(|source| VerifyError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| VerifyError::Malformed {
            path: path.to_path_buf(),
            detail: source.to_string(),
        })
    }

    /// Total declared bytes.
    pub fn declared_bytes(&self) -> u64 {
        self.content.iter().map(|e| e.size).sum()
    }
}

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a layout.json: {detail}")]
    Malformed { path: PathBuf, detail: String },

    #[error("no layout.json in {dir}")]
    NoLayout { dir: PathBuf },
}

/// A file that is present but the wrong size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeMismatch {
    pub path: String,
    pub declared: u64,
    pub found: u64,
}

/// What checks 1 and 2 found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackageReport {
    pub declared: usize,
    /// Declared and not on disk, in any case.
    pub missing: Vec<String>,
    /// Declared, on disk, wrong size.
    pub wrong_size: Vec<SizeMismatch>,
    /// On disk and not declared, `layout.json` and `manifest.json` aside.
    pub orphans: Vec<String>,
    /// Two or more files whose paths differ only by case.
    ///
    /// Rare and worth its own line, because Windows cannot represent it and
    /// Wine can only reach one of them. The sim will load whichever its
    /// case-insensitive lookup returns and never see the other, which is a
    /// far stranger failure than a missing file.
    pub case_collisions: Vec<Vec<String>>,
}

impl PackageReport {
    /// Everything declared is present at the right size, and nothing else is
    /// there.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
            && self.wrong_size.is_empty()
            && self.orphans.is_empty()
            && self.case_collisions.is_empty()
    }

    /// How many of the declared files are present and the right size.
    pub fn verified(&self) -> usize {
        self.declared
            .saturating_sub(self.missing.len())
            .saturating_sub(self.wrong_size.len())
    }
}

/// Checks 1 and 2: the package against its own `layout.json`.
pub fn verify_package(dir: &Path) -> Result<PackageReport, VerifyError> {
    let layout_path = dir.join("layout.json");
    if !layout_path.is_file() {
        return Err(VerifyError::NoLayout {
            dir: dir.to_path_buf(),
        });
    }
    let layout = Layout::read(&layout_path)?;

    let on_disk = index(dir);

    let mut missing = Vec::new();
    let mut wrong_size = Vec::new();
    let mut declared_keys = BTreeSet::new();

    for entry in &layout.content {
        let key = entry.path.to_lowercase();
        declared_keys.insert(key.clone());

        // Matched case-insensitively, because MSFS is a Windows application
        // and Windows paths are. See `index`.
        let Some(actual) = on_disk.get(&key).and_then(|found| found.first()) else {
            missing.push(entry.path.clone());
            continue;
        };
        match std::fs::metadata(dir.join(actual)) {
            Ok(meta) if meta.is_file() => {
                if meta.len() != entry.size {
                    wrong_size.push(SizeMismatch {
                        path: entry.path.clone(),
                        declared: entry.size,
                        found: meta.len(),
                    });
                }
            }
            _ => missing.push(entry.path.clone()),
        }
    }

    let undeclared: BTreeSet<String> = UNDECLARED.iter().map(|s| s.to_lowercase()).collect();
    let mut orphans: Vec<String> = on_disk
        .iter()
        .filter(|(key, _)| !declared_keys.contains(*key) && !undeclared.contains(*key))
        .flat_map(|(_, actual)| actual.iter().cloned())
        .collect();
    orphans.sort();

    let case_collisions = on_disk
        .into_values()
        .filter(|found| found.len() > 1)
        .collect();

    Ok(PackageReport {
        declared: layout.content.len(),
        missing,
        wrong_size,
        orphans,
        case_collisions,
    })
}

/// Every file in the package, keyed by its lowercased relative path.
///
/// **Case-insensitively, because Windows is.** `layout.json` is written by
/// whatever built the package, and the case it records need not match what is
/// on disk: the real `a2a-aircraft-pa24` declares
/// `Effects/texture/fx_0.png.DDS` and ships `Effects/texture/FX_0.PNG.DDS`.
/// Windows finds it, Wine's case-insensitive lookup finds it, and the sim
/// loads it. Only a Linux-side comparison sees a problem.
///
/// Comparing exactly turns that into a wall of noise: on that package, 390 of
/// 596 declared files read as missing where 7 were genuinely absent.
///
/// The value is a list because two files *can* differ only by case on a
/// case-sensitive filesystem — see [`PackageReport::case_collisions`].
fn index(dir: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut found: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    walk_files(dir, dir, &mut found);
    for actual in found.values_mut() {
        actual.sort();
    }
    found
}

fn walk_files(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<String>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
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
            walk_files(root, &path, out);
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.entry(rel.to_lowercase()).or_default().push(rel);
    }
}

/// Declared paths whose full Windows path would exceed [`MAX_PATH`].
///
/// `community_win_path` is where the package will sit as Windows sees it, for
/// example `C:\Users\steamuser\AppData\Roaming\Microsoft Flight Simulator
/// 2024\Packages\Community`. The list is **derived, never hardcoded**:
/// Commander 114 went from two too-long files to three between releases, so a
/// fixed list silently stops covering the thing it was written for.
pub fn too_long(layout: &Layout, community_win_path: &str, package: &str) -> Vec<String> {
    let prefix_len = community_win_path.trim_end_matches('\\').len() + 1 + package.len() + 1;
    layout
        .content
        .iter()
        .filter(|e| prefix_len + e.path.len() > MAX_PATH)
        .map(|e| e.path.clone())
        .collect()
}

/// Check 3's first half: pull one file out of an InstallShield cab by name.
///
/// **Matched on the basename, because that is what `unshield` matches.**
/// Asking for the full archive path `App_Executables\<package>\layout.json`
/// extracts nothing and reports success — zero files, exit 0 — which would
/// read as "the cab does not contain a layout" rather than "the query was
/// wrong". Verified against the real Commander 114 cab.
///
/// `unshield` still recreates the component directory under `out_dir` even
/// with `-j`, so the returned path is found rather than assumed.
pub fn extract_from_cab(cab: &Path, basename: &str, out_dir: &Path) -> Result<PathBuf, CabError> {
    if which_unshield().is_none() {
        return Err(CabError::NoUnshield);
    }
    std::fs::create_dir_all(out_dir).map_err(|source| CabError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    let output = Command::new("unshield")
        .arg("-j")
        .arg("-d")
        .arg(out_dir)
        .arg("x")
        .arg(cab)
        .arg(basename)
        .output()
        .map_err(|source| CabError::Io {
            path: cab.to_path_buf(),
            source,
        })?;

    if !output.status.success() {
        return Err(CabError::Failed {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    find_by_name(out_dir, basename).ok_or_else(|| CabError::NotInCab {
        cab: cab.to_path_buf(),
        basename: basename.to_string(),
    })
}

fn find_by_name(dir: &Path, basename: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_by_name(&path, basename) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == basename) {
            return Some(path);
        }
    }
    None
}

fn which_unshield() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("unshield"))
            .find(|p| p.is_file())
    })
}

#[derive(Debug, Error)]
pub enum CabError {
    #[error("unshield is not installed, so the cab cannot be opened")]
    NoUnshield,

    #[error("unshield failed: {detail}")]
    Failed { detail: String },

    #[error("{cab} contains no file named {basename}")]
    NotInCab { cab: PathBuf, basename: String },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Check 3: is what is installed what this installer would produce?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The layouts match. This installer's payload is what is on disk.
    Yes,
    /// They differ, with what differs.
    No {
        declared_by_installer: usize,
        declared_on_disk: usize,
        added: Vec<String>,
        removed: Vec<String>,
        resized: usize,
    },
}

/// Compare an installer's `layout.json` against the installed one.
pub fn applied(in_cab: &Layout, on_disk: &Layout) -> Applied {
    let cab: std::collections::BTreeMap<_, _> =
        in_cab.content.iter().map(|e| (&e.path, e.size)).collect();
    let disk: std::collections::BTreeMap<_, _> =
        on_disk.content.iter().map(|e| (&e.path, e.size)).collect();

    let added: Vec<String> = disk
        .keys()
        .filter(|p| !cab.contains_key(*p))
        .map(|p| (*p).clone())
        .collect();
    let removed: Vec<String> = cab
        .keys()
        .filter(|p| !disk.contains_key(*p))
        .map(|p| (*p).clone())
        .collect();
    let resized = cab
        .iter()
        .filter(|(p, size)| disk.get(*p).is_some_and(|d| d != *size))
        .count();

    if added.is_empty() && removed.is_empty() && resized == 0 {
        Applied::Yes
    } else {
        Applied::No {
            declared_by_installer: in_cab.content.len(),
            declared_on_disk: on_disk.content.len(),
            added,
            removed,
            resized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-verify-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, bytes).expect("write");
        }

        fn layout(&self, entries: &[(&str, u64)]) {
            let content: Vec<String> = entries
                .iter()
                .map(|(p, s)| format!(r#"{{"path":"{p}","size":{s},"date":133818860080000000}}"#))
                .collect();
            self.write(
                "layout.json",
                format!(r#"{{"content":[{}]}}"#, content.join(",")).as_bytes(),
            );
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn layout_of(entries: &[(&str, u64)]) -> Layout {
        Layout {
            content: entries
                .iter()
                .map(|(p, s)| Entry {
                    path: (*p).to_string(),
                    size: *s,
                    date: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn a_complete_package_verifies() {
        let s = Scratch::new("complete");
        s.write("manifest.json", b"{}");
        s.write("a.txt", b"hello");
        s.write("sub/b.bin", b"1234567890");
        s.layout(&[("a.txt", 5), ("sub/b.bin", 10)]);

        let report = verify_package(&s.0).expect("verified");
        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.declared, 2);
        assert_eq!(report.verified(), 2);
    }

    /// The two files `layout.json` never lists. Confirmed against the real
    /// packages: 1084 declared, 1086 on disk.
    #[test]
    fn layout_and_manifest_are_not_orphans() {
        let s = Scratch::new("undeclared");
        s.write("manifest.json", b"{}");
        s.write("a.txt", b"hello");
        s.layout(&[("a.txt", 5)]);

        let report = verify_package(&s.0).expect("verified");
        assert!(report.orphans.is_empty(), "{:?}", report.orphans);
    }

    #[test]
    fn a_missing_file_is_named() {
        let s = Scratch::new("missing");
        s.write("manifest.json", b"{}");
        s.layout(&[("gone.txt", 5)]);

        let report = verify_package(&s.0).expect("verified");
        assert_eq!(report.missing, vec!["gone.txt".to_string()]);
        assert_eq!(report.verified(), 0);
        assert!(!report.is_complete());
    }

    #[test]
    fn a_truncated_file_is_a_size_mismatch_not_a_missing_one() {
        let s = Scratch::new("truncated");
        s.write("manifest.json", b"{}");
        s.write("a.txt", b"he");
        s.layout(&[("a.txt", 5)]);

        let report = verify_package(&s.0).expect("verified");
        assert!(report.missing.is_empty());
        assert_eq!(
            report.wrong_size,
            vec![SizeMismatch {
                path: "a.txt".into(),
                declared: 5,
                found: 2,
            }]
        );
    }

    /// A file the new version no longer lists. The sim ignores it, which is
    /// exactly why nothing notices it accumulating.
    #[test]
    fn a_file_the_layout_does_not_list_is_an_orphan() {
        let s = Scratch::new("orphan");
        s.write("manifest.json", b"{}");
        s.write("a.txt", b"hello");
        s.write("sub/leftover.bin", b"old");
        s.layout(&[("a.txt", 5)]);

        let report = verify_package(&s.0).expect("verified");
        assert_eq!(report.orphans, vec!["sub/leftover.bin".to_string()]);
        assert!(!report.is_complete());
    }

    /// The case that mattered: `a2a-aircraft-pa24` declares
    /// `Effects/texture/fx_0.png.DDS` and ships `FX_0.PNG.DDS`. Comparing
    /// exactly reported 390 of 596 missing and 383 orphans; seven were
    /// genuinely absent.
    #[test]
    fn a_file_whose_case_differs_is_present_because_windows_says_so() {
        let s = Scratch::new("case");
        s.write("manifest.json", b"{}");
        s.write("Effects/texture/FX_0.PNG.DDS", b"hello");
        s.layout(&[("Effects/texture/fx_0.png.DDS", 5)]);

        let report = verify_package(&s.0).expect("verified");
        assert!(
            report.is_complete(),
            "case is not a difference to Windows: {report:?}"
        );
        assert!(report.orphans.is_empty(), "nor is it an orphan");
    }

    /// Wine can only reach one of them, and Windows cannot represent the
    /// situation at all.
    #[test]
    fn two_files_differing_only_by_case_are_reported() {
        let s = Scratch::new("collision");
        s.write("manifest.json", b"{}");
        s.write("a.txt", b"hello");
        s.write("A.TXT", b"hello");
        s.layout(&[("a.txt", 5)]);

        let report = verify_package(&s.0).expect("verified");
        assert_eq!(report.case_collisions.len(), 1);
        assert!(!report.is_complete());
    }

    #[test]
    fn a_directory_without_a_layout_is_not_a_package() {
        let s = Scratch::new("nolayout");
        s.write("manifest.json", b"{}");
        assert!(matches!(
            verify_package(&s.0),
            Err(VerifyError::NoLayout { .. })
        ));
    }

    #[test]
    fn a_malformed_layout_says_so_rather_than_reading_as_empty() {
        let s = Scratch::new("malformed");
        s.write("layout.json", b"{ not json");
        assert!(matches!(
            verify_package(&s.0),
            Err(VerifyError::Malformed { .. })
        ));
    }

    /// The real Community path, taken verbatim from FSUIPC7's installer log.
    const COMMUNITY: &str =
        r"C:\users\steamuser\AppData\Roaming\Microsoft Flight Simulator 2024\Packages\Community";

    #[test]
    fn too_long_is_derived_from_the_layout_rather_than_listed() {
        let short = "a.txt";
        let long = "html_ui/Pages/VCockpit/Instruments/Generic/Misc/bksq-aircraft-commander114/\
                    BlackSquareTablet/Shared/some/deeply/nested/thing/that/keeps/going/onwards.js";
        let layout = layout_of(&[(short, 1), (long, 1)]);

        let over = too_long(&layout, COMMUNITY, "bksq-aircraft-commander114");
        assert_eq!(over, vec![long.to_string()]);

        // The same package under a shorter Community path may have none.
        assert!(too_long(&layout, r"C:\C", "p").is_empty());
    }

    #[test]
    fn an_identical_layout_means_the_installer_applied() {
        let a = layout_of(&[("a.txt", 5), ("b.txt", 7)]);
        let b = layout_of(&[("b.txt", 7), ("a.txt", 5)]);
        assert_eq!(applied(&a, &b), Applied::Yes, "order is not content");
    }

    /// The real pair, as measured: the cab ships 912 entries at
    /// package_version 0.1.0, the installed copy declares 914 at 0.1.1, with
    /// two files added and 29 resized.
    #[test]
    fn a_newer_install_than_the_installer_is_detected() {
        let in_cab = layout_of(&[("a.txt", 5), ("b.txt", 7)]);
        let on_disk = layout_of(&[("a.txt", 5), ("b.txt", 9), ("c.txt", 1)]);

        match applied(&in_cab, &on_disk) {
            Applied::No {
                declared_by_installer,
                declared_on_disk,
                added,
                removed,
                resized,
            } => {
                assert_eq!(declared_by_installer, 2);
                assert_eq!(declared_on_disk, 3);
                assert_eq!(added, vec!["c.txt".to_string()]);
                assert!(removed.is_empty());
                assert_eq!(resized, 1);
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    #[test]
    fn declared_bytes_sums_the_layout() {
        assert_eq!(layout_of(&[("a", 5), ("b", 7)]).declared_bytes(), 12);
    }

    /// A cab that cannot be opened must not read as "the cab has no layout".
    #[test]
    fn a_cab_that_does_not_exist_is_an_error_rather_than_an_absence() {
        let s = Scratch::new("nocab");
        let result = extract_from_cab(Path::new("/nonexistent/data1.cab"), "layout.json", &s.0);
        assert!(matches!(
            result,
            Err(CabError::Failed { .. }) | Err(CabError::NoUnshield) | Err(CabError::Io { .. })
        ));
    }
}
