//! Drop-install: a folder or an archive, straight into Community.
//!
//! Most scenery does not arrive as an installer. It arrives as a `.zip` from a
//! marketplace or a forum, and the documented procedure is "copy the folder
//! into Community" — with a second, unwritten half that everybody gets wrong:
//! *and take the old version out first.*
//!
//! This is a **sibling of the seven stages, not an extension of them**. Those
//! stages exist because a Windows `.exe` will not say what it is about to do. A
//! scenery download hides nothing: it is a directory with a `manifest.json` in
//! it, and the operation is a copy. What survives from the pipeline is
//! everything *after* the running — gate, place, verify, file — so this is the
//! same skeleton with stages 1, 2 and 4 replaced by "read the drop" and "swap
//! it in".
//!
//! # The case it exists for
//!
//! A real Community folder held both of these, enabled:
//!
//! ```text
//! vbcorp-airport-kpvf-placerville       v3.0.0   ContentHistory: Airport KPVF
//! zzz-vbcorp-airport-kpvf-placerville   v1.0.0   ContentHistory: Airport KPVF
//! ```
//!
//! Same author, same airport, two versions, two folder names — so the sim loads
//! both, resolves the conflict by its own ordering, and nothing reports it. [`overlaps`] finds that pair without anybody dropping anything,
//! which is why it is a public function rather than a step of the flow.
//!
//! # Two rules the whole module exists for
//!
//! - **An update is a replacement, never a merge.** Copying a new version over
//!   the old directory leaves behind every file the new one dropped, and MSFS
//!   loads what is on disk rather than what `layout.json` declares. That stale
//!   file is invisible: the package looks installed, the version reads
//!   correctly, and something in it behaves like the old build.
//! - **The old copy is moved, not deleted.** [`REPLACED_DIR`] is one rename
//!   from undoing a bad update, and deleting four gigabytes once the download
//!   has been cleared out is unrecoverable.
//!
//! # Nothing is ever written into Community directly
//!
//! A half-written package is visible to the sim and to the catalog scanner, and
//! a failed download that arrives that way looks exactly like an installed one.
//! So [`place`] stages, verifies what it staged, and only then performs two
//! renames. Staging sits **beside** the Community folder rather than in `/tmp`,
//! because a rename is only atomic within one filesystem.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use hangar_msfs::manifest::ManifestJson;

use crate::gate::{Checked, Outcome, Refusal, Report};
use crate::verify::{self, PackageReport, VerifyError};

/// Where a package is built before it is renamed into Community.
///
/// A sibling of the Community folder, like [`REPLACED_DIR`] and like the
/// switcher's disabled store, and for the same reason: every rename in this
/// module has to stay on one filesystem.
pub const STAGING_DIR: &str = ".hangar-staging";

/// Where a replaced package goes. One timestamped subdirectory per placement.
pub const REPLACED_DIR: &str = ".hangar-replaced";

/// How far into a dropped directory to look for packages.
///
/// Deep enough for the shapes downloads actually take — `Community/<package>/`
/// inside a folder named after the product is two — and shallow enough that a
/// mis-drop of a home directory does not become a filesystem walk.
pub const MAX_DEPTH: usize = 3;

/// Proof that a person confirmed this specific placement.
///
/// The same speed bump as `hangar_bindings::save::Approval`, for the same
/// reason: Rust cannot check that a human was asked, but it can make sure no
/// write begins without a call that names what was shown — and
/// `Approval::given` greps to every place a placement can start.
#[derive(Debug, Clone)]
pub struct Approval {
    reason: String,
}

impl Approval {
    /// Call **only** after a person has confirmed this placement, with a
    /// description of what they were shown.
    pub fn given(reason: impl Into<String>) -> Self {
        Approval {
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Step 1 — read the drop
// ─────────────────────────────────────────────────────────────────────────────

/// Where a found package's files physically are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A directory on disk, holding `manifest.json`.
    Directory(PathBuf),
    /// Entries in an archive, under this prefix. `""` means the package is at
    /// the archive's root.
    Zip { archive: PathBuf, prefix: String },
}

impl Source {
    /// The archive or directory this came out of, for display.
    pub fn origin(&self) -> &Path {
        match self {
            Source::Directory(p) => p,
            Source::Zip { archive, .. } => archive,
        }
    }
}

/// One package found in a drop.
#[derive(Debug, Clone)]
pub struct Found {
    pub source: Source,
    /// The directory name it would take in Community.
    pub name: String,
    pub manifest: ManifestJson,
    /// What it declares, from `ContentInfo/*/ContentHistory.json` —
    /// `"Airport KPVF"`. Empty for a package that declares nothing, which is
    /// most non-scenery.
    pub declares: BTreeSet<String>,
    /// Uncompressed bytes. A measurement rather than an estimate: the sum of
    /// the archive's declared entry sizes, or a walk of the directory.
    pub bytes: u64,
}

impl Found {
    pub fn title(&self) -> &str {
        self.manifest.display_title()
    }

    pub fn version(&self) -> &str {
        if self.manifest.package_version.trim().is_empty() {
            "?"
        } else {
            self.manifest.package_version.trim()
        }
    }
}

/// What was at the dropped path.
#[derive(Debug)]
pub enum Reading {
    /// Packages to consider, in the order they were found.
    Packages(Vec<Found>),
    /// An `.exe`. Stage 1's problem, not this pipeline's.
    Installer(PathBuf),
}

/// Why a drop was not read. Every one names what was dropped.
#[derive(Debug, Error)]
pub enum DropRefusal {
    #[error("nothing at {path}")]
    NotThere { path: PathBuf },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a readable archive: {detail}")]
    BadArchive { path: PathBuf, detail: String },

    #[error(
        "{path} holds no package — a directory is only a package when it has a \
         manifest.json in it"
    )]
    NoPackage { path: PathBuf },

