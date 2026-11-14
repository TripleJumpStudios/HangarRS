//! The migration — the previous application's config, translated.
//!
//! ```bash
//! cargo run -p hangar-core --example migrate            # dry run: says everything, writes nothing
//! cargo run -p hangar-core --example migrate -- --write # writes ~/.config/hangar-rs/config.toml
//! ```
//!
//! `~/.config/msfs-manager/config.toml` is opened read-only and is never
//! written to, with or without the flag. `--write` refuses if a HangarRS config
//! is already there.

use hangar_core::config::Config;
use hangar_core::migrate::{self, Migration, Outcome};

fn main() {
    let write = std::env::args().any(|a| a == "--write");

    let migration = match Migration::from_legacy() {
        Ok(Some(m)) => m,
        Ok(None) => {
            println!("nothing to migrate: no previous configuration found.");
            return;
        }
        Err(e) => {
            eprintln!("could not read the previous configuration: {e}");
            std::process::exit(1);
        }
    };

    println!("from  {}", migration.source.display());
    match Config::path() {
        Some(path) => println!(
            "to    {}{}",
            path.display(),
            if path.exists() { "   (exists)" } else { "" }
        ),
        None => println!("to    (no config directory)"),
    }
    println!();

    report(&migration);

    if !migration.advisories.is_empty() {
        println!("worth a look");
        for advisory in &migration.advisories {
            println!("  · {advisory}");
        }
        println!();
    }

    println!("would write");
    println!();
    for line in migration.config.to_toml().unwrap_or_default().lines() {
        println!("    {line}");
    }
    println!();

    if !write {
        println!("dry run. Nothing was written. Add --write to do it.");
        return;
    }

    match migrate::write_new(&migration.config) {
        Ok(path) => println!("written to {}", path.display()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Every key of the old file, and what became of it.
///
/// Grouped by outcome rather than listed flat, because the interesting halves
/// are the two at the bottom: what could not be carried, and what this code has
/// never heard of.
fn report(migration: &Migration) {
    let (carried, same, dropped, unknown) = migration.summary();
    println!("{} key(s): {carried} carried, {same} already the same, {dropped} dropped, {unknown} unknown", migration.notes.len());
    println!();

    for note in &migration.notes {
        if let Outcome::Carried { to } = &note.outcome {
            println!("  carried   {:<32} {:<24} → {to}", note.key, note.value);
        }
    }
    for note in &migration.notes {
        if let Outcome::Same { because } = &note.outcome {
            println!(
                "  same      {:<32} {:<24}   {because}",
                note.key, note.value
            );
        }
    }
    println!();

    for note in &migration.notes {
        if let Outcome::Dropped { why } = &note.outcome {
            println!("  dropped   {} = {}", note.key, note.value);
            println!("            {why}");
        }
    }
    for note in migration.unknown_keys() {
        println!("  UNKNOWN   {} = {}", note.key, note.value);
        println!("            this version of HangarRS has no idea what that is.");
    }
    println!();
}
