//! Named sets of enabled packages, stored one TOML file per profile.

use hangar_core::paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("a profile named {0:?} already exists")]
    Exists(String),

    #[error("no profile named {0:?}")]
    NotFound(String),

    #[error("a profile needs a name")]
    EmptyName,

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a valid profile: {reason}")]
    Malformed { path: PathBuf, reason: String },
}

type Result<T> = std::result::Result<T, ProfileError>;

/// A named set of packages that should be enabled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Package directory names.
    ///
    /// A `BTreeSet` rather than a `Vec`: order carries no meaning, duplicates
    /// are not a thing, and the switcher wants set arithmetic anyway.
    #[serde(default)]
    pub packages: BTreeSet<String>,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            packages: BTreeSet::new(),
        }
    }

    /// Capture whatever is enabled right now.
    pub fn from_enabled(name: impl Into<String>, enabled: BTreeSet<String>) -> Self {
        Self {
            name: name.into(),
            packages: enabled,
        }
    }

    pub fn contains(&self, package: &str) -> bool {
        self.packages.contains(package)
    }

    pub fn set(&mut self, package: &str, on: bool) {
        if on {
            self.packages.insert(package.to_owned());
        } else {
            self.packages.remove(package);
        }
    }
}

/// Profiles on disk.
#[derive(Debug, Default)]
pub struct ProfileStore {
    dir: PathBuf,
    profiles: BTreeMap<String, Profile>,
}

impl ProfileStore {
    /// Load every `.toml` in `dir`. A missing directory is an empty store.
    pub fn load(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let mut profiles = BTreeMap::new();

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                match read_profile(&path) {
                    Ok(p) => {
                        profiles.insert(p.name.clone(), p);
                    }
                    // A malformed profile must not stop the others loading.
                    Err(e) => tracing::warn!("ignoring profile {}: {e}", path.display()),
                }
            }
        }

        Self { dir, profiles }
    }

    /// Load from the standard config location.
    pub fn load_default() -> Self {
        match paths::profiles_dir() {
            Some(dir) => Self::load(dir),
            None => Self::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn names(&self) -> Vec<&str> {
        self.profiles.keys().map(String::as_str).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Save a profile, creating or replacing it.
    pub fn save(&mut self, profile: Profile) -> Result<()> {
        if profile.name.trim().is_empty() {
            return Err(ProfileError::EmptyName);
        }
        write_profile(&self.dir, &profile)?;
        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }

    /// Create a profile, refusing to clobber an existing one.
    pub fn create(&mut self, profile: Profile) -> Result<()> {
        if self.profiles.contains_key(&profile.name) {
            return Err(ProfileError::Exists(profile.name));
        }
        self.save(profile)
    }

    pub fn delete(&mut self, name: &str) -> Result<()> {
        let profile = self
            .profiles
            .remove(name)
            .ok_or_else(|| ProfileError::NotFound(name.to_owned()))?;
        let path = self.dir.join(file_name(&profile.name));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ProfileError::Write { path, source }),
        }
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if to.trim().is_empty() {
            return Err(ProfileError::EmptyName);
        }
        if self.profiles.contains_key(to) {
            return Err(ProfileError::Exists(to.to_owned()));
        }
        let mut profile = self
            .profiles
            .remove(from)
            .ok_or_else(|| ProfileError::NotFound(from.to_owned()))?;
        let old_path = self.dir.join(file_name(&profile.name));
        profile.name = to.to_owned();
        write_profile(&self.dir, &profile)?;
        let _ = std::fs::remove_file(old_path);
        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }
}

