//! Inspect and explicitly purge preserved package replacements.
//! Never follows links, and never includes Community or project backups.
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Replacement backup could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("Replacement backup contains a link or special file; nothing was deleted.")]
    Unsafe,
    #[error("Replacement backup changed; refresh and review it again.")]
    Changed,
    #[error("Deletion requires permission and a stopped simulator.")]
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Identity {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Copy {
    root: PathBuf,
    root_identity: (u64, u64),
    path: PathBuf,
    entries: BTreeMap<PathBuf, Identity>,
    pub bytes: u64,
    pub packages: usize,
}
impl Copy {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

fn plain_metadata(path: &Path) -> Result<fs::Metadata, Error> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() && !m.is_dir() {
        return Err(Error::Unsafe);
    }
    Ok(m)
}

pub(crate) fn tree(
    path: &Path,
    entries: &mut BTreeMap<PathBuf, Identity>,
    bytes: &mut u64,
) -> Result<(), Error> {
    if entries.len() > 1_000_000 || path.components().count() > 256 {
        return Err(Error::Unsafe);
    }
    let m = plain_metadata(path)?;
    if entries
        .values()
        .next()
        .is_some_and(|first| first.device != m.dev())
    {
        return Err(Error::Unsafe);
    }
    entries.insert(path.to_path_buf(), Identity::of(&m));
    if m.is_dir() {
        for entry in fs::read_dir(path)? {
            tree(&entry?.path(), entries, bytes)?;
        }
    } else {
        *bytes = bytes.saturating_add(m.len());
    }
    Ok(())
}

/// Read one store, derived only from a discovered Community folder.
/// No store is created when none exists.
pub fn scan(community: &Path) -> Result<Vec<Copy>, Error> {
    if !crate::drop::is_community(community) {
        return Err(Error::Unsafe);
    }
    let parent = community.parent().ok_or(Error::Unsafe)?.canonicalize()?;
    let root = parent.join(crate::drop::REPLACED_DIR);
    let m = match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        result => result?,
    };
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Unsafe);
    }
    let mut copies = Vec::new();
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if !plain_metadata(&path)?.is_dir() {
            return Err(Error::Unsafe);
        }
        let mut entries = BTreeMap::new();
        let mut bytes = 0;
        tree(&path, &mut entries, &mut bytes)?;
        let packages = fs::read_dir(&path)?.count();
        copies.push(Copy {
            root: root.clone(),
            root_identity: (m.dev(), m.ino()),
            path,
            entries,
            bytes,
            packages,
        });
    }
    copies.sort_by_key(|copy| std::cmp::Reverse(copy.path.clone()));
    Ok(copies)
}

/// Remove only the previously inspected, explicitly approved copy.
/// New backups outside the selected timestamp are never included.
pub fn purge(
    copy: &Copy,
    writes_allowed: bool,
    sim_running: bool,
    approval: &crate::drop::Approval,
) -> Result<(), Error> {
    if !writes_allowed || sim_running || approval.reason().trim().is_empty() {
        return Err(Error::Refused);
    }
    let root = plain_metadata(&copy.root)?;
    if !root.is_dir()
        || (root.dev(), root.ino()) != copy.root_identity
        || copy.path.parent() != Some(copy.root.as_path())
    {
        return Err(Error::Changed);
    }
    let mut entries = BTreeMap::new();
    let mut bytes = 0;
    tree(&copy.path, &mut entries, &mut bytes)?;
    if entries != copy.entries {
        return Err(Error::Changed);
    }
    fs::remove_dir_all(&copy.path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "hangar-replacements-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(base.join("Community")).unwrap();
        fs::create_dir_all(base.join(".hangar-replaced/stamp/airport")).unwrap();
        fs::write(base.join(".hangar-replaced/stamp/airport/file"), b"old").unwrap();
        base
    }
    #[test]
    fn a_confirmed_copy_is_removed_without_touching_other_content() {
        let base = scratch();
        fs::write(base.join("Community/live"), b"live").unwrap();
        let copy = scan(&base.join("Community")).unwrap().remove(0);
        assert_eq!(copy.bytes, 3);
        assert_eq!(copy.packages, 1);
        fs::create_dir_all(base.join(".hangar-replaced/newer")).unwrap();
        purge(
            &copy,
            true,
            false,
            &crate::drop::Approval::given("stamp, 3 bytes"),
        )
        .unwrap();
        assert!(!copy.path().exists());
        assert!(base.join("Community/live").exists());
        assert!(base.join(".hangar-replaced/newer").exists());
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn changed_copies_and_closed_gates_refuse() {
        let base = scratch();
        let copy = scan(&base.join("Community")).unwrap().remove(0);
        let approval = crate::drop::Approval::given("stamp");
        assert!(matches!(
            purge(&copy, false, false, &approval),
            Err(Error::Refused)
        ));
        assert!(matches!(
            purge(&copy, true, true, &approval),
            Err(Error::Refused)
        ));
        fs::write(copy.path().join("new"), b"new").unwrap();
        assert!(matches!(
            purge(&copy, true, false, &approval),
            Err(Error::Changed)
        ));
        assert!(copy.path().exists());
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn links_are_never_followed_or_deleted() {
        let base = scratch();
        std::os::unix::fs::symlink(
            base.join("Community"),
            base.join(".hangar-replaced/stamp/link"),
        )
        .unwrap();
        assert!(matches!(scan(&base.join("Community")), Err(Error::Unsafe)));
        assert!(matches!(scan(&base), Err(Error::Unsafe)));
        fs::remove_dir_all(base).unwrap();
    }
}