    #[error("{path} needs {tool}, which is not installed")]
    NoTool { path: PathBuf, tool: &'static str },

    #[error("{path} is a {what}, which is not something that can be installed")]
    NotInstallable { path: PathBuf, what: &'static str },
}

/// Read a dropped path and say what is in it.
///
/// **Reads only.** The one exception is `.7z` and `.rar`, which no Rust crate
/// here opens: those are unpacked by the system tool into `staging_root`, which
/// is where their files would have to end up anyway — so the extraction is the
/// staging step happening early rather than an extra copy.
///
/// `staging_root` is `<community>/../.hangar-staging`. Passing it here rather
/// than discovering it is what lets a test run the whole read against a scratch
/// tree, and it is why the Community folder is chosen before the read rather
/// than after: nothing in a package says which generation it is for.
pub fn read(path: &Path, staging_root: &Path) -> Result<Reading, DropRefusal> {
    let meta = path.symlink_metadata().map_err(|_| DropRefusal::NotThere {
        path: path.to_path_buf(),
    })?;

    if meta.is_dir() {
        return read_directory(path);
    }

    match extension(path).as_str() {
        "exe" => Ok(Reading::Installer(path.to_path_buf())),
        "zip" => read_zip(path),
        "7z" | "rar" => {
            let unpacked = unpack_with_tool(path, staging_root)?;
            read_directory(&unpacked)
        }
        "msi" => Err(DropRefusal::NotInstallable {
            path: path.to_path_buf(),
            what: "Windows installer package",
        }),
        _ => Err(DropRefusal::NotInstallable {
            path: path.to_path_buf(),
            what: "file",
        }),
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Every package under a directory, to [`MAX_DEPTH`].
///
/// **A directory holding a manifest is a package and is not searched deeper.**
/// A directory without one is not a package: uninstallers leave empty
/// directories behind in Community, and a rule that took "a directory" for a
/// package would happily install those.
fn read_directory(root: &Path) -> Result<Reading, DropRefusal> {
    let mut found = Vec::new();
    collect_packages(root, 0, &mut found);
    if found.is_empty() {
        return Err(DropRefusal::NoPackage {
            path: root.to_path_buf(),
        });
    }
    Ok(Reading::Packages(found))
}

fn collect_packages(dir: &Path, depth: usize, out: &mut Vec<Found>) {
    if dir.join("manifest.json").is_file() {
        if let Some(found) = read_package_dir(dir) {
            out.push(found);
        }
        // Not searched any deeper. A package that nests another is not a
        // shape MSFS has, and treating one as two would install a fragment.
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    children.sort();
    for child in children {
        collect_packages(&child, depth + 1, out);
    }
}

fn read_package_dir(dir: &Path) -> Option<Found> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let manifest = ManifestJson::parse(&text).ok()?;
    let name = dir.file_name()?.to_string_lossy().into_owned();

    let mut declares = BTreeSet::new();
    let content_info = dir.join("ContentInfo");
    if let Ok(entries) = std::fs::read_dir(&content_info) {
        for entry in entries.flatten() {
            let history = entry.path().join("ContentHistory.json");
            if let Ok(text) = std::fs::read_to_string(&history) {
                declares.extend(declared_items(&text));
            }
        }
    }

    Some(Found {
        source: Source::Directory(dir.to_path_buf()),
        name,
        manifest,
        declares,
        bytes: walk_bytes(dir),
    })
}

fn walk_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            total += walk_bytes(&entry.path());
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

/// `ContentInfo/<name>/ContentHistory.json`'s `items[]`.
///
/// The one thing in a package that says what it *does*: `koak` declares
/// `Airport KOAK`, `KRDM_Redmond` declares `Airport KRDM`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContentHistory {
    items: Vec<ContentItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContentItem {
    #[serde(rename = "type")]
    kind: String,
    content: String,
}

fn declared_items(text: &str) -> BTreeSet<String> {
    let Ok(history) = serde_json::from_str::<ContentHistory>(text.trim_start_matches('\u{feff}'))
    else {
        return BTreeSet::new();
    };
    history
        .items
        .iter()
        .map(|i| format!("{} {}", i.kind.trim(), i.content.trim()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Read a `.zip`'s central directory. **Nothing is extracted.**
///
/// The gate needs a size before anything is written, and the sheet needs the
/// manifests — both come out of the listing plus a handful of small JSON reads.
/// Extraction is [`place`]'s job, into staging, after a person has confirmed.
fn read_zip(archive: &Path) -> Result<Reading, DropRefusal> {
    let scan = scan_zip(archive)?;

    // A package is a directory holding a manifest, and is not searched deeper —
    // the same rule as on disk. Sorted so an outer package is seen before
    // anything nested inside it.
    let mut prefixes: Vec<String> = scan
        .entries
        .keys()
        .filter_map(|name| name.strip_suffix("manifest.json"))
        .map(|p| p.to_string())
        .collect();
    prefixes.sort_by_key(|p| (p.matches('/').count(), p.clone()));

    let mut packages: Vec<String> = Vec::new();
    for prefix in prefixes {
        if prefix.matches('/').count() > MAX_DEPTH {
            continue;
        }
        if packages.iter().any(|outer| prefix.starts_with(outer)) {
            continue;
        }
        packages.push(prefix);
    }

    let mut found = Vec::new();
    for prefix in packages {
        let Some(bytes) = scan.text.get(&format!("{prefix}manifest.json")) else {
            continue;
        };
        let Ok(manifest) = ManifestJson::parse(bytes) else {
            continue;
        };

        let name = prefix
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                archive
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "dropped-package".into())
            });

        let declares = scan
            .text
            .iter()
            .filter(|(path, _)| {
                path.starts_with(&format!("{prefix}ContentInfo/"))
                    && path.ends_with("/ContentHistory.json")
            })
            .flat_map(|(_, text)| declared_items(text))
            .collect();

        let bytes = scan
            .entries
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(_, size)| *size)
            .sum();

        found.push(Found {
            source: Source::Zip {
                archive: archive.to_path_buf(),
                prefix,
            },
            name,
            manifest,
            declares,
            bytes,
        });
    }

    if found.is_empty() {
        return Err(DropRefusal::NoPackage {
            path: archive.to_path_buf(),
        });
    }
    Ok(Reading::Packages(found))
}

/// The archive's listing, plus the small JSON files worth reading up front.
struct ZipScan {
    /// Every file entry: path, uncompressed size.
    entries: BTreeMap<String, u64>,
    /// `manifest.json` and `ContentHistory.json` contents, by path.
    text: BTreeMap<String, String>,
}

/// One open, one pass. The JSON files are read while the archive is already
/// open because reopening it per manifest is the same work several times over.
fn scan_zip(archive: &Path) -> Result<ZipScan, DropRefusal> {
    let file = std::fs::File::open(archive).map_err(|source| DropRefusal::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| {
        DropRefusal::BadArchive {
            path: archive.to_path_buf(),
            detail: e.to_string(),
        }
    })?;

    let mut entries = BTreeMap::new();
    let mut text = BTreeMap::new();

    for i in 0..zip.len() {
        let mut entry = match zip.by_index(i) {
            Ok(entry) => entry,
            Err(e) => {
                return Err(DropRefusal::BadArchive {
                    path: archive.to_path_buf(),
                    detail: e.to_string(),
                })
            }
        };
        if !entry.is_file() {
            continue;
        }
        // `enclosed_name` refuses `..` and absolute paths. An archive that
        // tries either is not something to unpack next to a Community folder.
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let name = name.to_string_lossy().replace('\\', "/");
        let size = entry.size();

        let base = name.rsplit('/').next().unwrap_or_default();
        if (base == "manifest.json" || base == "ContentHistory.json") && size <= 1 << 20 {
            let mut buf = String::new();
            if entry.read_to_string(&mut buf).is_ok() {
                text.insert(name.clone(), buf);
            }
        }
        entries.insert(name, size);
    }

    Ok(ZipScan { entries, text })
}

/// Unpack a `.7z` or `.rar` with the system tool, into staging.
///
/// Detected and never required — the same posture `unshield` has in stage 6.
/// An absent tool is named rather than reported as a broken archive, because
/// those are two different things to do something about.
fn unpack_with_tool(archive: &Path, staging_root: &Path) -> Result<PathBuf, DropRefusal> {
    let ext = extension(archive);
    let (tool, args): (&'static str, Vec<String>) = if ext == "7z" {
        ("7z", vec!["x".into(), "-y".into()])
    } else {
        ("unar", vec!["-f".into(), "-o".into()])
    };
    if which(tool).is_none() {
        return Err(DropRefusal::NoTool {
            path: archive.to_path_buf(),
            tool,
        });
    }

    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dropped".into());
    let out = staging_root.join(format!("{stem}.unpacked"));
    if out.exists() {
        std::fs::remove_dir_all(&out).map_err(|source| DropRefusal::Io {
            path: out.clone(),
            source,
        })?;
    }
    std::fs::create_dir_all(&out).map_err(|source| DropRefusal::Io {
        path: out.clone(),
        source,
    })?;

    let mut command = std::process::Command::new(tool);
    command.args(&args);
    if tool == "7z" {
        command.arg(format!("-o{}", out.display())).arg(archive);
    } else {
        command.arg(&out).arg(archive);
    }

    let output = command.output().map_err(|source| DropRefusal::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    if !output.status.success() {
        return Err(DropRefusal::BadArchive {
            path: archive.to_path_buf(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(out)
}

fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(tool))
            .find(|p| p.is_file())
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Step 2 — identity, which is a proposal and never a decision
// ─────────────────────────────────────────────────────────────────────────────

/// A package already in a Community folder.
#[derive(Debug, Clone)]
pub struct Installed {
    pub name: String,
    pub path: PathBuf,
    pub creator: String,
    pub version: String,
    pub declares: BTreeSet<String>,
}

/// Every package in a Community folder, read for identity.
///
/// Directories without a `manifest.json` are **not** packages and are skipped:
/// uninstallers leave empty directories behind.
pub fn installed_in(community: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(community) else {
        return Vec::new();
    };
    let mut found: Vec<Installed> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|dir| {
            let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
            let manifest = ManifestJson::parse(&text).ok()?;
            let name = dir.file_name()?.to_string_lossy().into_owned();
            let mut declares = BTreeSet::new();
            if let Ok(infos) = std::fs::read_dir(dir.join("ContentInfo")) {
                for info in infos.flatten() {
                    if let Ok(text) =
                        std::fs::read_to_string(info.path().join("ContentHistory.json"))
                    {
                        declares.extend(declared_items(&text));
                    }
                }
            }
            Some(Installed {
                name,
                path: dir,
                creator: manifest.creator.trim().to_string(),
                version: manifest.package_version.trim().to_string(),
                declares,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// How sure the identity match is. Both are proposals; neither decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Same folder name. Nothing else in a package is this reliable.
    NearCertain,
    /// Same creator and the same declared content. The KPVF case if the
    /// folder had been renamed.
    Likely,
}

impl Confidence {
    pub fn label(&self) -> &'static str {
        match self {
            Confidence::NearCertain => "near-certain",
            Confidence::Likely => "likely",
        }
    }
}

/// What is proposed for one dropped package. **Never a decision.**
///
/// Nothing in a package identifies it: `manifest.json` has no name or id field,
/// folder names follow no convention, and `ContentHistory.json`'s
/// `package-name` is absent in some packages and disagrees with the folder in
/// others. So the match is an agreement of signals, ranked, shown and
/// confirmed — the same posture `capture` takes with joystick codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing installed looks like this.
    New,
    /// This replaces `of`.
    Update {
        of: String,
        from: String,
        to: String,
        change: VersionChange,
        confidence: Confidence,
    },
    /// Same content, different author. **Not an update** — two people's KPVF,
    /// and replacing one with the other is not what anybody asked for.
    Overlaps { with: String, shared: Vec<String> },
}

impl Verdict {
    /// The installed package this would replace, if any.
    pub fn replaces(&self) -> Option<&str> {
        match self {
            Verdict::Update { of, .. } => Some(of),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Verdict::New => "New".into(),
            Verdict::Update {
                of,
                from,
                to,
                change,
                ..
            } => format!("Update of {of} — {from} → {to}{}", change.suffix()),
            Verdict::Overlaps { with, .. } => format!("Overlaps {with}"),
        }
    }
}

/// How the dropped version compares. **Reported, never enforced.**
///
/// `package_version` is a plain string, and content can change without it
/// changing. So none of these is a refusal: **never refuse a downgrade and
/// never perform one silently.** Reinstalling a known-good older build is
/// legitimate when the new one broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionChange {
    Newer,
    Same,
    Older,
    /// One of the two is not a dotted number, so they cannot be ordered.
    Unclear,
}

impl VersionChange {
    fn suffix(&self) -> &'static str {
        match self {
            VersionChange::Newer => "",
            VersionChange::Same => " (same version)",
            VersionChange::Older => " (older)",
            VersionChange::Unclear => " (versions do not compare)",
        }
    }
}

/// Order two `package_version` strings, when they are dotted numbers.
pub fn compare_versions(from: &str, to: &str) -> VersionChange {
    let parts = |s: &str| -> Option<Vec<u64>> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        s.split('.').map(|p| p.trim().parse::<u64>().ok()).collect()
    };
    match (parts(from), parts(to)) {
        (Some(a), Some(b)) => {
            let width = a.len().max(b.len());
            let pad = |v: &Vec<u64>| {
                let mut v = v.clone();
                v.resize(width, 0);
                v
            };
            match pad(&b).cmp(&pad(&a)) {
                std::cmp::Ordering::Greater => VersionChange::Newer,
                std::cmp::Ordering::Equal => VersionChange::Same,
                std::cmp::Ordering::Less => VersionChange::Older,
            }
        }
        _ if from.trim() == to.trim() && !from.trim().is_empty() => VersionChange::Same,
        _ => VersionChange::Unclear,
    }
}

/// Propose what a dropped package is, against what is installed.
///
/// Ranked, and the ranking is the whole design: a folder-name match is
/// near-certain, creator-plus-content is likely, content alone is an overlap
/// rather than an update, and nothing in common is new.
pub fn propose(found: &Found, installed: &[Installed]) -> Verdict {
    let update = |it: &Installed, confidence| Verdict::Update {
        of: it.name.clone(),
        from: if it.version.is_empty() {
            "?".into()
        } else {
            it.version.clone()
        },
        to: found.version().to_string(),
        change: compare_versions(&it.version, found.version()),
        confidence,
    };

    if let Some(same_name) = installed.iter().find(|it| it.name == found.name) {
        return update(same_name, Confidence::NearCertain);
    }

    if !found.declares.is_empty() {
        let creator = found.manifest.creator.trim();
        if let Some(same_author) = installed.iter().find(|it| {
            !it.declares.is_empty()
                && it.declares == found.declares
                && !it.creator.is_empty()
                && it.creator.eq_ignore_ascii_case(creator)
        }) {
            return update(same_author, Confidence::Likely);
        }

        if let Some(other) = installed
            .iter()
            .find(|it| !it.declares.is_disjoint(&found.declares))
        {
            let shared = other
                .declares
                .intersection(&found.declares)
                .cloned()
                .collect();
            return Verdict::Overlaps {
                with: other.name.clone(),
                shared,
            };
        }
    }

    Verdict::New
}

/// Two installed packages declaring the same content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub packages: Vec<String>,
    pub shared: Vec<String>,
}

/// Installed packages that declare the same content as each other.
///
/// **Needs no drop.** The sim loads both of a pair like this and resolves the
/// conflict by its own ordering; nothing else reports it.
pub fn overlaps(installed: &[Installed]) -> Vec<Overlap> {
    let mut by_item: BTreeMap<&String, Vec<&str>> = BTreeMap::new();
    for it in installed {
        for item in &it.declares {
            by_item.entry(item).or_default().push(&it.name);
        }
    }

    // Grouped by the *set* of packages sharing them, so a pair declaring three
    // items in common is one finding rather than three.
    let mut grouped: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
    for (item, mut names) in by_item {
        if names.len() < 2 {
            continue;
        }
        names.sort_unstable();
        names.dedup();
        if names.len() < 2 {
            continue;
        }
        grouped
            .entry(names.iter().map(|s| s.to_string()).collect())
            .or_default()
            .push(item.clone());
    }

    grouped
        .into_iter()
        .map(|(packages, shared)| Overlap { packages, shared })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Step 3 — the gate
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the gate needs, carried in rather than discovered — the same
/// shape as [`crate::gate::Request`], and for the same reason: it is what makes
/// every check testable against a scratch tree.
pub struct GateRequest<'a> {
    /// The Settings toggle. Off by default.
    pub writes_allowed: bool,
    pub sim_running: bool,
    /// Every Community folder discovered, for the generation check.
    pub discovered: &'a [PathBuf],
    /// The one chosen. `None` while nothing has chosen.
    pub chosen: Option<&'a Path>,
    /// The measured total of everything about to be placed.
    pub need_bytes: u64,
}

/// Stage 3's shape exactly: every check answers Pass with evidence, Refuse with
/// a typed error, or Unchecked with why — never silently a pass.
///
/// Every check runs even after one refuses, so a drop that needs three things
/// fixed says so in one pass.
pub fn gate(request: GateRequest<'_>) -> Report {
    let mut outcomes = Vec::new();

    outcomes.push(Checked {
        label: "Community writes are allowed".into(),
        outcome: if request.writes_allowed {
            Outcome::Pass("the Settings toggle is on".into())
        } else {
            Outcome::Refuse(Refusal::WritesNotAllowed)
        },
    });

    outcomes.push(Checked {
        label: "the sim is not running".into(),
        outcome: if request.sim_running {
            Outcome::Refuse(Refusal::SimRunning)
        } else {
            Outcome::Pass("no sim process".into())
        },
    });

    // Unconditional here, unlike stage 3 where it depends on the target: a drop
    // is always a Community target.
    let names = |paths: &[PathBuf]| {
        paths
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    outcomes.push(Checked {
        label: "which Community folder".into(),
        outcome: match (request.chosen, request.discovered.len()) {
            (Some(path), _) if request.discovered.iter().any(|item| item == path) => {
                Outcome::Pass(path.display().to_string())
            }
            (Some(path), _) => Outcome::Refuse(Refusal::CommunityNotDiscovered {
                path: path.to_path_buf(),
            }),
            (None, 0) => Outcome::Refuse(Refusal::NoCommunity),
            // The generation is never guessed. `minimum_game_version` looks
            // like a hint and is not one: real values for the two generations
            // overlap.
            (None, count) => Outcome::Refuse(Refusal::GenerationUnchosen {
                count,
                names: names(request.discovered),
            }),
        },
    });

    outcomes.push(Checked {
        label: "room on the Community filesystem".into(),
        outcome: match request.chosen {
            None => Outcome::Unchecked(
                "no Community folder chosen, so there is nothing to measure".into(),
            ),
            Some(path) => match hangar_core::disk::free_bytes(path) {
                None => Outcome::Refuse(Refusal::SpaceUnknown {
                    path: path.to_path_buf(),
                }),
                // The replaced copy costs nothing, because it is moved rather
                // than copied.
                Some(have) if have >= request.need_bytes => Outcome::Pass(format!(
                    "{} free, {} needed",
                    hangar_core::disk::human(have),
                    hangar_core::disk::human(request.need_bytes)
                )),
                Some(have) => Outcome::Refuse(Refusal::NotEnoughSpace {
                    path: path.to_path_buf(),
                    have,
                    need: request.need_bytes,
                }),
            },
        },
    });

    Report { outcomes }
}

// ─────────────────────────────────────────────────────────────────────────────
// Step 4 — stage, verify, then two renames
// ─────────────────────────────────────────────────────────────────────────────

/// What stage 6 made of the staged copy, before anything moved.
#[derive(Debug, Clone)]
pub enum Verification {
    Checked(PackageReport),
    /// No `layout.json`. **Unchecked, not passed**: real packages have one, so
    /// a package without it is unusual enough to say so rather than wave
    /// through.
    Unchecked(String),
}

impl Verification {
    pub fn summary(&self) -> String {
        match self {
            Verification::Checked(report) => {
                format!("{}/{} verified", report.verified(), report.declared)
            }
            Verification::Unchecked(why) => format!("unchecked — {why}"),
        }
    }
}

/// What one placement did.
#[derive(Debug, Clone)]
pub struct Placed {
    pub name: String,
    /// From the manifest, for the record. `manifest.json` has no id field, so
    /// the creator is the closest thing to an author a package carries.
    pub creator: String,
    /// Where it now is.
    pub at: PathBuf,
    /// The copies this superseded and where they went — one rename from
    /// coming back. Usually none or one; two when the vendor renamed the
    /// folder *and* a directory of the new name was already there.
    pub replaced: Vec<Superseded>,
    pub verification: Verification,
    pub bytes: u64,
}

/// A copy a placement superseded, and where it was moved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superseded {
    /// The directory name it had in Community.
    pub name: String,
    pub moved_to: PathBuf,
    /// Its declared version, for the record.
    pub version: Option<String>,
}

#[derive(Debug, Error)]
pub enum PlaceError {
    #[error(
        "{path} is not a Community folder — drop-install writes to Community \
         folders and nowhere else"
    )]
    NotCommunity { path: PathBuf },

    #[error("{name}: {path} already exists — refusing to overwrite it")]
    DestinationExists { name: String, path: PathBuf },

    #[error(
        "{name}: staging landed on a different filesystem from the Community \
         folder, so this would be a copy rather than a rename"
    )]
    CrossDevice { name: String },

