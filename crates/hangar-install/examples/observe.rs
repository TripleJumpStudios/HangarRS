//! Stage 4 against the real machine.
//!
//! ```bash
//! cargo run -p hangar-install --example observe -- ~/Downloads/setup.exe
//! cargo run -p hangar-install --example observe -- ~/Downloads/setup.exe --run
//! ```
//!
//! **Without `--run` nothing is executed and nothing is written.** The dry run
//! walks stages 1 to 3, resolves the exact command stage 4 would use, and
//! takes the "before" snapshot so the three places are visible as they stand.
//! That is the useful half most of the time: the command line is what usually
//! turns out wrong.
//!
//! With `--run`, the installer is actually started, which **writes to a Proton
//! prefix and possibly to the Community folder**. It is refused unless the
//! stage 3 gate is clear.
//!
//! Either way, nothing here answers a dialog. InstallShield's MAX_PATH prompt
//! offers Abort, and Abort rolls back the whole install.

use std::collections::BTreeSet;
use std::sync::Arc;

use hangar_install::classify;
use hangar_install::gate::{self, Outcome, Request};
use hangar_install::record;
use hangar_install::run::{self, Invocation, Verdict};
use hangar_install::shim;
use hangar_install::snapshot::{Change, Diff, Places, Seen, Snapshot};
use hangar_install::vendor::{self, Prefix, Table};
use hangar_install::verify;
use hangar_install::{inspect, Engine};
use hangar_runtime::env::{legacy_companion_dir, ProtonEnv};
use hangar_runtime::sim;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let (path, execute) = match args.as_slice() {
        [path] => (path.clone(), false),
        // Both spellings, because `preflight` next door takes a bare `run`
        // and a person who learned one will type it at the other.
        [path, flag] if flag == "--run" || flag == "run" => (path.clone(), true),
        _ => {
            eprintln!("usage: observe <file.exe> [--run|run]");
            eprintln!();
            eprintln!("Without --run: inspects, gates, plans and snapshots. Runs nothing.");
            eprintln!("With --run:    actually starts the installer, which writes to a prefix.");
            std::process::exit(2);
        }
    };

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

    // Stage 1.
    let inspection = match inspect::inspect(std::path::Path::new(&path)) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };

    println!("{}", "─".repeat(72));
    println!("{path}");
    println!("  engine    {}", inspection.engine.label());
    println!(
        "  identity  {} — {}",
        inspection.identity.vendor.as_deref().unwrap_or("?"),
        inspection.identity.product.as_deref().unwrap_or("?")
    );

    // Stage 2.
    let table = Table::shipped();
    let Some(matched) = table.match_on(
        inspection.engine,
        inspection.identity.vendor.as_deref().unwrap_or(""),
        inspection.identity.product.as_deref().unwrap_or(""),
    ) else {
        println!();
        println!("No vendor profile matches, so stage 4 has no rules to run it under.");
        if inspection.engine == Engine::Unknown {
            println!("The engine is unrecognised, which is why.");
        }
        std::process::exit(1);
    };
    let profile = matched.profile.clone();

    println!(
        "  vendor    {} [{}]",
        profile.name,
        match profile.origin {
            hangar_install::vendor::Origin::Shipped => "shipped",
            hangar_install::vendor::Origin::User => "yours",
        }
    );
    println!(
        "  runs in   the {} prefix, `{}`, cwd {}",
        profile.prefix.label(),
        profile.verb.as_str(),
        profile.cwd.label()
    );

    let companion_prefix = legacy_companion_dir()
        .map(|d| d.join("pfx"))
        .filter(|p| p.exists());

    // Which prefix this lands in decides whether a running sim matters, so it
    // has to be known before the gate rather than after it.
    let gate_target = match profile.prefix {
        Prefix::Msfs => Some(msfs.install.prefix.clone()),
        Prefix::Companion => companion_prefix.clone(),
    };

    // Stage 3.
    let report = gate::run(Request {
        profile: &profile,
        msfs_prefix: &msfs.install.prefix,
        companion_prefix: companion_prefix.as_deref(),
        payload_bytes: profile.payload_estimate(&inspection),
        sim_running: sim::is_running(),
        reaches_sim_content: gate_target
            .as_deref()
            .and_then(|p| gate::reaches_sim_content(p, &msfs.install.prefix)),
    });

    println!();
    println!("gate");
    for checked in &report.outcomes {
        let (mark, detail) = match &checked.outcome {
            Outcome::Pass(evidence) => ("ok  ", evidence.clone()),
            Outcome::Unchecked(why) => ("??  ", why.clone()),
            Outcome::Refuse(refusal) => ("NO  ", refusal.to_string()),
        };
        println!("  {mark}{:<22} {detail}", checked.label);
    }

    // Stage 4, the half that runs nothing: what the command would be.
    let Some(target_prefix) = gate_target.clone() else {
        println!();
        println!(
            "No {} prefix, so there is nothing to run in.",
            profile.prefix.label()
        );
        std::process::exit(1);
    };

    let proton = match ProtonEnv::for_msfs(&msfs, &steam) {
        Some(base) => match profile.prefix {
            Prefix::Msfs => base,
            Prefix::Companion => match legacy_companion_dir() {
                Some(dir) => base.companion(&dir),
                None => {
                    eprintln!("no companion directory");
                    std::process::exit(2);
                }
            },
        },
        None => {
            eprintln!("no compat tool is configured for MSFS, so nothing can be run");
            std::process::exit(2);
        }
    };

    let invocation = match Invocation::plan(&profile, &proton, std::path::Path::new(&path), None) {
        Ok(plan) => plan,
        Err(e) => {
            println!();
            println!("cannot plan the run: {e}");
            std::process::exit(1);
        }
    };

    println!();
    println!("invocation");
    println!("  {}", invocation.command_line());
    println!("  cwd  {}", invocation.cwd.display());
    for (k, v) in &invocation.env {
        println!("  env  {}={}", k.to_string_lossy(), v.to_string_lossy());
    }

    let signatures = Invocation::signatures(&profile);
    println!();
    println!("watching the console for {} strings", signatures.len());
    for sig in &signatures {
        println!("  {sig}");
    }

    // The "before" observation. Read-only either way — this is what makes the
    // difference detectable afterwards.
    let exe_xml_path = msfs.exe_xml();
    let places = Places {
        community: Some(msfs.community.as_path()),
        prefix: Some(&target_prefix),
        exe_xml: Some(&exe_xml_path),
    };

    let before = Snapshot::take(places);
    println!();
    println!("before");
    describe(&before);

    if !execute {
        println!();
        println!("Dry run. Nothing was executed and nothing was written.");
        println!("Add --run to actually start it.");
        return;
    }

    if !report.is_clear() {
        println!();
        println!("Refusing to run: the gate is not clear.");
        for refusal in report.refusals() {
            println!("  {refusal}");
        }
        std::process::exit(1);
    }

    println!();
    println!("{}", "─".repeat(72));
    println!("Running it. This writes to {}.", target_prefix.display());
    println!("Nothing will answer a dialog — Abort rolls back the whole install.");
    println!("{}", "─".repeat(72));

    // Repair first, then park for this run. The guard lives until the end of
    // `main`, so the stub goes back even if the run below exits early.
    for restored in shim::restore_stray(&target_prefix) {
        println!("Put back {} from an earlier run.", restored.display());
    }
    let parked = match profile.powershell {
        vendor::PowerShell::Keep => shim::Parked::default(),
        vendor::PowerShell::Park => match shim::park(&target_prefix) {
            Ok(parked) => {
                for path in parked.paths() {
                    println!("Moved Wine's PowerShell stub aside: {}", path.display());
                }
                parked
            }
            Err(e) => {
                println!("Could not move Wine's PowerShell stub aside: {e}");
                shim::Parked::default()
            }
        },
    };
    let shimmed: Vec<String> = parked
        .paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let echo: run::OnLine = Arc::new(|line: &str| println!("  │ {line}"));
    let ran = match run::run(&invocation, &signatures, Some(echo)).await {
        Ok(ran) => ran,
        Err(e) => {
            println!("{e}");
            std::process::exit(1);
        }
    };

    let after = Snapshot::take(places);
    let diff = Diff::between(&before, &after);

    println!();
    println!("after {:.1}s, exit {:?}", ran.after.as_secs_f32(), ran.code);
    println!("  {} lines, app output: {}", ran.log.len(), ran.spoke);

    println!();
    println!("verdict");
    match run::judge(&ran) {
        Verdict::BadConsole(hits) => {
            println!("  the console said it failed, whatever the exit code was:");
            for hit in hits {
                println!("    [{}] {}", hit.signature, hit.line);
            }
        }
        Verdict::Vanished { after } => println!(
            "  it was gone in {:.1}s without saying anything — a stub torn down mid-unpack \
             looks exactly like this",
            after.as_secs_f32()
        ),
        Verdict::Killed { .. } => println!("  it was killed by a signal rather than exiting"),
        Verdict::ExitCode(code) => println!("  it exited {code}"),
        Verdict::NoComplaint => {
            println!("  nothing in the run said it failed — which is not the same as success")
        }
    }

    println!();
    println!("what changed");
    report_change("Community", &diff.community, |p| {
        format!(
            "{}{}",
            p.name,
            if p.has_manifest {
                ""
            } else {
                "  (no manifest.json)"
            }
        )
    });
    report_change("prefix .exe", &diff.executables, |p| {
        p.display().to_string()
    });
    report_change("EXE.xml", &diff.addons, |a| {
        format!("{} → {}", a.name, a.path)
    });

    for (place, why) in diff.unknown() {
        println!("  {place}: could not be compared — {why}");
    }
    for note in &diff.notes {
        println!("  note: {note}");
    }

    // Stage 5: what all that adds up to.
    let classified = classify::classify(
        &diff,
        &run::judge(&ran),
        profile.produces,
        &msfs.install.prefix,
    );

    println!();
    println!("what it is");
    if classified.landed.is_empty() {
        println!("  nothing this stage knows how to file");
    }
    for item in &classified.landed {
        println!("  {:<14} {}", item.kind().label(), item.name());
        match item {
            classify::Landed::CompanionApp { exe, also } => {
                println!("      {}", exe.display());
                for other in also {
                    println!("      (also {})", other.display());
                }
            }
            classify::Landed::SimAddon {
                win_path, resolved, ..
            } => {
                println!("      {win_path}");
                match resolved {
                    Some(p) => println!("      resolves to {}", p.display()),
                    None => println!("      does NOT resolve in the MSFS prefix"),
                }
            }
            classify::Landed::Package { .. } => {}
        }
    }

    // Stage 6, when a package landed: is it all there?
    let verified = classified.landed.iter().find_map(|item| match item {
        classify::Landed::Package { name } => {
            let dir = msfs.community.join(name);
            verify::verify_package(&dir)
                .ok()
                .map(|report| record::Verified::of(name, &report))
        }
        _ => None,
    });

    if let Some(v) = &verified {
        println!();
        println!("verified");
        println!(
            "  {}  {}/{} present at the right size, {} orphan",
            v.package, v.verified, v.declared, v.orphans
        );
    }

    if !classified.concerns.is_empty() {
        println!();
        println!("concerns");
        for concern in &classified.concerns {
            match concern {
                classify::Concern::NothingArrived { expected } => println!(
                    "  nothing arrived, and this profile expects {}",
                    expected.label()
                ),
                classify::Concern::RunObjected { detail } => {
                    println!("  the run objected: {detail}")
                }
                classify::Concern::AddonPathUnreachable { name, win_path } => {
                    println!("  {name} registered {win_path}, which the sim's prefix cannot follow")
                }
                classify::Concern::Unobserved { place, why } => {
                    println!("  {place}: {why}")
                }
                classify::Concern::Removed { place, what } => {
                    println!("  {place}: {} removed — {}", what.len(), what.join(", "))
                }
            }
        }
    } else if classified.is_clean() {
        println!();
        println!("Something landed and nothing about it is worrying.");
    }

    // Stage 7: file it. None of this is recoverable by scanning the machine
    // afterwards, which is the whole reason a record exists.
    let entry = record::InstallRecord::of(
        &inspection,
        &profile,
        &invocation,
        &ran,
        &run::judge(&ran),
        &classified,
        verified,
        shimmed,
    );
    println!();
    match record::file_default(&entry) {
        Ok(path) => println!("filed as {}", path.display()),
        Err(e) => println!("could not file this install: {e}"),
    }
}

fn describe(snapshot: &Snapshot) {
    describe_one("Community", &snapshot.community);
    describe_one("prefix .exe", &snapshot.executables);
    describe_one("EXE.xml", &snapshot.addons);
    for note in &snapshot.notes {
        println!("  note: {note}");
    }
}

fn describe_one<T: Ord>(label: &str, seen: &Seen<BTreeSet<T>>) {
    match seen {
        Seen::Read(items) => println!("  {label:<12} {} entries", items.len()),
        Seen::Unreadable(why) => println!("  {label:<12} not observed — {why}"),
    }
}

fn report_change<T: Ord>(label: &str, change: &Change<T>, show: impl Fn(&T) -> String) {
    match change {
        Change::Unknown(why) => println!("  {label:<12} unknown — {why}"),
        Change::Delta { added, removed } if added.is_empty() && removed.is_empty() => {
            println!("  {label:<12} unchanged")
        }
        Change::Delta { added, removed } => {
            println!("  {label:<12} +{} -{}", added.len(), removed.len());
            for item in added {
                println!("      + {}", show(item));
            }
            for item in removed {
                println!("      - {}", show(item));
            }
        }
    }
}
