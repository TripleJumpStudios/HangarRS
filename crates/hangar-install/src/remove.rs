//! Remove — the one path in HangarRS that deletes an add-on (D15, 2026-11-02).
//!
//! Two layers, kept apart on purpose:
//!
//! * **Package.** One directory with a `manifest.json`, directly inside a
//!   `Community*` folder or switched off into `.hangar-disabled/<Community*>/`.
//!   Removing it deletes that directory and nothing else. A vendor app that put
//!   it there stays installed and simply finds it gone.
//! * **Vendor.** A configured app with a Windows uninstall entry in its prefix.
//!   Removing it runs the vendor's own uninstaller first, and only once the
//!   app's exe is gone deletes the packages tied to it (each through the package
//!   path). HangarRS never deletes a vendor's program files itself: the
//!   uninstaller also takes its registry entry, shortcuts and URL handler, which
//!   a file delete would leave behind.
//!
//! Drop-install's "moved, never deleted" still holds for *updates*.
//!
//! # Deleting without a half-deleted package in Community
//!
//! The directory is first renamed to `<packages>/.hangar-removing/<stamp>/`, a
//! hidden sibling of the Community folder on the same filesystem, so the rename
//! is atomic and the sim stops seeing the package at once. The delete happens
//! there. If it stops partway, what is left sits where the sim never looks, and
//! the error names the path. Nothing depends on cleanup code running.
//!
//! # A link is removed, never followed
//!
//! A package can be a symlink into a vendor library (Orbx's usual layout).
//! Removing it removes the link: the package leaves the game and Community, and
//! the library copy stays for the vendor app to remove (decided 2026-11-02, R1).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::drop::Approval;
use crate::record::InstallRecord;
use crate::replacements::{tree, Identity};

/// Where a package goes for the moment between leaving Community and being
/// deleted. A sibling of the Community folder, like staging and the replaced store.
pub const REMOVING_DIR: &str = ".hangar-removing";

/// The switcher's hidden folder (`hangar_catalog::switcher::DISABLED_DIR`).
const DISABLED_DIR: &str = ".hangar-disabled";

#[derive(Debug, Error)]
pub enum Error {
    #[error("the package could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("this is not a package in a Community folder; nothing was deleted")]
    NotAPackage,
    #[error("the package contains a link or a special file; nothing was deleted")]
    Unsafe,
    #[error("the package changed since it was reviewed; review it again")]
    Changed,
    #[error("removing needs the Community write toggle on and the simulator stopped")]
    Refused,
    #[error("the package left Community, but deleting it stopped partway: {left_at} ({reason})")]
    Partial { left_at: PathBuf, reason: String },
}

impl From<crate::replacements::Error> for Error {
    fn from(e: crate::replacements::Error) -> Self {
        match e {
            crate::replacements::Error::Io(io) => Error::Io(io),
            _ => Error::Unsafe,
        }
    }
}

/// What a package is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A real directory. Removing it deletes it.
    Directory,
    /// A symlink into a library elsewhere. Removing it deletes the link only.
    Link { target: PathBuf },
}

/// One package, read and measured for a confirm sheet.
#[derive(Debug, Clone)]
pub struct Target {
    /// The directory entry: always one path component.
    pub name: String,
    pub path: PathBuf,
    /// The discovered Community folder this belongs to.
    pub community: PathBuf,
    pub shape: Shape,
    /// Measured by walking the tree; 0 for a link.
    pub bytes: u64,
    entries: BTreeMap<PathBuf, Identity>,
}

impl Target {
    /// Read `path` as a package of `community`, refusing anything that is not one.
    ///
    /// `path` must be directly inside `community`, or inside
    /// `.hangar-disabled/<community name>/` beside it. Never anywhere else,
    /// however the caller came by the path.
    pub fn inspect(path: &Path, community: &Path) -> Result<Target, Error> {
        if !crate::drop::is_community(community) {
            return Err(Error::NotAPackage);
        }
        let name = single_component(path).ok_or(Error::NotAPackage)?;
        let parent = path.parent().ok_or(Error::NotAPackage)?;
        if !is_allowed_parent(parent, community) {
            return Err(Error::NotAPackage);
        }
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Ok(Target {
                name,
                path: path.to_path_buf(),
                community: community.to_path_buf(),
                shape: Shape::Link {
                    target: fs::read_link(path)?,
                },
                bytes: 0,
                entries: BTreeMap::new(),
            });
        }
        if !meta.is_dir() || !path.join("manifest.json").is_file() {
            return Err(Error::NotAPackage);
        }
        let mut entries = BTreeMap::new();
        let mut bytes = 0;
        tree(path, &mut entries, &mut bytes)?;
        Ok(Target {
            name,
            path: path.to_path_buf(),
            community: community.to_path_buf(),
            shape: Shape::Directory,
            bytes,
            entries,
        })
    }

    /// Files and directories counted by the walk (the package itself included).
    pub fn entries(&self) -> usize {
        self.entries.len()
    }
}

