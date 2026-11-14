//! Stage 6 against the real machine, read-only.
//!
//! ```bash
//! cargo run -p hangar-install --example verify                    # every package
//! cargo run -p hangar-install --example verify -- <package>       # one of them
//! cargo run -p hangar-install --example verify -- <package> <data1.cab>
//! ```
//!
//! With a cab, it also answers the question no self-check can: is what is
//! installed what *that installer* would produce? It extracts one file and
//! compares, and writes nothing outside a temporary directory it removes.

use std::path::{Path, PathBuf};

use hangar_install::verify::{self, Applied, Layout};

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("steam: {e}");
            std::process::exit(2);
        }
    };
    let msfs = match hangar_msfs::MsfsEnv::discover(&steam) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("msfs: {e}");
            std::process::exit(2);
        }
    };

    // A name is looked up in every Community folder discovery found, not just
    // the active one — packages for both generations sit side by side here.
    let (packages, cab) = match args.as_slice() {
        [] => (all_packages(&msfs.community), None),
        [name] => (vec![locate(&msfs, name)], None),
        [name, cab] => (vec![locate(&msfs, name)], Some(PathBuf::from(cab))),
        _ => {
            eprintln!("usage: verify [<package> [data1.cab]]");
            std::process::exit(2);
        }
    };

    if packages.is_empty() {
        println!("no packages in {}", msfs.community.display());
        return;
    }

    let mut complete = 0usize;
    for dir in &packages {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());

        match verify::verify_package(dir) {
            Err(e) => println!("  {name:<44} {e}"),
            Ok(report) => {
                if report.is_complete() {
                    complete += 1;
                    println!("  {name:<44} {}/{} ok", report.verified(), report.declared);
                } else {
                    println!(
                        "  {name:<44} {}/{}  {} missing, {} wrong size, {} orphan",
                        report.verified(),
                        report.declared,
                        report.missing.len(),
                        report.wrong_size.len(),
                        report.orphans.len()
                    );
                    for path in report.missing.iter().take(5) {
                        println!("      missing  {path}");
                    }
                    for m in report.wrong_size.iter().take(5) {
                        println!(
                            "      size     {} — declared {}, found {}",
                            m.path, m.declared, m.found
                        );
                    }
                    for path in report.orphans.iter().take(5) {
                        println!("      orphan   {path}");
                    }
                }

                // Long paths are a property of where the package sits, so they
                // are reported whether or not anything else is wrong.
                if let Ok(layout) = Layout::read(&dir.join("layout.json")) {
                    let over = verify::too_long(&layout, COMMUNITY_WIN, &name);
                    if !over.is_empty() {
                        println!("      {} path(s) over MAX_PATH:", over.len());
                        for path in over.iter().take(5) {
                            println!("        {path}");
                        }
                    }
                }
            }
        }
    }

    if packages.len() > 1 {
        println!();
        println!("{complete}/{} complete", packages.len());
    }

    let (Some(cab), Some(dir)) = (cab, packages.first()) else {
        return;
    };
    compare_with_cab(&cab, dir);
}

/// Where Community sits as Windows sees it. Taken verbatim from FSUIPC7's own
/// installer log, which resolved it from `UserCfg.opt`.
const COMMUNITY_WIN: &str =
    r"C:\users\steamuser\AppData\Roaming\Microsoft Flight Simulator 2024\Packages\Community";

/// A package by name, or by path if one was given.
fn locate(msfs: &hangar_msfs::MsfsEnv, name: &str) -> PathBuf {
    let direct = PathBuf::from(name);
    if direct.join("layout.json").is_file() {
        return direct;
    }
    for folder in &msfs.communities {
        let candidate = folder.path.join(name);
        if candidate.join("layout.json").is_file() {
            return candidate;
        }
    }
    msfs.community.join(name)
}

fn all_packages(community: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(community) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("layout.json").is_file())
        .collect();
    found.sort();
    found
}

fn compare_with_cab(cab: &Path, dir: &Path) {
    println!();
    println!("did this installer apply?");

    let scratch = std::env::temp_dir().join(format!("hangar-verify-cab-{}", std::process::id()));
    let extracted = match verify::extract_from_cab(cab, "layout.json", &scratch) {
        Ok(path) => path,
        Err(e) => {
            println!("  cannot tell: {e}");
            let _ = std::fs::remove_dir_all(&scratch);
            return;
        }
    };

    let result = (|| {
        let in_cab = Layout::read(&extracted)?;
        let on_disk = Layout::read(&dir.join("layout.json"))?;
        Ok::<_, verify::VerifyError>((in_cab.content.len(), verify::applied(&in_cab, &on_disk)))
    })();
    let _ = std::fs::remove_dir_all(&scratch);

    match result {
        Err(e) => println!("  cannot tell: {e}"),
        Ok((_, Applied::Yes)) => {
            println!("  yes — the installed layout matches the one in the cab")
        }
        Ok((
            _,
            Applied::No {
                declared_by_installer,
                declared_on_disk,
                added,
                removed,
                resized,
            },
        )) => {
            println!("  no — what is installed is not what this installer ships");
            println!("     installer  {declared_by_installer} entries");
            println!("     on disk    {declared_on_disk} entries");
            println!(
                "     {} added, {} removed, {resized} a different size",
                added.len(),
                removed.len()
            );
            for path in added.iter().take(4) {
                println!("       + {path}");
            }
            for path in removed.iter().take(4) {
                println!("       - {path}");
            }
        }
    }
}
