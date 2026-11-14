//! The write gate: read every real input profile, write it back, compare bytes.
//!
//! ```bash
//! cargo run -p hangar-bindings --example roundtrip
//! ```
//!
//! **Read-only.** It opens the profiles, serialises to memory and compares.
//! Nothing is written to `remote/`, and nothing may be unless this is clean on
//! every profile: these files are Steam Cloud userdata and often the only copy
//! of the user's binding work.
//!
//! A failure prints the first divergence with both lines, because the writer
//! is a reproduction: the first difference is the bug and everything after it
//! is the same mistake repeated.

use hangar_bindings as bindings;

fn main() {
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
            "no Steam Cloud remote/ directory for MSFS 2024 (app {}) under {}",
            bindings::MSFS_2024_APP_ID,
            steam.userdata_root().display()
        );
        std::process::exit(2);
    }

    let mut checked = 0usize;
    let mut failed = 0usize;

    for dir in &remote_dirs {
        println!("{}", dir.display());

        let profiles = match bindings::list(dir) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  could not list: {e}");
                failed += 1;
                continue;
            }
        };

        if profiles.is_empty() {
            println!("  no input profiles here");
            continue;
        }

        for summary in profiles {
            checked += 1;
            let label = format!(
                "{}  {:<30}  {}",
                summary.id, summary.device_name, summary.friendly_name
            );

            match bindings::roundtrip_ok(&summary.path) {
                Ok(()) => println!("  OK        {label}  ({} bytes)", summary.bytes),
                Err(e) => {
                    failed += 1;
                    println!("  DIVERGED  {label}");
                    println!("    {e}");
                }
            }
        }
    }

    println!();
    if failed == 0 {
        println!("{checked} profiles, all byte-identical after a round-trip.");
    } else {
        println!("{checked} profiles, {failed} did not round-trip.");
        std::process::exit(1);
    }
}