fn single_component(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?.to_string();
    let ok = !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']);
    ok.then_some(name)
}

fn is_allowed_parent(parent: &Path, community: &Path) -> bool {
    if parent == community {
        return true;
    }
    let (Some(packages), Some(community_name)) = (community.parent(), community.file_name()) else {
        return false;
    };
    parent == packages.join(DISABLED_DIR).join(community_name)
}

/// What a removal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub name: String,
    pub bytes: u64,
    pub was_link: bool,
}

/// Delete one reviewed package. The guards are here, on the action.
///
/// `approval` is what the person was shown and confirmed. `stamp` names the
/// `.hangar-removing/<stamp>/` directory used for the moment before the delete.
pub fn delete(
    target: &Target,
    writes_allowed: bool,
    sim_running: bool,
    approval: &Approval,
    stamp: &str,
) -> Result<Removed, Error> {
    if !writes_allowed || sim_running || approval.reason().trim().is_empty() {
        return Err(Error::Refused);
    }
    // Re-derive everything from the reviewed values; a target is never trusted
    // to still describe the disk.
    if !is_allowed_parent(
        target.path.parent().ok_or(Error::NotAPackage)?,
        &target.community,
    ) || single_component(&target.path).as_deref() != Some(target.name.as_str())
    {
        return Err(Error::NotAPackage);
    }
    let meta = fs::symlink_metadata(&target.path)?;
    match &target.shape {
        Shape::Link { target: to } => {
            if !meta.file_type().is_symlink() || &fs::read_link(&target.path)? != to {
                return Err(Error::Changed);
            }
            fs::remove_file(&target.path)?;
            Ok(Removed {
                name: target.name.clone(),
                bytes: 0,
                was_link: true,
            })
        }
        Shape::Directory => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(Error::Changed);
            }
            let mut entries = BTreeMap::new();
            let mut bytes = 0;
            tree(&target.path, &mut entries, &mut bytes)?;
            if entries != target.entries {
                return Err(Error::Changed);
            }
            let packages = target.community.parent().ok_or(Error::NotAPackage)?;
            let holding = packages.join(REMOVING_DIR).join(stamp);
            fs::create_dir_all(&holding)?;
            let moved = holding.join(&target.name);
            if moved.exists() {
                return Err(Error::Changed);
            }
            fs::rename(&target.path, &moved)?;
            if let Err(e) = fs::remove_dir_all(&moved) {
                return Err(Error::Partial {
                    left_at: moved,
                    reason: e.to_string(),
                });
            }
            // Empty holding directories are tidied; a failure here leaves an
            // empty hidden folder and is not worth reporting.
            let _ = fs::remove_dir(&holding);
            let _ = fs::remove_dir(packages.join(REMOVING_DIR));
            Ok(Removed {
                name: target.name.clone(),
                bytes: target.bytes,
                was_link: false,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Vendor layer
// ---------------------------------------------------------------------------

/// A Windows uninstall entry, read from the prefix's registry files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Uninstaller {
    /// The key under `…\CurrentVersion\Uninstall\`.
    pub key: String,
    pub display_name: String,
    pub publisher: String,
    /// `UninstallString`: the interactive form, the one a person would click.
    pub command: String,
    pub install_location: String,
    pub display_icon: String,
}

impl Uninstaller {
    /// The uninstaller's own Windows path and its arguments, from `command`.
    pub fn split_command(&self) -> Option<(String, Vec<String>)> {
        split_command(&self.command)
    }
}

const UNINSTALL_KEYS: [&str; 2] = [
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\",
    r"Software\Wow6432Node\Microsoft\Windows\CurrentVersion\Uninstall\",
];

/// Every uninstall entry in one registry file's text.
pub fn uninstall_entries(text: &str) -> Vec<Uninstaller> {
    let mut out = Vec::new();
    let mut current: Option<Uninstaller> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('[') {
            out.extend(current.take().filter(|e| !e.command.is_empty()));
            let Some(end) = rest.find(']') else { continue };
            let key = rest[..end].replace("\\\\", "\\");
            let lowered = key.to_ascii_lowercase();
            for parent in UNINSTALL_KEYS {
                let parent = parent.to_ascii_lowercase();
                if let Some(child) = lowered.strip_prefix(&parent) {
                    if !child.is_empty() && !child.contains('\\') {
                        current = Some(Uninstaller {
                            key: key[parent.len()..].to_string(),
                            ..Default::default()
                        });
                    }
                }
            }
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        let Some((name, value)) = line.split_once("\"=") else {
            continue;
        };
        let name = name.trim_start_matches('"').to_ascii_lowercase();
        let value = value
            .trim()
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .map(unescape)
            .unwrap_or_default();
        match name.as_str() {
            "displayname" => entry.display_name = value,
            "publisher" => entry.publisher = value,
            "uninstallstring" => entry.command = value,
            "installlocation" => entry.install_location = value,
            "displayicon" => entry.display_icon = value,
            _ => {}
        }
    }
    out.extend(current.filter(|e| !e.command.is_empty()));
    out
}

fn unescape(value: &str) -> String {
    value.replace("\\\\", "\\").replace("\\\"", "\"")
}

/// `"C:\x\Uninstall Y.exe" /currentuser` → (`C:\x\Uninstall Y.exe`, [`/currentuser`]).
pub fn split_command(command: &str) -> Option<(String, Vec<String>)> {
    let command = command.trim();
    let (exe, rest) = if let Some(quoted) = command.strip_prefix('"') {
        let end = quoted.find('"')?;
        (quoted[..end].to_string(), &quoted[end + 1..])
    } else {
        // Unquoted: the exe runs to the first ".exe".
        let lowered = command.to_ascii_lowercase();
        let end = lowered.find(".exe")? + 4;
        (command[..end].to_string(), &command[end..])
    };
    let args = rest.split_whitespace().map(str::to_string).collect();
    (!exe.is_empty()).then_some((exe, args))
}

/// The command that runs a vendor's own uninstaller in `proton`'s prefix.
///
/// The interactive `UninstallString`, through Proton's `run` verb with the
/// uninstaller's own folder as the working directory: the same as clicking
/// Uninstall in Windows. Built and inspectable before anything runs.
pub fn uninstall_invocation(
    vendor: &Uninstaller,
    proton: &hangar_runtime::ProtonEnv,
) -> Result<crate::run::Invocation, String> {
    let (win, args) = vendor
        .split_command()
        .ok_or_else(|| format!("unreadable uninstall command: {}", vendor.command))?;
    let exe = hangar_msfs::env::win_path_in_prefix(&proton.wineprefix, &win)
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("the uninstaller is not on disk: {win}"))?;
    let cwd = exe.parent().map(Path::to_path_buf).ok_or("no folder")?;
    let (program, mut all) = if proton.proton_script.is_file() {
        (
            proton.proton_script.as_os_str().to_owned(),
            vec![std::ffi::OsString::from("run")],
        )
    } else {
        (proton.wine_binary.as_os_str().to_owned(), Vec::new())
    };
    all.push(exe.into_os_string());
    all.extend(args.into_iter().map(std::ffi::OsString::from));
    Ok(crate::run::Invocation {
        program,
        args: all,
        cwd,
        env: proton.vars(),
    })
}

/// The Wine prefix holding `exe`: the parent of its `drive_c` ancestor.
pub fn prefix_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "drive_c"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// The uninstall entry for the app at `exe`, if its prefix has one.
///
/// An entry belongs to the app when its install location, icon or uninstaller
/// lies in the app's own folder. Read from `user.reg` and `system.reg` as text;
/// no Wine process is started.
pub fn uninstaller_for(exe: &Path) -> Option<Uninstaller> {
    let prefix = prefix_of(exe)?;
    let mut entries = Vec::new();
    for hive in [
        crate::registry::Hive::CurrentUser,
        crate::registry::Hive::LocalMachine,
    ] {
        if let Ok(text) = fs::read_to_string(crate::registry::hive_path(&prefix, hive)) {
            entries.extend(uninstall_entries(&text));
        }
    }
    owning_entry(&prefix, exe, entries)
}

/// The parse, separated from the files so a test can hand it a fragment.
pub fn owning_entry(prefix: &Path, exe: &Path, entries: Vec<Uninstaller>) -> Option<Uninstaller> {
    let folder = exe.parent()?;
    let inside = |win: &str| -> bool {
        let win = win.trim().trim_matches('"');
        // `DisplayIcon` carries an icon index: `…\App.exe,0`.
        let win = win.rsplit_once(',').map_or(win, |(path, index)| {
            if index.trim().parse::<i32>().is_ok() {
                path
            } else {
                win
            }
        });
        !win.is_empty()
            && hangar_msfs::env::win_path_in_prefix(prefix, win)
                .is_some_and(|p| p == folder || p.starts_with(folder))
    };
    entries.into_iter().find(|e| {
        inside(&e.install_location)
            || inside(&e.display_icon)
            || split_command(&e.command).is_some_and(|(path, _)| inside(&path))
    })
}

/// How sure a tie between a package and a vendor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    /// HangarRS installed this vendor's product and that run landed the package.
    Certain,
    /// A Community link points into the vendor app's prefix.
    Likely,
    /// Only the manifest's maker matches the vendor's name.
    Guess,
}

