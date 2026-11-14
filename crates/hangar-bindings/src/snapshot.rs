//! Timestamped copies of every input profile.
//!
//! The bindings are Steam Cloud userdata: a bad sync, a reinstall or a second
//! machine can replace them without anyone doing anything, and **a
//! prefix-level backup misses them** because they are not in the prefix.
//!
//! So a snapshot is not a convenience for the editor. It writes only into
//! `~/.local/share/hangar-rs/bindings/`, and every write to `remote/` takes
//! one first.
//!
//! # What a snapshot is
//!
//! ```text
//! ~/.local/share/hangar-rs/bindings/2026-08-24T14-30-05Z/
//!     snapshot.toml            what this is, when, from where, and why
//!     inputprofile_0106087416  byte-for-byte copies
//!     inputprofile_0242070832
//!     …
//! ```
//!
//! The copies are plain files, not an archive. Restoring by hand with `cp` has
//! to remain possible when this application is not the thing that is working.
//!
//! Every copy is read back and compared after writing. A backup that was never
//! verified is a belief, not a backup.

use crate::discover;
use crate::{BindingsError, ProfileSummary, Result};
use hangar_core::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The manifest file inside a snapshot directory.
pub const MANIFEST: &str = "snapshot.toml";

/// One taken snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The directory holding it.
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Snapshot {
    /// When it was taken, from the directory name, falling back to the
    /// manifest. The name is authoritative because it is what sorts.
    pub fn taken(&self) -> Option<Utc> {
        self.dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(Utc::parse_stamp)
            .or_else(|| Utc::parse_stamp(&self.manifest.taken))
    }

    pub fn profile(&self, id: &str) -> Option<&SnapshotEntry> {
        self.manifest.profiles.iter().find(|p| p.id == id)
    }

    /// Total bytes copied.
    pub fn bytes(&self) -> u64 {
        self.manifest.profiles.iter().map(|p| p.bytes).sum()
    }
}

/// `snapshot.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `2026-08-24T14-30-05Z`.
    pub taken: String,
    /// The `remote/` directory the copies came from.
    pub source: PathBuf,
    /// Why it was taken — "before editing elevator trim", "manual", or the
    /// name of whatever operation asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, rename = "profile")]
    pub profiles: Vec<SnapshotEntry>,
}

/// One copied profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// `inputprofile_1035869184`, which is also the file name in both places.
    pub id: String,
    pub bytes: u64,
    /// The name the sim shows. Empty when the profile could not be read far
    /// enough to find it — the copy is still taken.
    #[serde(default)]
    pub friendly_name: String,
    #[serde(default)]
    pub device_name: String,
    /// Whether this crate could parse the file at the time of copying.
    ///
    /// A profile that does not parse is still copied — a backup of a file we
    /// do not understand is worth more than no backup — but it is recorded,
    /// because it means the editor cannot touch it.
    #[serde(default)]
    pub parsed: bool,
}

/// Copy every input profile in `remote_dir` into a new timestamped directory
/// under `into`.
///
/// Read-only with respect to Steam: the source files are opened for reading
/// and nothing else. The only writes are inside `into`.
pub fn take(
    remote_dir: impl AsRef<Path>,
    into: impl AsRef<Path>,
    note: Option<String>,
) -> Result<Snapshot> {
    let remote_dir = remote_dir.as_ref();
    let into = into.as_ref();
    let now = Utc::now();

    let summaries = discover::list(remote_dir)?;
    if summaries.is_empty() {
        return Err(BindingsError::malformed(
            remote_dir,
            "no input profiles here, so there is nothing to snapshot",
        ));
    }

    let dir = unused_dir(into, &now.stamp())?;
    std::fs::create_dir_all(&dir).map_err(write_err(&dir))?;

    let mut profiles = Vec::with_capacity(summaries.len());
    for summary in &summaries {
        profiles.push(copy_verified(summary, &dir)?);
    }

    let manifest = Manifest {
        taken: now.stamp(),
        source: remote_dir.to_path_buf(),
        note,
        profiles,
    };
    write_manifest(&dir, &manifest)?;

    tracing::info!(
        dir = %dir.display(),
        profiles = manifest.profiles.len(),
        "took a bindings snapshot"
    );
    Ok(Snapshot { dir, manifest })
}

/// Copy one profile and confirm the copy matches, byte for byte.
fn copy_verified(summary: &ProfileSummary, into: &Path) -> Result<SnapshotEntry> {
    let source = std::fs::read(&summary.path).map_err(|source| BindingsError::Read {
        path: summary.path.clone(),
        source,
    })?;

    let dest = into.join(&summary.id);
    std::fs::write(&dest, &source).map_err(write_err(&dest))?;

    let written = std::fs::read(&dest).map_err(|source| BindingsError::Read {
        path: dest.clone(),
        source,
    })?;
    if written != source {
        return Err(BindingsError::Snapshot {
            path: dest,
            reason: format!(
                "the copy does not match the original ({} bytes in, {} bytes back)",
                source.len(),
                written.len()
            ),
        });
    }

    // Whether it parses is recorded, not required. The copy is the point.
    let parsed = std::str::from_utf8(&source)
        .ok()
        .is_some_and(|text| crate::parse(text).is_ok());
    if !parsed {
        tracing::warn!(
            id = %summary.id,
            "copied a profile this crate cannot parse; it is backed up but not editable"
        );
    }

    Ok(SnapshotEntry {
        id: summary.id.clone(),
        bytes: source.len() as u64,
        friendly_name: summary.friendly_name.clone(),
        device_name: summary.device_name.clone(),
        parsed,
    })
}