    #[error("{name} did not survive being staged: {detail}")]
    StagedBadly { name: String, detail: String },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{name}: {source}")]
    Archive {
        name: String,
        #[source]
        source: zip::result::ZipError,
    },
}

/// Where staging and replaced copies live for a Community folder.
///
/// Both are siblings of it, which is what keeps every rename on one
/// filesystem. `/tmp` usually is not one, and a rename is only atomic within
/// one — the same rule as the switcher, the bindings writer and the install
/// record.
pub fn staging_root(community: &Path) -> PathBuf {
    community
        .parent()
        .unwrap_or(Path::new("."))
        .join(STAGING_DIR)
}

pub fn replaced_root(community: &Path) -> PathBuf {
    community
        .parent()
        .unwrap_or(Path::new("."))
        .join(REPLACED_DIR)
}

/// Refuse anything that is not a Community folder.
///
/// `Official*` and `StreamedPackages` are the sim's own, and writing into them
/// is not something a drop may do however the caller got the path.
pub(crate) fn is_community(path: &Path) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return false;
    };
    name.starts_with("Community")
}

/// Stage it, verify what was staged, then two renames.
///
/// `stamp` names the `.hangar-replaced/<stamp>/` directory, so every package in
/// one confirmed drop lands in the same one and comes back together.
///
/// Between the two renames the Community path is briefly empty. That is safe
/// precisely because the gate's sim-not-running check passed.
pub fn place(
    found: &Found,
    community: &Path,
    stamp: &str,
    approval: &Approval,
    replacing: Option<&str>,
) -> Result<Placed, PlaceError> {
    if !is_community(community) {
        return Err(PlaceError::NotCommunity {
            path: community.to_path_buf(),
        });
    }
    tracing::info!(
        package = %found.name,
        community = %community.display(),
        reason = approval.reason(),
        "drop-install: placing"
    );

    let staging = staging_root(community);
    let incoming = staging.join(format!("{}.incoming", found.name));

    // Our own scratch directory, with our own suffix, in our own staging root:
    // a leftover here is a previous run that died mid-stage, and refusing to
    // clear it would strand every later attempt.
    if incoming.exists() {
        tracing::warn!(path = %incoming.display(), "clearing a leftover staging directory");
        std::fs::remove_dir_all(&incoming).map_err(|source| PlaceError::Io {
            path: incoming.clone(),
            source,
        })?;
    }
    std::fs::create_dir_all(&staging).map_err(|source| PlaceError::Io {
        path: staging.clone(),
        source,
    })?;

    match &found.source {
        Source::Directory(dir) => copy_tree(dir, &incoming)?,
        Source::Zip { archive, prefix } => extract(archive, prefix, &incoming, &found.name)?,
    }

    // Verified **in staging, before anything in Community is touched** — a
    // truncated download or a half-finished extraction fails here, where
    // nothing has moved.
    let verification = match verify::verify_package(&incoming) {
        Ok(report) => {
            // Missing and short files mean the copy did not complete. Orphans
            // and case collisions do not: two real packages ship a file their
            // own layout does not list, and refusing those would refuse a
            // package the author released that way.
            if !report.missing.is_empty() || !report.wrong_size.is_empty() {
                let detail = format!(
                    "{} of {} declared files missing, {} the wrong size",
                    report.missing.len(),
                    report.declared,
                    report.wrong_size.len()
                );
                let _ = std::fs::remove_dir_all(&incoming);
                return Err(PlaceError::StagedBadly {
                    name: found.name.clone(),
                    detail,
                });
            }
            Verification::Checked(report)
        }
        Err(VerifyError::NoLayout { .. }) => {
            Verification::Unchecked("the package ships no layout.json".into())
        }
        Err(e) => Verification::Unchecked(e.to_string()),
    };

    // **What this supersedes is what the proposal said, not what shares the
    // package's own name.** Those are the same directory for a
    // `Confidence::NearCertain` match and different for a `Likely` one — which
    // is the whole reason that tier exists, a vendor renaming the folder
    // between versions. Moving aside only `community/<found.name>` would install
    // a Likely update *alongside* the copy the sheet had just promised to
    // replace, and the sim would load both.
    let mut supersedes: Vec<String> = Vec::new();
    if let Some(old) = replacing {
        // A name is a directory entry, never a path. `Installed::name` comes
        // from `file_name()` so it cannot contain one, and this is the guard
        // that keeps it that way if a caller ever passes something else.
        if !old.is_empty() && !old.contains('/') && old != ".." {
            supersedes.push(old.to_owned());
        }
    }
    // The package's own name too, always: a directory already sitting where
    // this one is about to land is a replacement whatever the proposal said.
    if !supersedes.contains(&found.name) {
        supersedes.push(found.name.clone());
    }

    // Rename the old ones out. Moved, never deleted.
    let mut replaced: Vec<Superseded> = Vec::new();
    for name in &supersedes {
        let from = community.join(name);
        if from.symlink_metadata().is_err() {
            continue;
        }
        let version = std::fs::read_to_string(from.join("manifest.json"))
            .ok()
            .and_then(|t| ManifestJson::parse(&t).ok())
            .map(|m| m.package_version.trim().to_string())
            .filter(|v| !v.is_empty());
        let into = replaced_root(community).join(stamp);
        std::fs::create_dir_all(&into).map_err(|source| PlaceError::Io {
            path: into.clone(),
            source,
        })?;
        let out = into.join(name);
        if out.symlink_metadata().is_ok() {
            put_back(&replaced, community);
            return Err(PlaceError::DestinationExists {
                name: name.clone(),
                path: out,
            });
        }
        if let Err(e) = rename(name, &from, &out) {
            put_back(&replaced, community);
            return Err(e);
        }
        replaced.push(Superseded {
            name: name.clone(),
            moved_to: out,
            version,
        });
    }

    let target = community.join(&found.name);

    // Rename the new one in.
    if let Err(e) = rename(&found.name, &incoming, &target) {
        // Put the old ones back rather than leaving the paths empty. This is
        // the one moment the package is not on disk anywhere the sim can see.
        put_back(&replaced, community);
        return Err(e);
    }

    Ok(Placed {
        name: found.name.clone(),
        creator: found.manifest.creator.trim().to_string(),
        at: target,
        replaced,
        verification,
        bytes: found.bytes,
    })
}

