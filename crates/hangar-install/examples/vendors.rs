//! The vendor table, as loaded — shipped entries plus the user's own.
//!
//! ```bash
//! cargo run -p hangar-install --example vendors
//! ```
//!
//! Reads nothing but the table. Use it to check that a hand-written entry in
//! `$XDG_CONFIG_HOME/hangar-rs/vendors.toml` parses and lands where it should:
//! a malformed file is a warning rather than a failure, so an entry with a
//! typo in it is otherwise invisible until an install behaves oddly.

use hangar_install::vendor::{Origin, Table};

fn main() {
    let table = Table::load();

    match hangar_install::vendor::user_table() {
        Some(path) if path.exists() => println!("user table: {}", path.display()),
        Some(path) => println!("user table: {} (not present)", path.display()),
        None => println!("user table: no config directory"),
    }
    for warning in &table.warnings {
        println!("  ! {warning}");
    }
    println!();

    for profile in &table.profiles {
        let origin = match profile.origin {
            Origin::Shipped => "shipped",
            Origin::User => "yours",
        };
        println!("{}  [{origin}]", profile.name);
        println!(
            "  matches      engine {}, company {}, product {}",
            profile
                .engine
                .map(|e| e.label().to_string())
                .unwrap_or_else(|| "any".into()),
            profile.company.as_str(),
            profile.product.as_str()
        );
        println!(
            "  runs         {} prefix, proton {}, cwd {}",
            profile.prefix.label(),
            profile.verb.as_str(),
            profile.cwd.label()
        );
        println!("  expect       {}", profile.produces.label());
        if !profile.runtimes.is_empty() {
            println!("  needs        {}", profile.runtimes.join(", "));
        }
        println!();
    }

    println!(
        "{} profile{}, {} of them yours",
        table.profiles.len(),
        if table.profiles.len() == 1 { "" } else { "s" },
        table
            .profiles
            .iter()
            .filter(|p| p.origin == Origin::User)
            .count()
    );
}