/// Every snapshot under `dir`, newest first.
///
/// A directory whose name is not a timestamp is not a snapshot and is skipped
/// silently — this is the user's own data directory and they may keep things
/// in it.
pub fn list(dir: impl AsRef<Path>) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(dir.as_ref()) else {
        return Vec::new();
    };

    let mut found: Vec<Snapshot> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|name| {
                        let stamp = name
                            .rsplit_once('.')
                            .filter(|(_, suffix)| suffix.parse::<u32>().is_ok())
                            .map_or(name, |(stamp, _)| stamp);
                        Utc::parse_stamp(stamp)
                    })
                    .is_some()
        })
        .filter_map(|p| match load(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(dir = %p.display(), error = %e, "skipping snapshot");
                None
            }
        })
        .collect();

    // Newest first: the stamp sorts by time, so this is a reverse name sort.
    found.sort_by(|a, b| b.dir.cmp(&a.dir));
    found
}

/// Read one snapshot's manifest.
pub fn load(dir: impl AsRef<Path>) -> Result<Snapshot> {
    let dir = dir.as_ref();
    let path = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&path).map_err(|source| BindingsError::Read {
        path: path.clone(),
        source,
    })?;
    let manifest: Manifest = toml::from_str(&text)
        .map_err(|e| BindingsError::malformed(&path, format!("not a snapshot manifest: {e}")))?;
    Ok(Snapshot {
        dir: dir.to_path_buf(),
        manifest,
    })
}

/// Confirm every file a snapshot claims is present and the right size.
///
/// Cheap enough to run when a snapshot is picked from a list, and it is the
/// difference between offering a restore and offering a half-restore.
pub fn verify(snapshot: &Snapshot) -> Result<()> {
    for entry in &snapshot.manifest.profiles {
        let path = snapshot.dir.join(&entry.id);
        let actual = std::fs::metadata(&path)
            .map(|m| m.len())
            .map_err(|source| BindingsError::Read {
                path: path.clone(),
                source,
            })?;
        if actual != entry.bytes {
            return Err(BindingsError::Snapshot {
                path,
                reason: format!("{} bytes on disk, manifest says {}", actual, entry.bytes),
            });
        }
    }
    Ok(())
}

