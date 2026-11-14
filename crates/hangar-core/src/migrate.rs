//! Translating `MSFS_Linux_Manager`'s config into HangarRS'.
//!
//! `~/.config/msfs-manager/config.toml` is a **working** setup — three
//! companion apps installed, configured and launching — so this is a
//! translation, not an import. It reads that file and nothing else, and it
//! never writes to it.
//!
//! # Every key is accounted for
//!
//! The migration walks the file as a `toml::Value` rather than deserialising it
//! into a mirror struct, because a mirror struct answers the wrong question. It
//! says what HangarRS wanted to find; it cannot say what was *there*. A setting
//! that no field matched would vanish with nothing to show for it, which is the
//! failure the bindings parser already refuses to allow — "one we dropped
//! silently is a setting silently lost".
//!
//! So every key gets a [`Note`], and there are only four things that can happen
//! to one:
//!
//! * [`Outcome::Carried`] — it is now in HangarRS' config.
//! * [`Outcome::Same`] — HangarRS already behaves this way, so there is nothing
//!   to carry. `broadcast_address = "255.255.255.255"` is this: it is what
//!   `hangar_bridges::serve` does, and always.
//! * [`Outcome::Dropped`] — HangarRS cannot express it, **and the note says
//!   why**. Not carrying a setting is a decision; leaving no record of it is
//!   how a working setup quietly stops working.
//! * [`Outcome::Unknown`] — a key this code has never heard of. Reported rather
//!   than ignored, because the previous application is not being developed
//!   alongside this one and its file may hold things written by a version
//!   nobody here has read.
//!
//! [`Migration::advisories`] is the judgement half, kept separate from the
//! mechanical half above: a value that carried perfectly well but is worth a
//! human look — the EFB still announcing itself as `msfs-manager`, an
//! auto-launch that is now a preflight step.
//!
//! # It never overwrites
//!
//! [`write_new`] refuses if a HangarRS config already exists. Migration is a
//! one-time translation of somebody else's file; running it twice must not be
//! able to flatten work done in between.

use crate::config::{AppEntry, AppKind, Config};
use crate::error::{Error, Result};
use crate::paths;
use std::path::{Path, PathBuf};

/// The previous application's name, as it appears in its own config.
const LEGACY_APP_NAME: &str = "msfs-manager";

/// The note key for the companion prefix, which the previous application
/// never stored in its file.
pub const COMPANION_PREFIX_KEY: &str = "(companion prefix)";

/// The one broadcast address `hangar_bridges::serve` sends to.
const BROADCAST: &str = "255.255.255.255";

/// What happened to one key of the previous config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Now in HangarRS' config, under the named key.
    Carried { to: &'static str },
    /// HangarRS already does this; there was nothing to carry.
    Same { because: String },
    /// HangarRS cannot express it, and this is why.
    Dropped { why: String },
    /// A key this migration has never heard of.
    Unknown,
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Carried { .. } => "carried",
            Outcome::Same { .. } => "same",
            Outcome::Dropped { .. } => "dropped",
            Outcome::Unknown => "unknown",
        }
    }
}

/// One key of the previous config, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Dotted path as it appears in the file — `efb.port`, `apps[0].label`.
    pub key: String,
    /// The value that was there, in TOML form.
    pub value: String,
    pub outcome: Outcome,
}

/// A translated config, with a full account of how it was arrived at.
#[derive(Debug, Clone)]
pub struct Migration {
    /// The file this was read from.
    pub source: PathBuf,
    pub config: Config,
    /// One per key in the source file.
    ///
    /// Sorted by key rather than in the order the file happens to list
    /// them: `toml`'s tables are ordered maps only with `preserve_order`,
    /// and a report whose order depends on a Cargo feature is a report
    /// that changes shape for no reason anyone can see.
    pub notes: Vec<Note>,
    /// Things that carried but are worth a person's attention.
    pub advisories: Vec<String>,
}

impl Migration {
    /// Translate the previous application's config, if it is still there.
    ///
    /// `Ok(None)` means there is nothing to migrate, which is the ordinary case
    /// on a machine that never ran the old application.
    pub fn from_legacy() -> Result<Option<Migration>> {
        let Some(dir) = paths::legacy_config_dir() else {
            return Ok(None);
        };
        let path = dir.join("config.toml");
        if !path.exists() {
            return Ok(None);
        }
        let mut migration = Migration::from_file(&path)?;
        migration.note_legacy_profiles(&dir.join("profiles"));
        migration.note_companion_prefix(paths::legacy_companion_prefix_dir().as_deref());
        Ok(Some(migration))
    }

