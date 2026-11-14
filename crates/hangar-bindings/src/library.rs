//! Everything loaded, once, in the shape the GUI reads it.
//!
//! A handful of profiles is over a megabyte of XML, far too much to parse per
//! frame. So this is built once, handed out behind an `Arc`, and replaced
//! wholesale when something changes.
//!
//! A profile that fails to parse does not take the others down: it lands in
//! [`Library::problems`] with the reason, and the rest still load.

use crate::catalog::Catalog;
use crate::{discover, Profile, ProfileSummary};
use std::path::{Path, PathBuf};

/// One profile, read and described.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub summary: ProfileSummary,
    pub profile: Profile,
}

impl Loaded {
    pub fn id(&self) -> &str {
        &self.summary.id
    }

    pub fn name(&self) -> &str {
        &self.profile.friendly_name.name
    }

    pub fn device(&self) -> &str {
        &self.profile.device.name
    }

    pub fn bound_count(&self) -> usize {
        self.profile.device.bound().count()
    }

    pub fn action_count(&self) -> usize {
        self.profile.device.actions().count()
    }
}

/// The whole binding library as of the last load.
#[derive(Debug, Clone, Default)]
pub struct Library {
    /// The Steam Cloud directory it came from, if one was found.
    pub remote_dir: Option<PathBuf>,
    pub profiles: Vec<Loaded>,
    pub catalog: Catalog,
    /// Profiles that could not be read, and why. Shown, never swallowed.
    pub problems: Vec<(String, String)>,
}

impl Library {
    /// Read every profile in a `remote/` directory.
    pub fn load(remote_dir: impl AsRef<Path>) -> Self {
        let remote_dir = remote_dir.as_ref();
        let mut profiles = Vec::new();
        let mut problems = Vec::new();

        let summaries = match discover::list(remote_dir) {
            Ok(s) => s,
            Err(e) => {
                return Library {
                    remote_dir: Some(remote_dir.to_path_buf()),
                    problems: vec![(remote_dir.display().to_string(), e.to_string())],
                    ..Default::default()
                }
            }
        };

        for summary in summaries {
            match crate::read(&summary.path) {
                Ok(profile) => profiles.push(Loaded { summary, profile }),
                Err(e) => problems.push((summary.id.clone(), e.to_string())),
            }
        }

        let catalog =
            Catalog::from_profiles(profiles.iter().map(|l| (l.summary.id.as_str(), &l.profile)));

        Library {
            remote_dir: Some(remote_dir.to_path_buf()),
            profiles,
            catalog,
            problems,
        }
    }

    /// Read from wherever Steam keeps them, if that can be found.
    pub fn discover(steam: &hangar_steam::SteamEnv) -> Self {
        match discover::remote_dirs(steam).into_iter().next() {
            Some(dir) => Self::load(dir),
            None => Library::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn get(&self, id: &str) -> Option<&Loaded> {
        self.profiles.iter().find(|l| l.id() == id)
    }

    /// Bindings across every profile.
    pub fn bound_count(&self) -> usize {
        self.profiles.iter().map(Loaded::bound_count).sum()
    }

    /// Distinct action names across every profile.
    pub fn distinct_actions(&self) -> usize {
        self.catalog.distinct_actions()
    }

    /// Every control bound to more than one action, across every profile.
    pub fn shared(&self) -> Vec<crate::catalog::Shared> {
        self.profiles
            .iter()
            .flat_map(|l| crate::catalog::shared(l.id(), &l.profile))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, bound: bool) -> String {
        let action = if bound {
            concat!(
                "\t\t<Action ActionName=\"KEY_ELEV_TRIM_UP\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
                "\t\t\t<Primary>\n\t\t\t\t<KEY Information=\"Joystick Button 8\">7</KEY>\n\t\t\t</Primary>\n",
                "\t\t</Action>\n",
            ).to_string()
        } else {
            "\t\t<Action ActionName=\"KEY_ELEV_TRIM_UP\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n".to_string()
        };
        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<Version Num=\"-1\"/>\n",
                "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">{}</FriendlyName>\n",
                "<Device DeviceName=\"V1\" GUID=\"{{9E5}}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                "\t<Context ContextName=\"AIRCRAFT\">\n{}\t</Context>\n",
                "</Device>\n\n",
            ),
            name, action
        )
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hangar-library-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_library_loads_every_profile_and_builds_one_catalogue() {
        let dir = scratch("load");
        std::fs::write(dir.join("inputprofile_0000000001"), fixture("Pedals", true)).unwrap();
        std::fs::write(dir.join("inputprofile_0000000002"), fixture("Stick", false)).unwrap();
        std::fs::write(dir.join("profile_00"), "not a binding file").unwrap();

        let library = Library::load(&dir);
        assert_eq!(library.len(), 2);
        assert_eq!(library.bound_count(), 1);
        assert_eq!(library.catalog.len(), 2);
        assert_eq!(library.distinct_actions(), 1, "the same action in both");
        assert!(library.problems.is_empty());
        assert_eq!(
            library.get("inputprofile_0000000001").unwrap().name(),
            "Pedals"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_broken_profile_does_not_hide_the_others() {
        let dir = scratch("broken");
        std::fs::write(dir.join("inputprofile_0000000001"), fixture("Pedals", true)).unwrap();
        // Peekable but truncated, so it lists and then fails to parse.
        std::fs::write(
            dir.join("inputprofile_0000000009"),
            fixture("Broken", true).replace("</Device>\n\n", ""),
        )
        .unwrap();

        let library = Library::load(&dir);
        assert_eq!(library.len(), 1, "the good one still loaded");
        assert_eq!(library.problems.len(), 1);
        assert_eq!(library.problems[0].0, "inputprofile_0000000009");
        assert!(!library.problems[0].1.is_empty(), "the reason is kept");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_directory_is_a_problem_not_a_panic() {
        let library = Library::load("/nonexistent/remote");
        assert!(library.is_empty());
        assert_eq!(library.problems.len(), 1);
    }

    #[test]
    fn an_empty_library_is_usable() {
        let library = Library::default();
        assert!(library.is_empty());
        assert_eq!(library.bound_count(), 0);
        assert_eq!(library.distinct_actions(), 0);
        assert!(library.shared().is_empty());
    }
}
