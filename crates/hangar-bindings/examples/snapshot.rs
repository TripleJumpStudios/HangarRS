//! Back up the real input profiles, and list the backups already taken.
//!
//! ```bash
//! cargo run -p hangar-bindings --example snapshot            # list, writes nothing
//! cargo run -p hangar-bindings --example snapshot take "why" # copy them
//! ```
//!
//! Listing is the default deliberately. `take` writes about 2 MB into
//! `~/.local/share/hangar-rs/bindings/<timestamp>/` and reads Steam's
//! `remote/` directory without touching it.
//!
//! Worth running at least once: these files are Steam Cloud userdata, not in
//! the Proton prefix, so no prefix backup includes them.

use hangar_bindings as bindings;

fn main() {
    let store = match hangar_core::paths::bindings_dir() {
        Some(dir) => dir,
        None => {
            eprintln!("no XDG data directory; is HOME set?");
            std::process::exit(2);
        }
    };

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None | Some("list") => list(&store),
        Some("take") => take(&store, args.next()),
        Some(other) => {
            eprintln!("unknown command {other:?}; expected `list` or `take [note]`");
            std::process::exit(2);
        }
    }
}

fn list(store: &std::path::Path) {
    let snapshots = bindings::snapshot::list(store);
    if snapshots.is_empty() {
        println!("No snapshots yet under {}", store.display());
        println!("Take one with:  cargo run -p hangar-bindings --example snapshot take \"why\"");
        return;
    }

    println!("{}", store.display());
    for snapshot in &snapshots {
        let when = snapshot
            .taken()
            .map(|t| t.readable())
            .unwrap_or_else(|| snapshot.manifest.taken.clone());
        let note = snapshot.manifest.note.as_deref().unwrap_or("-");
        println!(
            "\n  {when}   {} profiles, {:.1} MB   {note}",
            snapshot.manifest.profiles.len(),
            snapshot.bytes() as f64 / 1_048_576.0,
        );
        for entry in &snapshot.manifest.profiles {
            println!(
                "    {}{}  {:<52}  {}",
                entry.id,
                if entry.parsed { " " } else { "!" },
                entry.device_name,
                entry.friendly_name
            );
        }
        match bindings::snapshot::verify(snapshot) {
            Ok(()) => {}
            Err(e) => println!("    INCOMPLETE: {e}"),
        }
    }
}

fn take(store: &std::path::Path, note: Option<String>) {
    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("no Steam installation found: {e}");
            std::process::exit(2);
        }
    };

    let remote_dirs = bindings::remote_dirs(&steam);
    if remote_dirs.is_empty() {
        eprintln!(
            "no Steam Cloud remote/ directory for MSFS 2024 (app {})",
            bindings::MSFS_2024_APP_ID
        );
        std::process::exit(2);
    }

    for dir in &remote_dirs {
        match bindings::snapshot::take(dir, store, note.clone()) {
            Ok(snapshot) => {
                println!(
                    "{} profiles, {:.1} MB\n  from {}\n  into {}",
                    snapshot.manifest.profiles.len(),
                    snapshot.bytes() as f64 / 1_048_576.0,
                    dir.display(),
                    snapshot.dir.display()
                );
                let unparsed: Vec<_> = snapshot
                    .manifest
                    .profiles
                    .iter()
                    .filter(|p| !p.parsed)
                    .map(|p| p.id.as_str())
                    .collect();
                if !unparsed.is_empty() {
                    println!(
                        "  copied but not parseable, so not editable: {}",
                        unparsed.join(", ")
                    );
                }
            }
            Err(e) => {
                eprintln!("could not snapshot {}: {e}", dir.display());
                std::process::exit(1);
            }
        }
    }
}