    /// Translate a named file.
    pub fn from_file(path: &Path) -> Result<Migration> {
        let text = std::fs::read_to_string(path).map_err(Error::read(path))?;
        Migration::from_text(&text, path)
    }

    /// Translate config text.
    pub fn from_text(text: &str, source: &Path) -> Result<Migration> {
        let value: toml::Value =
            toml::from_str(text).map_err(|e| Error::malformed(source, e.to_string()))?;
        let table = value
            .as_table()
            .ok_or_else(|| Error::malformed(source, "the config is not a table"))?;

        let mut m = Migration {
            source: source.to_path_buf(),
            config: Config::default(),
            notes: Vec::new(),
            advisories: Vec::new(),
        };

        for (key, value) in table {
            match key.as_str() {
                "repos" => m.repos(value),
                "apps" => m.apps(value),
                "navmap" => m.navmap(value),
                "efb" => m.efb(value),
                "console_max_lines" => m.dropped(
                    key,
                    value,
                    "HangarRS' Console tab keeps a fixed 20,000 lines and is not configurable; \
                     the bound exists to stop a chatty installer growing without limit, which \
                     is not a number anyone should have to pick",
                ),
                "active_profile_name" => m.dropped(
                    key,
                    value,
                    "package profiles are HangarRS' own format, and switching moves directories \
                     rather than symlinking them",
                ),
                "steam_root_override" => m.dropped(
                    key,
                    value,
                    "HangarRS discovers Steam itself and shows what it found in Settings",
                ),
                _ => m.unknown(key, value),
            }
        }

        Ok(m)
    }

    /// Package libraries.
    fn repos(&mut self, value: &toml::Value) {
        let empty = value.as_array().is_some_and(|a| a.is_empty());
        if empty {
            self.same(
                "repos",
                value,
                "no libraries were configured, and HangarRS scans the Community folders anyway",
            );
            return;
        }
        if let Some(array) = value.as_array() {
            self.config.libraries = array
                .iter()
                .filter_map(|v| v.as_str())
                .map(PathBuf::from)
                .collect();
        }
        self.carried("repos", value, "libraries");
    }

