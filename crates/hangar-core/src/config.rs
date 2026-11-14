//! HangarRS' own configuration file.
//!
//! `$XDG_CONFIG_HOME/hangar-rs/config.toml`. It holds the things that cannot be
//! discovered by scanning the machine: which companion apps exist and how to
//! launch them, where any package libraries are, and how the output bridges are
//! set up.
//!
//! Everything else HangarRS knows — packages, `EXE.xml` add-ons, prefixes,
//! bindings — comes from a filesystem scan and is deliberately *not* stored
//! here. A config that duplicates the filesystem is a second copy of the truth,
//! and the two disagree the first time someone deletes a directory by hand.
//! That is the same reasoning that keeps `hangar-install`'s stage 7 to
//! recording an install rather than registering it.
//!
//! # An unknown key is an error, because saving would delete it
//!
//! `deny_unknown_fields`, and that is a stronger rule here than the tolerant
//! parse used for the legacy file next door. HangarRS *rewrites* this file, so
//! a key it shrugs at on read is a key it silently deletes on the next write.
//! A loud parse error naming the key is recoverable; a setting that quietly
//! stopped existing is not.
//!
//! # What the bridge section may contain
//!
//! Exactly the arguments `hangar_bridges::serve` takes, and nothing else. A
//! config key nothing honours is worse than a missing one: it reads as a
//! setting, changes nothing, and gives no sign of why. The legacy config's
//! `broadcast_address` is one such key — see [`crate::migrate`].

use crate::error::{Error, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The config format this build writes.
///
/// Stored so a future change of shape can be detected rather than guessed at.
/// A file from a newer HangarRS is refused by [`Config::parse`]'s
/// `deny_unknown_fields` long before this is read, which is the intended order:
/// the unknown key names what is wrong, this only says how old the file is.
pub const VERSION: u32 = 1;

/// Everything HangarRS remembers between runs, apart from preflights, package
/// profiles and the vendor table, which have files of their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    /// Package libraries to scan alongside the Community folders.
    pub libraries: Vec<PathBuf>,
    /// Companion apps and native tools, in the order they should be listed.
    pub apps: Vec<AppEntry>,
    pub bridges: Bridges,
    pub toggles: Toggles,
    pub navigation: Navigation,
    /// The companion prefix's directory — the parent of its `pfx/`.
    ///
    /// Absent is not a path; it means "follow the rule in
    /// [`Config::companion_prefix`]". The field-level `serde(default)` matters:
    /// without it a config written before this key existed would take the
    /// struct default, and an existing setup would silently move to an empty
    /// prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_prefix: Option<PathBuf>,
}

/// Why the companion prefix is where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionSource {
    /// `companion_prefix` names it.
    Configured,
    /// Not configured, and the previous application's prefix exists: an
    /// existing setup keeps the prefix its apps are installed in.
    Legacy,
    /// Not configured and no previous prefix: HangarRS' own location.
    Default,
}

impl CompanionSource {
    pub fn label(self) -> &'static str {
        match self {
            CompanionSource::Configured => "configured",
            CompanionSource::Legacy => "previous app's",
            CompanionSource::Default => "default",
        }
    }
}

/// The rule behind [`Config::companion_prefix`], with the two candidate
/// locations passed in so a test can point them at scratch directories.
///
/// The legacy prefix wins only when it is a directory. A config with no key on
/// a machine that never ran the previous application must not end up creating
/// `msfs-manager/` the first time someone sets a prefix up.
pub fn resolve_companion_prefix(
    configured: Option<&Path>,
    legacy: Option<PathBuf>,
    default: Option<PathBuf>,
) -> Option<(PathBuf, CompanionSource)> {
    if let Some(path) = configured {
        return Some((path.to_path_buf(), CompanionSource::Configured));
    }
    match legacy {
        Some(path) if path.is_dir() => Some((path, CompanionSource::Legacy)),
        _ => default.map(|path| (path, CompanionSource::Default)),
    }
}

/// Desktop navigation application; absence means read-only auto-discovery.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Navigation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub littlenavmap: Option<PathBuf>,
}

/// The Settings switches that outlive a run.
///
/// Stored here so they survive a relaunch: otherwise a person who turned
/// Community writes on, installed something and relaunched has the drop sheet
/// refuse an install they had already granted. Default off is the standing
/// constraint and it still holds — *default*, not *every time*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Toggles {
    /// May HangarRS move package directories in and out of Community?
    ///
    /// Off until a person says otherwise. Gates the profile switcher and every
    /// drop-install.
    pub community_writes: bool,
    /// Show the Prefix tab, which is hidden until asked for.
    pub show_prefix_tab: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: VERSION,
            libraries: Vec::new(),
            apps: Vec::new(),
            bridges: Bridges::default(),
            toggles: Toggles::default(),
            navigation: Navigation::default(),
            companion_prefix: None,
        }
    }
}

