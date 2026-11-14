//! Writing a profile back — the only part of HangarRS that touches
//! Steam-managed files.
//!
//! Everything else in this crate reads. This module is the exception, and it
//! carries the whole safety story with it, because the thing being written is
//! Steam Cloud userdata: the only copy of a lot of binding work, synced to
//! other machines, and read by the sim at startup.
//!
//! # Five gates, and none of them is optional
//!
//! 1. **Steam must be closed.** A running client holds its own idea of what
//!    `remote/` contains and will overwrite the edit on its next sync — or
//!    upload the stale copy and revert it everywhere. Checked through
//!    `hangar_steam::steam_processes`.
//! 2. **A snapshot is taken first**, automatically, into HangarRS' own data
//!    directory. Not offered — taken. An edit with no way back is not an edit
//!    anyone should be able to make by accident.
//! 3. **The profile must survive its own writer.** Serialise, re-parse,
//!    serialise again, compare bytes. If the model cannot reproduce what it is
//!    about to write, nothing is written.
//! 4. **The write is atomic.** A temporary file in the same directory, then a
//!    rename. A half-written profile is worse than an unwritten one, and a
//!    rename within one filesystem cannot be interrupted into existence.
//! 5. **The result is read back and compared.** A write that reported success
//!    and produced different bytes is exactly the failure a backup exists for,
//!    and it should be found now rather than by the sim.
//!
//! And an [`Approval`] has to be presented, which no code path can produce
//! without saying what a person confirmed.

use crate::{parse, snapshot, write, BindingsError, Profile, Result};
use std::path::{Path, PathBuf};

/// Proof that a person confirmed this specific write.
///
/// A speed bump rather than a guarantee — Rust cannot check that a human was
/// asked. What it does buy: no write happens without a call that names its
/// reason, the reason is logged, and `Approval::given` greps to every place a
/// write can begin.
#[derive(Debug, Clone)]
pub struct Approval {
    reason: String,
}

impl Approval {
    /// Call **only** after a person has confirmed this write, with a
    /// description of what they were shown.
    pub fn given(reason: impl Into<String>) -> Self {
        Approval {
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Why a write did not happen.
///
/// Separate from [`BindingsError`] so a refusal cannot be mistaken for a
/// transient failure and retried in a loop. None of these get better by trying
/// again; every one needs something to change first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Steam is up. `who` names the processes.
    SteamRunning { who: String },
    /// The pre-write snapshot could not be taken, so there is no way back.
    NoSnapshot { reason: String },
    /// The profile does not survive its own writer.
    WouldNotRoundTrip { reason: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::SteamRunning { who } => write!(
                f,
                "Steam is running ({who}). Close it fully — Steam Cloud will \
                 otherwise overwrite this edit on its next sync."
            ),
            Refusal::NoSnapshot { reason } => write!(
                f,
                "no snapshot could be taken, so the edit has no way back: {reason}"
            ),
            Refusal::WouldNotRoundTrip { reason } => write!(
                f,
                "this profile does not survive being written and read again, \
                 so it will not be written: {reason}"
            ),
        }
    }
}

/// What a completed write did.
#[derive(Debug, Clone)]
pub struct Saved {
    pub path: PathBuf,
    pub bytes: usize,
    /// The snapshot taken immediately before, to restore from.
    pub snapshot_dir: PathBuf,
}

/// Whether the file being written is one Steam owns.
///
/// The Steam-closed gate exists because Steam Cloud syncs `userdata/`. It has
/// nothing to say about a scratch copy, an export, or a test tree — and
/// applying it there is not extra safety, it is a gate that cannot be opened
/// and a write path that never gets exercised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// Under Steam's `userdata/`. Steam must be closed.
    SteamUserdata,
    /// Anywhere else. The client's state is irrelevant.
    Elsewhere,
}

impl Destination {
    /// Decide from the path, erring towards `SteamUserdata`.
    ///
    /// A `userdata` component anywhere in the path is enough. Steam always
    /// uses that name, and the failure directions are not symmetric: calling a
    /// scratch directory Steam-managed costs an unnecessary refusal, while the
    /// reverse costs someone their bindings on the next sync.
    pub fn of(path: &Path) -> Self {
        if path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("userdata"))
        {
            Destination::SteamUserdata
        } else {
            Destination::Elsewhere
        }
    }
}