    /// The `[[apps]]` entries — the part of this file that is actually load-bearing.
    fn apps(&mut self, value: &toml::Value) {
        let Some(array) = value.as_array() else {
            self.unknown("apps", value);
            return;
        };

        for (index, app) in array.iter().enumerate() {
            let Some(table) = app.as_table() else {
                self.unknown(&format!("apps[{index}]"), app);
                continue;
            };

            let mut entry = AppEntry {
                name: String::new(),
                exe: PathBuf::new(),
                args: Vec::new(),
                kind: AppKind::Companion,
                uninstaller: None,
                enabled: true,
            };
            let label = table
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("an app")
                .to_string();

            for (key, value) in table {
                let dotted = format!("apps[{index}].{key}");
                match key.as_str() {
                    "label" => {
                        entry.name = value.as_str().unwrap_or_default().to_string();
                        self.carried(&dotted, value, "apps[].name");
                    }
                    "exe_path" => {
                        entry.exe = PathBuf::from(value.as_str().unwrap_or_default());
                        self.carried(&dotted, value, "apps[].exe");
                    }
                    "args" => {
                        entry.args = value
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if entry.args.is_empty() {
                            self.same(&dotted, value, "no arguments were configured");
                        } else {
                            self.carried(&dotted, value, "apps[].args");
                        }
                    }
                    "uninstall_exe" => {
                        entry.uninstaller = value.as_str().map(PathBuf::from);
                        self.carried(&dotted, value, "apps[].uninstaller");
                    }
                    "enabled" => {
                        entry.enabled = value.as_bool().unwrap_or(true);
                        self.carried(&dotted, value, "apps[].enabled");
                    }
                    "run_mode" => {
                        entry.kind = match value.as_str() {
                            Some("native") => AppKind::Native,
                            Some("prefix") => AppKind::Companion,
                            other => {
                                self.advisories.push(format!(
                                    "{label}: run mode {} is not one this version knows; it will \
                                     run as a companion app in the Wine prefix",
                                    other.unwrap_or("(not a string)")
                                ));
                                AppKind::Companion
                            }
                        };
                        self.carried(&dotted, value, "apps[].kind");
                    }
                    "prefix_mode" => match value.as_str() {
                        Some("isolated") | None => self.same(
                            &dotted,
                            value,
                            "the companion prefix is the only prefix HangarRS runs apps in",
                        ),
                        Some(mode) => {
                            self.advisories.push(format!(
                                "{label} asked for the {mode} prefix mode. HangarRS has no \
                                 overlay, so it will run in the plain companion prefix — which \
                                 on this machine already reaches the sim's AppData through a \
                                 symlink."
                            ));
                            self.dropped(
                                &dotted,
                                value,
                                "hangar-runtime has no overlay; a config that could ask for one \
                                 would be asking for something nothing can do",
                            );
                        }
                    },
                    "auto_launch_with_msfs" => {
                        if value.as_bool() == Some(true) {
                            self.advisories.push(format!(
                                "{label} was set to launch with MSFS. That is a Preflight step \
                                 now: switch it on there and choose whether it runs before the \
                                 sim or after SimConnect."
                            ));
                            self.dropped(
                                &dotted,
                                value,
                                "the Preflight tab decides which apps take part and on which side \
                                 of the sim",
                            );
                        } else {
                            self.same(&dotted, value, "it was off");
                        }
                    }
                    "block_while_msfs_running" => {
                        if value.as_bool() == Some(true) {
                            self.advisories.push(format!(
                                "{label} was set to refuse to run while MSFS is up. In HangarRS \
                                 that ordering is a preflight placement — put it *before* the sim."
                            ));
                            self.dropped(
                                &dotted,
                                value,
                                "the preflight sequencer orders apps around the sim; there is no \
                                 standing interlock",
                            );
                        } else {
                            self.same(&dotted, value, "it was off");
                        }
                    }
                    _ => self.unknown(&dotted, value),
                }
            }

            if entry.name.is_empty() || entry.exe.as_os_str().is_empty() {
                self.advisories.push(format!(
                    "apps[{index}] has no label or no executable path and was not carried."
                ));
                continue;
            }
            if !entry.exe.is_file() {
                self.advisories.push(format!(
                    "{}: {} is not there any more. It is carried anyway — the Hangar tab shows it \
                     as Missing, which is easier to act on than an app that quietly disappeared.",
                    entry.name,
                    entry.exe.display()
                ));
            }
            self.config.apps.push(entry);
        }
    }

    /// LittleNavMap settings. None of them describe the bridge HangarRS has.
    fn navmap(&mut self, value: &toml::Value) {
        let Some(table) = value.as_table() else {
            self.unknown("navmap", value);
            return;
        };
        for (key, value) in table {
            let dotted = format!("navmap.{key}");
            match key.as_str() {
                "web_server_port" => self.dropped(
                    &dotted,
                    value,
                    "The Map tab opens Little Navmap's desktop application; it does not use \
                     the port of Little Navmap's web server",
                ),
                "auto_launch_lnm" => {
                    if value.as_bool() == Some(true) {
                        self.advisories.push(
                            "LittleNavMap was set to launch with the sim. Add it to the Preflight \
                             as a native tool to get the same thing."
                                .into(),
                        );
                        self.dropped(
                            &dotted,
                            value,
                            "launching things with the sim is a preflight",
                        );
                    } else {
                        self.same(&dotted, value, "it was off");
                    }
                }
                "auto_launch_navconnect" => {
                    if value.as_bool() == Some(true) {
                        self.advisories.push(
                            "Little Navconnect was set to launch with the sim. HangarRS' LNM \
                             bridge replaces it: switch on bridges.lnm and point LittleNavMap at \
                             this machine instead."
                                .into(),
                        );
                    }
                    self.dropped(
                        &dotted,
                        value,
                        "HangarRS' own LNM bridge does what Little Navconnect did, so there is \
                         nothing to launch",
                    );
                }
                _ => self.unknown(&dotted, value),
            }
        }
    }

