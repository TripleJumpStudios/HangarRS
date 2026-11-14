//! Resolving which Proton build a game is configured to run under.
//!
//! Two places to look, and they hold different things:
//!
//! * `compatibilitytools.d` — **third-party only** (Proton-GE, CachyOS). Each
//!   tool ships a `compatibilitytool.vdf` naming itself.
//! * `steamapps/common` — **Valve's own** Protons. These ship a
//!   `toolmanifest.vdf` with no name field and *no* `compatibilitytool.vdf` at
//!   all, so the only thing to match against is the directory name.
//!
//! `config.vdf` refers to them by an internal name (`proton_experimental`) that
//! appears nowhere on disk. Bridging that to `Proton - Experimental/` is what
//! [`normalize_tool_name`] is for.

use crate::vdf::{self, VdfNode};
use crate::{Result, SteamError};
use std::path::{Path, PathBuf};

/// A resolved Proton/Wine compatibility tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatTool {
    /// Directory or display name, whichever identified it.
    pub display_name: String,
    /// The internal name `config.vdf` referred to it by.
    pub configured_name: String,
    /// The `proton` launcher script.
    pub proton_script: PathBuf,
    /// The Wine binary.
    ///
    /// Modern Proton ships **no `wine64`** — only `files/bin/wine`. Proton-GE
    /// and older builds have both.
    pub wine_binary: PathBuf,
    /// Whether this came from `compatibilitytools.d` rather than Valve.
    pub is_third_party: bool,
}

/// Collapse a tool name to a comparable form.
///
/// `"Proton - Experimental"` and `"proton_experimental"` both become
/// `"protonexperimental"`.
pub fn normalize_tool_name(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The Wine binary inside a Proton tool directory.
///
/// Prefers `wine64` where it exists (Proton-GE, older builds) and falls back to
/// `wine` (current Proton, which dropped the suffixed binary).
fn wine_binary_in(tool_dir: &Path) -> PathBuf {
    let wine64 = tool_dir.join("files/bin/wine64");
    if wine64.is_file() {
        wine64
    } else {
        tool_dir.join("files/bin/wine")
    }
}

/// Read the compat tool name configured for `app_id` in `config/config.vdf`.
///
/// Returns `Ok(None)` when Steam has no mapping — meaning the game runs
/// natively or with whatever the global default is.
pub fn configured_tool_name(steam_root: &Path, app_id: &str) -> Result<Option<String>> {
    let path = steam_root.join("config/config.vdf");
    let content = std::fs::read_to_string(&path).map_err(|source| SteamError::Read {
        path: path.clone(),
        source,
    })?;
    let root = vdf::parse(&content).map_err(|reason| SteamError::Vdf { path, reason })?;

    let name = root
        .get_path("installconfigstore.software.valve.steam.compattoolmapping")
        .and_then(VdfNode::as_map)
        .and_then(|m| m.get(app_id))
        .and_then(|n| n.get_path("name"))
        .and_then(VdfNode::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok(name)
}

/// Find a third-party tool in a `compatibilitytools.d` directory.
fn find_third_party(dirs: &[PathBuf], wanted: &str) -> Option<CompatTool> {
    let wanted_norm = normalize_tool_name(wanted);

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        // Sorted so the result does not depend on filesystem iteration order.
        let mut tool_dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        tool_dirs.sort();

        for tool_dir in tool_dirs {
            let manifest = tool_dir.join("compatibilitytool.vdf");
            if !manifest.is_file() {
                continue;
            }
            let dir_name = file_name(&tool_dir);
            let display = display_name_from_manifest(&manifest);

            let matches = normalize_tool_name(&dir_name) == wanted_norm
                || display
                    .as_deref()
                    .is_some_and(|d| normalize_tool_name(d) == wanted_norm);

            if matches {
                return Some(CompatTool {
                    display_name: display.unwrap_or_else(|| dir_name.clone()),
                    configured_name: wanted.to_owned(),
                    proton_script: tool_dir.join("proton"),
                    wine_binary: wine_binary_in(&tool_dir),
                    is_third_party: true,
                });
            }
        }
    }
    None
}

/// Pull any `display_name` out of a `compatibilitytool.vdf`.
fn display_name_from_manifest(manifest: &Path) -> Option<String> {
    let content = std::fs::read_to_string(manifest).ok()?;
    let root = vdf::parse(&content).ok()?;
    let tools = root
        .get_path("compatibilitytools.compat_tools")
        .and_then(VdfNode::as_map)?;
    tools
        .values()
        .find_map(|n| n.get_path("display_name").and_then(VdfNode::as_str))
        .map(str::to_owned)
}

/// Find one of Valve's own Protons by directory name across every library.
///
/// Exact normalized match first, then a prefix match — so a mapping of
/// `proton_9` can still reach `Proton 9.0 (Beta)` without an exact candidate
/// being stolen by a longer directory name.
fn find_official(library_roots: &[PathBuf], wanted: &str) -> Option<CompatTool> {
    let wanted_norm = normalize_tool_name(wanted);

    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    for root in library_roots {
        let Ok(entries) = std::fs::read_dir(root.join("steamapps/common")) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            // The launcher script is what makes a directory a Proton tool;
            // "Proton BattlEye Runtime" and friends sit alongside without one.
            if dir.join("proton").is_file() {
                candidates.push((normalize_tool_name(&file_name(&dir)), dir));
            }
        }
    }

    // Sorted by normalized name, then by path, so the choice is deterministic.
    // `read_dir` order is not, and a prefix match over an unsorted list picks a
    // different Proton depending on how the directory happens to be laid out.
    candidates.sort();

    let (_, dir) = candidates
        .iter()
        .find(|(norm, _)| *norm == wanted_norm)
        .or_else(|| {
            // Shortest name wins a prefix tie: it is the closest match, and
            // `proton_9` should reach "Proton 9.0" rather than "Proton 9.0 RC2".
            candidates
                .iter()
                .filter(|(norm, _)| norm.starts_with(&wanted_norm))
                .min_by_key(|(norm, _)| norm.len())
        })?;

    Some(CompatTool {
        display_name: file_name(dir),
        configured_name: wanted.to_owned(),
        proton_script: dir.join("proton"),
        wine_binary: wine_binary_in(dir),
        is_third_party: false,
    })
}