/// Check everything that must hold before a write, and do nothing.
///
/// Split out so a UI can grey out a save button and say why, without the user
/// having to press it to find out.
pub fn check(profile: &Profile, destination: Destination) -> std::result::Result<(), Refusal> {
    if destination == Destination::SteamUserdata {
        let steam = hangar_steam::steam_processes();
        if steam.is_running() {
            return Err(Refusal::SteamRunning {
                who: steam.summary(),
            });
        }
    }
    survives_its_own_writer(profile)
}

/// Check a write to a particular path, deciding the destination from it.
pub fn check_path(profile: &Profile, path: impl AsRef<Path>) -> std::result::Result<(), Refusal> {
    check(profile, Destination::of(path.as_ref()))
}

/// Serialise, re-parse, serialise again, compare.
///
/// Gate 3. A model that has drifted from its writer produces a file that is
/// *almost* right, and "almost" is not a thing anyone can debug from a sim
/// that silently ignores a binding.
fn survives_its_own_writer(profile: &Profile) -> std::result::Result<(), Refusal> {
    let once = write::to_xml(profile);
    let reparsed = parse(&once).map_err(|e| Refusal::WouldNotRoundTrip {
        reason: format!("what it would write does not parse: {e}"),
    })?;
    let twice = write::to_xml(&reparsed);

    match crate::first_divergence(&once, &twice) {
        None => Ok(()),
        Some(d) => Err(Refusal::WouldNotRoundTrip {
            reason: d.to_string(),
        }),
    }
}

/// Write a profile back to `path`, taking a snapshot of `remote/` first.
///
/// `snapshot_store` is HangarRS' own data directory — normally
/// [`hangar_core::paths::bindings_dir`]. The snapshot covers the whole
/// directory, not just the file being edited, because restoring one profile
/// into a set that has moved on is its own kind of mess.
pub fn save(
    profile: &Profile,
    path: impl AsRef<Path>,
    snapshot_store: impl AsRef<Path>,
    approval: &Approval,
) -> Result<std::result::Result<Saved, Refusal>> {
    let path = path.as_ref();

    if let Err(refusal) = check(profile, Destination::of(path)) {
        tracing::warn!(path = %path.display(), %refusal, "refusing to write");
        return Ok(Err(refusal));
    }

    let Some(remote_dir) = path.parent() else {
        return Err(BindingsError::malformed(
            path,
            "has no parent directory, so there is nothing to snapshot",
        ));
    };

    // Gate 2, before a single byte is written.
    let taken = match snapshot::take(
        remote_dir,
        snapshot_store.as_ref(),
        Some(format!("before: {}", approval.reason())),
    ) {
        Ok(taken) => taken,
        Err(e) => {
            return Ok(Err(Refusal::NoSnapshot {
                reason: e.to_string(),
            }))
        }
    };

    let contents = write::to_xml(profile);
    write_atomic(path, contents.as_bytes())?;

    // Gate 5.
    let back = std::fs::read(path).map_err(|source| BindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if back != contents.as_bytes() {
        return Err(BindingsError::RoundTrip {
            path: path.to_path_buf(),
            reason: format!(
                "wrote {} bytes and read back {} — restore from {}",
                contents.len(),
                back.len(),
                taken.dir.display()
            ),
        });
    }

    tracing::info!(
        path = %path.display(),
        bytes = contents.len(),
        snapshot = %taken.dir.display(),
        reason = approval.reason(),
        "wrote an input profile"
    );

    Ok(Ok(Saved {
        path: path.to_path_buf(),
        bytes: contents.len(),
        snapshot_dir: taken.dir,
    }))
}

/// Write via a temporary file in the same directory, then rename.
///
/// Same directory is not tidiness: a rename is only atomic within one
/// filesystem, and `/tmp` is very often a different one.
///
/// The original's permissions are carried over. MSFS writes these `0755`, and
/// quietly turning them into `0644` is the sort of difference that surfaces
/// weeks later as "Steam stopped syncing" with nothing to connect it to.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("profile");
    let temp = dir.join(format!(".{name}.hangar-tmp"));

    let mut file = std::fs::File::create(&temp).map_err(write_err(&temp))?;
    file.write_all(contents).map_err(write_err(&temp))?;
    // Durable before the rename, or a crash can leave the new name pointing at
    // a file whose contents never reached the disk.
    file.sync_all().map_err(write_err(&temp))?;
    drop(file);

    if let Ok(original) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&temp, original.permissions());
    }

    std::fs::rename(&temp, path).map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        BindingsError::Write {
            path: path.to_path_buf(),
            source,
        }
    })?;

    Ok(())
}