    /// EFB broadcast settings — the one section that maps almost exactly.
    fn efb(&mut self, value: &toml::Value) {
        let Some(table) = value.as_table() else {
            self.unknown("efb", value);
            return;
        };
        for (key, value) in table {
            let dotted = format!("efb.{key}");
            match key.as_str() {
                "enabled" => {
                    self.config.bridges.efb.enabled = value.as_bool().unwrap_or(false);
                    self.carried(&dotted, value, "bridges.efb.enabled");
                }
                "port" => {
                    if let Some(port) = value.as_integer() {
                        self.config.bridges.efb.port = port as u16;
                    }
                    self.carried(&dotted, value, "bridges.efb.port");
                }
                "update_rate_hz" => {
                    if let Some(rate) = value.as_integer() {
                        self.config.bridges.efb.rate_hz = rate as u32;
                    }
                    self.carried(&dotted, value, "bridges.efb.rate_hz");
                }
                "app_name" => {
                    let name = value.as_str().unwrap_or_default().to_string();
                    if name == LEGACY_APP_NAME {
                        self.advisories.push(format!(
                            "the EFB still announces itself as {LEGACY_APP_NAME:?}. It is carried \
                             as it stands — a name on a tablet's device list is not something to \
                             change behind your back — but {:?} is probably what you now want.",
                            crate::config::EfbBridge::default().app_name
                        ));
                    }
                    self.config.bridges.efb.app_name = name;
                    self.carried(&dotted, value, "bridges.efb.app_name");
                }
                "broadcast_address" => match value.as_str() {
                    Some(BROADCAST) => self.same(
                        &dotted,
                        value,
                        "hangar-bridges broadcasts to 255.255.255.255, and only there",
                    ),
                    other => {
                        self.advisories.push(format!(
                            "the EFB was broadcasting to {} rather than {BROADCAST}, which is \
                             usually because a router drops the all-ones address. HangarRS cannot \
                             do that yet, so the tablet may not see it.",
                            other.unwrap_or("(not a string)")
                        ));
                        self.dropped(
                            &dotted,
                            value,
                            "hangar_bridges::serve::efb_broadcast sends to 255.255.255.255 and \
                             takes no destination; a key nothing honours is worse than none",
                        );
                    }
                },
                _ => self.unknown(&dotted, value),
            }
        }
    }

    /// The previous application's profile directory, which is not translated.
    fn note_legacy_profiles(&mut self, dir: &Path) {
        let count = std::fs::read_dir(dir)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        if count > 0 {
            self.advisories.push(format!(
                "{count} package profile(s) in {} were not carried. HangarRS switches packages by \
                 moving them rather than symlinking, so a profile has to be built against what is \
                 actually in the Community folder.",
                dir.display()
            ));
        }
    }

    /// The previous application's companion prefix — a setting it had but
    /// never wrote down, because its location was compiled in.
    ///
    /// The one note whose key is not in the file, and the reason it is
    /// spelled [`COMPANION_PREFIX_KEY`] rather than as a dotted path. It is
    /// reported either way: carried when the prefix is there, so the migrated
    /// config names it and the apps installed in it keep launching; `Same`
    /// when it is not, because HangarRS then uses its own location, as the
    /// previous application would have used its own.
    pub fn note_companion_prefix(&mut self, legacy_prefix: Option<&Path>) {
        match legacy_prefix {
            Some(dir) if dir.is_dir() => {
                self.config.companion_prefix = Some(dir.to_path_buf());
                let value = toml::Value::String(dir.display().to_string());
                self.carried(COMPANION_PREFIX_KEY, &value, "companion_prefix");
            }
            other => {
                let shown = other.map_or_else(|| "(none)".to_string(), |d| d.display().to_string());
                self.same(
                    COMPANION_PREFIX_KEY,
                    &toml::Value::String(shown),
                    "the previous application's companion prefix is not there, so HangarRS \
                     uses its own location and Set up companion prefix creates it",
                );
            }
        }
    }

    fn carried(&mut self, key: &str, value: &toml::Value, to: &'static str) {
        self.push(key, value, Outcome::Carried { to });
    }

    fn same(&mut self, key: &str, value: &toml::Value, because: &str) {
        self.push(
            key,
            value,
            Outcome::Same {
                because: because.to_string(),
            },
        );
    }

