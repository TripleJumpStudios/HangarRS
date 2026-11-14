//! Search the real action catalogue, and list what shares a control.
//!
//! ```bash
//! cargo run -p hangar-bindings --example actions            # totals and contexts
//! cargo run -p hangar-bindings --example actions trim       # search
//! cargo run -p hangar-bindings --example actions trim unbound
//! cargo run -p hangar-bindings --example actions --shared   # controls on two actions
//! ```
//!
//! Read-only. This is the "not everything is in the settings menu" problem
//! made searchable: roughly two thousand actions per profile are in the file
//! and nowhere in the sim's UI.

use hangar_bindings as bindings;
use hangar_bindings::catalog::{Bound, Catalog, Query};

fn main() {
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

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--shared") {
        report_shared(&loaded);
        return;
    }

    let catalog = Catalog::from_profiles(loaded.iter().map(|(id, p)| (id.as_str(), p)));
    match args.first() {
        None => overview(&catalog, loaded.len()),
        Some(text) => {
            let bound = match args.get(1).map(String::as_str) {
                Some("unbound") => Bound::Never,
                Some("bound") => Bound::Only,
                _ => Bound::Any,
            };
            search(&catalog, text, bound);
        }
    }
}

fn overview(catalog: &Catalog, profiles: usize) {
    println!(
        "{} action rows over {} profiles — {} distinct action names, {} bound\n",
        catalog.len(),
        profiles,
        catalog.distinct_actions(),
        catalog.bound_count(),
    );
    let counts = catalog.counts_by_context(&Query::default());
    let bound_counts = catalog.counts_by_context(&Query {
        bound: Bound::Only,
        ..Default::default()
    });
    println!("{:<34}  {:>7}  {:>6}", "context", "actions", "bound");
    for (context, total) in counts {
        println!(
            "{context:<34}  {total:>7}  {:>6}",
            bound_counts.get(context).copied().unwrap_or(0)
        );
    }
}

fn search(catalog: &Catalog, text: &str, bound: Bound) {
    let query = Query {
        text: text.to_string(),
        bound,
        ..Default::default()
    };
    let hits = catalog.search(&query);
    let counts = catalog.counts_by_context(&query);

    println!("{} matches for {text:?}", hits.len());
    let where_ = counts
        .iter()
        .map(|(c, n)| format!("{c} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    if !where_.is_empty() {
        println!("{where_}\n");
    }

    for entry in hits.iter().take(60) {
        let d = bindings::describe(&entry.action);
        println!(
            "  {:<20}  {:<34}  {:<9}  {}",
            entry.context,
            d.title,
            d.kind.as_str(),
            entry.label()
        );
        println!("  {:<20}  {}", "", d.summary);
    }
    if hits.len() > 60 {
        println!("  … {} more", hits.len() - 60);
    }
}

fn report_shared(loaded: &[(String, bindings::Profile)]) {
    let mut total = 0usize;
    for (id, profile) in loaded {
        let shared = bindings::catalog::shared(id, profile);
        if shared.is_empty() {
            continue;
        }
        println!("\n{id}  {}", profile.friendly_name.name);
        for s in &shared {
            total += 1;
            println!("  {:<16}  {:<24}  {}", s.context, s.label, s.kind.as_str());
            for (action, slot) in &s.uses {
                println!("      {action}  ({})", slot.as_str());
            }
        }
    }
    println!("\n{total} shared control(s).");
    println!(
        "Shared is not the same as wrong: one lever ganged to two engines and Asobo's own\n\
         keyboard menu defaults both look like this."
    );
}
