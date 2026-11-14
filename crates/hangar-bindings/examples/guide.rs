//! Export a printable cheat sheet of the real bindings.
//!
//! ```bash
//! cargo run -p hangar-bindings --example guide                 # to ./bindings-guide.html
//! cargo run -p hangar-bindings --example guide out.html
//! cargo run -p hangar-bindings --example guide out.html --all  # keep DEBUG/DEVMODE and unbound
//! ```
//!
//! Reads the profiles and writes one self-contained HTML file. Open it and use
//! the browser's print dialog for PDF — the stylesheet has a print block that
//! drops to black on white and starts each device on a fresh page.

use hangar_bindings as bindings;
use hangar_bindings::guide::Options;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let all = args.iter().any(|a| a == "--all");
    let out = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "bindings-guide.html".to_string());

    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("no Steam installation found: {e}");
            std::process::exit(2);
        }
    };
    let Some(remote) = bindings::remote_dirs(&steam).into_iter().next() else {
        eprintln!("no Steam Cloud remote/ directory for MSFS 2024");
        std::process::exit(2);
    };

    let summaries = match bindings::list(&remote) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not list profiles: {e}");
            std::process::exit(2);
        }
    };

    let mut loaded = Vec::new();
    for summary in &summaries {
        match bindings::read(&summary.path) {
            Ok(profile) => loaded.push((summary.id.clone(), profile)),
            Err(e) => eprintln!("skipping {}: {e}", summary.id),
        }
    }

    let options = Options {
        include_unbound: all,
        include_developer: all,
        ..Default::default()
    };
    let html = bindings::guide::to_html(loaded.iter().map(|(id, p)| (id.as_str(), p)), &options);

    if let Err(e) = std::fs::write(&out, &html) {
        eprintln!("could not write {out}: {e}");
        std::process::exit(1);
    }

    let shown: usize = loaded
        .iter()
        .map(|(_, p)| {
            p.device
                .contexts
                .iter()
                .filter(|c| options.include_developer || !is_developer(&c.name))
                .flat_map(|c| c.actions.iter())
                .filter(|a| options.include_unbound || a.is_bound())
                .count()
        })
        .sum();

    println!(
        "{out}  —  {} profiles, {shown} rows, {:.0} kB",
        loaded.len(),
        html.len() as f64 / 1024.0
    );
    if !all {
        println!("Unbound actions and DEBUG/DEVMODE/EDITOR left out. Pass --all to keep them.");
    }
    println!("Open it and print to PDF; each device starts on a new page.");
}

fn is_developer(context: &str) -> bool {
    bindings::guide::DEVELOPER_CONTEXTS
        .iter()
        .any(|p| context.starts_with(p))
}