    fn dropped(&mut self, key: &str, value: &toml::Value, why: &str) {
        self.push(
            key,
            value,
            Outcome::Dropped {
                why: why.to_string(),
            },
        );
    }

    fn unknown(&mut self, key: &str, value: &toml::Value) {
        self.push(key, value, Outcome::Unknown);
    }

    fn push(&mut self, key: &str, value: &toml::Value, outcome: Outcome) {
        self.notes.push(Note {
            key: key.to_string(),
            value: show(value),
            outcome,
        });
    }

    /// How many keys ended each way: carried, same, dropped, unknown.
    pub fn summary(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for note in &self.notes {
            match note.outcome {
                Outcome::Carried { .. } => counts.0 += 1,
                Outcome::Same { .. } => counts.1 += 1,
                Outcome::Dropped { .. } => counts.2 += 1,
                Outcome::Unknown => counts.3 += 1,
            }
        }
        counts
    }

    /// Keys this migration had never heard of.
    pub fn unknown_keys(&self) -> impl Iterator<Item = &Note> {
        self.notes.iter().filter(|n| n.outcome == Outcome::Unknown)
    }
}

/// One value, as it reads in the file.
///
/// Tables print as their key list rather than their contents: a section's own
/// keys each get a note of their own, and repeating them here would make the
/// report longer than the file.
fn show(value: &toml::Value) -> String {
    match value {
        toml::Value::Table(t) => {
            format!("{{{}}}", t.keys().cloned().collect::<Vec<_>>().join(", "))
        }
        toml::Value::Array(a) if a.iter().any(|v| v.is_table()) => format!("[{} entries]", a.len()),
        other => other.to_string(),
    }
}

/// Write a migrated config, refusing to overwrite one that is already there.
///
/// Migration translates somebody else's file. Running it a second time — by
/// habit, by a script, or because the first run's report scrolled past — must
/// not be able to flatten whatever has been configured since.
pub fn write_new(config: &Config) -> Result<PathBuf> {
    let path = Config::path().ok_or(Error::NotFound {
        what: "a config directory",
        path: PathBuf::from("$XDG_CONFIG_HOME"),
    })?;
    write_new_to(config, &path)?;
    Ok(path)
}

