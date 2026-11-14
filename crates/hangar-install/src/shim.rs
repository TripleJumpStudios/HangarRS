//! Wine's PowerShell stub, and why it has to be out of the way for stage 4.
//!
//! # The bug this exists for
//!
//! Electron-builder installers refuse to install over a running copy, and they
//! decide whether one is running by shelling out to PowerShell:
//!
//! ```text
//! powershell -C "if ((Get-CimInstance -ClassName Win32_Process | ? {$_.Path -and
//!   $_.Path.StartsWith('…\Programs\orbx-central','CurrentCultureIgnoreCase')}).Count -gt 0)
//!   { exit 0 } else { exit 1 }"
//! ```
//!
//! Exit 0 means *running*. Wine has no PowerShell — it ships a builtin stub
//! that accepts any command and **exits 0 for all of them**. So the installer
//! asks, the stub says 0, and the installer concludes the application is
//! running. It then offers to close something that was never open, fails, and
//! shows *"cannot be closed. Please close it manually and click Retry"*.
//!
//! Nothing about the prefix changes that answer: Orbx Central fails the same
//! way in a prefix with no Orbx install, no Orbx registry key, and every
//! companion app stopped.
//!
//! The installers all carry a working fallback — `tasklist … | find …`, which
//! Wine implements correctly and which returns the right answer here — but
//! they only reach it once they decide PowerShell is unavailable, and the stub
//! reports itself as perfectly available.
//!
//! # Why parking it is the fix, and not a hack
//!
//! A stub that exits 0 for every query is never useful to anybody. Every
//! caller that consults it gets an answer that is wrong whenever the true
//! answer is "no". Removing it does not lose a capability; it converts a
//! silent wrong answer into a visible absent one, which is the same choice
//! [`crate::gate`] makes with `Unchecked`.
//!
//! Parked, not deleted, and the file is Wine's own symlink inside the prefix —
//! **nothing here touches the Proton installation**, only the prefix's link
//! into it. `wineboot -u` recreates the link, so the worst case of a missed
//! restore is a prefix update putting it back.
//!
//! # It must survive not being cleaned up
//!
//! *Anything that is only safe because a shutdown handler fires is not safe.*
//! So there are two restores, and neither depends on the other:
//!
//! * [`Parked`] restores on drop, including when stage 4 panics or the future
//!   is cancelled.
//! * [`restore_stray`] restores anything a killed process left parked, and is
//!   called before a run rather than after one — the prefix is repaired by the
//!   next install attempt whether or not HangarRS ever ran again in between.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The marker in the DOS stub of every Wine builtin PE.
///
/// Present in `powershell.exe` from Proton Experimental, Proton Hotfix and
/// Proton-GE, and absent from a Microsoft-shipped `powershell.exe`, which is
/// an order of magnitude larger and carries a real version resource. Checked
/// by reading the head of the file, so this stays a read: the gate is not
/// allowed to start a Wine process to answer a question about a prefix.
pub const BUILTIN_MARKER: &[u8] = b"Wine builtin DLL";

/// How far into the file the marker is looked for. It sits in the DOS stub,
/// inside the first 128 bytes on every build seen; a page is generous.
const HEAD: usize = 4096;

/// The suffix a parked file takes. Deliberately conspicuous: a person who
/// finds one in a prefix should be able to guess both what put it there and
/// how to undo it.
pub const PARKED_SUFFIX: &str = ".hangar-off";

/// Where a prefix keeps PowerShell, per architecture.
///
/// Both are parked. A 32-bit installer's `C:\windows\system32\…` is redirected
/// to `syswow64` by WOW64, so parking only the 64-bit copy leaves the stub
/// reachable by exactly the installers most likely to call it — NSIS stubs are
/// usually 32-bit.
pub const LOCATIONS: [&str; 2] = [
    r"drive_c/windows/system32/WindowsPowerShell/v1.0/powershell.exe",
    r"drive_c/windows/syswow64/WindowsPowerShell/v1.0/powershell.exe",
];

/// What the `powershell.exe` at one location turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Nothing there. Already harmless — an installer that probes for it gets
    /// a spawn failure, which is the answer the fallback path wants.
    Missing,
    /// Wine's builtin stub: exits 0 whatever it is asked.
    Builtin,
    /// A PE without the builtin marker. Trusted to answer for itself.
    Real,
    /// It is there and could not be read, with why. Never treated as either
    /// of the two above.
    Unreadable(String),
}

/// One location and what is at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    pub path: PathBuf,
    pub kind: Kind,
}

/// Look at every location, without running anything.
pub fn survey(prefix: &Path) -> Vec<Survey> {
    LOCATIONS
        .iter()
        .map(|rel| {
            let path = prefix.join(rel);
            let kind = classify(&path);
            Survey { path, kind }
        })
        .collect()
}

