//! The Proton/Wine environment companion apps run in.

use hangar_msfs::MsfsEnv;
use hangar_steam::SteamEnv;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Everything needed to invoke Proton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonEnv {
    /// `WINEPREFIX`.
    pub wineprefix: PathBuf,
    /// The `proton` launcher script.
    pub proton_script: PathBuf,
    /// The Wine binary. Modern Proton ships no `wine64`.
    pub wine_binary: PathBuf,
    /// `STEAM_COMPAT_DATA_PATH` — the parent of `pfx`.
    pub compat_data: PathBuf,
    /// `STEAM_COMPAT_INSTALL_PATH` — the game's install directory.
    pub compat_install: PathBuf,
    /// `STEAM_COMPAT_CLIENT_INSTALL_PATH` — the Steam client root.
    pub steam_client: PathBuf,
    pub is_flatpak: bool,
}

impl ProtonEnv {
    /// The environment for the sim's own prefix.
    ///
    /// `None` when no compat tool is configured — there is nothing to run with.
    pub fn for_msfs(msfs: &MsfsEnv, steam: &SteamEnv) -> Option<Self> {
        let tool = msfs.compat_tool.as_ref()?;
        Some(Self {
            wineprefix: msfs.install.prefix.clone(),
            proton_script: tool.proton_script.clone(),
            wine_binary: tool.wine_binary.clone(),
            compat_data: msfs.install.compat_data.clone(),
            compat_install: msfs.install.install_dir.clone(),
            steam_client: steam.steam_root.clone(),
            is_flatpak: steam.is_flatpak,
        })
    }

    /// The environment for the companion prefix.
    ///
    /// The Proton toolchain is shared; only the prefix location differs.
    ///
    /// **This is not isolation, whatever it looks like.** A separate
    /// `WINEPREFIX` separates the two `drive_c` trees and nothing else, and not
    /// always even that: a companion prefix whose
    /// `users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024` is a
    /// symlink into the MSFS prefix can write Community and the sim's
    /// `EXE.xml` from inside the "separate" prefix.
    ///
    /// Anything that needs to know whether a prefix can reach the sim's
    /// content has to ask the filesystem — `hangar_install::gate::
    /// reaches_sim_content` — rather than reasoning from which prefix it is.
    pub fn companion(&self, companion_dir: &Path) -> Self {
        Self {
            wineprefix: companion_dir.join("pfx"),
            compat_data: companion_dir.to_path_buf(),
            ..self.clone()
        }
    }

    /// Environment variables for a Proton child process.
    pub fn vars(&self) -> Vec<(OsString, OsString)> {
        [
            ("WINEPREFIX", self.wineprefix.as_os_str()),
            ("STEAM_COMPAT_DATA_PATH", self.compat_data.as_os_str()),
            ("STEAM_COMPAT_INSTALL_PATH", self.compat_install.as_os_str()),
            (
                "STEAM_COMPAT_CLIENT_INSTALL_PATH",
                self.steam_client.as_os_str(),
            ),
            ("WINE_LARGE_ADDRESS_AWARE", std::ffi::OsStr::new("1")),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v.to_owned()))
        .collect()
    }

    /// How to invoke Wine: `(program, leading args)`.
    ///
    /// Prefers the `proton` script wherever it exists. The script sets up
    /// `LD_LIBRARY_PATH` for Proton's bundled libraries, which invoking the wine
    /// binary directly does not — so a direct call can fail on a program the
    /// script runs fine.
    pub fn invocation(&self) -> (OsString, Vec<OsString>) {
        if self.proton_script.is_file() {
            (
                self.proton_script.as_os_str().to_owned(),
                vec![OsString::from("run")],
            )
        } else {
            (self.wine_binary.as_os_str().to_owned(), Vec::new())
        }
    }
}

/// The previous application's companion prefix.
///
/// The app resolves its companion prefix through
/// `hangar_core::config::Config::companion_prefix`, which keeps this one for
/// an existing setup and uses HangarRS' own location for a new one. The
/// diagnostic examples still read this directly.
pub fn legacy_companion_dir() -> Option<PathBuf> {
    hangar_core::paths::legacy_companion_prefix_dir()
}

/// The `SimConnect.cfg` an in-prefix app needs to reach the sim.
///
/// SimConnect's default transport is a named pipe, which cannot cross from the
/// companion prefix's wineserver to the sim's. TCP can.
pub const SIMCONNECT_CFG: &str = "[SimConnect]\n\
                                  Protocol=IPv4\n\
                                  Address=127.0.0.1\n\
                                  Port=5557\n\
                                  MaxReceiveSize=41088\n\
                                  DisableNagle=1\n";

/// Where a `SimConnect.cfg` has to be written inside a prefix.
///
/// **Two places, because clients disagree about where to look**, and this is
/// the least obvious thing in the whole runtime:
///
/// * `Documents/` — where Microsoft's official SimConnect DLL searches, so
///   PilotEdge and BeyondATC find it there.
/// * the user profile root — `node-simconnect`, which Electron apps like
///   Project Airports use, only checks `process.cwd()` and `os.homedir()`. It
///   never looks in `Documents`. Without a hit it silently falls back to the
///   named pipe and retries forever, which looks like the sim not running.
pub fn simconnect_cfg_paths(prefix: &Path) -> Vec<PathBuf> {
    let user = prefix.join("drive_c/users/steamuser");
    vec![
        user.join("Documents").join("SimConnect.cfg"),
        user.join("SimConnect.cfg"),
    ]
}

