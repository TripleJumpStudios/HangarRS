//! Compare the live bindings against the most recent snapshot.
//!
//! ```bash
//! cargo run -p hangar-bindings --example compare              # newest snapshot vs live
//! cargo run -p hangar-bindings --example compare <a> <b>      # two snapshot directories
//! ```
//!
//! Read-only. This is the answer to "did something replace my bindings?" —
//! Steam Cloud can do exactly that on a bad sync or a second machine, and
//! without a snapshot to compare against there is no way to tell.

use hangar_bindings as bindings;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.len() {
        0 => against_live(),
        2 => two_snapshots(Path::new(&args[0]), Path::new(&args[1])),
        _ => {
            eprintln!("usage: compare [<snapshot-dir> <snapshot-dir>]");
            std::process::exit(2);
        }
    }
}

fn against_live() {
    let Some(store) = hangar_core::paths::bindings_dir() else {
        eprintln!("no XDG data directory; is HOME set?");
        std::process::exit(2);
    };

    let snapshots = bindings::snapshot::list(&store);
    let Some(newest) = snapshots.first() else {
        eprintln!("no snapshots under {}", store.display());
        eprintln!("take one first: cargo run -p hangar-bindings --example snapshot take \"why\"");
        std::process::exit(2);
    };

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

    let when = newest
        .taken()
        .map(|t| t.readable())
        .unwrap_or_else(|| newest.manifest.taken.clone());
    println!("snapshot of {when}");
    println!("against live {}\n", remote.display());

    let mut differing = 0usize;
    for entry in &newest.manifest.profiles {
        let before = bindings::snapshot::profile_path(newest, &entry.id);
        let after = remote.join(&entry.id);
        if !after.exists() {
            println!("GONE      {}  {}", entry.id, entry.friendly_name);
            differing += 1;
            continue;
        }
        if report(&entry.id, &entry.friendly_name, &before, &after) {
            differing += 1;
        }
    }

    // A profile that exists live but not in the snapshot is new since it.
    if let Ok(live) = bindings::list(&remote) {
        for summary in live {
            if newest.profile(&summary.id).is_none() {
                println!("NEW       {}  {}", summary.id, summary.friendly_name);
                differing += 1;
            }
        }
    }

    println!();
    if differing == 0 {
        println!("Nothing has changed since that snapshot.");
    } else {
        println!("{differing} profile(s) differ.");
    }
}

fn two_snapshots(a: &Path, b: &Path) {
    let (Ok(left), Ok(right)) = (bindings::snapshot::load(a), bindings::snapshot::load(b)) else {
        eprintln!("both arguments must be snapshot directories containing snapshot.toml");
        std::process::exit(2);
    };

    println!(
        "{}\nagainst {}\n",
        left.manifest.taken, right.manifest.taken
    );
    let mut ids: Vec<&str> = left
        .manifest
        .profiles
        .iter()
        .chain(&right.manifest.profiles)
        .map(|p| p.id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();

    for id in ids {
        let before: PathBuf = bindings::snapshot::profile_path(&left, id);
        let after: PathBuf = bindings::snapshot::profile_path(&right, id);
        match (before.exists(), after.exists()) {
            (true, true) => {
                let name = left
                    .profile(id)
                    .map(|p| p.friendly_name.clone())
                    .unwrap_or_default();
                report(id, &name, &before, &after);
            }
            (true, false) => println!("GONE      {id}"),
            (false, true) => println!("NEW       {id}"),
            (false, false) => {}
        }
    }
}

/// Print one profile's differences. Returns whether there were any.
fn report(id: &str, name: &str, before: &Path, after: &Path) -> bool {
    let (from, to) = match (bindings::read(before), bindings::read(after)) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            for e in [a.err(), b.err()].into_iter().flatten() {
                println!("UNREADABLE {id}  {e}");
            }
            return true;
        }
    };

    let diff = bindings::compare(&from, &to);
    if diff.is_empty() {
        println!("same      {id}  {name}");
        return false;
    }

    println!("CHANGED   {id}  {name}  —  {}", diff.summary());
    // Bindings first: they are what someone actually cares about.
    for change in diff.bindings() {
        println!("    {change}");
    }
    for change in diff.changes.iter().filter(|c| !c.is_binding()) {
        println!("    {change}");
    }
    true
}
