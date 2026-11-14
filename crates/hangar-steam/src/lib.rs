//! Steam discovery: install root, library folders, app installs, Proton tools.
//!
//! Deliberately knows nothing about Microsoft Flight Simulator. It answers
//! "where is Steam, where is app N installed, and what does it run under" for
//! any app id; everything MSFS-specific layers on top in `hangar-msfs`.
//! Reaching into the MSFS config from here would make the Steam layer
//! un-reusable.
//!
//! # Read-only
//!
//! Nothing in this crate writes. Steam's files belong to Steam, and — for the
//! input profiles under `userdata/` — to Steam Cloud, which will happily
//! overwrite anything changed while the client is running.

pub mod appmanifest;
pub mod compat;
pub mod running;
pub mod vdf;

pub use appmanifest::AppManifest;
pub use compat::CompatTool;
pub use running::{is_running, steam_processes, SteamProcesses};

use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SteamError {
    #[error("no Steam installation found (checked Flatpak, native, and $STEAM_ROOT)")]
    SteamNotFound,

    #[error("app {app_id} is not installed in any Steam library")]
    AppNotInstalled { app_id: String },

    #[error(
        "compat tool {name:?} is configured but was not found in compatibilitytools.d \
         or any steamapps/common"
    )]
    CompatToolUnresolved { name: String },

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid VDF: {reason}")]
    Vdf { path: PathBuf, reason: String },
}

pub type Result<T> = std::result::Result<T, SteamError>;

/// One Steam library folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamLibrary {
    pub path: PathBuf,
    pub label: Option<String>,
    /// True for the library inside the Steam install itself.
    pub is_steam_root: bool,
}

/// A game as installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInstall {
    pub app_id: String,
    /// Name from the app manifest, e.g. "Microsoft Flight Simulator 2024".
    pub name: String,
    /// The library folder holding this game.
    pub library_root: PathBuf,
    /// `<library>/steamapps/common/<installdir>`.
    pub install_dir: PathBuf,
    /// `<library>/steamapps/compatdata/<app_id>`.
    pub compat_data: PathBuf,
    /// `<compat_data>/pfx` — the Wine prefix.
    pub prefix: PathBuf,
}

/// Where Steam is and what libraries it knows about.
#[derive(Debug, Clone)]
pub struct SteamEnv {
    pub steam_root: PathBuf,
    /// Whether this is the Flatpak Steam.
    ///
    /// A Flatpak Steam is sandboxed, and anything spawned into its prefix has
    /// to go through the sandbox rather than straight at the filesystem.
    pub is_flatpak: bool,
    /// Every library, with the one inside `steam_root` first.
    pub libraries: Vec<SteamLibrary>,
}

impl SteamEnv {
    /// Find Steam and read its library list.
    ///
    /// Probe order: Flatpak, native, then `$STEAM_ROOT`.
    pub fn discover() -> Result<Self> {
        let (steam_root, is_flatpak) = find_steam_root().ok_or(SteamError::SteamNotFound)?;
        tracing::info!("Steam at {} (flatpak={is_flatpak})", steam_root.display());
        let libraries = read_libraries(&steam_root);
        Ok(Self {
            steam_root,
            is_flatpak,
            libraries,
        })
    }

    /// Build from a known root, for tests and for an explicit override.
    pub fn at(steam_root: impl Into<PathBuf>, is_flatpak: bool) -> Self {
        let steam_root = steam_root.into();
        let libraries = read_libraries(&steam_root);
        Self {
            steam_root,
            is_flatpak,
            libraries,
        }
    }

    /// Library paths, in probe order.
    pub fn library_roots(&self) -> Vec<PathBuf> {
        self.libraries.iter().map(|l| l.path.clone()).collect()
    }

    /// Locate an installed app.
    ///
    /// **The compatdata trap lives here, resolved once.** A game's prefix is in
    /// the library that holds *the game*, which is not necessarily the library
    /// inside `steam_root`. Deriving it from `steam_root` yields a path that
    /// does not exist. Callers get
    /// [`AppInstall::prefix`] and cannot recompute it wrongly.
    pub fn find_app(&self, app_id: &str) -> Result<AppInstall> {
        let manifest_name = format!("appmanifest_{app_id}.acf");

        for library in &self.libraries {
            let acf = library.path.join("steamapps").join(&manifest_name);
            if !acf.is_file() {
                continue;
            }
            let Ok(manifest) = appmanifest::parse(&acf) else {
                tracing::warn!("unreadable app manifest at {}", acf.display());
                continue;
            };
            let install_dir = library
                .path
                .join("steamapps/common")
                .join(&manifest.install_dir);
            if !install_dir.is_dir() {
                tracing::warn!(
                    "app {app_id} is registered in {} but {} is missing",
                    library.path.display(),
                    install_dir.display()
                );
                continue;
            }

            let compat_data = library.path.join("steamapps/compatdata").join(app_id);
            return Ok(AppInstall {
                app_id: app_id.to_owned(),
                name: manifest.name,
                library_root: library.path.clone(),
                install_dir,
                prefix: compat_data.join("pfx"),
                compat_data,
            });
        }

        Err(SteamError::AppNotInstalled {
            app_id: app_id.to_owned(),
        })
    }