fn write_err(path: &Path) -> impl FnOnce(std::io::Error) -> BindingsError {
    let path = path.to_path_buf();
    move |source| BindingsError::Write { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Version Num=\"-1\"/>\n",
        "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">Pedals</FriendlyName>\n",
        "<Device DeviceName=\"V1\" GUID=\"{9E5}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
        "\t<Context ContextName=\"AIRCRAFT\">\n",
        "\t\t<Action ActionName=\"KEY_ELEV_TRIM_UP\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
        "\t</Context>\n",
        "</Device>\n",
        "\n",
    );

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hangar-save-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_profile_that_round_trips_passes_gate_three() {
        let profile = parse(FIXTURE).unwrap();
        assert_eq!(survives_its_own_writer(&profile), Ok(()));
    }

    #[test]
    fn an_atomic_write_leaves_no_temporary_behind() {
        let dir = scratch("atomic");
        let path = dir.join("inputprofile_0000000001");
        std::fs::write(&path, "old contents").unwrap();

        write_atomic(&path, FIXTURE.as_bytes()).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("hangar-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary left behind: {leftovers:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_original_permissions_survive_a_write() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perms");
        let path = dir.join("inputprofile_0000000001");
        std::fs::write(&path, "old").unwrap();
        // MSFS writes these 0755; a silent drop to 0644 is a bug that surfaces
        // weeks later with nothing to connect it to.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_atomic(&path, FIXTURE.as_bytes()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "permissions changed to {mode:o}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        let refusal = Refusal::SteamRunning {
            who: "steam (4697)".into(),
        };
        let text = refusal.to_string();
        assert!(text.contains("Close it fully"), "{text}");
        assert!(text.contains("4697"), "{text}");
    }

    #[test]
    fn an_approval_carries_what_was_confirmed() {
        let approval = Approval::given("rebind KEY_ELEV_TRIM_UP to Joystick Button 9");
        assert!(approval.reason().contains("KEY_ELEV_TRIM_UP"));
    }

    #[test]
    fn a_write_to_steams_own_files_is_refused_while_steam_is_running() {
        // Whichever way this machine is right now, the two states must not be
        // confusable: running means refused, closed means gate three decides.
        let profile = parse(FIXTURE).unwrap();
        match check(&profile, Destination::SteamUserdata) {
            Err(Refusal::SteamRunning { who }) => {
                assert!(hangar_steam::is_running());
                assert!(!who.is_empty(), "a refusal must name what it found");
            }
            other => {
                assert!(!hangar_steam::is_running(), "refused wrongly: {other:?}");
                assert_eq!(other, Ok(()));
            }
        }
    }

    #[test]
    fn a_write_somewhere_else_does_not_care_what_steam_is_doing() {
        // Passes whether or not Steam is up, which is the point.
        let profile = parse(FIXTURE).unwrap();
        assert_eq!(check(&profile, Destination::Elsewhere), Ok(()));
    }

    #[test]
    fn the_destination_is_read_off_the_path() {
        assert_eq!(
            Destination::of(Path::new(
                "/home/j/.local/share/Steam/userdata/123456789/2537590/remote/inputprofile_1"
            )),
            Destination::SteamUserdata
        );
        assert_eq!(
            Destination::of(Path::new("/tmp/hangar-edit-scratch/remote/inputprofile_1")),
            Destination::Elsewhere
        );
        // Erring towards refusing: a scratch tree that happens to be called
        // userdata is treated as Steam's.
        assert_eq!(
            Destination::of(Path::new("/tmp/userdata/inputprofile_1")),
            Destination::SteamUserdata
        );
    }
}