impl Strength {
    pub fn label(self) -> &'static str {
        match self {
            Strength::Certain => "installed by it",
            Strength::Likely => "links into its library",
            Strength::Guess => "same maker",
        }
    }

    /// Whether the confirm sheet ticks it before the person does.
    pub fn preticked(self) -> bool {
        !matches!(self, Strength::Guess)
    }
}

/// A package, as much of it as a tie needs.
#[derive(Debug, Clone)]
pub struct PackageRef {
    pub name: String,
    pub path: PathBuf,
    pub creator: String,
}

/// One package tied to a vendor, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tie {
    pub name: String,
    pub path: PathBuf,
    pub strength: Strength,
}

/// Which packages a vendor put in place, ranked. Proposes; never decides.
///
/// `layout.json` lists files, not owners, and the vendor table says how an
/// installer runs, not what it owns, so neither is consulted.
pub fn ties(
    vendor: &Uninstaller,
    vendor_exe: &Path,
    packages: &[PackageRef],
    records: &[InstallRecord],
) -> Vec<Tie> {
    let prefix = prefix_of(vendor_exe);
    let name = vendor.display_name.to_ascii_lowercase();
    let recorded = |package: &str| {
        // A drop's "product" is only the dropped file's name, never a vendor's.
        records
            .iter()
            .filter(|r| r.profile != "drop-install")
            .any(|r| {
                r.product.as_deref().is_some_and(|p| {
                    !p.trim().is_empty() && name.starts_with(&p.to_ascii_lowercase())
                }) && r
                    .landed
                    .iter()
                    .any(|l| l.kind == "Package" && l.name == package)
            })
    };
    let maker = first_word(&vendor.publisher)
        .or_else(|| first_word(&vendor.display_name))
        .unwrap_or_default();
    let mut out: Vec<Tie> = packages
        .iter()
        .filter_map(|p| {
            let strength = if recorded(&p.name) {
                Strength::Certain
            } else if prefix
                .as_ref()
                .is_some_and(|prefix| fs::read_link(&p.path).is_ok_and(|to| to.starts_with(prefix)))
            {
                Strength::Likely
            } else if !maker.is_empty() && first_word(&p.creator).is_some_and(|c| c == maker) {
                Strength::Guess
            } else {
                return None;
            };
            Some(Tie {
                name: p.name.clone(),
                path: p.path.clone(),
                strength,
            })
        })
        .collect();
    out.sort_by(|a, b| a.strength.cmp(&b.strength).then(a.name.cmp(&b.name)));
    out
}

