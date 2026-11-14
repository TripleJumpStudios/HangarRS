//! Where HangarRS keeps its own files.
//!
//! Every path here is under XDG directories that belong to this application.
//! Nothing in this module points at Steam, the MSFS prefix, or the Community
//! folder — those are discovered at runtime by `hangar-steam` and `hangar-msfs`,
//! and are subject to the standing read-only constraint.

use std::path::PathBuf;

/// Directory name used under every XDG base directory.
///
/// Matches the binary name and the Wayland `app_id`, so a `.desktop` file, the
/// config directory and the window all agree.
pub const APP_DIR: &str = "hangar-rs";

/// The previous application's config directory.
///
/// Read by the migration, and by the catalog until one has run, so apps
/// configured there are not registered again. Read-only: HangarRS never
/// writes here.
pub const LEGACY_APP_DIR: &str = "msfs-manager";

/// `$XDG_CONFIG_HOME/hangar-rs`, falling back to `~/.config/hangar-rs`.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_DIR))
}

/// `$XDG_DATA_HOME/hangar-rs`, falling back to `~/.local/share/hangar-rs`.
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(APP_DIR))
}

/// `$XDG_CACHE_HOME/hangar-rs` — regenerable things only.
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join(APP_DIR))
}

/// The main config file.
pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// One TOML file per profile.
pub fn profiles_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("profiles"))
}

/// Timestamped binding snapshots.
///
/// Data rather than config: these are the only copies of the input profiles
/// that live outside Steam Cloud, so losing them to a cache clear would defeat
/// the point of taking them.
pub fn bindings_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("bindings"))
}

/// Saved preflights, one TOML file each.
///
/// Config rather than data: a preflight is a choice someone made about how they
/// like to start a flight, in the same category as a package profile.
pub fn preflights_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("preflights"))
}

/// The user's additions to the vendor table.
///
/// Config, and optional: HangarRS ships its own table compiled in, and this
/// file adds to it or overrides an entry. A missing one is the normal case.
pub fn vendors_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("vendors.toml"))
}

/// Install history — one record per completed install.
pub fn install_history_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("installs"))
}

/// The previous application's config directory, for migration.
pub fn legacy_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(LEGACY_APP_DIR))
}

/// The previous application's data directory.
///
/// Holds its companion Wine prefix, which an existing setup keeps using rather
/// than reinstalling working apps into a new one.
pub fn legacy_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(LEGACY_APP_DIR))
}

/// Where a new setup's companion prefix goes:
/// `$XDG_DATA_HOME/hangar-rs/companion-prefix`.
///
/// Data rather than cache: installed companion apps live in it.
pub fn companion_prefix_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("companion-prefix"))
}

/// The previous application's companion prefix,
/// `$XDG_DATA_HOME/msfs-manager/companion-prefix`.
pub fn legacy_companion_prefix_dir() -> Option<PathBuf> {
    legacy_data_dir().map(|d| d.join("companion-prefix"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_sits_under_its_xdg_base() {
        let config = config_dir().expect("a config dir");
        assert!(config.ends_with(APP_DIR));
        assert!(config_file().unwrap().starts_with(&config));
        assert!(profiles_dir().unwrap().starts_with(&config));

        let data = data_dir().expect("a data dir");
        assert!(bindings_dir().unwrap().starts_with(&data));
        assert!(install_history_dir().unwrap().starts_with(&data));
        assert!(companion_prefix_dir().unwrap().starts_with(&data));
    }

    /// Snapshots must not live in the cache: they are the only copies of the
    /// input profiles outside Steam Cloud.
    #[test]
    fn bindings_are_data_not_cache() {
        let bindings = bindings_dir().unwrap();
        assert!(bindings.starts_with(data_dir().unwrap()));
        if let Some(cache) = cache_dir() {
            assert!(!bindings.starts_with(cache));
        }
    }

    #[test]
    fn the_legacy_directory_is_a_different_application() {
        assert_ne!(APP_DIR, LEGACY_APP_DIR);
        assert_ne!(config_dir(), legacy_config_dir());
        assert_ne!(companion_prefix_dir(), legacy_companion_prefix_dir());
    }
}