/// One launchable thing HangarRS did not find by scanning.
///
/// Its [`AppKind`] decides how it is run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppEntry {
    /// Display name, and the key preflight settings are stored against.
    pub name: String,
    pub exe: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub kind: AppKind,
    /// The app's own uninstaller, where it has one.
    ///
    /// Recorded rather than derived: it is the one path that cannot be found
    /// again once the app is gone, which is exactly when it is wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uninstaller: Option<PathBuf>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// How an app is run.
///
/// `overlay`, a kind the legacy config had, is absent because `hangar-runtime`
/// has no overlay: a config that can ask for one would be asking for something
/// nothing can do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    /// A Windows `.exe` in the companion Wine prefix.
    #[default]
    Companion,
    /// A Windows app inside the running simulator's Steam session.
    #[serde(rename = "sim-session")]
    SimSession,
    /// A native Linux binary — LittleNavMap, HeadtrackRS, `efbconnect`.
    Native,
}

/// The telemetry output bridges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bridges {
    pub lnm: LnmBridge,
    pub efb: EfbBridge,
}

/// LittleNavMap, over TCP. It connects to us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LnmBridge {
    pub enabled: bool,
    /// Must equal `hangar_bridges::lnm::DEFAULT_PORT`; a test over there says so.
    pub port: u16,
    pub rate_hz: u32,
}

impl Default for LnmBridge {
    fn default() -> Self {
        LnmBridge {
            enabled: false,
            port: 51968,
            rate_hz: 5,
        }
    }
}

/// Garmin Pilot and ForeFlight, over UDP broadcast. They never connect back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EfbBridge {
    pub enabled: bool,
    /// Must equal `hangar_bridges::efb::DEFAULT_PORT`; a test over there says so.
    ///
    /// Not configurable at the EFB's end, so changing it here only stops the
    /// tablet hearing anything.
    pub port: u16,
    /// An EFB redraws far slower than the sim produces frames, and every
    /// datagram is a broadcast every device on the network must look at.
    pub rate_hz: u32,
    /// The name embedded in the XGPS/XATT sentences, shown as the device name.
    pub app_name: String,
}

impl Default for EfbBridge {
    fn default() -> Self {
        EfbBridge {
            enabled: false,
            port: 49002,
            rate_hz: 5,
            app_name: "HangarRS".to_string(),
        }
    }
}

/// What reading the config file found.
///
/// [`Load::Missing`] and [`Load::Malformed`] are separate on purpose, and the
/// difference decides behaviour: no file at all means HangarRS has never been
/// configured, so the catalog may fall back to importing the previous
/// application's apps. A file that will not parse means it *has* been, and
/// falling back there would quietly show apps from a config the person migrated
/// away from — an edit that appears to have had no effect at all.
#[derive(Debug)]
pub enum Load {
    Missing,
    Malformed { path: PathBuf, reason: String },
    Present(Box<Config>),
}

impl Load {
    /// The config, if there is a readable one.
    pub fn ok(self) -> Option<Config> {
        match self {
            Load::Present(config) => Some(*config),
            _ => None,
        }
    }
}

impl Config {
    /// `$XDG_CONFIG_HOME/hangar-rs/config.toml`.
    pub fn path() -> Option<PathBuf> {
        paths::config_file()
    }

    /// Read the config from its usual place.
    pub fn load() -> Load {
        let Some(path) = Config::path() else {
            return Load::Missing;
        };
        Config::read(&path)
    }

