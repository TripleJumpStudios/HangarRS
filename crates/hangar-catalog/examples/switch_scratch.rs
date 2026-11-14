//! Exercise a profile switch on a scratch copy of the real Community folder.
//!
//! `cargo run -p hangar-catalog --example switch_scratch`
//!
//! The live Community folder is **read only** here:
//! the scratch tree is built from the real package *names* and `manifest.json`
//! files, and nothing else is copied — so it takes milliseconds instead of
//! moving 16.7 GB, while still exercising the real names and manifests.
//!
//! Everything this writes lives under one temporary directory, which it removes
//! on the way out.

use hangar_catalog::switcher::Switcher;
use hangar_catalog::{apply_diff, plan_diff, Profile};
use hangar_msfs::MsfsEnv;
use hangar_steam::SteamEnv;
use std::collections::BTreeSet;

fn main() {
    let steam = SteamEnv::discover().expect("Steam");
    let env = MsfsEnv::discover(&steam).expect("MSFS");

    let scratch = std::env::temp_dir().join(format!("hangar-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let community = scratch.join("Community2024");
    std::fs::create_dir_all(&community).unwrap();

    // Copy names and manifests only. The live folder is opened read-only.
    let mut copied = Vec::new();
    for entry in std::fs::read_dir(&env.community).unwrap().flatten() {
        let manifest = entry.path().join("manifest.json");
        if !manifest.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let dest = community.join(&name);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(&manifest, dest.join("manifest.json")).unwrap();
        copied.push(name);
    }
    copied.sort();

    println!(
        "scratch copy of {} \u{2014} {} packages, manifests only",
        env.community.display(),
        copied.len()
    );
    println!("  at {}\n", community.display());

    let switcher = Switcher::new(&community);
    let all: BTreeSet<String> = copied.iter().cloned().collect();

    // A profile holding roughly half of them, chosen deterministically.
    let vfr: BTreeSet<String> = copied.iter().step_by(2).cloned().collect();
    let profile = Profile::from_enabled("VFR Only", vfr.clone());

    let (on, off) = plan_diff(&all, &profile.packages);
    println!(
        "switching to {:?}: {} to enable, {} to disable",
        profile.name,
        on.len(),
        off.len()
    );

    let started = std::time::Instant::now();
    let report = apply_diff(&switcher, &all, &profile.packages);
    let elapsed = started.elapsed();

    println!("  {} in {elapsed:.2?}", report.summary());
    assert!(report.is_clean(), "failures: {:?}", report.failed);
    assert_eq!(switcher.enabled_names(), vfr, "Community holds the profile");
    println!(
        "  Community2024: {} packages, disabled store: {}",
        switcher.enabled_names().len(),
        switcher.disabled_names().len()
    );

    // Switching back must restore exactly what was there.
    let started = std::time::Instant::now();
    let back = apply_diff(&switcher, &vfr, &all);
    println!(
        "switching back: {} in {:.2?}",
        back.summary(),
        started.elapsed()
    );
    assert!(back.is_clean(), "failures: {:?}", back.failed);
    assert_eq!(switcher.enabled_names(), all, "everything is restored");
    assert!(
        switcher.disabled_names().is_empty(),
        "the store is empty again"
    );

    // And a no-op switch must touch nothing at all.
    let noop = apply_diff(&switcher, &all, &all);
    assert_eq!(noop.touched(), 0);
    println!("re-applying the same set: {} moves", noop.touched());

    std::fs::remove_dir_all(&scratch).unwrap();
    println!("\nscratch tree removed. The live Community folder was never written to.");
}
