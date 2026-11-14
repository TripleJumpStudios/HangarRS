//! Prove the write path end to end, on a scratch copy of the real profiles.
//!
//! ```bash
//! cargo run -p hangar-bindings --example edit_scratch
//! ```
//!
//! **The live `remote/` directory is opened read-only and never written.** The
//! profiles are copied to a scratch tree first, the edit is made there, and the
//! result is diffed and re-parsed: nothing writes to Steam-managed files
//! without explicit approval.
//!
//! It also reports what the real gate says right now — which, with Steam
//! running, is a refusal. That refusal is the feature.

use hangar_bindings as bindings;
use hangar_bindings::{Approval, Slot};

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

    // ── What the gate says about the live directory, without touching it ──
    println!("Live directory: {}", remote.display());
    let summaries = bindings::list(&remote).expect("list the profiles");
    let Some(first) = summaries.first() else {
        eprintln!("no profiles found");
        std::process::exit(2);
    };
    let profile = bindings::read(&first.path).expect("read a profile");

    match bindings::save::check_path(&profile, &first.path) {
        Ok(()) => println!("  gate: open — Steam is closed and the profile round-trips"),
        Err(refusal) => println!("  gate: REFUSED — {refusal}"),
    }

    // ── The real exercise, on a scratch copy ──
    let scratch = std::env::temp_dir().join(format!("hangar-edit-scratch-{}", std::process::id()));
    let remote_copy = scratch.join("remote");
    let store = scratch.join("snapshots");
    std::fs::create_dir_all(&remote_copy).expect("make a scratch directory");

    for summary in &summaries {
        std::fs::copy(&summary.path, remote_copy.join(&summary.id)).expect("copy a profile");
    }
    println!(
        "\nScratch copy of {} profiles at {}",
        summaries.len(),
        remote_copy.display()
    );

    let target = remote_copy.join(&first.id);
    let before = bindings::read(&target).expect("read the copy");

    // Find something bound to change, so the diff has content.
    let Some((context_name, action_name, old_label)) =
        before.device.bound().next().map(|(c, a)| {
            (
                c.name.clone(),
                a.name.clone(),
                a.primary.as_ref().map(|b| b.label()),
            )
        })
    else {
        println!("nothing bound in {} — nothing to demonstrate", first.id);
        let _ = std::fs::remove_dir_all(&scratch);
        return;
    };

    let mut edited = before.clone();
    let action = edited
        .device
        .contexts
        .iter_mut()
        .find(|c| c.name == context_name)
        .and_then(|c| c.action_mut(&action_name))
        .expect("the action is still there");
    action.bind(
        Slot::Primary,
        bindings::Binding::single(bindings::Key::new("Joystick Button 31", "30")),
    );

    println!(
        "\nEditing {context_name}/{action_name}\n  from {}\n  to   Joystick Button 31",
        old_label.unwrap_or_else(|| "nothing".into())
    );

    // The gate runs against the scratch file exactly as it would against the
    // real one — including the Steam check, which is why this may refuse.
    let approval = Approval::given(format!("rebind {action_name} (scratch demonstration)"));
    match bindings::save::save(&edited, &target, &store, &approval) {
        Ok(Ok(saved)) => {
            println!(
                "\nWrote {} bytes\n  snapshot: {}",
                saved.bytes,
                saved.snapshot_dir.display()
            );
            let after = bindings::read(&target).expect("re-read what was written");
            let diff = bindings::compare(&before, &after);
            println!("  diff: {}", diff.summary());
            for change in &diff.changes {
                println!("    {change}");
            }
            assert_eq!(
                diff.len(),
                1,
                "an edit of one binding should produce exactly one change"
            );
        }
        Ok(Err(refusal)) => println!("\nRefused: {refusal}"),
        Err(e) => {
            eprintln!("\nfailed: {e}");
            let _ = std::fs::remove_dir_all(&scratch);
            std::process::exit(1);
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
    println!("\nScratch removed. The live directory was never written.");
}