    /// Read a config from a named file.
    pub fn read(path: &Path) -> Load {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // Anything that is not "no such file" is still a file that exists
            // and could not be read — a permissions problem reported as
            // "never configured" would silently resurrect the legacy apps.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Load::Missing,
            Err(e) => {
                return Load::Malformed {
                    path: path.to_path_buf(),
                    reason: e.to_string(),
                };
            }
        };
        match Config::parse(&text) {
            Ok(config) => Load::Present(Box::new(config)),
            Err(e) => Load::Malformed {
                path: path.to_path_buf(),
                reason: e.to_string(),
            },
        }
    }

    /// Parse config text.
    pub fn parse(text: &str) -> std::result::Result<Config, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialise to the text that would be written.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::malformed("config.toml", e.to_string()))
    }

    /// Write to the usual place, creating the directory if needed.
    pub fn save(&self) -> Result<()> {
        let Some(path) = Config::path() else {
            return Err(Error::NotFound {
                what: "a config directory",
                path: PathBuf::from("$XDG_CONFIG_HOME"),
            });
        };
        self.write_to(&path)
    }

    /// Write to a named file, atomically.
    ///
    /// Temp file in the **same directory**, `sync_all`, then rename — the rule
    /// the bindings writer and the install record already follow, for the same
    /// reason: a rename is only atomic within one filesystem, and `/tmp`
    /// usually is not one.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()?;
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(Error::write(dir))?;

        let temp = dir.join(format!(
            ".{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp).map_err(Error::write(&temp))?;
            file.write_all(text.as_bytes())
                .map_err(Error::write(&temp))?;
            file.sync_all().map_err(Error::write(&temp))?;
        }
        std::fs::rename(&temp, path).map_err(Error::write(path))?;
        Ok(())
    }

    /// Where the companion prefix is, and why.
    ///
    /// `companion_prefix` when set; otherwise the previous application's
    /// prefix if it exists, so an existing setup keeps working; otherwise
    /// `$XDG_DATA_HOME/hangar-rs/companion-prefix`.
    pub fn companion_prefix(&self) -> Option<(PathBuf, CompanionSource)> {
        resolve_companion_prefix(
            self.companion_prefix.as_deref(),
            paths::legacy_companion_prefix_dir(),
            paths::companion_prefix_dir(),
        )
    }

    /// The apps that should be offered, in file order.
    pub fn enabled_apps(&self) -> impl Iterator<Item = &AppEntry> {
        self.apps.iter().filter(|a| a.enabled)
    }
}

#[cfg(test)]
mod tests {
    /// Community writes must survive a relaunch, or the drop sheet refuses an
    /// install the person had already granted.
    #[test]
    fn the_settings_switches_survive_a_round_trip() {
        let mut config = Config::default();
        assert!(
            !config.toggles.community_writes,
            "off is still the default, and that is the standing constraint"
        );
        assert!(!config.toggles.show_prefix_tab);

        config.toggles.community_writes = true;
        config.toggles.show_prefix_tab = true;
        let text = config.to_toml().expect("serialises");
        let back: Config = toml::from_str(&text).expect("parses");
        assert_eq!(back.toggles, config.toggles);
    }

    /// `deny_unknown_fields` is on this struct, so a *new* field is the case
    /// that could have broken every config already on disk. It does not,
    /// because the struct carries `serde(default)` too.
    #[test]
    fn a_config_written_before_the_switches_existed_still_parses() {
        let old = r#"
version = 1
libraries = []
apps = []

[bridges]
"#;
        let config: Config = toml::from_str(old).expect("an older config still parses");
        assert!(!config.toggles.community_writes, "and is off, as it was");
    }

    use super::*;

    fn sample() -> Config {
        Config {
            apps: vec![AppEntry {
                name: "Project Airports".into(),
                exe: "/prefix/Project Airports.exe".into(),
                args: vec!["--no-sandbox".into()],
                kind: AppKind::Companion,
                uninstaller: Some("/prefix/Uninstall Project Airports.exe".into()),
                enabled: true,
            }],
            ..Config::default()
        }
    }

    #[test]
    fn a_config_survives_its_own_writer() {
        let text = sample().to_toml().unwrap();
        assert_eq!(Config::parse(&text).unwrap(), sample());
    }

    #[test]
    fn navigation_selection_roundtrips_and_old_configs_keep_discovery() {
        let old = Config::parse("version = 1").unwrap();
        assert!(old.navigation.littlenavmap.is_none());
        let mut configured = sample();
        configured.navigation.littlenavmap = Some("/apps/Little Navmap/littlenavmap".into());
        assert_eq!(
            Config::parse(&configured.to_toml().unwrap()).unwrap(),
            configured
        );
        assert!(Config::parse("[navigation]\nlittlenavma = '/misspelled'").is_err());
    }

    /// The whole reason for `deny_unknown_fields`: `save` rewrites the file, so
    /// a key tolerated on read is a key deleted on write.
    #[test]
    fn an_unknown_key_is_refused_rather_than_dropped() {
        let text = "version = 1\nlibraries = []\nfavourite_colour = \"orange\"\n";
        let err = Config::parse(text).unwrap_err().to_string();
        assert!(
            err.contains("favourite_colour"),
            "the error names it: {err}"
        );
    }

    #[test]
    fn an_unknown_key_on_an_app_is_refused_too() {
        let text = "[[apps]]\nname = \"X\"\nexe = \"/x\"\nprefix_mode = \"overlay\"\n";
        let err = Config::parse(text).unwrap_err().to_string();
        assert!(err.contains("prefix_mode"), "the error names it: {err}");
    }