/// The path of one profile inside a snapshot.
pub fn profile_path(snapshot: &Snapshot, id: &str) -> PathBuf {
    snapshot.dir.join(id)
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<()> {
    let path = dir.join(MANIFEST);
    let text = toml::to_string_pretty(manifest)
        .map_err(|e| BindingsError::malformed(&path, format!("could not render manifest: {e}")))?;
    std::fs::write(&path, text).map_err(write_err(&path))
}

/// A stamp is one second wide, so two snapshots in the same second would land
/// on the same directory. Overwriting one backup with another is exactly the
/// failure a backup exists to prevent, so suffix instead.
fn unused_dir(into: &Path, stamp: &str) -> Result<PathBuf> {
    let first = into.join(stamp);
    if !first.exists() {
        return Ok(first);
    }
    for n in 2..100 {
        let candidate = into.join(format!("{stamp}.{n}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(BindingsError::Snapshot {
        path: first,
        reason: "a hundred snapshots already exist for this second".to_string(),
    })
}

fn write_err(path: &Path) -> impl FnOnce(std::io::Error) -> BindingsError {
    let path = path.to_path_buf();
    move |source| BindingsError::Write { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but real-shaped profile, enough to parse and to peek at.
    fn fixture(friendly: &str, device: &str) -> String {
        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<Version Num=\"-1\"/>\n",
                "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">{}</FriendlyName>\n",
                "<Device DeviceName=\"{}\" GUID=\"{{9E5}}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                "\t<Context ContextName=\"AIRCRAFT\">\n",
                "\t\t<Action ActionName=\"KEY_ELEV_TRIM_UP\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
                "\t</Context>\n",
                "</Device>\n",
                "\n",
            ),
            friendly, device
        )
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hangar-bindings-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_snapshot_copies_every_profile_and_nothing_else() {
        let root = scratch("take");
        let remote = root.join("remote");
        let store = root.join("store");
        std::fs::create_dir_all(&remote).unwrap();

        std::fs::write(
            remote.join("inputprofile_0000000001"),
            fixture("Pedals", "V1"),
        )
        .unwrap();
        std::fs::write(
            remote.join("inputprofile_0000000002"),
            fixture("Stick", "EVO"),
        )
        .unwrap();
        // The things that also live in remote/ and are not bindings.
        std::fs::write(remote.join("profile_00"), "not a binding").unwrap();
        std::fs::write(remote.join("kh_logbook"), "also not").unwrap();

        let snapshot = take(&remote, &store, Some("before a test".into())).unwrap();

        assert_eq!(snapshot.manifest.profiles.len(), 2);
        assert!(snapshot.dir.join("inputprofile_0000000001").is_file());
        assert!(snapshot.dir.join("inputprofile_0000000002").is_file());
        assert!(!snapshot.dir.join("profile_00").exists());
        assert!(!snapshot.dir.join("kh_logbook").exists());
        assert_eq!(snapshot.manifest.note.as_deref(), Some("before a test"));
        assert!(snapshot.manifest.profiles.iter().all(|p| p.parsed));
        assert_eq!(
            snapshot
                .profile("inputprofile_0000000001")
                .map(|p| p.friendly_name.as_str()),
            Some("Pedals")
        );

        verify(&snapshot).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_copies_are_byte_identical() {
        let root = scratch("bytes");
        let remote = root.join("remote");
        let store = root.join("store");
        std::fs::create_dir_all(&remote).unwrap();
        let original = fixture("Pedals", "V1");
        std::fs::write(remote.join("inputprofile_0000000001"), &original).unwrap();

        let snapshot = take(&remote, &store, None).unwrap();
        let copied =
            std::fs::read_to_string(profile_path(&snapshot, "inputprofile_0000000001")).unwrap();
        assert_eq!(copied, original);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_profile_that_does_not_parse_is_still_backed_up() {
        let root = scratch("unparseable");
        let remote = root.join("remote");
        let store = root.join("store");
        std::fs::create_dir_all(&remote).unwrap();
        // Peekable — it has a FriendlyName and a DeviceName — but truncated,
        // so the parser cannot finish it.
        let broken = fixture("Broken", "Ghost").replace("</Device>\n\n", "");
        std::fs::write(remote.join("inputprofile_0000000009"), &broken).unwrap();

        let snapshot = take(&remote, &store, None).unwrap();
        let entry = snapshot
            .profile("inputprofile_0000000009")
            .expect("it was copied");
        assert!(!entry.parsed, "a truncated profile should be flagged");
        assert_eq!(
            std::fs::read_to_string(profile_path(&snapshot, "inputprofile_0000000009")).unwrap(),
            broken,
            "and copied anyway"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_snapshot_never_overwrites_an_earlier_one() {
        let root = scratch("collide");
        let store = root.join("store");
        std::fs::create_dir_all(store.join("2026-08-24T14-30-05Z")).unwrap();

        let second = unused_dir(&store, "2026-08-24T14-30-05Z").unwrap();
        assert_eq!(second.file_name().unwrap(), "2026-08-24T14-30-05Z.2");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn snapshots_come_back_newest_first_and_ignore_other_directories() {
        let root = scratch("list");
        let remote = root.join("remote");
        let store = root.join("store");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(
            remote.join("inputprofile_0000000001"),
            fixture("Pedals", "V1"),
        )
        .unwrap();

        for stamp in ["2026-08-24T14-30-05Z", "2025-01-02T03-04-05Z"] {
            let dir = store.join(stamp);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(
                remote.join("inputprofile_0000000001"),
                dir.join("inputprofile_0000000001"),
            )
            .unwrap();
            write_manifest(
                &dir,
                &Manifest {
                    taken: stamp.to_string(),
                    source: remote.clone(),
                    note: None,
                    profiles: vec![SnapshotEntry {
                        id: "inputprofile_0000000001".into(),
                        bytes: std::fs::metadata(dir.join("inputprofile_0000000001"))
                            .unwrap()
                            .len(),
                        friendly_name: "Pedals".into(),
                        device_name: "V1".into(),
                        parsed: true,
                    }],
                },
            )
            .unwrap();
        }
        // Not a snapshot, and not to be mistaken for one.
        std::fs::create_dir_all(store.join("scratch-notes")).unwrap();

        let found = list(&store);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].manifest.taken, "2026-08-24T14-30-05Z");
        assert_eq!(found[1].manifest.taken, "2025-01-02T03-04-05Z");
        assert_eq!(found[0].taken().unwrap().year, 2026);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn verify_notices_a_file_that_changed_since() {
        let root = scratch("verify");
        let remote = root.join("remote");
        let store = root.join("store");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(
            remote.join("inputprofile_0000000001"),
            fixture("Pedals", "V1"),
        )
        .unwrap();

        let snapshot = take(&remote, &store, None).unwrap();
        verify(&snapshot).unwrap();

        std::fs::write(
            profile_path(&snapshot, "inputprofile_0000000001"),
            "truncated",
        )
        .unwrap();
        let err = verify(&snapshot).expect_err("a shortened copy must not verify");
        assert!(err.to_string().contains("bytes on disk"), "{err}");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
