//! Stage 3 against the real machine, read-only.
//!
//! ```bash
//! cargo run -p hangar-install --example gate -- ~/Downloads/setup.exe
//! cargo run -p hangar-install --example gate -- setup.exe 4000000000
//! ```
//!
//! Inspects the file (stage 1), looks up its vendor profile (stage 2), then
//! runs the pre-flight gate (stage 3) against the environment as it is right
//! now: the real prefixes, the real registry text, the real free space, and
//! whether the sim is actually running. Nothing is executed and nothing is
//! written.
//!
//! The optional second argument is a payload estimate in bytes. Without one
//! the gate is given four times the installer's size — a crude stand-in,
//! labelled as such in the output — because guessing nothing would leave the
//! space check permanently unchecked.

use hangar_install::gate::{self, Outcome, Request};
use hangar_install::vendor::Table;
use hangar_install::{inspect, Engine};
use hangar_runtime::env::legacy_companion_dir;
use hangar_runtime::sim;

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let [path, payload] = match args.as_slice() {
        [path] => [Some(path.clone()), None],
        [path, bytes] => [Some(path.clone()), Some(bytes.clone())],
        _ => {
            eprintln!("usage: gate <file.exe> [payload-bytes]");
            eprintln!();
            eprintln!("Runs the pre-flight checks against the real environment.");
            eprintln!("Runs nothing, writes nothing.");
            std::process::exit(2);
        }
    };
    let path = path.unwrap();

    // The environment, discovered rather than assumed. If discovery fails the
    // gate has nothing honest to say about prefixes, so that is fatal here.
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

    let msfs_prefix = msfs.install.prefix.clone();
    // A companion directory is compat-data-shaped: the prefix itself is
    // `pfx/` inside it, same as Steam lays out the sim's own compatdata.
    let companion_prefix = legacy_companion_dir()
        .map(|d| d.join("pfx"))
        .filter(|p| p.exists());

    println!("{}", "─".repeat(72));
    println!("environment");
    println!("  MSFS prefix       {}", msfs_prefix.display());
    println!(
        "  companion prefix  {}",
        companion_prefix
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none found".into())
    );
    println!(
        "  sim               {}",
        if sim::is_running() {
            "running"
        } else {
            "not running"
        }
    );

    // Stage 1: what is this file?
    let report = match inspect(std::path::Path::new(&path)) {
        Ok(report) => report,
        Err(e) => {
            println!("{}", "─".repeat(72));
            println!("{path}: {e}");
            std::process::exit(1);
        }
    };

    println!("{}", "─".repeat(72));
    println!("stage 1 — {}", report.path.display());
    println!(
        "  {:.1} MB, engine {}, identity {}",
        report.size as f64 / 1_048_576.0,
        report.engine.label(),
        report.identity.describe(),
    );

    // Stage 2: whose rules does it run under?
    let table = Table::load();
    let Some(found) = table.lookup(&report) else {
        println!("stage 2 — no vendor profile; there is nothing to gate against");
        if report.engine == Engine::Unknown && report.packed.is_none() {
            println!("  → treat as an ordinary application, not an installer");
        }
        std::process::exit(1);
    };
    println!(
        "stage 2 — {} ({})",
        found.profile.name,
        found.why.join(", ")
    );

    // The estimate: explicit, or a labelled guess from the file size.
    let payload_bytes = match payload.as_deref() {
        Some(raw) => match raw.parse::<u64>() {
            Ok(bytes) => {
                println!("stage 3 — payload estimate given on the command line");
                Some(bytes)
            }
            Err(_) => {
                eprintln!("payload estimate {raw:?} is not a number of bytes");
                std::process::exit(2);
            }
        },
        None if found.profile.payload == hangar_install::vendor::Payload::Downloads => {
            println!("stage 3 — download size unknown; space check unchecked");
            None
        }
        None => {
            let crude = report.payload_estimate();
            println!(
                "stage 3 — payload estimated at {} ({})",
                hangar_core::disk::human(crude),
                if report.siblings.cabs.is_empty() {
                    "crude: 4× the file"
                } else {
                    "crude: 3× the cabs beside it, because the stub is not the payload"
                }
            );
            Some(crude)
        }
    };

    // Stage 3: may this run, as things stand?
    let verdict = gate::run(Request {
        profile: found.profile,
        msfs_prefix: &msfs_prefix,
        companion_prefix: companion_prefix.as_deref(),
        payload_bytes,
        sim_running: sim::is_running(),
        reaches_sim_content: match found.profile.prefix {
            hangar_install::Prefix::Msfs => Some(true),
            hangar_install::Prefix::Companion => companion_prefix
                .as_deref()
                .and_then(|p| gate::reaches_sim_content(p, &msfs_prefix)),
        },
    });

    println!("{}", "─".repeat(72));
    let mut refused = 0;
    let mut unchecked = 0;
    for check in &verdict.outcomes {
        match &check.outcome {
            Outcome::Pass(evidence) => println!("  ✓ {:<28} {evidence}", check.label),
            Outcome::Unchecked(why) => {
                unchecked += 1;
                println!("  ? {:<28} {why}", check.label);
            }
            Outcome::Refuse(r) => {
                refused += 1;
                println!("  ✗ {:<28} {r}", check.label);
            }
        }
    }

    println!("{}", "─".repeat(72));
    if refused > 0 {
        println!(
            "REFUSED — {refused} check{} said no",
            if refused == 1 { "" } else { "s" }
        );
    } else if unchecked > 0 {
        println!("CLEAR with {unchecked} unchecked — decide those by hand before running");
    } else {
        println!("CLEAR — nothing observed stands in the way");
    }
}
