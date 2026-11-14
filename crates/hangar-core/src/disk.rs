//! How much room is left where something is about to be written.
//!
//! One `statvfs` call, wrapped so callers do not each have to remember that
//! the answer they want is `f_bavail` rather than `f_bfree` — the difference
//! is the reserved blocks only root may use, which is typically 5% of the
//! filesystem and is not room a package install can have.

use std::path::Path;

/// Free bytes available to this user at `path`.
///
/// `None` when the path cannot be reached — an unmounted library, a prefix on
/// a disk that is not there. That is not the same answer as zero, and the
/// difference matters: zero means refuse, unknown means say so.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let stat = nix::sys::statvfs::statvfs(path).ok()?;
    Some(stat.blocks_available() as u64 * stat.fragment_size() as u64)
}

/// Total bytes of the filesystem holding `path`.
pub fn total_bytes(path: &Path) -> Option<u64> {
    let stat = nix::sys::statvfs::statvfs(path).ok()?;
    Some(stat.blocks() as u64 * stat.fragment_size() as u64)
}

/// What a directory actually occupies, walked.
///
/// **Measured, not declared.** A package's `manifest.json` may carry a
/// `total_package_size`, but only some do, and the ones that do report the
/// *download* rather than what landed — so a library reporting a mix of the two
/// has one column meaning two different things, with a dash for the rest.
/// Walking is affordable: 18,202 files across a 19 GiB Community folder answer
/// in under a third of a second.
///
/// `None` when the root cannot be read at all. An unreadable *subdirectory* is
/// skipped rather than failing the whole answer — the alternative is one bad
/// permission turning a 4 GB aeroplane into a dash.
///
/// Symlinks below the root are not followed, so a package that links to a file
/// counted elsewhere is not counted twice, and a cycle cannot hang the scan.
/// The root itself is followed, because a linked package *is* a symlink and its
/// size is the target's.
pub fn dir_bytes(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_file() {
        return Some(meta.len());
    }
    if !meta.is_dir() {
        return None;
    }

    let mut total = 0_u64;
    // Explicit stack rather than recursion: a deep scenery tree is not worth a
    // stack overflow, and this runs on the scan thread.
    let mut queue = vec![path.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `DirEntry::metadata` does not traverse a symlink, which is what
            // keeps this from following one out of the package.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                queue.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Some(total)
}

/// Bytes as a person would say them: `4.0 GB`, `812 MB`.
pub fn human(bytes: u64) -> String {
    const GB: f64 = 1_073_741_824.0;
    const MB: f64 = 1_048_576.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else {
        format!("{bytes:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of measuring rather than reading the manifest: an answer for
    /// every package, and one that is about this disk.
    #[test]
    fn a_directory_is_measured_by_what_is_in_it() {
        let dir = std::env::temp_dir().join(format!("hangar-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested/deeper")).unwrap();
        std::fs::write(dir.join("a"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("nested/b"), vec![0u8; 250]).unwrap();
        std::fs::write(dir.join("nested/deeper/c"), vec![0u8; 7]).unwrap();

        assert_eq!(dir_bytes(&dir), Some(357), "every level counts");
        assert_eq!(
            dir_bytes(&dir.join("a")),
            Some(100),
            "a file is its own size"
        );
        assert_eq!(dir_bytes(Path::new("/nonexistent/place")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A link out of the package would count somebody else's bytes, and a link
    /// back into it would count its own twice.
    #[test]
    fn a_symlink_inside_a_package_is_not_followed() {
        let dir = std::env::temp_dir().join(format!("hangar-disk-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("outside"), vec![0u8; 9_000]).unwrap();
        std::fs::write(dir.join("pkg/real"), vec![0u8; 11]).unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("pkg/link")).unwrap();

        assert_eq!(dir_bytes(&dir.join("pkg")), Some(11));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_filesystem_answers() {
        let free = free_bytes(Path::new("/")).expect("/ is mounted");
        let total = total_bytes(Path::new("/")).expect("/ is mounted");
        assert!(free <= total);
        assert!(total > 0);
    }

    /// Unknown is not zero. A refusal to install because a disk reported no
    /// space and a refusal because the disk could not be reached are different
    /// problems with different fixes.
    #[test]
    fn a_path_that_is_not_there_is_unknown_rather_than_empty() {
        assert_eq!(free_bytes(Path::new("/nonexistent/place")), None);
    }

    #[test]
    fn sizes_read_the_way_people_say_them() {
        assert_eq!(human(4 * 1_073_741_824), "4.0 GB");
        assert_eq!(human(812 * 1_048_576), "812 MB");
        assert_eq!(human(512), "512 B");
    }
}
