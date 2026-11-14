//! Drop-install against the real machine: read a drop, print the plan, execute
//! nothing.
//!
//! ```bash
//! cargo run -p hangar-install --example drop                    # overlaps, no drop needed
//! cargo run -p hangar-install --example drop -- ~/Downloads/kpvf.zip
//! cargo run -p hangar-install --example drop -- ~/Downloads/kpvf.zip --write
//! cargo run -p hangar-install --example drop -- <path> --into Community2024
//! ```
//!
//! With no path it reports **overlapping packages** — two installed packages
//! declaring the same content, which the sim loads both of and resolves by its
//! own ordering. That needs no drop.
//!
//! With a path it reads the drop, proposes an identity for each package it
//! finds against what is installed, runs the gate, and stops. `--write`
//! performs the placement, and only then: it copies into staging beside the
//! Community folder, verifies what it staged, moves any old version into
//! `.hangar-replaced/<stamp>/`, and renames the new one in. **Nothing is ever
//! deleted.**

use std::path::PathBuf;

use hangar_install::drop::{self, Approval, GateRequest, Reading, Verdict};
use hangar_install::gate::Outcome;
use hangar_install::record;
use hangar_msfs::manifest::format_size;
use hangar_runtime::sim;

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let write = args.iter().any(|a| a == "--write");
    let into = args
        .iter()
        .position(|a| a == "--into")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .filter(|a| Some(*a) != into.as_ref())
        .cloned();

    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(steam) => steam,
        Err(e) => {
            eprintln!("steam: {e}");
            std::process::exit(2);
        }
    };
    let msfs = match hangar_msfs::MsfsEnv::discover(&steam) {
        Ok(msfs) => msfs,
        Err(e) => {
            eprintln!("msfs: {e}");
            std::process::exit(2);
        }
    };

    let communities: Vec<PathBuf> = msfs.communities.iter().map(|c| c.path.clone()).collect();

    println!("{}", "─".repeat(72));
    println!("Community folders");
    for community in &communities {
        println!(
            "  {}  {} packages",
            community.display(),
            drop::installed_in(community).len()
        );
    }
    println!();

    let Some(path) = path else {
        report_overlaps(&communities);
        return;
    };

    let chosen = match choose(&communities, into.as_deref()) {
        Some(chosen) => chosen,
        None => {
            // Not fatal: the gate says the same thing in its own shape below,
            // and the reading half is worth seeing either way.
            eprintln!(
                "two Community folders were found. Name one with --into <folder> to go further."
            );
            eprintln!();
            communities.first().cloned().unwrap_or_default()
        }
    };

    let path = PathBuf::from(expand_tilde(&path));
    let staging = drop::staging_root(&chosen);

    let reading = match drop::read(&path, &staging) {
        Ok(reading) => reading,
        Err(e) => {
            eprintln!("refused: {e}");
            std::process::exit(1);
        }
    };

    let found = match reading {
        Reading::Installer(exe) => {
            println!("{} is a Windows installer.", exe.display());
            println!("That is the seven-stage pipeline, not this one:");
            println!(
                "  cargo run -p hangar-install --example gate -- {}",
                exe.display()
            );
            return;
        }
        Reading::Packages(found) => found,
    };

    let installed = drop::installed_in(&chosen);
    let total: u64 = found.iter().map(|f| f.bytes).sum();

    println!("{}", "─".repeat(72));
    println!("what was dropped — {}", path.display());
    let mut verdicts = Vec::new();
    for package in &found {
        let verdict = drop::propose(package, &installed);
        println!();
        println!("  {}", package.name);
        println!("    title      {}", package.title());
        println!("    creator    {}", package.manifest.creator);
        println!("    type       {}", package.manifest.content_type().label());
        println!("    version    {}", package.version());
        println!("    size       {}", format_size(package.bytes));
        if package.declares.is_empty() {
            println!("    declares   nothing (no ContentInfo)");
        } else {
            println!(
                "    declares   {}",
                package
                    .declares
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        match &verdict {
            Verdict::New => println!("    verdict    New"),
            Verdict::Update { confidence, .. } => {
                println!(
                    "    verdict    {} [{}]",
                    verdict.label(),
                    confidence.label()
                )
            }
            Verdict::Overlaps { shared, .. } => {
                println!(
                    "    verdict    {} — shares {}",
                    verdict.label(),
                    shared.join(", ")
                );
                println!("               not an update; nothing will be replaced");
            }
        }
        verdicts.push(verdict);
    }

    println!();
    println!("{}", "─".repeat(72));
    println!("the gate");
    let report = drop::gate(GateRequest {
        writes_allowed: write,
        sim_running: sim::is_running(),
        discovered: &communities,
        chosen: choose(&communities, into.as_deref()).as_deref(),
        need_bytes: total,
    });
    for checked in &report.outcomes {
        let (mark, detail) = match &checked.outcome {
            Outcome::Pass(evidence) => ("PASS", evidence.clone()),
            Outcome::Unchecked(why) => ("?", why.clone()),
            Outcome::Refuse(refusal) => ("REFUSE", refusal.to_string()),
        };
        println!("  {mark:<6}  {:<38}  {detail}", checked.label);
    }

    println!();
    if !write {
        println!("Nothing was written. Add --write to place these.");
        let replacing = verdicts.iter().filter(|v| v.replaces().is_some()).count();
        println!(
            "It would place {} package(s) and move {} aside into {}/<stamp>/.",
            found.len(),
            replacing,
            drop::replaced_root(&chosen).display()
        );
        return;
    }

    if !report.is_clear() {
        eprintln!("The gate refused. Nothing was written.");
        std::process::exit(1);
    }

    let stamp = hangar_core::time::Utc::now().stamp();
    let approval = Approval::given(format!(
        "the drop example was run with --write against {}",
        path.display()
    ));

    println!("{}", "─".repeat(72));
    println!("placing");
    let mut placed = Vec::new();
    for package in &found {
        // What the proposal said this replaces, not what shares its name —
        // a vendor that renames the folder between versions makes those two
        // different directories.
        let replacing = drop::propose(package, &installed)
            .replaces()
            .map(str::to_owned);
        match drop::place(package, &chosen, &stamp, &approval, replacing.as_deref()) {
            Ok(one) => {
                println!(
                    "  placed   {}  {}  {}",
                    one.name,
                    one.verification.summary(),
                    one.replaced
                        .iter()
                        .map(|o| format!("replaced {} → {}", o.name, o.moved_to.display()))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                placed.push(one);
            }
            Err(e) => eprintln!("  refused  {}: {e}", package.name),
        }
    }

    if placed.is_empty() {
        eprintln!("Nothing was placed.");
        std::process::exit(1);
    }

    let dropped_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(total);
    let filed = record::InstallRecord::of_drop(&path, dropped_bytes, &chosen, &placed);
    match record::file_default(&filed) {
        Ok(at) => println!("\nfiled  {}", at.display()),
        Err(e) => eprintln!("\nplaced, but the record could not be filed: {e}"),
    }
}

/// Which Community folder. **Never guessed** — one means one, two means the
/// caller says which.
fn choose(communities: &[PathBuf], into: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = into {
        return communities
            .iter()
            .find(|c| c.file_name().is_some_and(|n| n == name))
            .cloned();
    }
    match communities {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Two installed packages declaring the same content.
fn report_overlaps(communities: &[PathBuf]) {
    println!("{}", "─".repeat(72));
    println!("packages declaring the same content as each other");
    println!();

    let mut any = false;
    for community in communities {
        let installed = drop::installed_in(community);
        let found = drop::overlaps(&installed);
        if found.is_empty() {
            continue;
        }
        any = true;
        println!("  in {}", community.display());
        for overlap in found {
            println!("    {}", overlap.shared.join(", "));
            for name in &overlap.packages {
                let version = installed
                    .iter()
                    .find(|i| &i.name == name)
                    .map(|i| i.version.clone())
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "?".into());
                println!("      {name}   v{version}");
            }
            println!(
                "      the sim loads both and resolves this by its own ordering; \
                 nothing reports it"
            );
            println!();
        }
    }

    if !any {
        println!("  none — no two packages declare the same content.");
        println!();
    }
    println!("Drop a folder or a .zip on this to install one:");
    println!("  cargo run -p hangar-install --example drop -- <path>");
}

fn expand_tilde(raw: &str) -> String {
    match raw.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest).display().to_string())
            .unwrap_or_else(|| raw.to_string()),
        None => raw.to_string(),
    }
}
