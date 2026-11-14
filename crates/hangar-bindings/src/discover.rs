//! Finding the profiles.
//!
//! They are **not** in the Proton prefix. There is no `Input/` folder anywhere
//! under `AppData/Roaming/Microsoft Flight Simulator 2024/`; MSFS 2024 keeps
//! bindings in Steam Cloud userdata:
//!
//! ```text
//! ~/.local/share/Steam/userdata/<account>/2537590/remote/inputprofile_<10 digits>
//! ```
//!
//! Three consequences. They are **cloud-synced**, so a bad sync, a reinstall
//! or a second machine can replace them without this app being involved. They
//! sit **outside the prefix**, so a prefix-level backup misses them. And
//! `remotecache.vdf` beside them tracks sync state, readable with
//! `hangar-steam`'s VDF parser.

use crate::{BindingsError, Result};
use std::path::{Path, PathBuf};

/// MSFS 2024's Steam App ID.
pub const MSFS_2024_APP_ID: &str = "2537590";

/// A profile file found on disk, described without parsing all of it.
///
/// Listing profiles should not cost a full parse of each, so this reads the
/// head of each and stops. [`crate::read`] gets the rest when a profile is
/// actually opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSummary {
    pub path: PathBuf,
    /// `inputprofile_1035869184` — the file name, which is also the profile's
    /// identity as far as the sim is concerned.
    pub id: String,
    /// The name shown in the sim's controls menu.
    pub friendly_name: String,
    /// The device as its driver reports it.
    pub device_name: String,
    pub locked: bool,
    pub bytes: u64,
}

/// Every `remote/` directory for MSFS 2024, one per Steam account on the box.
pub fn remote_dirs(steam: &hangar_steam::SteamEnv) -> Vec<PathBuf> {
    steam.cloud_remote_dirs(MSFS_2024_APP_ID)
}

/// List the input profiles in one `remote/` directory.
///
/// Sorted by file name, so the order is stable between runs — the directory
/// order is not.
pub fn list(remote_dir: impl AsRef<Path>) -> Result<Vec<ProfileSummary>> {
    let remote_dir = remote_dir.as_ref();
    let entries = std::fs::read_dir(remote_dir).map_err(|source| BindingsError::Read {
        path: remote_dir.to_path_buf(),
        source,
    })?;

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_profile_name(p))
        .collect();
    paths.sort();

    let mut found = Vec::with_capacity(paths.len());
    for path in paths {
        match peek(&path) {
            Ok(summary) => found.push(summary),
            // One unreadable profile should not hide the others.
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "skipping profile"),
        }
    }
    Ok(found)
}

/// `inputprofile_` followed by digits, and nothing else in the directory.
///
/// `remote/` also holds `profile_00`, `kh_logbook`, `datausage` and whatever
/// else the sim and its add-ons keep in the cloud, none of which are bindings.
fn is_profile_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    match name.strip_prefix("inputprofile_") {
        Some(rest) => !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// How much of the file to read for a summary. The declaration, `<Version>`,
/// `<FriendlyName>` and the `<Device>` open tag fit inside this several times
/// over; the longest real device name is 62 characters.
const HEAD_BYTES: usize = 4096;

/// Read enough of a profile to describe it, without parsing the rest.
pub fn peek(path: impl AsRef<Path>) -> Result<ProfileSummary> {
    use std::io::Read;

    let path = path.as_ref();
    let mut file = std::fs::File::open(path).map_err(|source| BindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = file
        .metadata()
        .map(|m| m.len())
        .map_err(|source| BindingsError::Read {
            path: path.to_path_buf(),
            source,
        })?;

    let mut head = vec![0u8; HEAD_BYTES];
    let read = file.read(&mut head).map_err(|source| BindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    head.truncate(read);
    // A multi-byte character can straddle the cut. The fields wanted here are
    // far earlier than that, so a lossy decode of the tail is harmless.
    let head = String::from_utf8_lossy(&head);

    let friendly_name = between(&head, "<FriendlyName", "</FriendlyName>")
        .and_then(|s| s.split_once('>').map(|(_, name)| name.to_string()))
        .ok_or_else(|| BindingsError::malformed(path, "no <FriendlyName> in the first 4 kB"))?;
    let locked = attr(&head, "Locked").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let device_name = attr(&head, "DeviceName").ok_or_else(|| {
        BindingsError::malformed(path, "no <Device DeviceName> in the first 4 kB")
    })?;

    Ok(ProfileSummary {
        id: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_path_buf(),
        friendly_name: unescape_minimal(&friendly_name),
        device_name: unescape_minimal(&device_name),
        locked,
        bytes,
    })
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let rest = &text[from..];
    let to = rest.find(end)?;
    Some(&rest[..to])
}

fn attr(text: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let from = text.find(&needle)? + needle.len();
    let rest = &text[from..];
    let to = rest.find('"')?;
    Some(rest[..to].to_string())
}

/// The inverse of the writer's minimal escape. No real profile contains an
/// entity, but a summary should not show `&amp;` to someone if one appears.
fn unescape_minimal(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_input_profiles_are_listed() {
        assert!(is_profile_name(Path::new("/r/inputprofile_1035869184")));
        assert!(!is_profile_name(Path::new("/r/profile_00")));
        assert!(!is_profile_name(Path::new("/r/kh_logbook")));
        assert!(!is_profile_name(Path::new("/r/datausage")));
        assert!(!is_profile_name(Path::new("/r/inputprofile_")));
        assert!(!is_profile_name(Path::new("/r/inputprofile_backup1")));
    }

    #[test]
    fn a_summary_comes_out_of_the_head_alone() {
        let head = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<Version Num=\"-1\"/>\n",
            "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">My General V1 Pedals</FriendlyName>\n",
            "<Device DeviceName=\"Turtle Beach VelocityOne Rudder\" GUID=\"{9E5}\" ",
            "ProductID=\"28690\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
        );
        assert_eq!(
            between(head, "<FriendlyName", "</FriendlyName>")
                .and_then(|s| s.split_once('>'))
                .map(|(_, n)| n),
            Some("My General V1 Pedals")
        );
        assert_eq!(
            attr(head, "DeviceName").as_deref(),
            Some("Turtle Beach VelocityOne Rudder")
        );
        assert_eq!(attr(head, "Locked").as_deref(), Some("false"));
    }
}
