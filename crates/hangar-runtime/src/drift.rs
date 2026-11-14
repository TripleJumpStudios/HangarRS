//! Is the configured program the copy the prefix's installer knows about?
//!
//! An app can update a copy the configuration does not start. BeyondATC, for
//! one, updated itself on every launch because the configuration started a
//! 1.10.2 copy in the companion prefix while the sim prefix's installer had put
//! 1.10.4 in `C:\BeyondATC\`. The sim prefix's registry names the right copy:
//!
//! ```text
//! [Software\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{CF88F4D6-…}_is1]
//! "DisplayName"="BeyondATC version 1.10.4.Experimental"
//! "InstallLocation"="C:\\BeyondATC\\"
//! ```
//!
//! Read-only: `system.reg` and `user.reg` are plain text, read with no Wine
//! process started and nothing written. An entry matches when its install
//! folder holds a file with the configured executable's name.

use std::path::{Path, PathBuf};

/// One installer record whose folder holds the executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    /// As the registry writes it, e.g. `C:\BeyondATC\`.
    pub windows: String,
    /// The same folder on this machine.
    pub folder: PathBuf,
}

/// The configured program sits outside every folder the prefix's installer names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub configured: PathBuf,
    pub installed: Installed,
}

impl Drift {
    /// The installed copy's executable: the file in the recorded folder with
    /// the configured program's name, matched without regard to case (the
    /// registry and the filesystem disagree on case often enough).
    pub fn installed_exe(&self) -> Option<PathBuf> {
        let name = self.configured.file_name()?.to_str()?;
        std::fs::read_dir(&self.installed.folder)
            .ok()?
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .map(|e| e.path())
    }

    /// One line for Activity and the Flight list.
    pub fn describe(&self, app: &str) -> String {
        format!(
            "{app}: the simulator prefix has {} installed in {}; the configured program is elsewhere.",
            self.installed.name, self.installed.windows
        )
    }
}

/// Check `exe` against the uninstall records in `prefix`.
///
/// `None` when the prefix records no install of this executable, or when the
/// configured path is inside one it records.
pub fn check(exe: &Path, prefix: &Path) -> Option<Drift> {
    let name = exe.file_name()?.to_str()?;
    let configured = canonical(exe);
    let mut found = Vec::new();
    for hive in ["system.reg", "user.reg"] {
        let Ok(bytes) = std::fs::read(prefix.join(hive)) else {
            continue;
        };
        found.extend(
            uninstall_entries(&String::from_utf8_lossy(&bytes))
                .into_iter()
                .filter_map(|(display, location)| {
                    let folder = to_unix(prefix, &location)?;
                    has_file(&folder, name).then(|| Installed {
                        name: display,
                        windows: location,
                        folder: canonical(&folder),
                    })
                }),
        );
    }
    if found.iter().any(|i| configured.starts_with(&i.folder)) {
        return None;
    }
    found.into_iter().next().map(|installed| Drift {
        configured,
        installed,
    })
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

fn has_file(folder: &Path, name: &str) -> bool {
    std::fs::read_dir(folder).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
    })
}

/// `(DisplayName, InstallLocation)` for every uninstall key that has a location.
fn uninstall_entries(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_uninstall = false;
    let mut display: Option<String> = None;
    let mut location: Option<String> = None;
    let mut flush = |display: &mut Option<String>, location: &mut Option<String>| {
        if let Some(loc) = location.take().filter(|l| !l.is_empty()) {
            out.push((
                display.take().unwrap_or_else(|| "an installed copy".into()),
                loc,
            ));
        }
        *display = None;
    };
    for line in text.lines() {
        if let Some(key) = line.strip_prefix('[') {
            flush(&mut display, &mut location);
            let key = key.to_ascii_lowercase();
            in_uninstall = key.contains("\\\\currentversion\\\\uninstall\\\\");
        } else if in_uninstall {
            if let Some(v) = line.strip_prefix("\"DisplayName\"=") {
                display = string_value(v);
            } else if let Some(v) = line.strip_prefix("\"InstallLocation\"=") {
                location = string_value(v);
            }
        }
    }
    flush(&mut display, &mut location);
    out
}

