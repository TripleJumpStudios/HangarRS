//! Read-only environment report.
//!
//! `cargo run -p hangar-msfs --example discover`
//!
//! A standing diagnostic: point it at the real
//! machine and it prints everything HangarRS believes about the environment
//! without touching a single byte. Every path is checked for existence, so a
//! wrong guess shows up as `MISSING` rather than as a confident-looking string.

use hangar_msfs::{ContentType, ManifestJson, MsfsEnv};
use hangar_steam::SteamEnv;
use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let steam = match SteamEnv::discover() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Steam discovery failed: {e}");
            std::process::exit(1);
        }
    };

    header("STEAM");
    row("root", &steam.steam_root);
    println!("  {:<22} {}", "flatpak", steam.is_flatpak);
    for lib in &steam.libraries {
        let tag = if lib.is_steam_root {
            "library (root)"
        } else {
            "library"
        };
        row(tag, &lib.path);
    }

    let msfs = match MsfsEnv::discover(&steam) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("\nMSFS discovery failed: {e}");
            std::process::exit(1);
        }
    };

    header("MSFS 2024");
    println!("  {:<22} {}", "name", msfs.install.name);
    println!("  {:<22} {}", "app id", msfs.install.app_id);
    row("install dir", &msfs.install.install_dir);
    row("library holding it", &msfs.install.library_root);
    row("compatdata", &msfs.install.compat_data);
    row("prefix", &msfs.install.prefix);

    // The trap, checked explicitly: the prefix is in the library holding the
    // game, so the steam_root-derived path should not exist.
    let wrong = steam
        .steam_root
        .join("steamapps/compatdata")
        .join(&msfs.install.app_id);
    println!(
        "  {:<22} {} {}",
        "not under steam_root",
        wrong.display(),
        if wrong.exists() {
            "<- EXISTS?!"
        } else {
            "(correctly absent)"
        }
    );

    header("PROTON");
    match &msfs.compat_tool {
        Some(t) => {
            println!("  {:<22} {}", "configured as", t.configured_name);
            println!("  {:<22} {}", "resolved to", t.display_name);
            println!("  {:<22} {}", "third party", t.is_third_party);
            row("proton script", &t.proton_script);
            row("wine binary", &t.wine_binary);
        }
        None => println!("  none configured"),
    }

    header("PACKAGES");
    row("UserCfg.opt", &msfs.user_cfg);
    row("packages root", &msfs.packages_root);
    for c in &msfs.communities {
        let active = if c.path == msfs.community {
            " <- active"
        } else {
            ""
        };
        println!(
            "  {:<22} {} [{:?}]{active}",
            "community",
            c.path.display(),
            c.generation
        );
    }
    for o in msfs.official_folders() {
        row("official", &o);
    }

    for c in &msfs.communities {
        let (total, bad, kinds, bytes) = scan(&c.path);
        let name = c.path.file_name().unwrap_or_default().to_string_lossy();
        println!("\n  {name}: {total} packages, {bad} unreadable");
        for (kind, n) in &kinds {
            println!("      {n:>3}  {kind}");
        }
        if bytes > 0 {
            println!(
                "      declared size: {}",
                hangar_msfs::manifest::format_size(bytes)
            );
        }
    }

    header("BINDINGS (Steam Cloud, not the prefix)");
    let dirs = msfs.binding_dirs(&steam);
    if dirs.is_empty() {
        println!("  no cloud remote directories found");
    }
    for dir in dirs {
        row("remote", &dir);
        let profiles = count_matching(&dir, "inputprofile_");
        println!("  {:<22} {profiles}", "input profiles");
        let cache = dir.parent().map(|p| p.join("remotecache.vdf"));
        if let Some(cache) = cache {
            row("remotecache.vdf", &cache);
        }
    }

    println!("\nRead-only: nothing above was modified.");
}

fn header(title: &str) {
    println!("\n\x1b[1m{title}\x1b[0m");
}

/// Print a path, flagging it when it is not actually there.
///
/// The existence check is the point of this report: a discovered path that does
/// not exist is the most useful thing it can tell you, and the easiest to hide
/// behind a confident-looking string.
fn row(label: &str, path: &Path) {
    let mark = if path.exists() {
        ""
    } else {
        "   \x1b[31mMISSING\x1b[0m"
    };
    println!("  {:<22} {}{mark}", label, path.display());
}

/// Count packages, classify them, and total their declared sizes.
fn scan(community: &Path) -> (usize, usize, BTreeMap<String, usize>, u64) {
    let mut total = 0;
    let mut bad = 0;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut bytes = 0u64;

    let Ok(entries) = std::fs::read_dir(community) else {
        return (0, 0, kinds, 0);
    };
    for entry in entries.flatten() {
        let manifest = entry.path().join("manifest.json");
        if !manifest.is_file() {
            continue;
        }
        total += 1;
        match std::fs::read_to_string(&manifest)
            .ok()
            .and_then(|s| ManifestJson::parse(&s).ok())
        {
            Some(m) => {
                let label = match m.content_type() {
                    ContentType::Other(s) => format!("{s} (unrecognised)"),
                    other => other.label().to_string(),
                };
                *kinds.entry(label).or_default() += 1;
                bytes += m.size_bytes().unwrap_or(0);
            }
            None => {
                bad += 1;
                *kinds.entry("<unparseable>".into()).or_default() += 1;
            }
        }
    }
    (total, bad, kinds, bytes)
}

fn count_matching(dir: &Path, prefix: &str) -> usize {
    std::fs::read_dir(dir)
        .map(|e| {
            e.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .count()
        })
        .unwrap_or(0)
}
