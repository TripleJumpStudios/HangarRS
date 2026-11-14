//! Reading a Wine prefix's registry without running Wine.
//!
//! `system.reg` and `user.reg` are plain text at the top of the prefix, so a
//! pre-flight check can look at them with the sim closed, with no `wineserver`
//! started and nothing written. Starting a Wine process to answer a question
//! about a prefix is both slower and a change to the thing being measured.
//!
//! # The one value this exists for
//!
//! ```text
//! [Software\\dotnet\\Setup\\InstalledVersions\\x64] 1785710557
//! #time=1dd22d0377dc946
//! "InstallLocation"="C:\\Program Files\\dotnet"
//! ```
//!
//! Wine's MSI run creates `InstalledVersions\x64\sharedhost` and **omits
//! `InstallLocation`**. Without it a 64-bit .NET app spawned by a 32-bit
//! installer inherits `ProgramFiles=C:\Program Files (x86)` and looks for the
//! runtime in a directory that does not exist — while `dotnet --list-runtimes`
//! keeps working perfectly from a shell, which is what makes it deceptive.
//!
//! # Shape notes
//!
//! Keys are written **without their hive**: `HKEY_LOCAL_MACHINE\Software\…`
//! appears in `system.reg` as `[Software\\…]`. Backslashes are doubled in both
//! key names and values, so everything is unescaped on the way out.

use std::path::{Path, PathBuf};

/// Which of the prefix's two registry files to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    /// `HKEY_LOCAL_MACHINE` — `system.reg`.
    LocalMachine,
    /// `HKEY_CURRENT_USER` — `user.reg`.
    CurrentUser,
}

impl Hive {
    pub fn file(self) -> &'static str {
        match self {
            Hive::LocalMachine => "system.reg",
            Hive::CurrentUser => "user.reg",
        }
    }
}

/// The path of a hive inside a prefix.
pub fn hive_path(prefix: &Path, hive: Hive) -> PathBuf {
    prefix.join(hive.file())
}

/// Read one value, or `None` if the prefix, the key or the value is absent.
///
/// Absent is deliberately not distinguished from unreadable here: to a
/// pre-flight check, a registry it cannot read and a key that is not set both
/// mean the requirement is not demonstrably met.
pub fn value(prefix: &Path, hive: Hive, key: &str, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(hive_path(prefix, hive)).ok()?;
    find_value(&text, key, name)
}

/// The parse, separated from the file so it can be tested on a fragment.
///
/// `key` is given in ordinary Windows form — `Software\dotnet\Setup` — and
/// matched case-insensitively, because Wine writes what the installer wrote.
pub fn find_value(text: &str, key: &str, name: &str) -> Option<String> {
    let wanted_key = escape(key).to_ascii_lowercase();
    let wanted_name = format!("\"{name}\"=").to_ascii_lowercase();

    let mut inside = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('[') {
            // `[Key\\Path] 1785710557` — the timestamp follows the bracket.
            let Some(end) = rest.find(']') else { continue };
            inside = rest[..end].to_ascii_lowercase() == wanted_key;
            continue;
        }
        if !inside {
            continue;
        }
        let lowered = line.to_ascii_lowercase();
        if let Some(rest) = lowered.strip_prefix(&wanted_name) {
            // Take the value from the original line, not the lowered one.
            let raw = &line[line.len() - rest.len()..];
            return Some(unescape(raw.trim().trim_matches('"')));
        }
    }
    None
}

/// Is a key present at all, whatever it holds?
pub fn has_key(prefix: &Path, hive: Hive, key: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(hive_path(prefix, hive)) else {
        return false;
    };
    let wanted = format!("[{}]", escape(key)).to_ascii_lowercase();
    text.lines()
        .any(|l| l.to_ascii_lowercase().starts_with(&wanted))
}

fn escape(key: &str) -> String {
    key.trim_matches('\\').replace('\\', "\\\\")
}

fn unescape(value: &str) -> String {
    value.replace("\\\\", "\\").replace("\\\"", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from the real MSFS prefix — including the
    /// `sharedhost` sibling that Wine's MSI writes and the `InstallLocation`
    /// it does not.
    const REAL: &str = r#"
[Software\\dotnet\\Setup\\InstalledVersions\\x64] 1785710557
#time=1dd22d0377dc946
"InstallLocation"="C:\\Program Files\\dotnet"

[Software\\dotnet\\Setup\\InstalledVersions\\x64\\sharedhost] 1785709223
#time=1dd22cd1c9069de
"Path"="C:\\Program Files\\dotnet\\"
"Version"="8.0.29"
"#;

    #[test]
    fn reads_the_key_that_makes_dotnet_findable() {
        assert_eq!(
            find_value(
                REAL,
                r"Software\dotnet\Setup\InstalledVersions\x64",
                "InstallLocation"
            )
            .as_deref(),
            Some(r"C:\Program Files\dotnet")
        );
    }

    /// The sub-key has a `Path` and the parent does not. Reading values
    /// without tracking which section they are in answers the wrong question.
    #[test]
    fn a_value_belongs_to_its_own_key() {
        assert_eq!(
            find_value(REAL, r"Software\dotnet\Setup\InstalledVersions\x64", "Path"),
            None
        );
        assert_eq!(
            find_value(
                REAL,
                r"Software\dotnet\Setup\InstalledVersions\x64\sharedhost",
                "Version"
            )
            .as_deref(),
            Some("8.0.29")
        );
    }

    #[test]
    fn a_missing_key_or_value_is_none() {
        assert_eq!(
            find_value(REAL, r"Software\nothing", "InstallLocation"),
            None
        );
        assert_eq!(
            find_value(REAL, r"Software\dotnet\Setup\InstalledVersions\x64", "Nope"),
            None
        );
        assert_eq!(find_value("", r"Software\x", "y"), None);
    }

    #[test]
    fn hives_are_the_two_files_at_the_top_of_the_prefix() {
        assert_eq!(Hive::LocalMachine.file(), "system.reg");
        assert_eq!(Hive::CurrentUser.file(), "user.reg");
        assert_eq!(
            hive_path(Path::new("/pfx"), Hive::CurrentUser),
            PathBuf::from("/pfx/user.reg")
        );
    }
}