fn read_profile(path: &Path) -> Result<Profile> {
    let text = std::fs::read_to_string(path).map_err(|source| ProfileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let profile: Profile = toml::from_str(&text).map_err(|e| ProfileError::Malformed {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    if profile.name.trim().is_empty() {
        return Err(ProfileError::Malformed {
            path: path.to_path_buf(),
            reason: "the profile has no name".into(),
        });
    }
    Ok(profile)
}

/// Write a profile, atomically.
///
/// Temp file plus rename, so an interrupted write cannot leave a half-written
/// profile that then fails to parse and quietly disappears from the list.
fn write_profile(dir: &Path, profile: &Profile) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|source| ProfileError::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(file_name(&profile.name));
    let text = toml::to_string_pretty(profile).map_err(|e| ProfileError::Malformed {
        path: path.clone(),
        reason: e.to_string(),
    })?;

    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|source| ProfileError::Write {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, &path).map_err(|source| ProfileError::Write {
        path: path.clone(),
        source,
    })
}

/// A filesystem-safe file name for a profile.
///
/// The profile's real name lives *inside* the file, so this only has to be
/// unique and safe — it never has to round-trip back to the name.
fn file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.toml", safe.trim_matches('_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-profile-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn profile(name: &str, packages: &[&str]) -> Profile {
        Profile::from_enabled(name, packages.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn a_saved_profile_round_trips_through_disk() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        assert!(store.is_empty());

        store
            .save(profile("VFR Bush", &["koak", "bfg-aca-scout"]))
            .unwrap();

        let reloaded = ProfileStore::load(&dir);
        assert_eq!(reloaded.len(), 1);
        let p = reloaded.get("VFR Bush").unwrap();
        assert_eq!(p.name, "VFR Bush");
        assert!(p.contains("koak"));
        assert!(p.contains("bfg-aca-scout"));
        assert!(!p.contains("something-else"));
    }

    #[test]
    fn creating_over_an_existing_name_is_refused_but_saving_replaces() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.create(profile("Default", &["a"])).unwrap();

        let err = store.create(profile("Default", &["b"])).unwrap_err();
        assert!(matches!(err, ProfileError::Exists(_)));
        assert!(store.get("Default").unwrap().contains("a"));

        store.save(profile("Default", &["b"])).unwrap();
        assert!(store.get("Default").unwrap().contains("b"));
    }

    #[test]
    fn deleting_removes_the_file_and_the_entry() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.save(profile("Gone", &[])).unwrap();
        store.delete("Gone").unwrap();

        assert!(store.is_empty());
        assert!(ProfileStore::load(&dir).is_empty());
        assert!(matches!(
            store.delete("Gone").unwrap_err(),
            ProfileError::NotFound(_)
        ));
    }

    #[test]
    fn renaming_moves_the_file_and_keeps_the_packages() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.save(profile("Old", &["koak"])).unwrap();
        store.rename("Old", "New").unwrap();

        let reloaded = ProfileStore::load(&dir);
        assert_eq!(reloaded.names(), ["New"]);
        assert!(reloaded.get("New").unwrap().contains("koak"));
        assert!(reloaded.get("Old").is_none());
    }

    #[test]
    fn renaming_onto_an_existing_name_is_refused() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.save(profile("A", &[])).unwrap();
        store.save(profile("B", &[])).unwrap();
        assert!(matches!(
            store.rename("A", "B").unwrap_err(),
            ProfileError::Exists(_)
        ));
        assert_eq!(store.len(), 2);
    }

    /// A name with slashes must not escape the profiles directory.
    #[test]
    fn awkward_names_produce_safe_file_names() {
        assert_eq!(file_name("VFR Bush"), "VFR_Bush.toml");
        assert_eq!(file_name("../../etc/passwd"), "etc_passwd.toml");
        assert!(!file_name("a/b").contains('/'));

        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.save(profile("../escape", &["x"])).unwrap();
        // The real name survives inside the file, whatever the file is called.
        assert!(ProfileStore::load(&dir).get("../escape").is_some());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn an_unnamed_profile_is_refused() {
        let mut store = ProfileStore::load(scratch());
        assert!(matches!(
            store.save(Profile::new("   ")).unwrap_err(),
            ProfileError::EmptyName
        ));
    }

    /// One bad file must not hide the good ones.
    #[test]
    fn a_malformed_profile_does_not_stop_the_others_loading() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.save(profile("Good", &["a"])).unwrap();
        std::fs::write(dir.join("broken.toml"), "this is not = valid = toml").unwrap();
        std::fs::write(dir.join("nameless.toml"), "packages = []").unwrap();
        std::fs::write(dir.join("ignored.txt"), "not a profile").unwrap();

        let reloaded = ProfileStore::load(&dir);
        assert_eq!(reloaded.names(), ["Good"]);
    }

    /// An interrupted write must not leave a profile that fails to parse.
    #[test]
    fn writes_leave_no_partial_file_behind() {
        let dir = scratch();
        let mut store = ProfileStore::load(&dir);
        store.save(profile("Atomic", &["a", "b"])).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn toggling_a_package_updates_the_set() {
        let mut p = Profile::new("P");
        p.set("koak", true);
        p.set("koak", true); // idempotent
        assert_eq!(p.packages.len(), 1);
        p.set("koak", false);
        assert!(p.packages.is_empty());
    }
}
