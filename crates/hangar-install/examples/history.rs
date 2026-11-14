//! Stage 7's history — every install HangarRS has performed.
//!
//! ```bash
//! cargo run -p hangar-install --example history
//! cargo run -p hangar-install --example history -- <timestamp>
//! ```
//!
//! Read-only. Records live in `$XDG_DATA_HOME/hangar-rs/installs/`, one JSON
//! file per install, named for when it happened.

use hangar_install::record;

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let records = record::list_default();
    if records.is_empty() {
        match hangar_core::paths::install_history_dir() {
            Some(dir) => println!("no installs recorded in {}", dir.display()),
            None => println!("no data directory, so no history"),
        }
        return;
    }

    if let Some(which) = args.first() {
        let Some(found) = records.iter().find(|r| r.at.starts_with(which)) else {
            eprintln!("no record starting {which}");
            std::process::exit(1);
        };
        detail(found);
        return;
    }

    for entry in &records {
        println!("  {}", entry.summary());
        if !entry.concerns.is_empty() {
            println!("      {} concern(s)", entry.concerns.len());
        }
    }
    println!();
    println!("{} install(s). Name one to see it in full.", records.len());
}

fn detail(entry: &record::InstallRecord) {
    println!("{}", "─".repeat(72));
    println!("{}", entry.at);
    println!("  installer  {}", entry.installer.display());
    println!("  engine     {}", entry.engine);
    println!(
        "  identity   {} — {}",
        entry.vendor.as_deref().unwrap_or("?"),
        entry.product.as_deref().unwrap_or("?")
    );
    println!(
        "  profile    {} [{}], {} prefix",
        entry.profile, entry.profile_origin, entry.prefix
    );
    println!("  command    {}", entry.command);
    println!("  cwd        {}", entry.cwd.display());
    println!(
        "  ran        {:.1}s, exit {:?}, verdict: {}",
        entry.ran_for_secs, entry.exit_code, entry.verdict
    );

    if !entry.bad_console.is_empty() {
        println!();
        println!("  console said:");
        for line in &entry.bad_console {
            println!("    {line}");
        }
    }

    println!();
    if entry.landed.is_empty() {
        println!("  nothing landed");
    }
    for item in &entry.landed {
        println!("  {:<14} {}", item.kind, item.name);
        println!("      {}", item.detail);
    }

    if let Some(v) = &entry.verified {
        println!();
        println!(
            "  verified   {} — {}/{} present, {} missing, {} wrong size, {} orphan",
            v.package, v.verified, v.declared, v.missing, v.wrong_size, v.orphans
        );
    }

    if !entry.concerns.is_empty() {
        println!();
        println!("  concerns:");
        for concern in &entry.concerns {
            println!("    {concern}");
        }
    }
}