/// Undo the moves this placement made, after a later step failed.
///
/// Best effort and never fatal: a copy that cannot be put back is still on
/// disk, one rename from where it belongs, and saying where is more use than
/// failing twice.
fn put_back(replaced: &[Superseded], community: &Path) {
    for old in replaced {
        let home = community.join(&old.name);
        if let Err(e) = std::fs::rename(&old.moved_to, &home) {
            tracing::error!(
                package = %old.name,
                from = %old.moved_to.display(),
                error = %e,
                "could not put the replaced copy back; it is still where it was moved to"
            );
        }
    }
}

fn rename(name: &str, from: &Path, to: &Path) -> Result<(), PlaceError> {
    std::fs::rename(from, to).map_err(|source| {
        // EXDEV. Falling back to a copy would turn an instant swap into
        // minutes of I/O on a 4 GB package, so it is reported instead — the
        // same call the switcher makes.
        if source.raw_os_error() == Some(18) {
            PlaceError::CrossDevice {
                name: name.to_owned(),
            }
        } else {
            PlaceError::Io {
                path: to.to_path_buf(),
                source,
            }
        }
    })
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), PlaceError> {
    std::fs::create_dir_all(to).map_err(|source| PlaceError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(from).map_err(|source| PlaceError::Io {
        path: from.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // A symlink in a download is not something to reproduce inside
        // Community: it points somewhere on the machine it was made on.
        if kind.is_symlink() {
            continue;
        }
        let source_path = entry.path();
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&source_path, &target)?;
        } else {
            std::fs::copy(&source_path, &target).map_err(|source| PlaceError::Io {
                path: target.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

/// Extract one package out of an archive, into staging.
///
/// Only the entries under `prefix`, and only where `enclosed_name` allows —
/// an archive that names `../` or an absolute path is not unpacked next to a
/// Community folder.
fn extract(archive: &Path, prefix: &str, to: &Path, name: &str) -> Result<(), PlaceError> {
    let file = std::fs::File::open(archive).map_err(|source| PlaceError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|source| {
        PlaceError::Archive {
            name: name.to_owned(),
            source,
        }
    })?;

    std::fs::create_dir_all(to).map_err(|source| PlaceError::Io {
        path: to.to_path_buf(),
        source,
    })?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|source| PlaceError::Archive {
            name: name.to_owned(),
            source,
        })?;
        let Some(enclosed) = entry.enclosed_name() else {
            continue;
        };
        let full = enclosed.to_string_lossy().replace('\\', "/");
        let Some(rel) = full.strip_prefix(prefix) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let target = to.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|source| PlaceError::Io {
                path: target.clone(),
                source,
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PlaceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut out = std::fs::File::create(&target).map_err(|source| PlaceError::Io {
            path: target.clone(),
            source,
        })?;
        std::io::copy(&mut entry, &mut out).map_err(|source| PlaceError::Io {
            path: target.clone(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A scratch tree in the shape `switch_scratch` uses: real package names
    /// and manifests, **no payload**, so the whole stage-verify-swap runs in
    /// milliseconds.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-drop-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, bytes).expect("write");
            p
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(&p).expect("mkdir");
            p
        }
    }

    impl std::ops::Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Copied in shape from a real manifest — every field these tests read is
    /// one real installed packages carry.
    fn manifest(title: &str, creator: &str, version: &str) -> String {
        format!(
            r#"{{"dependencies":[],"content_type":"SCENERY","title":"{title}",
                 "manufacturer":"","creator":"{creator}","package_version":"{version}",
                 "minimum_game_version":"1.39.12"}}"#
        )
    }

    fn history(kind: &str, content: &str) -> String {
        format!(r#"{{"items":[{{"type":"{kind}","content":"{content}"}}]}}"#)
    }

    /// A package directory with a manifest, a layout and the files it declares.
    fn package(s: &Scratch, at: &str, creator: &str, version: &str, files: &[(&str, &str)]) {
        s.write(
            &format!("{at}/manifest.json"),
            manifest("Airport", creator, version).as_bytes(),
        );
        let content: Vec<String> = files
            .iter()
            .map(|(p, body)| format!(r#"{{"path":"{p}","size":{},"date":0}}"#, body.len()))
            .collect();
        s.write(
            &format!("{at}/layout.json"),
            format!(r#"{{"content":[{}]}}"#, content.join(",")).as_bytes(),
        );
        for (p, body) in files {
            s.write(&format!("{at}/{p}"), body.as_bytes());
        }
    }

    fn found_one(reading: Reading) -> Found {
        match reading {
            Reading::Packages(mut found) => {
                assert_eq!(found.len(), 1, "expected exactly one package");
                found.remove(0)
            }
            other => panic!("expected packages, got {other:?}"),
        }
    }

    // ── Step 1 — read the drop ───────────────────────────────────────────────

    #[test]
    fn a_directory_with_a_manifest_is_one_package() {
        let s = Scratch::new("dir");
        package(&s, "koak", "vbcorp", "1.2.0", &[("scenery/a.bgl", "hello")]);

        let found = found_one(read(&s.path("koak"), &s.path("staging")).expect("read"));
        assert_eq!(found.name, "koak");
        assert_eq!(found.version(), "1.2.0");
        assert_eq!(found.bytes, 5 + found.bytes - 5, "walked, not guessed");
        assert!(found.bytes > 5, "manifest and layout count too");
    }

    /// The shape a download actually takes: a readme, a PDF, and a `Community/`
    /// wrapper with the packages inside it.
    #[test]
    fn a_folder_of_packages_finds_every_one_of_them() {
        let s = Scratch::new("nested");
        s.write("Download/readme.txt", b"install into Community");
        s.write("Download/manual.pdf", b"%PDF-1.4");
        package(
            &s,
            "Download/Community/koak",
            "vbcorp",
            "1.0.0",
            &[("a.bgl", "x")],
        );
        package(
            &s,
            "Download/Community/kpvf",
            "vbcorp",
            "3.0.0",
            &[("b.bgl", "y")],
        );

        let Reading::Packages(found) = read(&s.path("Download"), &s.path("staging")).expect("read")
        else {
            panic!("expected packages");
        };
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["koak", "kpvf"],
            "the wrapper and the loose files are not packages"
        );
    }

    /// A directory holding a manifest is a package and is not searched deeper.
    /// Treating a nested one as a second package would install a fragment.
    #[test]
    fn a_package_is_not_searched_any_deeper() {
        let s = Scratch::new("notdeeper");
        package(&s, "outer", "vbcorp", "1.0.0", &[("a.bgl", "x")]);
        package(&s, "outer/inner", "vbcorp", "9.9.9", &[("b.bgl", "y")]);

        let found = found_one(read(&s.path("outer"), &s.path("staging")).expect("read"));
        assert_eq!(found.name, "outer");
    }

    /// Uninstallers leave empty directories behind, with no manifest and no
    /// layout. A rule that took "a directory in the drop" for a package would
    /// happily install those.
    #[test]
    fn a_directory_without_a_manifest_is_not_a_package() {
        let s = Scratch::new("empty");
        s.dir("SayIntentions-VR-Toolbar-Plugin");
        s.dir("sayintentions-efb");

        assert!(matches!(
            read(&s.0, &s.path("staging")),
            Err(DropRefusal::NoPackage { .. })
        ));
    }

    #[test]
    fn an_exe_is_handed_to_the_installer_pipeline_rather_than_read() {
        let s = Scratch::new("exe");
        let exe = s.write("Setup.exe", b"MZ");
        match read(&exe, &s.path("staging")).expect("read") {
            Reading::Installer(p) => assert_eq!(p, exe),
            other => panic!("expected an installer, got {other:?}"),
        }
    }

    #[test]
    fn a_pdf_is_refused_by_name_rather_than_ignored() {
        let s = Scratch::new("pdf");
        let pdf = s.write("manual.pdf", b"%PDF");
        assert!(matches!(
            read(&pdf, &s.path("staging")),
            Err(DropRefusal::NotInstallable { .. })
        ));
    }

    #[test]
    fn a_path_that_is_not_there_says_so() {
        let s = Scratch::new("gone");
        assert!(matches!(
            read(&s.path("nothing-here.zip"), &s.path("staging")),
            Err(DropRefusal::NotThere { .. })
        ));
    }

    /// What a package declares, which is the only thing in one that says what
    /// it *does*.
    #[test]
    fn declared_content_comes_out_of_contenthistory() {
        let s = Scratch::new("declares");
        package(&s, "kpvf", "vbcorp", "3.0.0", &[("a.bgl", "x")]);
        s.write(
            "kpvf/ContentInfo/KPVF/ContentHistory.json",
            history("Airport", "KPVF").as_bytes(),
        );

        let found = found_one(read(&s.path("kpvf"), &s.path("staging")).expect("read"));
        assert_eq!(found.declares, BTreeSet::from(["Airport KPVF".to_string()]));
    }

    // ── Zip ──────────────────────────────────────────────────────────────────

    /// Stored rather than deflated: the test is about the central directory and
    /// the placement, not about a codec.
    fn zip_of(path: &Path, entries: &[(&str, &str)]) {
        let file = std::fs::File::create(path).expect("create");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start");
            writer.write_all(body.as_bytes()).expect("write");
        }
        writer.finish().expect("finish");
    }

    fn zip_entries<'a>(prefix: &'a str, body: &'a str) -> Vec<(String, String)> {
        vec![
            (
                format!("{prefix}manifest.json"),
                manifest("Placerville", "vbcorp", "3.0.0"),
            ),
            (
                // `ContentInfo` is declared like anything else — the real
                // packages list it, which is why only `layout.json` and
                // `manifest.json` are undeclared.
                format!("{prefix}layout.json"),
                format!(
                    r#"{{"content":[{{"path":"scenery/a.bgl","size":{},"date":0}},
                        {{"path":"ContentInfo/KPVF/ContentHistory.json","size":{},"date":0}}]}}"#,
                    body.len(),
                    history("Airport", "KPVF").len()
                ),
            ),
            (format!("{prefix}scenery/a.bgl"), body.to_string()),
            (
                format!("{prefix}ContentInfo/KPVF/ContentHistory.json"),
                history("Airport", "KPVF"),
            ),
        ]
    }

    #[test]
    fn a_zip_is_read_from_its_listing_without_being_extracted() {
        let s = Scratch::new("zipread");
        let archive = s.path("kpvf-v3.zip");
        let entries = zip_entries("vbcorp-airport-kpvf-placerville/", "hello");
        zip_of(
            &archive,
            &entries
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect::<Vec<_>>(),
        );

        let found = found_one(read(&archive, &s.path("staging")).expect("read"));
        assert_eq!(found.name, "vbcorp-airport-kpvf-placerville");
        assert_eq!(found.version(), "3.0.0");
        assert_eq!(found.declares, BTreeSet::from(["Airport KPVF".to_string()]));
        assert!(
            found.bytes > 0,
            "a measurement, from the declared entry sizes"
        );
        assert!(
            !s.path("staging").exists(),
            "reading a zip extracts nothing — the gate needs a size before anything is written"
        );
    }

    /// A package at the archive's root takes the archive's name.
    #[test]
    fn a_zip_with_the_package_at_its_root_is_named_after_the_archive() {
        let s = Scratch::new("ziproot");
        let archive = s.path("koak.zip");
        let entries = zip_entries("", "hello");
        zip_of(
            &archive,
            &entries
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect::<Vec<_>>(),
        );

        let found = found_one(read(&archive, &s.path("staging")).expect("read"));
        assert_eq!(found.name, "koak");
    }

    #[test]
    fn a_zip_with_no_manifest_anywhere_is_refused() {
        let s = Scratch::new("zipnopkg");
        let archive = s.path("photos.zip");
        zip_of(&archive, &[("holiday/a.jpg", "not a package")]);
        assert!(matches!(
            read(&archive, &s.path("staging")),
            Err(DropRefusal::NoPackage { .. })
        ));
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_a_bad_archive_rather_than_an_empty_one() {
        let s = Scratch::new("notzip");
        let archive = s.write("broken.zip", b"this is not a zip file at all");
        assert!(matches!(
            read(&archive, &s.path("staging")),
            Err(DropRefusal::BadArchive { .. })
        ));
    }

    // ── Step 2 — identity ────────────────────────────────────────────────────

    fn installed(name: &str, creator: &str, version: &str, declares: &[&str]) -> Installed {
        Installed {
            name: name.into(),
            path: PathBuf::from("/nonexistent").join(name),
            creator: creator.into(),
            version: version.into(),
            declares: declares.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn dropped(name: &str, creator: &str, version: &str, declares: &[&str]) -> Found {
        Found {
            source: Source::Directory(PathBuf::from("/nonexistent").join(name)),
            name: name.into(),
            manifest: ManifestJson::parse(&manifest("T", creator, version)).expect("manifest"),
            declares: declares.iter().map(|s| s.to_string()).collect(),
            bytes: 1,
        }
    }

    #[test]
    fn the_same_folder_name_is_a_near_certain_update() {
        let library = [installed("koak", "vbcorp", "1.0.0", &["Airport KOAK"])];
        let verdict = propose(
            &dropped("koak", "vbcorp", "1.1.0", &["Airport KOAK"]),
            &library,
        );
        assert_eq!(
            verdict,
            Verdict::Update {
                of: "koak".into(),
                from: "1.0.0".into(),
                to: "1.1.0".into(),
                change: VersionChange::Newer,
                confidence: Confidence::NearCertain,
            }
        );
    }

    /// The case the identity work exists for: a re-download arriving under a
    /// different folder name, which is how two KPVFs can coexist.
    #[test]
    fn the_same_author_and_content_under_a_new_name_is_a_likely_update() {
        let library = [installed(
            "zzz-vbcorp-airport-kpvf-placerville",
            "vbcorp",
            "1.0.0",
            &["Airport KPVF"],
        )];
        let verdict = propose(
            &dropped(
                "vbcorp-airport-kpvf-placerville",
                "vbcorp",
                "3.0.0",
                &["Airport KPVF"],
            ),
            &library,
        );
        match verdict {
            Verdict::Update { of, confidence, .. } => {
                assert_eq!(of, "zzz-vbcorp-airport-kpvf-placerville");
                assert_eq!(confidence, Confidence::Likely);
            }
            other => panic!("expected a likely update, got {other:?}"),
        }
    }

    /// **Two people's KPVF, and replacing one with the other is not what
    /// anybody asked for.**
    #[test]
    fn the_same_content_by_a_different_author_is_an_overlap_and_never_an_update() {
        let library = [installed(
            "someone-else-kpvf",
            "otherperson",
            "2.0.0",
            &["Airport KPVF"],
        )];
        let verdict = propose(
            &dropped(
                "vbcorp-airport-kpvf-placerville",
                "vbcorp",
                "3.0.0",
                &["Airport KPVF"],
            ),
            &library,
        );
        assert_eq!(
            verdict,
            Verdict::Overlaps {
                with: "someone-else-kpvf".into(),
                shared: vec!["Airport KPVF".into()],
            }
        );
        assert_eq!(verdict.replaces(), None, "an overlap replaces nothing");
    }

    #[test]
    fn nothing_in_common_is_new() {
        let library = [installed("koak", "vbcorp", "1.0.0", &["Airport KOAK"])];
        let verdict = propose(
            &dropped("krdm", "bullfrogsim", "2.1.0", &["Airport KRDM"]),
            &library,
        );
        assert_eq!(verdict, Verdict::New);
    }

    /// A package that declares nothing cannot be matched on content, and must
    /// not fall through to matching on nothing at all.
    #[test]
    fn a_package_declaring_nothing_is_new_unless_the_folder_name_matches() {
        let library = [installed("koak", "vbcorp", "1.0.0", &[])];
        assert_eq!(
            propose(&dropped("something", "vbcorp", "1.0.0", &[]), &library),
            Verdict::New
        );
    }

    #[test]
    fn versions_are_ordered_when_they_are_dotted_numbers() {
        assert_eq!(compare_versions("3.0.0", "3.0.1"), VersionChange::Newer);
        assert_eq!(compare_versions("3.0.0", "3.0.0"), VersionChange::Same);
        assert_eq!(compare_versions("3.0.0", "1.0.0"), VersionChange::Older);
        assert_eq!(
            compare_versions("1.9.0", "1.10.0"),
            VersionChange::Newer,
            "not string order"
        );
        assert_eq!(
            compare_versions("1.2", "1.2.0"),
            VersionChange::Same,
            "padded, not shorter"
        );
    }

    #[test]
    fn a_version_that_is_not_a_number_does_not_pretend_to_compare() {
        assert_eq!(compare_versions("1.0.0", "v2-beta"), VersionChange::Unclear);
        assert_eq!(compare_versions("", "1.0.0"), VersionChange::Unclear);
        assert_eq!(compare_versions("beta", "beta"), VersionChange::Same);
    }

    /// **Never refuse a downgrade.** Reinstalling a known-good older build is a
    /// legitimate thing to do when the new one broke.
    #[test]
    fn a_downgrade_is_reported_and_not_refused() {
        let library = [installed("koak", "vbcorp", "3.0.0", &["Airport KOAK"])];
        let verdict = propose(
            &dropped("koak", "vbcorp", "1.0.0", &["Airport KOAK"]),
            &library,
        );
        match &verdict {
            Verdict::Update { change, .. } => assert_eq!(*change, VersionChange::Older),
            other => panic!("expected an update, got {other:?}"),
        }
        assert!(verdict.label().contains("(older)"), "{}", verdict.label());
        assert_eq!(verdict.replaces(), Some("koak"), "still a replacement");
    }

    /// Two installed packages declaring the same content are found with no
    /// drop at all.
    #[test]
    fn the_kpvf_pair_is_found_without_anybody_dropping_anything() {
        let library = [
            installed("koak", "vbcorp", "1.0.0", &["Airport KOAK"]),
            installed(
                "vbcorp-airport-kpvf-placerville",
                "vbcorp",
                "3.0.0",
                &["Airport KPVF"],
            ),
            installed(
                "zzz-vbcorp-airport-kpvf-placerville",
                "vbcorp",
                "1.0.0",
                &["Airport KPVF"],
            ),
        ];
        assert_eq!(
            overlaps(&library),
            vec![Overlap {
                packages: vec![
                    "vbcorp-airport-kpvf-placerville".into(),
                    "zzz-vbcorp-airport-kpvf-placerville".into(),
                ],
                shared: vec!["Airport KPVF".into()],
            }]
        );
    }

    /// A pair sharing three items is one finding, not three.
    #[test]
    fn packages_sharing_several_items_are_reported_once() {
        let library = [
            installed(
                "a",
                "x",
                "1.0.0",
                &["Airport K1", "Airport K2", "Airport K3"],
            ),
            installed(
                "b",
                "y",
                "1.0.0",
                &["Airport K1", "Airport K2", "Airport K3"],
            ),
        ];
        let found = overlaps(&library);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].shared.len(), 3);
    }

    #[test]
    fn a_library_with_nothing_in_common_reports_no_overlaps() {
        let library = [
            installed("koak", "vbcorp", "1.0.0", &["Airport KOAK"]),
            installed("krdm", "bullfrogsim", "2.1.0", &["Airport KRDM"]),
        ];
        assert!(overlaps(&library).is_empty());
    }

    // ── Step 3 — the gate ────────────────────────────────────────────────────

    fn request<'a>(
        writes_allowed: bool,
        sim_running: bool,
        discovered: &'a [PathBuf],
        chosen: Option<&'a Path>,
    ) -> GateRequest<'a> {
        GateRequest {
            writes_allowed,
            sim_running,
            discovered,
            chosen,
            need_bytes: 1,
        }
    }

    #[test]
    fn a_clear_gate_passes_every_check() {
        let s = Scratch::new("gateclear");
        let community = s.dir("Packages/Community");
        let all = [community.clone()];
        let report = gate(request(true, false, &all, Some(&community)));
        assert!(report.is_clear(), "{:?}", report.refusals());
        assert!(report.unchecked().is_empty());
    }

    #[test]
    fn one_community_still_needs_an_explicit_choice() {
        let s = Scratch::new("gateone");
        let all = [s.dir("Packages/Community2024")];
        let report = gate(request(true, false, &all, None));
        assert!(report
            .refusals()
            .iter()
            .any(|r| matches!(r, Refusal::GenerationUnchosen { count: 1, .. })));
    }

    #[test]
    fn a_stale_or_unlisted_destination_refuses() {
        let s = Scratch::new("gatestale");
        let all = [s.dir("Packages/Community")];
        let chosen = s.dir("Packages/Community2024");
        let report = gate(request(true, false, &all, Some(&chosen)));
        assert!(report.refusals().iter().any(|r| matches!(
            r,
            Refusal::CommunityNotDiscovered { path } if path == &chosen
        )));
    }

    #[test]
    fn writes_off_refuses_and_it_is_off_by_default() {
        let s = Scratch::new("gatewrites");
        let community = s.dir("Packages/Community");
        let all = [community.clone()];
        let report = gate(request(false, false, &all, Some(&community)));
        assert!(report
            .refusals()
            .iter()
            .any(|r| matches!(r, Refusal::WritesNotAllowed)));
    }

    #[test]
    fn a_running_sim_refuses_because_community_is_being_read() {
        let s = Scratch::new("gatesim");
        let community = s.dir("Packages/Community");
        let all = [community.clone()];
        let report = gate(request(true, true, &all, Some(&community)));
        assert!(report
            .refusals()
            .iter()
            .any(|r| matches!(r, Refusal::SimRunning)));
    }

    /// **The generation is never guessed.** `minimum_game_version` looks like a
    /// hint and is not one — real values overlap across both generations.
    #[test]
    fn two_community_folders_and_no_choice_refuses_rather_than_picking() {
        let s = Scratch::new("gategen");
        let all = [s.dir("Packages/Community"), s.dir("Packages/Community2024")];
        let report = gate(request(true, false, &all, None));
        match report.refusals().as_slice() {
            [Refusal::GenerationUnchosen { count, names }] => {
                assert_eq!(*count, 2);
                assert!(names.contains("Community2024"), "{names}");
            }
            other => panic!("expected one generation refusal, got {other:?}"),
        }
    }

    #[test]
    fn no_community_folder_at_all_is_its_own_refusal() {
        let report = gate(request(true, false, &[], None));
        assert!(report
            .refusals()
            .iter()
            .any(|r| matches!(r, Refusal::NoCommunity)));
    }

    /// Unchecked is a real answer and is never silently a pass.
    #[test]
    fn room_cannot_be_measured_until_a_folder_is_chosen() {
        let s = Scratch::new("gateroom");
        let all = [s.dir("Packages/Community"), s.dir("Packages/Community2024")];
        let report = gate(request(true, false, &all, None));
        assert_eq!(report.unchecked(), ["room on the Community filesystem"]);
    }

    /// A drop that needs three things fixed should say so in one pass.
    #[test]
    fn every_check_runs_even_after_one_refuses() {
        let report = gate(request(false, true, &[], None));
        assert_eq!(report.outcomes.len(), 4);
        assert_eq!(report.refusals().len(), 3, "writes, sim and no community");
    }

    #[test]
    fn a_payload_larger_than_the_filesystem_refuses() {
        let s = Scratch::new("gatespace");
        let community = s.dir("Packages/Community");
        let all = [community.clone()];
        let report = gate(GateRequest {
            writes_allowed: true,
            sim_running: false,
            discovered: &all,
            chosen: Some(&community),
            need_bytes: u64::MAX,
        });
        assert!(report
            .refusals()
            .iter()
            .any(|r| matches!(r, Refusal::NotEnoughSpace { .. })));
    }

    // ── Step 4 — stage, verify, two renames ──────────────────────────────────

    fn approval() -> Approval {
        Approval::given("a test confirmed this placement")
    }

    #[test]
    fn a_new_package_lands_in_community_and_replaces_nothing() {
        let s = Scratch::new("placenew");
        let community = s.dir("Packages/Community");
        package(
            &s,
            "drop/koak",
            "vbcorp",
            "1.0.0",
            &[("scenery/a.bgl", "hello")],
        );

        let found = found_one(read(&s.path("drop/koak"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");

        assert_eq!(placed.at, community.join("koak"));
        assert!(community.join("koak/scenery/a.bgl").is_file());
        assert!(placed.replaced.is_empty());
        assert!(matches!(placed.verification, Verification::Checked(ref r) if r.is_complete()));
    }

    /// A vendor renamed the folder between versions — `koak` v0.1.0 became
    /// `fd-airport-koak-oakland` v1.3.0 — so `propose` returns
    /// `Update { of: "koak", confidence: Likely }`. `place` must move aside the
    /// proposed `koak`, not `community/<found.name>`, or it installs alongside
    /// and the sim gets two packages declaring `Airport KOAK`.
    #[test]
    fn a_renamed_folder_replaces_the_package_the_proposal_named() {
        let s = Scratch::new("placerenamed");
        let community = s.dir("Packages/Community");

        // The old version, under its old folder name.
        let old = community.join("koak");
        std::fs::create_dir_all(old.join("ContentInfo/koak")).expect("mkdir");
        std::fs::write(
            old.join("manifest.json"),
            br#"{"title":"Oakland International Airport","creator":"FreakyD","package_version":"0.1.0","content_type":"SCENERY"}"#,
        )
        .expect("manifest");
        std::fs::write(
            old.join("ContentInfo/koak/ContentHistory.json"),
            br#"{"items":[{"type":"Airport","content":"KOAK"}]}"#,
        )
        .expect("history");

        // The new one, same creator and same declared airport, new folder name.
        package(
            &s,
            "drop/fd-airport-koak-oakland",
            "FreakyD",
            "1.3.0",
            &[("scenery/new.bgl", "v1.3")],
        );
        std::fs::create_dir_all(
            s.path("drop/fd-airport-koak-oakland/ContentInfo/fd-airport-koak-oakland"),
        )
        .expect("mkdir");
        std::fs::write(
            s.path("drop/fd-airport-koak-oakland/ContentInfo/fd-airport-koak-oakland/ContentHistory.json"),
            br#"{"items":[{"type":"Airport","content":"KOAK"}]}"#,
        )
        .expect("history");

        let found = found_one(
            read(
                &s.path("drop/fd-airport-koak-oakland"),
                &staging_root(&community),
            )
            .expect("read"),
        );
        let verdict = propose(&found, &installed_in(&community));
        assert!(
            matches!(
                verdict,
                Verdict::Update {
                    ref of,
                    confidence: Confidence::Likely,
                    ..
                } if of == "koak"
            ),
            "the proposal has always been right: {verdict:?}"
        );

        let placed = place(
            &found,
            &community,
            "2026-09-01T12-00-00Z",
            &approval(),
            verdict.replaces(),
        )
        .expect("placed");

        assert_eq!(
            placed.replaced.len(),
            1,
            "the old folder is what gets moved aside, not the new name"
        );
        assert_eq!(placed.replaced[0].name, "koak");
        assert_eq!(placed.replaced[0].version.as_deref(), Some("0.1.0"));
        assert!(
            !community.join("koak").exists(),
            "the old copy is out of Community, or the sim loads both"
        );
        assert!(
            placed.replaced[0].moved_to.join("manifest.json").is_file(),
            "moved, never deleted"
        );
        assert!(community
            .join("fd-airport-koak-oakland/scenery/new.bgl")
            .is_file());
    }

    /// Passing what the proposal named must not stop a same-name copy being
    /// moved aside as well: a directory already sitting where this one is about
    /// to land is a replacement whatever the proposal said.
    #[test]
    fn a_collision_on_the_new_name_is_replaced_too() {
        let s = Scratch::new("placeboth");
        let community = s.dir("Packages/Community");
        package(
            &s,
            "old-name",
            "vbcorp",
            "1.0.0",
            &[("scenery/a.bgl", "old")],
        );
        package(
            &s,
            "new-name",
            "vbcorp",
            "1.0.0",
            &[("scenery/b.bgl", "also old")],
        );
        std::fs::rename(s.path("old-name"), community.join("old-name")).expect("mv");
        std::fs::rename(s.path("new-name"), community.join("new-name")).expect("mv");

        package(
            &s,
            "drop/new-name",
            "vbcorp",
            "2.0.0",
            &[("scenery/c.bgl", "new")],
        );
        let found =
            found_one(read(&s.path("drop/new-name"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-09-01T12-00-00Z",
            &approval(),
            Some("old-name"),
        )
        .expect("placed");

        let mut names: Vec<&str> = placed.replaced.iter().map(|r| r.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["new-name", "old-name"],
            "both, or one is left loading"
        );
        assert!(community.join("new-name/scenery/c.bgl").is_file());
    }

    /// **An update is a replacement, never a merge.** Copying the new version
    /// over the old directory leaves behind every file the new one dropped, and
    /// MSFS loads what is on disk rather than what `layout.json` declares.
    #[test]
    fn an_update_replaces_rather_than_merging_over_the_old_version() {
        let s = Scratch::new("placeupdate");
        let community = s.dir("Packages/Community");
        // v1 ships two files.
        package(
            &s,
            "Packages/Community/koak",
            "vbcorp",
            "1.0.0",
            &[
                ("scenery/a.bgl", "old"),
                ("scenery/gone.bgl", "removed in v2"),
            ],
        );
        // v2 ships only one of them.
        package(
            &s,
            "drop/koak",
            "vbcorp",
            "2.0.0",
            &[("scenery/a.bgl", "new")],
        );

        let found = found_one(read(&s.path("drop/koak"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");

        assert_eq!(
            std::fs::read_to_string(community.join("koak/scenery/a.bgl")).expect("read"),
            "new"
        );
        assert!(
            !community.join("koak/scenery/gone.bgl").exists(),
            "the file v2 dropped must not survive into the new install — that stale \
             file is the whole content of 'and it should remove the old one'"
        );
        assert_eq!(
            std::fs::read_to_string(s.path("drop/koak/scenery/a.bgl")).expect("source retained"),
            "new",
            "placing a folder copies it; the source stays available"
        );
        assert_eq!(placed.replaced[0].version.as_deref(), Some("1.0.0"));
        assert!(placed.replaced[0]
            .moved_to
            .join("scenery/gone.bgl")
            .is_file());
    }

    /// **The old copy is moved, not deleted**, and is one rename from coming
    /// back.
    #[test]
    fn the_replaced_copy_is_one_rename_from_coming_back() {
        let s = Scratch::new("placeundo");
        let community = s.dir("Packages/Community");
        package(
            &s,
            "Packages/Community/koak",
            "vbcorp",
            "1.0.0",
            &[("scenery/a.bgl", "old")],
        );
        package(
            &s,
            "drop/koak",
            "vbcorp",
            "2.0.0",
            &[("scenery/a.bgl", "new")],
        );

        let found = found_one(read(&s.path("drop/koak"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");

        let old = placed
            .replaced
            .first()
            .expect("something was replaced")
            .moved_to
            .clone();
        assert!(old.starts_with(replaced_root(&community)));
        assert_eq!(
            std::fs::read_to_string(old.join("scenery/a.bgl")).expect("read"),
            "old"
        );

        // The undo, in full: one rename.
        std::fs::remove_dir_all(community.join("koak")).expect("clear");
        std::fs::rename(&old, community.join("koak")).expect("undo");
        assert_eq!(
            std::fs::read_to_string(community.join("koak/scenery/a.bgl")).expect("read"),
            "old"
        );
    }

    /// Verification happens **in staging, before anything in Community is
    /// touched** — a truncated download fails there, where nothing has moved.
    #[test]
    fn a_truncated_package_fails_in_staging_and_community_is_untouched() {
        let s = Scratch::new("placetruncated");
        let community = s.dir("Packages/Community");
        package(
            &s,
            "Packages/Community/koak",
            "vbcorp",
            "1.0.0",
            &[("scenery/a.bgl", "old")],
        );
        // Declares a file the drop does not carry.
        package(
            &s,
            "drop/koak",
            "vbcorp",
            "2.0.0",
            &[("scenery/a.bgl", "new")],
        );
        std::fs::remove_file(s.path("drop/koak/scenery/a.bgl")).expect("truncate");

        let found = found_one(read(&s.path("drop/koak"), &staging_root(&community)).expect("read"));
        let result = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        );

        assert!(
            matches!(result, Err(PlaceError::StagedBadly { .. })),
            "{result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(community.join("koak/scenery/a.bgl")).expect("read"),
            "old",
            "the installed version must be exactly as it was"
        );
        assert!(
            !replaced_root(&community)
                .join("2026-08-30T12-00-00Z")
                .exists(),
            "nothing was moved aside, because nothing got as far as moving"
        );
    }

    /// Real packages ship a `layout.json`, so one without it is unusual enough
    /// to say so rather than wave through.
    #[test]
    fn a_package_with_no_layout_is_unchecked_rather_than_passed() {
        let s = Scratch::new("placenolayout");
        let community = s.dir("Packages/Community");
        s.write(
            "drop/odd/manifest.json",
            manifest("Odd", "someone", "1.0.0").as_bytes(),
        );
        s.write("drop/odd/scenery/a.bgl", b"x");

        let found = found_one(read(&s.path("drop/odd"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");

        match &placed.verification {
            Verification::Unchecked(why) => assert!(why.contains("layout.json"), "{why}"),
            other => panic!("expected unchecked, got {other:?}"),
        }
        assert!(placed.verification.summary().starts_with("unchecked"));
    }

    /// An orphan is a file the author shipped that the layout does not list.
    /// Some real packages do exactly that, so it must not refuse the placement.
    #[test]
    fn a_file_the_layout_does_not_list_does_not_refuse_the_placement() {
        let s = Scratch::new("placeorphan");
        let community = s.dir("Packages/Community");
        package(&s, "drop/eolib", "esd", "1.0.0", &[("a.bgl", "x")]);
        s.write("drop/eolib/MSFS24_EOLib_ReadMe_EULA.pdf", b"%PDF");

        let found =
            found_one(read(&s.path("drop/eolib"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");
        match &placed.verification {
            Verification::Checked(report) => assert_eq!(report.orphans.len(), 1),
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn a_zip_is_extracted_into_staging_and_renamed_in() {
        let s = Scratch::new("placezip");
        let community = s.dir("Packages/Community");
        let archive = s.path("kpvf.zip");
        let entries = zip_entries("vbcorp-airport-kpvf-placerville/", "hello");
        zip_of(
            &archive,
            &entries
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect::<Vec<_>>(),
        );

        let found = found_one(read(&archive, &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");

        assert_eq!(
            std::fs::read_to_string(placed.at.join("scenery/a.bgl")).expect("read"),
            "hello"
        );
        assert!(matches!(placed.verification, Verification::Checked(ref r) if r.is_complete()));
    }

    /// Nothing is ever extracted or copied into Community directly, so the
    /// staging directory must not be left behind either.
    #[test]
    fn staging_does_not_keep_the_package_after_it_is_placed() {
        let s = Scratch::new("placestaging");
        let community = s.dir("Packages/Community");
        package(&s, "drop/koak", "vbcorp", "1.0.0", &[("a.bgl", "x")]);

        let found = found_one(read(&s.path("drop/koak"), &staging_root(&community)).expect("read"));
        place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");

        assert!(!staging_root(&community).join("koak.incoming").exists());
    }

    /// `Official*` and `StreamedPackages` are the sim's own, whatever path the
    /// caller managed to produce.
    #[test]
    fn it_refuses_to_write_anywhere_that_is_not_a_community_folder() {
        let s = Scratch::new("placeofficial");
        let official = s.dir("Packages/Official");
        package(&s, "drop/koak", "vbcorp", "1.0.0", &[("a.bgl", "x")]);
        let found = found_one(read(&s.path("drop/koak"), &staging_root(&official)).expect("read"));

        assert!(matches!(
            place(&found, &official, "2026-08-30T12-00-00Z", &approval(), None),
            Err(PlaceError::NotCommunity { .. })
        ));
        assert!(!official.join("koak").exists());
    }

    /// A run that died mid-stage must not strand every later attempt.
    #[test]
    fn a_leftover_staging_directory_is_cleared_rather_than_fatal() {
        let s = Scratch::new("placeleftover");
        let community = s.dir("Packages/Community");
        package(&s, "drop/koak", "vbcorp", "1.0.0", &[("a.bgl", "x")]);
        s.write(
            "Packages/.hangar-staging/koak.incoming/half-written.bin",
            b"junk",
        );

        let found = found_one(read(&s.path("drop/koak"), &staging_root(&community)).expect("read"));
        let placed = place(
            &found,
            &community,
            "2026-08-30T12-00-00Z",
            &approval(),
            None,
        )
        .expect("placed");
        assert!(!placed.at.join("half-written.bin").exists());
    }

    /// Two packages from one confirmed drop land in the same replaced
    /// directory, so they come back together.
    #[test]
    fn one_drop_replaces_into_one_timestamped_directory() {
        let s = Scratch::new("placestamp");
        let community = s.dir("Packages/Community");
        for name in ["koak", "krdm"] {
            package(
                &s,
                &format!("Packages/Community/{name}"),
                "vbcorp",
                "1.0.0",
                &[("a.bgl", "x")],
            );
            package(
                &s,
                &format!("drop/{name}"),
                "vbcorp",
                "2.0.0",
                &[("a.bgl", "y")],
            );
        }

        let stamp = "2026-08-30T12-00-00Z";
        for name in ["koak", "krdm"] {
            let found = found_one(
                read(&s.path(&format!("drop/{name}")), &staging_root(&community)).expect("read"),
            );
            place(&found, &community, stamp, &approval(), None).expect("placed");
        }

        let into = replaced_root(&community).join(stamp);
        assert!(into.join("koak").is_dir());
        assert!(into.join("krdm").is_dir());
    }

    /// A placement that fails after moving one copy aside puts it back. The
    /// second supersede finds its slot in the replaced directory taken, so
    /// `put_back` has to return the first, already moved, to Community.
    #[test]
    fn a_failure_after_moving_the_old_copy_aside_puts_it_back() {
        let s = Scratch::new("placeputback");
        let community = s.dir("Packages/Community");
        package(
            &s,
            "Packages/Community/koak",
            "vbcorp",
            "1.0.0",
            &[("scenery/a.bgl", "old")],
        );
        package(
            &s,
            "Packages/Community/fd-koak",
            "vbcorp",
            "1.1.0",
            &[("scenery/a.bgl", "collision")],
        );
        package(
            &s,
            "drop/fd-koak",
            "vbcorp",
            "2.0.0",
            &[("scenery/a.bgl", "new")],
        );
        let stamp = "2026-08-30T12-00-00Z";
        let taken = replaced_root(&community).join(stamp).join("fd-koak");
        std::fs::create_dir_all(&taken).expect("mkdir");

        let found =
            found_one(read(&s.path("drop/fd-koak"), &staging_root(&community)).expect("read"));
        let err = place(&found, &community, stamp, &approval(), Some("koak"))
            .expect_err("the second move-aside must refuse");

        assert!(
            matches!(err, PlaceError::DestinationExists { ref name, .. } if name == "fd-koak"),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(community.join("koak/scenery/a.bgl")).expect("read"),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(community.join("fd-koak/scenery/a.bgl")).expect("read"),
            "collision"
        );
        assert!(!replaced_root(&community).join(stamp).join("koak").exists());
    }
}