/// Write `SimConnect.cfg` into both locations, without overwriting.
///
/// Idempotent, and never clobbers an existing file — someone may have tuned it.
pub fn ensure_simconnect_cfg(prefix: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for path in simconnect_cfg_paths(prefix) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if !path.exists() {
            std::fs::write(&path, SIMCONNECT_CFG)?;
            written.push(path);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-env-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn env_at(root: &Path) -> ProtonEnv {
        ProtonEnv {
            wineprefix: root.join("compatdata/2537590/pfx"),
            proton_script: root.join("Proton - Experimental/proton"),
            wine_binary: root.join("Proton - Experimental/files/bin/wine"),
            compat_data: root.join("compatdata/2537590"),
            compat_install: root.join("common/MSFS2024"),
            steam_client: root.join("Steam"),
            is_flatpak: false,
        }
    }

    #[test]
    fn the_companion_prefix_shares_the_toolchain_but_not_the_prefix() {
        let root = scratch();
        let msfs = env_at(&root);
        let companion_dir = root.join("companion-prefix");
        let companion = msfs.companion(&companion_dir);

        assert_eq!(companion.wineprefix, companion_dir.join("pfx"));
        assert_eq!(companion.compat_data, companion_dir);
        assert_ne!(companion.wineprefix, msfs.wineprefix);
        // Same Proton, so no second toolchain to keep up to date.
        assert_eq!(companion.proton_script, msfs.proton_script);
        assert_eq!(companion.wine_binary, msfs.wine_binary);
    }

    /// Proton wants `WINEPREFIX == $STEAM_COMPAT_DATA_PATH/pfx`; getting that
    /// wrong makes it try to re-initialise the prefix.
    #[test]
    fn compat_data_is_always_the_parent_of_the_prefix() {
        let root = scratch();
        for env in [env_at(&root), env_at(&root).companion(&root.join("c"))] {
            assert_eq!(env.wineprefix.parent(), Some(env.compat_data.as_path()));
        }
    }

    #[test]
    fn the_proton_script_is_preferred_when_it_exists() {
        let root = scratch();
        let env = env_at(&root);

        // Missing script: fall back to the wine binary directly.
        let (cmd, args) = env.invocation();
        assert_eq!(cmd, env.wine_binary.as_os_str());
        assert!(args.is_empty());

        std::fs::create_dir_all(env.proton_script.parent().unwrap()).unwrap();
        std::fs::write(&env.proton_script, "#!/usr/bin/env python3\n").unwrap();
        let (cmd, args) = env.invocation();
        assert_eq!(cmd, env.proton_script.as_os_str());
        assert_eq!(args, [OsString::from("run")]);
    }

    #[test]
    fn the_environment_carries_every_variable_proton_needs() {
        let env = env_at(&scratch());
        let vars: Vec<String> = env
            .vars()
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for needed in [
            "WINEPREFIX",
            "STEAM_COMPAT_DATA_PATH",
            "STEAM_COMPAT_INSTALL_PATH",
            "STEAM_COMPAT_CLIENT_INSTALL_PATH",
        ] {
            assert!(vars.contains(&needed.to_string()), "missing {needed}");
        }
    }

    /// **The dual placement.** One location is not enough: the official DLL
    /// looks in Documents, node-simconnect never does.
    #[test]
    fn simconnect_cfg_lands_in_both_places() {
        let prefix = scratch();
        let written = ensure_simconnect_cfg(&prefix).unwrap();
        assert_eq!(written.len(), 2);

        let documents = prefix.join("drive_c/users/steamuser/Documents/SimConnect.cfg");
        let home = prefix.join("drive_c/users/steamuser/SimConnect.cfg");
        assert!(
            documents.is_file(),
            "the official SimConnect DLL looks here"
        );
        assert!(home.is_file(), "node-simconnect only looks here");
        assert_eq!(std::fs::read_to_string(&home).unwrap(), SIMCONNECT_CFG);
    }

    #[test]
    fn the_config_points_at_tcp_not_the_named_pipe() {
        assert!(SIMCONNECT_CFG.contains("Protocol=IPv4"));
        assert!(SIMCONNECT_CFG.contains("Port=5557"));
    }

    /// Someone may have tuned theirs; writing is idempotent and never clobbers.
    #[test]
    fn an_existing_config_is_left_alone() {
        let prefix = scratch();
        ensure_simconnect_cfg(&prefix).unwrap();

        let home = prefix.join("drive_c/users/steamuser/SimConnect.cfg");
        std::fs::write(&home, "[SimConnect]\nPort=9999\n").unwrap();

        let written = ensure_simconnect_cfg(&prefix).unwrap();
        assert!(written.is_empty(), "nothing needed writing");
        assert!(std::fs::read_to_string(&home).unwrap().contains("9999"));
    }
}
