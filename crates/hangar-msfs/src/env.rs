//! Discovering the MSFS 2024 installation: prefix, config, package folders.
//!
//! Everything here is **read-only**. Nothing writes to the prefix or the
//! Community folder.

use crate::{MsfsError, Result};
use hangar_core::{MSFS2024_APPDATA_DIR, MSFS2024_APP_ID};
use hangar_steam::{AppInstall, CompatTool, SteamEnv};
use std::path::{Path, PathBuf};

/// Which generation of package folder this is.
///
/// MSFS 2024 keeps both: it can load 2020-era content, so `Community` and
/// `Community2024` coexist and both may hold packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generation {
    Msfs2020,
    Msfs2024,
}

/// A Community folder found under the packages root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityFolder {
    pub path: PathBuf,
    pub generation: Generation,
}

/// A resolved MSFS 2024 environment.
#[derive(Debug, Clone)]
pub struct MsfsEnv {
    /// Steam's view: install directory, compatdata, prefix.
    pub install: AppInstall,
    /// The Proton build MSFS is configured to run under, if any.
    pub compat_tool: Option<CompatTool>,
    /// `<prefix>/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024`.
    pub app_data: PathBuf,
    /// `<app_data>/UserCfg.opt`.
    pub user_cfg: PathBuf,
    /// `InstalledPackagesPath` from `UserCfg.opt`, translated to a Linux path.
    pub packages_root: PathBuf,
    /// The Community folder add-ons should be managed in.
    pub community: PathBuf,
    /// Every Community folder found, newest generation first.
    pub communities: Vec<CommunityFolder>,
}

impl MsfsEnv {
    /// Discover MSFS 2024 through an already-discovered Steam.
    pub fn discover(steam: &SteamEnv) -> Result<Self> {
        Self::for_app(steam, MSFS2024_APP_ID)
    }

    /// Discover a specific app id, so a test can point at a fake tree.
    pub fn for_app(steam: &SteamEnv, app_id: &str) -> Result<Self> {
        let install = steam.find_app(app_id)?;

        // A missing or broken compat-tool mapping must not stop discovery:
        // everything read-only still works without Proton, and the Settings tab
        // is where that gets reported.
        let compat_tool = match steam.compat_tool_for(app_id) {
            Ok(tool) => tool,
            Err(e) => {
                tracing::warn!("no Proton resolved for app {app_id}: {e}");
                None
            }
        };

        let app_data = install
            .prefix
            .join("drive_c/users/steamuser/AppData/Roaming")
            .join(MSFS2024_APPDATA_DIR);
        let user_cfg = app_data.join("UserCfg.opt");

        let packages_root = read_packages_root(&user_cfg, &install.prefix)?;
        let communities = find_communities(&packages_root);
        let community = communities.first().map(|c| c.path.clone()).ok_or_else(|| {
            MsfsError::NoCommunityFolder {
                packages_root: packages_root.clone(),
            }
        })?;

        Ok(Self {
            install,
            compat_tool,
            app_data,
            user_cfg,
            packages_root,
            community,
            communities,
        })
    }

    /// `<app_data>/EXE.xml` — the in-sim add-on launch list.
    pub fn exe_xml(&self) -> PathBuf {
        self.app_data.join("EXE.xml")
    }

    /// Whether a Proton build was resolved.
    pub fn has_proton(&self) -> bool {
        self.compat_tool.is_some()
    }

    /// `Official2024` / `Official2020`, whichever exist.
    ///
    /// Never managed — Asobo owns these — but the scanner needs to know they
    /// are packages so it can tell "installed by the sim" from "installed by
    /// the user".
    pub fn official_folders(&self) -> Vec<PathBuf> {
        ["Official2024", "Official2020", "OneStore"]
            .iter()
            .map(|n| self.packages_root.join(n))
            .filter(|p| p.is_dir())
            .collect()
    }

    /// Where MSFS 2024 keeps control bindings.
    ///
    /// **Not under [`Self::app_data`].** There is no `Input/` folder anywhere in
    /// the prefix; the input profiles are Steam Cloud userdata. Exposed here so
    /// nobody goes looking in the prefix for them again.
    pub fn binding_dirs(&self, steam: &SteamEnv) -> Vec<PathBuf> {
        steam.cloud_remote_dirs(&self.install.app_id)
    }
}