    /// Resolve the Proton build configured for an app.
    pub fn compat_tool_for(&self, app_id: &str) -> Result<Option<CompatTool>> {
        compat::resolve(&self.steam_root, &self.library_roots(), app_id)
    }

    /// `<steam_root>/userdata` — per-account Steam Cloud storage.
    pub fn userdata_root(&self) -> PathBuf {
        self.steam_root.join("userdata")
    }

    /// Every account directory under `userdata`, by Steam3 account ID.
    ///
    /// More than one is normal — Steam leaves directories behind for accounts
    /// that have signed in on this machine — so callers should look at all of
    /// them rather than assuming one.
    pub fn userdata_accounts(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.userdata_root()) else {
            return Vec::new();
        };
        let mut accounts: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
            })
            .collect();
        accounts.sort();
        accounts
    }

    /// Steam Cloud `remote/` directories for an app, one per account.
    ///
    /// This is where MSFS 2024 keeps control bindings — **not** the Proton
    /// prefix, which has no `Input/` folder at all. Being cloud-synced is why
    /// every prefix-level backup misses them, and why anything written here has
    /// to wait for Steam to be closed.
    pub fn cloud_remote_dirs(&self, app_id: &str) -> Vec<PathBuf> {
        self.userdata_accounts()
            .into_iter()
            .map(|a| a.join(app_id).join("remote"))
            .filter(|p| p.is_dir())
            .collect()
    }
}

fn find_steam_root() -> Option<(PathBuf, bool)> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;

    let flatpak = home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
    if flatpak.join("steamapps").is_dir() {
        return Some((flatpak, true));
    }

    let native = home.join(".local/share/Steam");
    if native.join("steamapps").is_dir() {
        return Some((native, false));
    }

    let env_root = PathBuf::from(std::env::var_os("STEAM_ROOT")?);
    env_root
        .join("steamapps")
        .is_dir()
        .then_some((env_root, false))
}