fn classify(path: &Path) -> Kind {
    // `symlink_metadata` would report the link; the question is about what it
    // points at, and in a Proton prefix it always points into the Proton
    // installation's `lib/wine/<arch>-windows/`.
    if !path.exists() {
        return Kind::Missing;
    }
    match read_head(path) {
        Err(e) => Kind::Unreadable(e.to_string()),
        Ok(head) => {
            if head
                .windows(BUILTIN_MARKER.len())
                .any(|w| w == BUILTIN_MARKER)
            {
                Kind::Builtin
            } else {
                Kind::Real
            }
        }
    }
}

fn read_head(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut head = vec![0u8; HEAD];
    let mut filled = 0;
    // A short read is not the end of the file, so this loops rather than
    // trusting one `read` to fill the buffer.
    loop {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if filled == HEAD {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    head.truncate(filled);
    Ok(head)
}

/// Whether this prefix holds a PowerShell that will answer every question
/// wrongly. The one thing callers usually want to know.
pub fn lies(prefix: &Path) -> bool {
    survey(prefix).iter().any(|s| s.kind == Kind::Builtin)
}

/// Files moved aside, and the promise to move them back.
///
/// Restores on drop. It also restores on [`Self::restore`], which is what a
/// caller uses when it wants to see the error rather than have it logged —
/// dropping cannot return one.
#[derive(Debug, Default)]
pub struct Parked {
    moved: Vec<(PathBuf, PathBuf)>,
}

impl Parked {
    /// The original paths that are currently moved aside.
    pub fn paths(&self) -> Vec<&Path> {
        self.moved.iter().map(|(from, _)| from.as_path()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.moved.is_empty()
    }

    /// Move everything back now, reporting the first failure. Idempotent: a
    /// later drop finds nothing left to do.
    pub fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        for (from, to) in std::mem::take(&mut self.moved) {
            if let Err(e) = fs::rename(&to, &from) {
                tracing::warn!(
                    "could not restore {} from {}: {e}",
                    from.display(),
                    to.display()
                );
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Move every Wine stub in this prefix aside for the duration of a run.
///
/// Only [`Kind::Builtin`] is touched — a real PowerShell is left exactly where
/// it is, and so is a location that is already empty. If one rename succeeds
/// and the next fails, the successful one is put back before returning the
/// error: a half-parked prefix is worse than an un-parked one, because it is
/// the state nobody tests against.
pub fn park(prefix: &Path) -> io::Result<Parked> {
    let mut parked = Parked::default();
    for entry in survey(prefix) {
        if entry.kind != Kind::Builtin {
            continue;
        }
        let aside = parked_path(&entry.path);
        // Something is already parked here — a previous run died. Leave it be
        // and take the live file out of the way anyway; `restore_stray` is
        // what resolves the older one, and it runs before this does.
        if aside.exists() {
            tracing::warn!(
                "{} already exists — leaving it and not parking again",
                aside.display()
            );
            continue;
        }
        if let Err(e) = fs::rename(&entry.path, &aside) {
            let _ = parked.restore();
            return Err(e);
        }
        parked.moved.push((entry.path, aside));
    }
    Ok(parked)
}

/// The name a parked file takes. Appended rather than substituted, so the
/// original name is still legible in a directory listing.
pub fn parked_path(original: &Path) -> PathBuf {
    let mut name = original.as_os_str().to_owned();
    name.push(PARKED_SUFFIX);
    PathBuf::from(name)
}

/// Put back anything a killed run left parked, and say what was restored.
///
/// Called **before** a run, not after one. A process that is killed between
/// the rename and the restore leaves the prefix missing its PowerShell, and
/// nothing in a `Drop` can help with that because the drop never happened.
/// Repairing on the way in means the prefix is fixed by the next attempt
/// whether or not HangarRS was the thing that died.
///
/// A parked file whose original is *also* present is a state nothing here
/// creates; the parked copy is left alone and named in the return value rather
/// than overwriting a file somebody else put there.
pub fn restore_stray(prefix: &Path) -> Vec<PathBuf> {
    let mut restored = Vec::new();
    for rel in LOCATIONS {
        let original = prefix.join(rel);
        let aside = parked_path(&original);
        if !aside.exists() {
            continue;
        }
        if original.exists() {
            tracing::warn!(
                "{} exists alongside {} — leaving both alone",
                original.display(),
                aside.display()
            );
            continue;
        }
        match fs::rename(&aside, &original) {
            Ok(()) => {
                tracing::info!("restored {} after an earlier run", original.display());
                restored.push(original);
            }
            Err(e) => tracing::warn!("could not restore {}: {e}", original.display()),
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's scratch-directory pattern, as `drop`'s tests use it: a real
    /// tree under the system temp directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-shim-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A prefix with a `powershell.exe` at both locations, each with the
    /// contents given.
    fn prefix_with(dir: &Path, head: &[u8]) {
        for rel in LOCATIONS {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().expect("has a parent")).expect("made dirs");
            fs::write(&path, head).expect("wrote");
        }
    }

    fn wine_stub() -> Vec<u8> {
        let mut bytes = b"MZ\x90\x00".to_vec();
        bytes.extend_from_slice(BUILTIN_MARKER);
        bytes.extend_from_slice(b"\r\nThis program cannot be run in DOS mode.\r\n");
        bytes
    }

    #[test]
    fn a_wine_builtin_is_recognised_by_its_dos_stub() {
        let dir = Scratch::new("builtin");
        prefix_with(dir.path(), &wine_stub());
        assert!(lies(dir.path()));
        assert!(survey(dir.path()).iter().all(|s| s.kind == Kind::Builtin));
    }

    /// The marker is the whole test. A real PowerShell is a PE like any other
    /// and must be left alone.
    #[test]
    fn a_real_powershell_is_left_alone() {
        let dir = Scratch::new("real");
        prefix_with(
            dir.path(),
            b"MZ\x90\x00\r\nThis program cannot be run in DOS mode.\r\n",
        );
        assert!(!lies(dir.path()));
        let parked = park(dir.path()).expect("parked");
        assert!(parked.is_empty());
        for rel in LOCATIONS {
            assert!(dir.path().join(rel).exists(), "{rel} should be untouched");
        }
    }

    #[test]
    fn an_absent_powershell_is_missing_not_real() {
        let dir = Scratch::new("absent");
        assert_eq!(
            survey(dir.path())
                .into_iter()
                .map(|s| s.kind)
                .collect::<Vec<_>>(),
            vec![Kind::Missing, Kind::Missing]
        );
        assert!(!lies(dir.path()));
    }

    /// Both arches, because a 32-bit NSIS stub reaches the syswow64 copy and
    /// parking only the 64-bit one would leave the bug in place for exactly
    /// the installers that hit it.
    #[test]
    fn both_arches_are_parked_and_restored() {
        let dir = Scratch::new("both-arches");
        prefix_with(dir.path(), &wine_stub());

        let mut parked = park(dir.path()).expect("parked");
        assert_eq!(parked.paths().len(), 2);
        for rel in LOCATIONS {
            assert!(!dir.path().join(rel).exists(), "{rel} should be parked");
            assert!(parked_path(&dir.path().join(rel)).exists());
        }

        parked.restore().expect("restored");
        for rel in LOCATIONS {
            assert!(dir.path().join(rel).exists(), "{rel} should be back");
            assert!(!parked_path(&dir.path().join(rel)).exists());
        }
    }

    /// The guarantee stage 4 relies on: a panic between park and restore still
    /// leaves the prefix as it was found.
    #[test]
    fn dropping_the_guard_restores_the_prefix() {
        let dir = Scratch::new("drop-guard");
        prefix_with(dir.path(), &wine_stub());
        {
            let _parked = park(dir.path()).expect("parked");
            assert!(!dir.path().join(LOCATIONS[0]).exists());
        }
        for rel in LOCATIONS {
            assert!(dir.path().join(rel).exists(), "{rel} should be back");
        }
    }

    /// The other half of the same guarantee, for when the drop never runs
    /// because the process was killed.
    #[test]
    fn a_killed_run_is_repaired_on_the_way_in() {
        let dir = Scratch::new("killed");
        prefix_with(dir.path(), &wine_stub());

        // Park, then forget the guard without running it — a SIGKILL.
        let parked = park(dir.path()).expect("parked");
        std::mem::forget(parked);
        for rel in LOCATIONS {
            assert!(!dir.path().join(rel).exists());
        }

        let restored = restore_stray(dir.path());
        assert_eq!(restored.len(), 2);
        for rel in LOCATIONS {
            assert!(dir.path().join(rel).exists(), "{rel} should be repaired");
        }
    }

    #[test]
    fn restoring_a_prefix_with_nothing_parked_does_nothing() {
        let dir = Scratch::new("nothing-parked");
        prefix_with(dir.path(), &wine_stub());
        assert!(restore_stray(dir.path()).is_empty());
        assert!(lies(dir.path()));
    }

    /// Never overwrite a file somebody else put back by hand.
    #[test]
    fn a_parked_copy_beside_a_live_one_is_left_alone() {
        let dir = Scratch::new("beside-live");
        prefix_with(dir.path(), &wine_stub());
        let original = dir.path().join(LOCATIONS[0]);
        fs::write(parked_path(&original), b"older").expect("wrote");

        assert!(restore_stray(dir.path()).is_empty());
        assert_eq!(
            fs::read(parked_path(&original)).expect("read"),
            b"older",
            "the parked copy must not be moved over a live file"
        );
        assert!(original.exists());
    }
}