/// Read `InstalledPackagesPath` from `UserCfg.opt` and translate it.
fn read_packages_root(user_cfg: &Path, prefix: &Path) -> Result<PathBuf> {
    let content = std::fs::read_to_string(user_cfg).map_err(|source| MsfsError::Read {
        path: user_cfg.to_path_buf(),
        source,
    })?;

    let win_path = parse_installed_packages_path(&content).ok_or_else(|| MsfsError::Malformed {
        path: user_cfg.to_path_buf(),
        reason: "no InstalledPackagesPath entry".into(),
    })?;

    win_path_in_prefix(prefix, &win_path).ok_or_else(|| MsfsError::Malformed {
        path: user_cfg.to_path_buf(),
        reason: format!("InstalledPackagesPath {win_path:?} is not a drive-letter path"),
    })
}

/// Pull the quoted value out of the `InstalledPackagesPath` line.
///
/// `UserCfg.opt` is its own format — not VDF, not INI. The line reads:
///
/// ```text
/// InstalledPackagesPath "C:\users\steamuser\AppData\Roaming\Microsoft Flight Simulator 2024\Packages"
/// ```
///
/// Backslashes are **not** doubled here, unlike Steam's VDF files.
pub fn parse_installed_packages_path(content: &str) -> Option<String> {
    let line = content
        .lines()
        .find(|l| l.trim_start().starts_with("InstalledPackagesPath"))?;
    let value = line.split('"').nth(1)?;
    (!value.trim().is_empty()).then(|| value.to_owned())
}

/// Translate a Windows path into its location inside a Wine prefix.
///
/// `C:\users\steamuser\X` becomes `<prefix>/drive_c/users/steamuser/X`.
///
/// The lookup is case-insensitive component by component. Windows is not
/// case-sensitive and Linux is, so a config written as `C:\Users\...` against a
/// prefix containing `users/` resolves on Windows and fails here — a difference
/// that produces a "file not found" for a file that is plainly there.
pub fn win_path_in_prefix(prefix: &Path, win_path: &str) -> Option<PathBuf> {
    let (drive, rest) = win_path.split_once(':')?;
    let drive = drive.chars().next()?.to_ascii_lowercase();
    if !drive.is_ascii_alphabetic() {
        return None;
    }

    let mut path = prefix.join(format!("drive_{drive}"));
    for component in rest.split(['\\', '/']).filter(|c| !c.is_empty()) {
        path = match resolve_component(&path, component) {
            Some(next) => next,
            // Past the last directory that exists, keep building the path
            // literally: the caller may be describing something not yet created.
            None => path.join(component),
        };
    }
    Some(path)
}

/// Join `name` onto `dir`, matching an existing entry case-insensitively.
fn resolve_component(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        return Some(exact);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    entries.flatten().find_map(|e| {
        let file_name = e.file_name();
        let candidate = file_name.to_str()?;
        candidate.eq_ignore_ascii_case(name).then(|| e.path())
    })
}