fn first_word(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "hangar-remove-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("Packages/Community")).unwrap();
        base
    }

    fn package(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::create_dir_all(p.join("SimObjects/Airplanes")).unwrap();
        fs::write(p.join("manifest.json"), br#"{"creator":"Orbx"}"#).unwrap();
        fs::write(p.join("layout.json"), b"{}").unwrap();
        fs::write(p.join("SimObjects/Airplanes/model.bin"), vec![0u8; 1000]).unwrap();
        p
    }

    fn yes() -> Approval {
        Approval::given("Delete orbx-aircraft, 1.0 KB")
    }

    #[test]
    fn removing_a_package_deletes_it_and_nothing_beside_it() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let target_path = package(&community, "orbx-aircraft");
        let neighbour = package(&community, "orbx-airport");
        let target = Target::inspect(&target_path, &community).unwrap();
        assert_eq!(target.shape, Shape::Directory);
        assert!(target.bytes >= 1000);
        let removed = delete(&target, true, false, &yes(), "stamp").unwrap();
        assert_eq!(removed.name, "orbx-aircraft");
        assert!(!target_path.exists());
        assert!(neighbour.join("manifest.json").is_file());
        assert!(!base.join("Packages").join(REMOVING_DIR).exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_switched_off_package_can_be_removed_too() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let disabled = base.join("Packages/.hangar-disabled/Community");
        let path = package(&disabled, "off");
        let target = Target::inspect(&path, &community).unwrap();
        delete(&target, true, false, &yes(), "s").unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn the_gates_refuse_and_leave_the_package_in_place() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let path = package(&community, "p");
        let target = Target::inspect(&path, &community).unwrap();
        for (writes, sim) in [(false, false), (true, true)] {
            assert!(matches!(
                delete(&target, writes, sim, &yes(), "s"),
                Err(Error::Refused)
            ));
        }
        assert!(matches!(
            delete(&target, true, false, &Approval::given(" "), "s"),
            Err(Error::Refused)
        ));
        assert!(path.join("manifest.json").is_file());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_package_that_changed_after_review_is_not_deleted() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let path = package(&community, "p");
        let target = Target::inspect(&path, &community).unwrap();
        fs::write(path.join("new.bin"), b"arrived later").unwrap();
        assert!(matches!(
            delete(&target, true, false, &yes(), "s"),
            Err(Error::Changed)
        ));
        assert!(path.join("new.bin").is_file());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn nothing_outside_a_community_folder_is_a_package() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let official = base.join("Packages/Official/OneStore");
        let path = package(&official, "asobo-aircraft");
        assert!(matches!(
            Target::inspect(&path, &community),
            Err(Error::NotAPackage)
        ));
        assert!(matches!(
            Target::inspect(&path, &official),
            Err(Error::NotAPackage)
        ));
        // Nested one level down is a fragment, not a package.
        let outer = package(&community, "outer");
        let inner = package(&outer, "inner");
        assert!(matches!(
            Target::inspect(&inner, &community),
            Err(Error::NotAPackage)
        ));
        // A directory without a manifest (an uninstall leftover) is not one either.
        fs::create_dir_all(community.join("empty-leftover")).unwrap();
        assert!(matches!(
            Target::inspect(&community.join("empty-leftover"), &community),
            Err(Error::NotAPackage)
        ));
        assert!(path.is_dir() && inner.is_dir());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_link_inside_a_package_refuses_the_whole_delete() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let path = package(&community, "p");
        let outside = base.join("precious");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, path.join("escape")).unwrap();
        assert!(matches!(
            Target::inspect(&path, &community),
            Err(Error::Unsafe)
        ));
        assert!(outside.join("keep").is_file());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_linked_package_loses_its_link_and_the_library_copy_stays() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let library = base.join("Orbx Library");
        let real = package(&library, "orbx-aircraft");
        let link = community.join("orbx-aircraft");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let target = Target::inspect(&link, &community).unwrap();
        assert!(matches!(target.shape, Shape::Link { .. }));
        let removed = delete(&target, true, false, &yes(), "s").unwrap();
        assert!(removed.was_link);
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(real.join("manifest.json").is_file());
        fs::remove_dir_all(base).unwrap();
    }

    /// The shape of the real companion prefix's `user.reg` (2026-11-02), trimmed.
    const USER_REG: &str = r#"