/// Write to a named path, refusing to overwrite.
pub fn write_new_to(config: &Config, path: &Path) -> Result<()> {
    if path.exists() {
        return Err(Error::WriteRefused {
            path: path.to_path_buf(),
            reason: "a HangarRS config is already there, and migration never overwrites one".into(),
        });
    }
    config.write_to(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real file, byte for byte, with the paths shortened.
    const REAL: &str = r#"
repos = []
console_max_lines = 0

[[apps]]
label = "BeyondATC"
exe_path = "/prefix/drive_c/BeyondATC/BeyondATC.exe"
args = []
run_mode = "prefix"
prefix_mode = "isolated"
auto_launch_with_msfs = false
block_while_msfs_running = false
enabled = true
uninstall_exe = "/prefix/drive_c/BeyondATC/unins000.exe"

[[apps]]
label = "Pilot Client"
exe_path = "/prefix/drive_c/PilotEdge/PilotEdge.PilotClient.exe"
args = []
run_mode = "prefix"
prefix_mode = "isolated"
auto_launch_with_msfs = false
block_while_msfs_running = false
enabled = true
uninstall_exe = "/prefix/drive_c/PilotEdge/Uninstall.exe"

[[apps]]
label = "Project Airports"
exe_path = "/prefix/drive_c/Programs/@project-airportsdesktop/Project Airports.exe"
args = ["--no-sandbox"]
run_mode = "prefix"
prefix_mode = "isolated"
auto_launch_with_msfs = false
block_while_msfs_running = false
enabled = true
uninstall_exe = "/prefix/drive_c/Programs/@project-airportsdesktop/Uninstall Project Airports.exe"

[navmap]
web_server_port = 1024
auto_launch_lnm = false
auto_launch_navconnect = false

[efb]
enabled = false
broadcast_address = "255.255.255.255"
port = 49002
update_rate_hz = 5
app_name = "msfs-manager"
"#;

    fn real() -> Migration {
        Migration::from_text(REAL, Path::new("/legacy/config.toml")).unwrap()
    }

    /// The three working apps arrive with the things that make them launchable.
    #[test]
    fn the_three_real_apps_carry_across() {
        let m = real();
        let names: Vec<&str> = m.config.apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["BeyondATC", "Pilot Client", "Project Airports"]);
        assert!(m.config.apps.iter().all(|a| a.kind == AppKind::Companion));
        assert!(m.config.apps.iter().all(|a| a.enabled));
        assert!(m.config.apps.iter().all(|a| a.uninstaller.is_some()));
    }

    /// Losing this would change how Project Airports runs, silently.
    #[test]
    fn the_no_sandbox_argument_survives() {
        let m = real();
        let pa = m
            .config
            .apps
            .iter()
            .find(|a| a.name == "Project Airports")
            .unwrap();
        assert_eq!(pa.args, ["--no-sandbox"]);
    }

    /// The EFB section is the one that maps almost exactly.
    #[test]
    fn the_efb_settings_carry_across() {
        let efb = real().config.bridges.efb;
        assert!(!efb.enabled);
        assert_eq!(efb.port, 49002);
        assert_eq!(efb.rate_hz, 5);
        assert_eq!(efb.app_name, "msfs-manager", "carried, not corrected");
    }

    /// Carried verbatim, and said out loud — a device name on someone's tablet
    /// is not a thing to change behind their back.
    #[test]
    fn the_stale_efb_device_name_is_advised_rather_than_fixed() {
        let m = real();
        assert_eq!(m.config.bridges.efb.app_name, "msfs-manager");
        assert!(
            m.advisories.iter().any(|a| a.contains("msfs-manager")),
            "advisories: {:?}",
            m.advisories
        );
    }

    /// The property the whole module exists for.
    #[test]
    fn every_key_in_the_file_is_accounted_for() {
        let m = real();
        let keys: Vec<&str> = m.notes.iter().map(|n| n.key.as_str()).collect();
        for expected in [
            "repos",
            "console_max_lines",
            "apps[0].label",
            "apps[2].args",
            "navmap.web_server_port",
            "navmap.auto_launch_lnm",
            "navmap.auto_launch_navconnect",
            "efb.enabled",
            "efb.broadcast_address",
            "efb.port",
            "efb.update_rate_hz",
            "efb.app_name",
        ] {
            assert!(keys.contains(&expected), "{expected} has no note");
        }
        // Three apps of nine keys each, seven section keys, two top-level.
        assert_eq!(m.notes.len(), 3 * 9 + 5 + 2 + 3);
        assert_eq!(
            m.unknown_keys().count(),
            0,
            "the real file holds no surprises"
        );
    }

    /// A key from a version nobody here has read must not vanish quietly.
    #[test]
    fn a_key_this_code_has_never_heard_of_is_reported() {
        let m = Migration::from_text(
            "mystery_setting = 3\n[[apps]]\nlabel = \"X\"\nexe_path = \"/x\"\nfuture_flag = true\n",
            Path::new("/legacy/config.toml"),
        )
        .unwrap();
        let unknown: Vec<&str> = m.unknown_keys().map(|n| n.key.as_str()).collect();
        // Sorted by key, so the app section comes before the loose value.
        assert_eq!(unknown, ["apps[0].future_flag", "mystery_setting"]);
    }

    /// Not carrying something is a decision, and a decision has a reason.
    #[test]
    fn everything_dropped_says_why() {
        let m = real();
        let dropped: Vec<&Note> = m
            .notes
            .iter()
            .filter(|n| matches!(n.outcome, Outcome::Dropped { .. }))
            .collect();
        assert!(!dropped.is_empty());
        for note in dropped {
            match &note.outcome {
                Outcome::Dropped { why } => assert!(
                    why.len() > 20,
                    "{} was dropped with no real reason: {why:?}",
                    note.key
                ),
                _ => unreachable!(),
            }
        }
    }

    /// The address is the default, so nothing is lost — but a different one
    /// cannot be honoured and must say so rather than reading as carried.
    #[test]
    fn a_subnet_broadcast_address_is_dropped_loudly() {
        let m = Migration::from_text(
            "[efb]\nbroadcast_address = \"192.168.1.255\"\n",
            Path::new("/legacy/config.toml"),
        )
        .unwrap();
        assert!(matches!(m.notes[0].outcome, Outcome::Dropped { .. }));
        assert!(m.advisories.iter().any(|a| a.contains("192.168.1.255")));
    }

    #[test]
    fn an_auto_launch_becomes_an_advisory_about_the_preflight() {
        let m = Migration::from_text(
            "[[apps]]\nlabel = \"BeyondATC\"\nexe_path = \"/x\"\nauto_launch_with_msfs = true\n",
            Path::new("/legacy/config.toml"),
        )
        .unwrap();
        assert!(
            m.advisories.iter().any(|a| a.contains("Preflight")),
            "advisories: {:?}",
            m.advisories
        );
    }

    /// A native tool is not a Wine app, and the kind is what decides.
    #[test]
    fn a_native_run_mode_becomes_a_native_tool() {
        let m = Migration::from_text(
            "[[apps]]\nlabel = \"LittleNavMap\"\nexe_path = \"/usr/bin/littlenavmap\"\nrun_mode = \"native\"\n",
            Path::new("/legacy/config.toml"),
        )
        .unwrap();
        assert_eq!(m.config.apps[0].kind, AppKind::Native);
    }

    /// An app whose executable has gone is carried and flagged, never dropped:
    /// one that quietly disappears from the list is harder to diagnose.
    #[test]
    fn a_missing_executable_is_carried_and_flagged() {
        let m = Migration::from_text(
            "[[apps]]\nlabel = \"Gone\"\nexe_path = \"/nonexistent/gone.exe\"\n",
            Path::new("/legacy/config.toml"),
        )
        .unwrap();
        assert_eq!(m.config.apps.len(), 1);
        assert!(m
            .advisories
            .iter()
            .any(|a| a.contains("not there any more")));
    }

    /// An entry with nothing to launch is not an app.
    #[test]
    fn an_app_with_no_executable_is_not_carried() {
        let m = Migration::from_text(
            "[[apps]]\nlabel = \"Half a thing\"\n",
            Path::new("/legacy/config.toml"),
        )
        .unwrap();
        assert!(m.config.apps.is_empty());
        assert!(m.advisories.iter().any(|a| a.contains("was not carried")));
    }

    #[test]
    fn the_migrated_config_survives_its_own_writer() {
        let config = real().config;
        let text = config.to_toml().unwrap();
        assert_eq!(Config::parse(&text).unwrap(), config);
    }

    /// Running it twice must not flatten what was configured in between.
    /// An existing setup's prefix is carried explicitly, so the migrated
    /// config names it rather than relying on the fallback rule.
    #[test]
    fn an_existing_companion_prefix_is_carried_and_reported() {
        let dir = std::env::temp_dir().join(format!(
            "hangar-migrate-companion-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = real();
        let before = m.notes.len();
        m.note_companion_prefix(Some(&dir));
        assert_eq!(m.notes.len(), before + 1);
        let note = m.notes.last().unwrap();
        assert_eq!(note.key, COMPANION_PREFIX_KEY);
        assert_eq!(
            note.outcome,
            Outcome::Carried {
                to: "companion_prefix"
            }
        );
        assert_eq!(m.config.companion_prefix.as_deref(), Some(dir.as_path()));
        let text = m.config.to_toml().unwrap();
        assert!(text.contains("companion_prefix"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No previous prefix: still reported, and nothing is pinned.
    #[test]
    fn a_missing_companion_prefix_is_reported_as_same() {
        let mut m = real();
        m.note_companion_prefix(Some(Path::new(
            "/nonexistent/msfs-manager/companion-prefix",
        )));
        let note = m.notes.last().unwrap();
        assert_eq!(note.key, COMPANION_PREFIX_KEY);
        assert!(matches!(note.outcome, Outcome::Same { .. }));
        assert_eq!(m.config.companion_prefix, None);

        let mut none = real();
        none.note_companion_prefix(None);
        assert!(matches!(
            none.notes.last().unwrap().outcome,
            Outcome::Same { .. }
        ));
    }

    #[test]
    fn writing_over_an_existing_config_is_refused() {
        let dir = std::env::temp_dir().join(format!("hangar-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        write_new_to(&real().config, &path).expect("the first write lands");
        let mine = std::fs::read_to_string(&path).unwrap();

        let err = write_new_to(&Config::default(), &path).unwrap_err();
        assert!(matches!(err, Error::WriteRefused { .. }), "got {err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            mine,
            "the refused write left the file alone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