/// Find the Community folders under a packages root, newest generation first.
///
/// `Community2024` is preferred when both exist — it is the one MSFS 2024 loads
/// its own generation of content from. `*Backup` directories are deliberately
/// excluded: they are copies left by other tools, and offering to manage one is
/// a good way to have a profile switch write into a backup.
fn find_communities(packages_root: &Path) -> Vec<CommunityFolder> {
    [
        ("Community2024", Generation::Msfs2024),
        ("Community", Generation::Msfs2020),
    ]
    .into_iter()
    .filter_map(|(name, generation)| {
        let path = packages_root.join(name);
        path.is_dir()
            .then_some(CommunityFolder { path, generation })
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
            "hangar-msfs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The exact line from the real `UserCfg.opt`, in context.
    const REAL_USERCFG: &str = r#"Version 66
{Video
	Adapter "AMD Radeon RX 7900 XTX (RADV NAVI31)"
	Resolution 2560 1440
}
InstalledPackagesPath "C:\users\steamuser\AppData\Roaming\Microsoft Flight Simulator 2024\Packages"
"#;

    #[test]
    fn reads_the_packages_path_from_a_real_usercfg() {
        assert_eq!(
            parse_installed_packages_path(REAL_USERCFG).as_deref(),
            Some(r"C:\users\steamuser\AppData\Roaming\Microsoft Flight Simulator 2024\Packages")
        );
    }

    #[test]
    fn a_usercfg_without_the_entry_yields_none() {
        assert_eq!(parse_installed_packages_path("Version 66\n"), None);
        assert_eq!(parse_installed_packages_path(""), None);
        assert_eq!(
            parse_installed_packages_path("InstalledPackagesPath \"\"\n"),
            None
        );
    }

    #[test]
    fn translates_a_windows_path_into_the_prefix() {
        let tmp = scratch();
        let got = win_path_in_prefix(&tmp, r"C:\users\steamuser\AppData").unwrap();
        assert_eq!(got, tmp.join("drive_c/users/steamuser/AppData"));
    }

    /// Windows is case-insensitive; Linux is not. A config written with a
    /// different case must still resolve to the directory that exists.
    #[test]
    fn windows_path_lookup_is_case_insensitive() {
        let tmp = scratch();
        std::fs::create_dir_all(tmp.join("drive_c/users/steamuser/AppData/Roaming")).unwrap();

        let got = win_path_in_prefix(&tmp, r"C:\Users\SteamUser\appdata\ROAMING").unwrap();
        assert_eq!(got, tmp.join("drive_c/users/steamuser/AppData/Roaming"));
        assert!(got.is_dir(), "the resolved path must actually exist");
    }

    #[test]
    fn windows_path_accepts_either_separator_and_any_drive() {
        let tmp = scratch();
        assert_eq!(
            win_path_in_prefix(&tmp, "C:/users/steamuser").unwrap(),
            tmp.join("drive_c/users/steamuser")
        );
        assert_eq!(
            win_path_in_prefix(&tmp, r"D:\Packages").unwrap(),
            tmp.join("drive_d/Packages")
        );
        assert!(win_path_in_prefix(&tmp, r"\no\drive\letter").is_none());
        assert!(win_path_in_prefix(&tmp, "relative/path").is_none());
    }

    /// Both generations coexist on a real install; 2024 is the active one, and
    /// the `*Backup` directories beside them must never be offered.
    #[test]
    fn community2024_wins_and_backups_are_excluded() {
        let tmp = scratch();
        for name in [
            "Community",
            "Community2024",
            "CommunityBackup",
            "Community2024Backup",
            "Official2024",
        ] {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
        }

        let found = find_communities(&tmp);
        assert_eq!(found.len(), 2, "only the two live folders");
        assert_eq!(found[0].path, tmp.join("Community2024"));
        assert_eq!(found[0].generation, Generation::Msfs2024);
        assert_eq!(found[1].generation, Generation::Msfs2020);
        assert!(
            !found
                .iter()
                .any(|c| c.path.to_string_lossy().contains("Backup")),
            "a backup folder must never be managed"
        );
    }

    #[test]
    fn a_2020_only_install_still_resolves() {
        let tmp = scratch();
        std::fs::create_dir_all(tmp.join("Community")).unwrap();
        let found = find_communities(&tmp);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].generation, Generation::Msfs2020);
    }

    /// Full discovery against a tree shaped like the real machine: Steam in one
    /// place, the game and its prefix in another library.
    #[test]
    fn discovers_a_complete_environment() {
        let tmp = scratch();
        let steam_root = tmp.join("Steam");
        let lib = tmp.join("SteamLibrary");
        let prefix = lib.join("steamapps/compatdata/2537590/pfx");
        let app_data =
            prefix.join("drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024");

        std::fs::create_dir_all(steam_root.join("steamapps")).unwrap();
        std::fs::create_dir_all(lib.join("steamapps/common/MSFS2024")).unwrap();
        std::fs::create_dir_all(app_data.join("Packages/Community2024")).unwrap();
        std::fs::create_dir_all(app_data.join("Packages/Community")).unwrap();
        std::fs::create_dir_all(app_data.join("Packages/Official2024")).unwrap();

        std::fs::write(
            steam_root.join("steamapps/libraryfolders.vdf"),
            format!(
                r#""libraryfolders" {{ "0" {{ "path" "{}" }} "1" {{ "path" "{}" }} }}"#,
                steam_root.display(),
                lib.display()
            ),
        )
        .unwrap();
        std::fs::write(
            lib.join("steamapps/appmanifest_2537590.acf"),
            r#""AppState" { "appid" "2537590" "name" "MSFS 2024" "installdir" "MSFS2024" }"#,
        )
        .unwrap();
        std::fs::write(app_data.join("UserCfg.opt"), REAL_USERCFG).unwrap();

        let steam = SteamEnv::at(&steam_root, false);
        let msfs = MsfsEnv::discover(&steam).unwrap();

        assert_eq!(msfs.install.prefix, prefix);
        assert_eq!(msfs.packages_root, app_data.join("Packages"));
        assert_eq!(msfs.community, app_data.join("Packages/Community2024"));
        assert_eq!(msfs.communities.len(), 2);
        assert_eq!(msfs.user_cfg, app_data.join("UserCfg.opt"));
        assert_eq!(
            msfs.official_folders(),
            vec![app_data.join("Packages/Official2024")]
        );
        // No compat tool is configured in this tree, and that is not fatal.
        assert!(!msfs.has_proton());
    }

    #[test]
    fn a_missing_usercfg_is_reported_with_its_path() {
        let tmp = scratch();
        let err = read_packages_root(&tmp.join("UserCfg.opt"), &tmp).unwrap_err();
        assert!(err.to_string().contains("UserCfg.opt"), "got: {err}");
    }
}