/// A `"…"` registry string, unescaped. Other types are not install paths.
fn string_value(raw: &str) -> Option<String> {
    let inner = raw.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `C:\X` → `<prefix>/drive_c/X`; other letters through `dosdevices`.
fn to_unix(prefix: &Path, windows: &str) -> Option<PathBuf> {
    let mut chars = windows.chars();
    let letter = chars.next()?.to_ascii_lowercase();
    if !letter.is_ascii_alphabetic() || chars.next()? != ':' {
        return None;
    }
    let rest: Vec<&str> = windows[2..].split('\\').filter(|p| !p.is_empty()).collect();
    let mut path = if letter == 'c' {
        prefix.join("drive_c")
    } else {
        prefix.join("dosdevices").join(format!("{letter}:"))
    };
    path.extend(rest);
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real key, as the sim prefix's `system.reg` has it.
    const BATC: &str = r#"WINE REGISTRY Version 2
[Software\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{CF88F4D6-A60E-473D-A1D3-ABB5FE336EFA}_is1] 1791668178
#time=1dc3a
"DisplayIcon"="C:\\BeyondATC\\BeyondATC.exe"
"DisplayName"="BeyondATC version 1.10.4.Experimental"
"InstallLocation"="C:\\BeyondATC\\"
"UninstallString"="\"C:\\BeyondATC\\unins000.exe\""

[Software\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{E414058D-38CD-42D0-9050-C8C13E7EE911}] 1787342437
"DisplayName"="Something without a location"

[Software\\Valve\\Steam] 1
"InstallLocation"="C:\\NotAnUninstallKey\\"
"#;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hangar-drift-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pfx/drive_c/BeyondATC")).unwrap();
        std::fs::write(dir.join("pfx/drive_c/BeyondATC/BeyondATC.exe"), b"").unwrap();
        std::fs::write(dir.join("pfx/system.reg"), BATC).unwrap();
        std::fs::create_dir_all(dir.join("companion/BeyondATC")).unwrap();
        std::fs::write(dir.join("companion/BeyondATC/BeyondATC.exe"), b"").unwrap();
        dir
    }

    #[test]
    fn the_l2_two_copy_setup_is_named() {
        let dir = scratch("two-copies");
        let drift = check(
            &dir.join("companion/BeyondATC/BeyondATC.exe"),
            &dir.join("pfx"),
        )
        .expect("drift");
        assert_eq!(drift.installed.windows, r"C:\BeyondATC\");
        assert!(
            drift.describe("BeyondATC").contains("1.10.4"),
            "{}",
            drift.describe("BeyondATC")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_installed_copy_and_an_unrecorded_app_are_quiet() {
        let dir = scratch("one-copy");
        assert_eq!(
            check(
                &dir.join("pfx/drive_c/BeyondATC/BeyondATC.exe"),
                &dir.join("pfx")
            ),
            None
        );
        // Case of the exe name does not matter; Windows does not care either.
        assert_eq!(
            check(
                &dir.join("pfx/drive_c/BeyondATC/beyondatc.exe"),
                &dir.join("pfx")
            ),
            None
        );
        assert_eq!(
            check(&dir.join("companion/Other.exe"), &dir.join("pfx")),
            None
        );
        assert_eq!(
            check(
                &dir.join("companion/BeyondATC/BeyondATC.exe"),
                &dir.join("missing")
            ),
            None
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_uninstall_keys_with_a_location_count() {
        let entries = uninstall_entries(BATC);
        assert_eq!(
            entries,
            vec![(
                "BeyondATC version 1.10.4.Experimental".into(),
                r"C:\BeyondATC\".into()
            )]
        );
    }

    #[test]
    fn windows_paths_map_into_the_prefix() {
        let p = Path::new("/pfx");
        assert_eq!(
            to_unix(p, r"C:\BeyondATC\"),
            Some(PathBuf::from("/pfx/drive_c/BeyondATC"))
        );
        assert_eq!(
            to_unix(p, r"Z:\home\j\x"),
            Some(PathBuf::from("/pfx/dosdevices/z:/home/j/x"))
        );
        assert_eq!(to_unix(p, "relative"), None);
    }
}