/// Read `libraryfolders.vdf`, always including `steam_root` itself first.
///
/// Failure is not fatal: a missing or malformed file just means the only
/// library is the Steam install.
fn read_libraries(steam_root: &Path) -> Vec<SteamLibrary> {
    let mut libraries = vec![SteamLibrary {
        path: steam_root.to_path_buf(),
        label: None,
        is_steam_root: true,
    }];

    let path = steam_root.join("steamapps/libraryfolders.vdf");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return libraries;
    };
    let Ok(root) = vdf::parse(&content) else {
        tracing::warn!("{} is not valid VDF — ignoring", path.display());
        return libraries;
    };
    let Some(folders) = root
        .get_path("libraryfolders")
        .and_then(vdf::VdfNode::as_map)
    else {
        return libraries;
    };

    for node in folders.values() {
        let Some(path_str) = node.get_str("path") else {
            continue;
        };
        let path = PathBuf::from(path_str);
        // Entry 0 is normally the Steam install, already first in the list.
        if path == steam_root {
            continue;
        }
        libraries.push(SteamLibrary {
            path,
            label: node
                .get_str("label")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            is_steam_root: false,
        });
    }

    libraries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-steam-env-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a Steam root plus a second library holding the app, mirroring the
    /// real machine: Steam in `~/.local/share/Steam`, MSFS on another disk.
    fn fake_steam(app_id: &str) -> (PathBuf, PathBuf) {
        let tmp = scratch();
        let root = tmp.join("Steam");
        let lib = tmp.join("SteamLibrary");
        std::fs::create_dir_all(root.join("steamapps")).unwrap();
        std::fs::create_dir_all(lib.join("steamapps/common/MSFS2024")).unwrap();
        std::fs::create_dir_all(lib.join(format!("steamapps/compatdata/{app_id}/pfx"))).unwrap();

        std::fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                r#"
"libraryfolders"
{{
    "0" {{ "path" "{}" "label" "" }}
    "1" {{ "path" "{}" "label" "games" }}
}}
"#,
                root.display(),
                lib.display()
            ),
        )
        .unwrap();

        std::fs::write(
            lib.join(format!("steamapps/appmanifest_{app_id}.acf")),
            format!(
                r#""AppState" {{ "appid" "{app_id}" "name" "Microsoft Flight Simulator 2024" "installdir" "MSFS2024" }}"#
            ),
        )
        .unwrap();

        (root, lib)
    }

    #[test]
    fn steam_root_is_always_the_first_library() {
        let (root, lib) = fake_steam("2537590");
        let env = SteamEnv::at(&root, false);
        assert_eq!(env.libraries.len(), 2);
        assert!(env.libraries[0].is_steam_root);
        assert_eq!(env.libraries[0].path, root);
        assert_eq!(env.libraries[1].path, lib);
        assert_eq!(env.libraries[1].label.as_deref(), Some("games"));
    }

    /// **The compatdata trap.** The prefix must come from the library holding
    /// the game, not from `steam_root`.
    #[test]
    fn compatdata_resolves_in_the_library_that_holds_the_game() {
        let (root, lib) = fake_steam("2537590");
        let env = SteamEnv::at(&root, false);
        let app = env.find_app("2537590").unwrap();

        assert_eq!(app.library_root, lib);
        assert_eq!(app.compat_data, lib.join("steamapps/compatdata/2537590"));
        assert_eq!(app.prefix, lib.join("steamapps/compatdata/2537590/pfx"));
        assert!(
            app.prefix.is_dir(),
            "the real prefix must exist at that path"
        );

        assert!(
            !app.prefix.starts_with(&root),
            "deriving the prefix from steam_root yields a path that does not exist"
        );
        assert!(!root.join("steamapps/compatdata/2537590/pfx").exists());
    }

    #[test]
    fn find_app_reports_the_manifest_name_and_install_dir() {
        let (root, lib) = fake_steam("2537590");
        let app = SteamEnv::at(&root, false).find_app("2537590").unwrap();
        assert_eq!(app.name, "Microsoft Flight Simulator 2024");
        assert_eq!(app.install_dir, lib.join("steamapps/common/MSFS2024"));
    }

    #[test]
    fn a_missing_app_is_reported_as_not_installed() {
        let (root, _) = fake_steam("2537590");
        let err = SteamEnv::at(&root, false).find_app("123456").unwrap_err();
        assert!(matches!(err, SteamError::AppNotInstalled { .. }));
    }

    /// A manifest whose install directory is gone must not be reported as a
    /// working install — that is an uninstall Steam did not fully clean up.
    #[test]
    fn a_manifest_without_its_install_dir_is_skipped() {
        let (root, lib) = fake_steam("2537590");
        std::fs::remove_dir_all(lib.join("steamapps/common/MSFS2024")).unwrap();
        let err = SteamEnv::at(&root, false).find_app("2537590").unwrap_err();
        assert!(matches!(err, SteamError::AppNotInstalled { .. }));
    }

    #[test]
    fn a_missing_libraryfolders_leaves_steam_root_as_the_only_library() {
        let tmp = scratch();
        std::fs::create_dir_all(tmp.join("steamapps")).unwrap();
        let env = SteamEnv::at(&tmp, false);
        assert_eq!(env.libraries.len(), 1);
        assert!(env.libraries[0].is_steam_root);
    }

    #[test]
    fn a_malformed_libraryfolders_is_survivable() {
        let tmp = scratch();
        std::fs::create_dir_all(tmp.join("steamapps")).unwrap();
        std::fs::write(tmp.join("steamapps/libraryfolders.vdf"), "{{{ not vdf").unwrap();
        assert_eq!(SteamEnv::at(&tmp, false).libraries.len(), 1);
    }

    #[test]
    fn cloud_remote_dirs_are_found_per_account() {
        let tmp = scratch();
        std::fs::create_dir_all(tmp.join("steamapps")).unwrap();
        std::fs::create_dir_all(tmp.join("userdata/123456789/2537590/remote")).unwrap();
        std::fs::create_dir_all(tmp.join("userdata/999/2537590/remote")).unwrap();
        // Not an account directory.
        std::fs::create_dir_all(tmp.join("userdata/ac_config")).unwrap();

        let env = SteamEnv::at(&tmp, false);
        assert_eq!(env.userdata_accounts().len(), 2);

        let remotes = env.cloud_remote_dirs("2537590");
        assert_eq!(remotes.len(), 2);
        assert!(remotes
            .iter()
            .any(|p| p.ends_with("123456789/2537590/remote")));
        // An app with no cloud data yields nothing rather than a phantom path.
        assert!(env.cloud_remote_dirs("111111").is_empty());
    }
}
