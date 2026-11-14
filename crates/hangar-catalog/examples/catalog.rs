//! Read-only library report.
//!
//! `cargo run -p hangar-catalog --example catalog`
//!
//! Scan the real environment and list everything by
//! kind, with the state each item is actually in. Writes nothing.

use hangar_catalog::{CatalogSnapshot, ItemKind, ItemState};
use hangar_msfs::MsfsEnv;
use hangar_steam::SteamEnv;

fn main() {
    let steam = SteamEnv::discover().expect("Steam");
    let env = MsfsEnv::discover(&steam).expect("MSFS");

    // No package libraries are configured, which is itself the finding.
    let libraries: Vec<std::path::PathBuf> = Vec::new();
    let snapshot = CatalogSnapshot::from_scan(hangar_catalog::scan(&env, &libraries));

    // An optional search argument, to show the section counts following it.
    let search = std::env::args().nth(1).unwrap_or_default();
    if !search.is_empty() {
        println!("\nsearching for {search:?}");
    }

    println!("\n\x1b[1mLIBRARY\x1b[0m  {} items", snapshot.len());
    for (section, n) in snapshot.counts_by_section(&search) {
        println!("  {n:>3}  {}", section.label());
    }
    println!("  \u{2014}");
    for (kind, n) in snapshot.counts_by_kind() {
        println!("  {n:>3}  {}", kind.plural());
    }

    let (bytes, measured) = snapshot.measured_size();
    println!(
        "\n  {} active, {} switchable, {} resident, {} broken",
        snapshot.active_count(),
        snapshot.switchable_count(),
        snapshot.resident_count(),
        snapshot.broken_count(),
    );
    println!(
        "  {} on disk, measured across {measured}/{} items",
        hangar_msfs::manifest::format_size(bytes),
        snapshot.len()
    );

    for kind in ItemKind::ALL {
        let items = snapshot.view(Some(kind), &search);
        if items.is_empty() {
            continue;
        }
        println!("\n\x1b[1m{}\x1b[0m", kind.plural().to_uppercase());
        for item in items {
            let state = match &item.state {
                ItemState::Resident => "resident".to_string(),
                ItemState::Linked { .. } => "linked".to_string(),
                ItemState::Available => "available".to_string(),
                ItemState::Dangling { target } => format!("BROKEN -> {}", target.display()),
                ItemState::SimEnabled => "enabled".to_string(),
                ItemState::SimDisabled => "disabled".to_string(),
                ItemState::Ready => "ready".to_string(),
                ItemState::MissingExecutable => "MISSING EXE".to_string(),
            };
            let size = item
                .size_bytes
                .map(hangar_msfs::manifest::format_size)
                .unwrap_or_else(|| "—".into());
            println!(
                "  {:<11} {:<9} {:<38} {:<14} {}",
                state,
                item.content_type.as_ref().map_or("", |c| c.label()),
                truncate(item.display_title(), 38),
                truncate(&item.creator, 14),
                size,
            );
        }
    }

    if !snapshot.problems().is_empty() {
        println!("\n\x1b[1mPROBLEMS\x1b[0m");
        for p in snapshot.problems() {
            println!("  {} — {}", p.path.display(), p.reason);
        }
    }

    let stuck = snapshot.resident_count();
    if stuck > 0 && snapshot.switchable_count() == 0 {
        println!(
            "\n\x1b[33mNote:\x1b[0m all {stuck} packages are resident — real directories inside\n\
             the Community folder. No package library is configured, so a profile\n\
             switch would have nothing to switch."
        );
    }

    println!("\nRead-only: nothing above was modified.");
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