[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\7cf15176-b7c3-5704-8319-ddca84b92c9a] 1788217874
#time=1dd1c2b7a1b2c3d
"DisplayIcon"="C:\\users\\steamuser\\AppData\\Local\\Programs\\orbx-central\\Orbx Central.exe,0"
"DisplayName"="Orbx Central 4.5.48"
"Publisher"="Orbx Simulation Systems Pty Ltd"
"QuietUninstallString"="\"C:\\users\\steamuser\\AppData\\Local\\Programs\\orbx-central\\Uninstall Orbx Central.exe\" /currentuser /S"
"UninstallString"="\"C:\\users\\steamuser\\AppData\\Local\\Programs\\orbx-central\\Uninstall Orbx Central.exe\" /currentuser"

[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\PilotEdge Pilot Client] 1776738452
"DisplayName"="Pilot Client"
"UninstallString"="C:\\users\\steamuser\\AppData\\Local\\PilotEdge\\Pilot Client\\Uninstall.exe"

[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Empty] 1776738452
"DisplayName"="Has no uninstaller"
"#;

    #[test]
    fn uninstall_entries_are_read_with_their_interactive_command() {
        let entries = uninstall_entries(USER_REG);
        assert_eq!(entries.len(), 2);
        let orbx = &entries[0];
        assert_eq!(orbx.display_name, "Orbx Central 4.5.48");
        assert_eq!(
            orbx.split_command().unwrap(),
            (
                r"C:\users\steamuser\AppData\Local\Programs\orbx-central\Uninstall Orbx Central.exe"
                    .to_string(),
                vec!["/currentuser".to_string()]
            )
        );
        assert_eq!(
            entries[1].split_command().unwrap().0,
            r"C:\users\steamuser\AppData\Local\PilotEdge\Pilot Client\Uninstall.exe"
        );
    }

    #[test]
    fn the_entry_is_the_one_inside_the_apps_own_folder() {
        let base = scratch();
        let prefix = base.join("pfx");
        let programs = prefix.join("drive_c/users/steamuser/AppData/Local/Programs");
        fs::create_dir_all(programs.join("orbx-central")).unwrap();
        fs::create_dir_all(
            prefix.join("drive_c/users/steamuser/AppData/Local/PilotEdge/Pilot Client"),
        )
        .unwrap();
        let exe = programs.join("orbx-central/Orbx Central.exe");
        assert_eq!(prefix_of(&exe).as_deref(), Some(prefix.as_path()));
        let found = owning_entry(&prefix, &exe, uninstall_entries(USER_REG)).unwrap();
        assert_eq!(found.display_name, "Orbx Central 4.5.48");
        let unrelated = programs.join("somebody-else/App.exe");
        assert!(owning_entry(&prefix, &unrelated, uninstall_entries(USER_REG)).is_none());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn ties_rank_records_then_links_then_makers_and_only_guesses_start_unticked() {
        let base = scratch();
        let community = base.join("Packages/Community");
        let prefix = base.join("pfx");
        let exe = prefix.join("drive_c/Programs/orbx-central/Orbx Central.exe");
        let library = prefix.join("drive_c/users/steamuser/Orbx Library");
        let linked_real = package(&library, "orbx-linked");
        std::os::unix::fs::symlink(&linked_real, community.join("orbx-linked")).unwrap();
        let recorded = package(&community, "orbx-recorded");
        let maker_only = package(&community, "orbx-maker-only");
        let stranger = package(&community, "someone-else");
        let vendor = Uninstaller {
            display_name: "Orbx Central 4.5.48".into(),
            publisher: "Orbx Simulation Systems Pty Ltd".into(),
            command: "x".into(),
            ..Default::default()
        };
        let record: InstallRecord = serde_json::from_value(serde_json::json!({
            "at": "2026-08-31T23-11-17Z", "installer": "/x.exe", "installer_bytes": 1,
            "engine": "NSIS", "product": "Orbx Central", "vendor": "Orbx", "profile": "p",
            "profile_origin": "shipped", "prefix": "companion", "command": "c", "cwd": "/",
            "ran_for_secs": 1.0, "exit_code": 0, "verdict": "v", "bad_console": [],
            "landed": [{"kind": "Package", "name": "orbx-recorded", "detail": ""}],
            "concerns": [], "verified": null
        }))
        .unwrap();
        // A drop of a folder named "Orbx" lands a package too; its "product"
        // is that folder's name, so it must not make the package "certain".
        let mut dropped = record.clone();
        dropped.profile = "drop-install".into();
        dropped.product = Some("Orbx".into());
        dropped.landed[0].name = "orbx-maker-only".into();
        let refs = |p: &Path, creator: &str| PackageRef {
            name: p.file_name().unwrap().to_string_lossy().into_owned(),
            path: p.to_path_buf(),
            creator: creator.into(),
        };
        let packages = [
            refs(&community.join("orbx-linked"), "Somebody"),
            refs(&recorded, ""),
            refs(&maker_only, "ORBX"),
            refs(&stranger, "Other Studio"),
        ];
        let found = ties(&vendor, &exe, &packages, &[record, dropped]);
        let got: Vec<_> = found
            .iter()
            .map(|t| (t.name.as_str(), t.strength))
            .collect();
        assert_eq!(
            got,
            [
                ("orbx-recorded", Strength::Certain),
                ("orbx-linked", Strength::Likely),
                ("orbx-maker-only", Strength::Guess),
            ]
        );
        assert!(Strength::Certain.preticked() && Strength::Likely.preticked());
        assert!(!Strength::Guess.preticked());
        fs::remove_dir_all(base).unwrap();
    }
}