    /// An empty file is a valid config, not a broken one: it is what a first
    /// run would leave behind.
    #[test]
    fn an_empty_file_reads_as_the_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    /// Absent means enabled. An app written by hand without the key should be
    /// offered, not silently withheld.
    #[test]
    fn an_app_is_enabled_unless_it_says_otherwise() {
        let config = Config::parse("[[apps]]\nname = \"X\"\nexe = \"/x\"\n").unwrap();
        assert!(config.apps[0].enabled);
        assert_eq!(config.apps[0].kind, AppKind::Companion);
        assert_eq!(config.enabled_apps().count(), 1);
    }

    /// `AppKind` is serialised lowercase; a rename here would turn a real value
    /// into an unknown one.
    #[test]
    fn app_kinds_round_trip_through_their_written_spelling() {
        for (spelling, kind) in [
            ("companion", AppKind::Companion),
            ("native", AppKind::Native),
            ("sim-session", AppKind::SimSession),
        ] {
            let text = format!("[[apps]]\nname = \"X\"\nexe = \"/x\"\nkind = \"{spelling}\"\n");
            assert_eq!(Config::parse(&text).unwrap().apps[0].kind, kind);
        }
    }

    #[test]
    fn a_missing_file_is_missing_and_not_malformed() {
        assert!(matches!(
            Config::read(Path::new("/nonexistent/hangar-rs/config.toml")),
            Load::Missing
        ));
    }

    #[test]
    fn a_file_that_will_not_parse_is_malformed_and_names_itself() {
        let dir = std::env::temp_dir().join(format!("hangar-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "this is not = valid = toml").unwrap();
        match Config::read(&path) {
            Load::Malformed { path: p, .. } => assert_eq!(p, path),
            other => panic!("expected Malformed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_written_config_reads_back_as_itself() {
        let dir = std::env::temp_dir().join(format!("hangar-config-w-{}", std::process::id()));
        let path = dir.join("config.toml");
        sample().write_to(&path).unwrap();
        match Config::read(&path) {
            Load::Present(config) => assert_eq!(*config, sample()),
            other => panic!("expected Present, got {other:?}"),
        }
        // Nothing left behind but the file itself.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            leftovers.len(),
            1,
            "a temp file was left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hangar-config-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The key that keeps an existing setup where it is: a config written
    /// before `companion_prefix` existed parses to `None`, not to the new
    /// default, and `None` is not written back.
    #[test]
    fn a_config_without_the_companion_key_keeps_it_absent() {
        let config = Config::parse("version = 1\n").unwrap();
        assert_eq!(config.companion_prefix, None);
        assert!(!config.to_toml().unwrap().contains("companion_prefix"));

        let set = Config {
            companion_prefix: Some("/somewhere/companion".into()),
            ..Config::default()
        };
        assert_eq!(Config::parse(&set.to_toml().unwrap()).unwrap(), set);
    }

    #[test]
    fn a_configured_companion_prefix_wins_over_everything() {
        let root = scratch("cfg");
        let legacy = root.join("msfs-manager/companion-prefix");
        std::fs::create_dir_all(&legacy).unwrap();
        let chosen = root.join("mine");
        assert_eq!(
            resolve_companion_prefix(Some(&chosen), Some(legacy), Some(root.join("new"))),
            Some((chosen, CompanionSource::Configured))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An existing setup keeps the previous application's prefix.
    #[test]
    fn an_unconfigured_setup_with_a_legacy_prefix_keeps_it() {
        let root = scratch("legacy");
        let legacy = root.join("msfs-manager/companion-prefix");
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(
            resolve_companion_prefix(None, Some(legacy.clone()), Some(root.join("new"))),
            Some((legacy, CompanionSource::Legacy))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A new user gets HangarRS' own location, never a fresh `msfs-manager/`.
    #[test]
    fn a_new_setup_gets_the_hangar_rs_location() {
        let root = scratch("new");
        let default = root.join("hangar-rs/companion-prefix");
        assert_eq!(
            resolve_companion_prefix(
                None,
                Some(root.join("msfs-manager/companion-prefix")),
                Some(default.clone())
            ),
            Some((default, CompanionSource::Default))
        );
        assert!(!root.join("msfs-manager").exists(), "nothing was created");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_default_location_belongs_to_hangar_rs() {
        let default = paths::companion_prefix_dir().unwrap();
        assert!(default.starts_with(paths::data_dir().unwrap()));
        assert!(default.ends_with("hangar-rs/companion-prefix"));
    }

    /// The EFB device name defaults to this application, not the previous one.
    #[test]
    fn the_efb_announces_itself_as_hangar_rs() {
        assert_eq!(EfbBridge::default().app_name, "HangarRS");
    }
}