/// Resolve the compat tool configured for `app_id` to a real install.
///
/// `Ok(None)` means no tool is configured; an unresolvable *configured* name is
/// an error, because that is a broken setup rather than an absent one.
pub fn resolve(
    steam_root: &Path,
    library_roots: &[PathBuf],
    app_id: &str,
) -> Result<Option<CompatTool>> {
    let Some(name) = configured_tool_name(steam_root, app_id)? else {
        return Ok(None);
    };

    let third_party_dirs = [
        steam_root.join("compatibilitytools.d"),
        // A Flatpak Steam can also see a system-wide location.
        PathBuf::from("/usr/share/steam/compatibilitytools.d"),
    ];

    if let Some(tool) = find_third_party(&third_party_dirs, &name) {
        tracing::debug!(
            "compat tool {name:?} resolved to third-party {}",
            tool.display_name
        );
        return Ok(Some(tool));
    }
    if let Some(tool) = find_official(library_roots, &name) {
        tracing::debug!(
            "compat tool {name:?} resolved to official {}",
            tool.display_name
        );
        return Ok(Some(tool));
    }

    Err(SteamError::CompatToolUnresolved { name })
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge between what `config.vdf` says and what is on disk.
    #[test]
    fn normalization_bridges_config_names_to_directory_names() {
        assert_eq!(
            normalize_tool_name("proton_experimental"),
            normalize_tool_name("Proton - Experimental")
        );
        assert_eq!(normalize_tool_name("Proton 9.0 (Beta)"), "proton90beta");
        assert_eq!(normalize_tool_name("Proton-GE Latest"), "protongelatest");
        assert_eq!(normalize_tool_name(""), "");
    }

    #[test]
    fn normalization_keeps_distinct_tools_distinct() {
        assert_ne!(
            normalize_tool_name("Proton - Experimental"),
            normalize_tool_name("Proton Hotfix")
        );
        assert_ne!(
            normalize_tool_name("proton_9"),
            normalize_tool_name("proton_experimental")
        );
    }

    fn tool_dir(root: &Path, name: &str, with_wine64: bool) -> PathBuf {
        let dir = root.join("steamapps/common").join(name);
        std::fs::create_dir_all(dir.join("files/bin")).unwrap();
        std::fs::write(dir.join("proton"), "#!/usr/bin/env python3\n").unwrap();
        std::fs::write(dir.join("files/bin/wine"), "").unwrap();
        if with_wine64 {
            std::fs::write(dir.join("files/bin/wine64"), "").unwrap();
        }
        dir
    }

    /// The documented trap, end to end: `proton_experimental` must resolve to
    /// `Proton - Experimental/`, matched by directory name alone.
    #[test]
    fn official_proton_is_found_by_directory_name() {
        let tmp = tempdir();
        let lib = tmp.join("SteamLibrary");
        tool_dir(&lib, "Proton - Experimental", false);
        tool_dir(&lib, "Proton Hotfix", false);
        // Sits in the same directory but is not a Proton — no launcher script.
        std::fs::create_dir_all(lib.join("steamapps/common/Proton BattlEye Runtime")).unwrap();

        let found = find_official(std::slice::from_ref(&lib), "proton_experimental").unwrap();
        assert_eq!(found.display_name, "Proton - Experimental");
        assert!(!found.is_third_party);
        assert!(found
            .proton_script
            .ends_with("Proton - Experimental/proton"));
    }

    #[test]
    fn a_directory_without_a_proton_script_is_not_a_candidate() {
        let tmp = tempdir();
        let lib = tmp.join("SteamLibrary");
        std::fs::create_dir_all(lib.join("steamapps/common/Proton BattlEye Runtime")).unwrap();
        assert!(find_official(&[lib], "proton_battleye_runtime").is_none());
    }

    /// Modern Proton ships no `wine64`; Proton-GE does.
    #[test]
    fn wine_binary_falls_back_when_wine64_is_absent() {
        let tmp = tempdir();
        let modern = tool_dir(&tmp, "Proton - Experimental", false);
        assert!(wine_binary_in(&modern).ends_with("files/bin/wine"));

        let older = tool_dir(&tmp, "Proton-GE", true);
        assert!(wine_binary_in(&older).ends_with("files/bin/wine64"));
    }

    /// An exact match must never be lost to a longer directory that happens to
    /// share a prefix.
    #[test]
    fn an_exact_match_beats_a_prefix_match() {
        let tmp = tempdir();
        let lib = tmp.join("SteamLibrary");
        tool_dir(&lib, "Proton 9.0", false);
        tool_dir(&lib, "Proton 9.0 (Beta)", false);

        let found = find_official(&[lib], "Proton 9.0").unwrap();
        assert_eq!(found.display_name, "Proton 9.0");
    }

    /// With no exact match, the closest prefix wins — and the result must not
    /// depend on `read_dir` order, which is why candidates are sorted.
    #[test]
    fn a_prefix_match_is_deterministic_and_picks_the_closest() {
        let tmp = tempdir();
        let lib = tmp.join("SteamLibrary");
        tool_dir(&lib, "Proton 9.0 Release Candidate 2", false);
        tool_dir(&lib, "Proton 9.0 (Beta)", false);

        for _ in 0..5 {
            let found = find_official(std::slice::from_ref(&lib), "proton_9").unwrap();
            assert_eq!(found.display_name, "Proton 9.0 (Beta)");
        }
    }

    #[test]
    fn protons_are_found_across_every_library() {
        let tmp = tempdir();
        let root = tmp.join("Steam");
        let other = tmp.join("SteamLibrary");
        std::fs::create_dir_all(root.join("steamapps/common")).unwrap();
        tool_dir(&other, "Proton - Experimental", false);

        assert!(find_official(std::slice::from_ref(&root), "proton_experimental").is_none());
        assert!(find_official(&[root, other], "proton_experimental").is_some());
    }

    #[test]
    fn third_party_tools_match_by_directory_or_display_name() {
        let tmp = tempdir();
        let d = tmp.join("compatibilitytools.d");
        let ge = d.join("Proton-GE Latest");
        std::fs::create_dir_all(ge.join("files/bin")).unwrap();
        std::fs::write(ge.join("proton"), "").unwrap();
        std::fs::write(
            ge.join("compatibilitytool.vdf"),
            r#""compatibilitytools" { "compat_tools" { "GE-Proton" { "display_name" "GE-Proton9-20" } } }"#,
        )
        .unwrap();

        let by_dir = find_third_party(std::slice::from_ref(&d), "Proton-GE Latest").unwrap();
        assert_eq!(by_dir.display_name, "GE-Proton9-20");
        assert!(by_dir.is_third_party);

        let by_display = find_third_party(std::slice::from_ref(&d), "GE-Proton9-20").unwrap();
        assert_eq!(by_display.display_name, "GE-Proton9-20");

        assert!(find_third_party(&[d], "something-else").is_none());
    }

    #[test]
    fn configured_tool_name_reads_the_mapping() {
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("config")).unwrap();
        std::fs::write(
            tmp.join("config/config.vdf"),
            r#"
"InstallConfigStore"
{
    "Software" { "Valve" { "Steam" { "CompatToolMapping"
    {
        "2537590" { "name" "proton_experimental" "config" "" "priority" "250" }
        "526870"  { "name" "" }
    } } } }
}
"#,
        )
        .unwrap();

        assert_eq!(
            configured_tool_name(&tmp, "2537590").unwrap().as_deref(),
            Some("proton_experimental")
        );
        // An empty name is "no tool", not a tool called "".
        assert_eq!(configured_tool_name(&tmp, "526870").unwrap(), None);
        assert_eq!(configured_tool_name(&tmp, "999999").unwrap(), None);
    }

    /// A configured tool that cannot be found is a broken setup and must not be
    /// reported as "no tool configured" — that would silently fall back to
    /// running without Proton.
    #[test]
    fn an_unresolvable_configured_tool_is_an_error() {
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("config")).unwrap();
        std::fs::write(
            tmp.join("config/config.vdf"),
            r#""InstallConfigStore" { "Software" { "Valve" { "Steam" { "CompatToolMapping"
               { "2537590" { "name" "proton_nonexistent" } } } } } }"#,
        )
        .unwrap();

        let err = resolve(&tmp, std::slice::from_ref(&tmp), "2537590").unwrap_err();
        assert!(matches!(err, SteamError::CompatToolUnresolved { .. }));
    }

    /// A unique temporary directory, without pulling in a dev-dependency.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-steam-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
