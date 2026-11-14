//! Reading the previous application's registered apps.
//!
//! `MSFS_Linux_Manager` keeps its companion apps in
//! `~/.config/msfs-manager/config.toml` as `[[apps]]` entries. Without them an
//! unmigrated setup's Apps section would be empty of the very things it exists
//! to show.
//!
//! **Read-only, and a fallback.** [`crate::apps`] reads HangarRS' own config
//! first and only comes here when there is no such file — a machine that has
//! not been migrated yet. `hangar_core::migrate` is the translation that makes
//! this module stop being consulted; nothing here writes to the old config or
//! runs anything.

use crate::item::{Item, ItemKind, ItemState};
use hangar_core::paths;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The `[[apps]]` entries, and nothing else from that file.
#[derive(Debug, Default, Deserialize)]
struct LegacyConfig {
    #[serde(default)]
    apps: Vec<LegacyApp>,
}

#[derive(Debug, Deserialize)]
struct LegacyApp {
    label: String,
    exe_path: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    enabled: bool,
}

/// Read the previous app's registered apps, if that config is still around.
pub fn legacy_apps() -> Vec<Item> {
    let Some(path) = paths::legacy_config_dir().map(|d| d.join("config.toml")) else {
        return Vec::new();
    };
    read_apps(&path)
}

/// Parse `[[apps]]` from a config file.
pub fn read_apps(path: &Path) -> Vec<Item> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_apps(&text)
}

/// Parse `[[apps]]` from config text.
pub fn parse_apps(text: &str) -> Vec<Item> {
    // A config carrying settings this struct does not model must still yield
    // its apps, so unknown keys are ignored rather than failing the parse.
    let config: LegacyConfig = match toml::from_str(text) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not read legacy apps: {e}");
            return Vec::new();
        }
    };

    config
        .apps
        .into_iter()
        .filter(|app| app.enabled)
        .map(|app| Item {
            // The executable's presence is the honest thing to report. Whether
            // it is *running* is `hangar-runtime`'s answer.
            state: if app.exe_path.is_file() {
                ItemState::Ready
            } else {
                ItemState::MissingExecutable
            },
            title: app.label.clone(),
            name: app.label,
            kind: ItemKind::CompanionApp,
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: app.exe_path,
            community: None,
            args: app.args,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real config, including the keys this struct ignores.
    const REAL: &str = r#"
repos = []
console_max_lines = 0

[[apps]]
label = "BeyondATC"
exe_path = "/nonexistent/BeyondATC.exe"
args = []
run_mode = "prefix"
prefix_mode = "isolated"
auto_launch_with_msfs = false
enabled = true
uninstall_exe = "/nonexistent/unins000.exe"

[[apps]]
label = "Project Airports"
exe_path = "/nonexistent/Project Airports.exe"
args = ["--no-sandbox"]
run_mode = "prefix"
enabled = true

[[apps]]
label = "Retired Thing"
exe_path = "/nonexistent/old.exe"
enabled = false

[navmap]
web_server_port = 1024
"#;

    #[test]
    fn reads_the_enabled_apps_from_a_real_shaped_config() {
        let apps = parse_apps(REAL);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["BeyondATC", "Project Airports"]);
        assert!(apps.iter().all(|a| a.kind == ItemKind::CompanionApp));
    }

    /// Keys this struct does not model — `navmap`, `run_mode`, `uninstall_exe`
    /// — must not stop the apps loading.
    #[test]
    fn unmodelled_keys_are_ignored() {
        assert_eq!(parse_apps(REAL).len(), 2);
    }

    /// Project Airports needs `--no-sandbox`; losing it would change how it runs.
    #[test]
    fn configured_arguments_are_kept() {
        let apps = parse_apps(REAL);
        let pa = apps.iter().find(|a| a.name == "Project Airports").unwrap();
        assert_eq!(pa.args, ["--no-sandbox"]);
        let bata = apps.iter().find(|a| a.name == "BeyondATC").unwrap();
        assert!(bata.args.is_empty());
    }

    #[test]
    fn a_disabled_app_is_not_imported() {
        let apps = parse_apps(REAL);
        assert!(!apps.iter().any(|a| a.name == "Retired Thing"));
    }

    /// A missing executable is reported rather than hidden: an app that quietly
    /// disappears from the list is harder to diagnose than one marked broken.
    #[test]
    fn a_missing_executable_is_flagged_not_hidden() {
        let apps = parse_apps(REAL);
        assert!(apps.iter().all(|a| a.state == ItemState::MissingExecutable));
        assert!(apps[0].state.is_broken());
        assert!(!apps[0].state.is_active());
    }

    #[test]
    fn a_present_executable_reads_as_ready() {
        let dir = std::env::temp_dir().join(format!("hangar-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"MZ").unwrap();

        let apps = parse_apps(&format!(
            "[[apps]]\nlabel = \"Present\"\nexe_path = {:?}\nenabled = true\n",
            exe.display().to_string()
        ));
        assert_eq!(apps[0].state, ItemState::Ready);
        assert!(!apps[0].state.is_broken());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_config_yields_nothing() {
        assert!(read_apps(Path::new("/nonexistent/config.toml")).is_empty());
        assert!(parse_apps("this is not = valid = toml").is_empty());
        assert!(parse_apps("").is_empty());
    }
}
