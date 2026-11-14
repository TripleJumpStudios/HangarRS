//! Check the keyboard table against the real keyboard profiles.
//!
//! ```bash
//! cargo run -p hangar-bindings --example keys
//! ```
//!
//! Read-only. Every `<KEY>` in every keyboard profile (`GUID="{0}"`) must be a
//! pair `keyboard::OBSERVED` already has, and every chord must use a modifier
//! set and order `keyboard::CHORD_ORDERS` allows. Exits 1 if not: a new pair
//! in the files means the table is out of date, and the fix is to add the
//! observed pair, never to compute one.

use hangar_bindings as bindings;
use hangar_bindings::keyboard::{self, Modifier};
use std::collections::BTreeMap;

fn main() {
    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("no Steam installation found: {e}");
            std::process::exit(2);
        }
    };
    let library = bindings::Library::discover(&steam);

    let mut pairs: BTreeMap<(u32, String), usize> = BTreeMap::new();
    let mut shapes: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut profiles = 0;

    for loaded in &library.profiles {
        if loaded.profile.device.guid != bindings::devices::KEYBOARD_GUID {
            continue;
        }
        profiles += 1;
        for (context, action) in loaded.profile.device.bound() {
            for (slot, binding) in action.bindings() {
                let mut codes = Vec::new();
                for key in &binding.keys {
                    let Some(code) = key.code_num().and_then(|c| u32::try_from(c).ok()) else {
                        problems.push(format!(
                            "{}/{}: code {:?}",
                            context.name, action.name, key.code
                        ));
                        continue;
                    };
                    *pairs.entry((code, key.information.clone())).or_default() += 1;
                    codes.push(code);
                }
                // Modifiers in front of a final key: which set, in what order.
                if codes.len() > 1 {
                    let (last, mods) = codes.split_last().unwrap();
                    let names: Vec<String> = mods.iter().map(|c| c.to_string()).collect();
                    *shapes.entry(names.clone()).or_default() += 1;
                    if (160..=165).contains(last) {
                        problems.push(format!(
                            "{}/{} {}: a chord ends in a modifier",
                            context.name,
                            action.name,
                            slot.as_str()
                        ));
                    }
                    let as_mods: Option<Vec<Modifier>> =
                        mods.iter().map(|c| modifier(*c)).collect();
                    let allowed = as_mods.as_ref().is_some_and(|m| {
                        keyboard::CHORD_ORDERS
                            .iter()
                            .any(|order| order == &m.as_slice())
                    });
                    if !allowed {
                        problems.push(format!(
                            "{}/{} {}: modifiers {:?} are not an allowed order",
                            context.name,
                            action.name,
                            slot.as_str(),
                            mods
                        ));
                    }
                }
            }
        }
    }

    println!("{profiles} keyboard profile(s)\n");
    println!("{:>6}  {:<14} {:>6}  table", "code", "label", "seen");
    for ((code, label), seen) in &pairs {
        let table = keyboard::label(*code);
        let verdict = match table {
            Some(t) if t == label => "ok",
            Some(_) => "LABEL DIFFERS",
            None => "MISSING",
        };
        if verdict != "ok" {
            problems.push(format!("code {code} {label:?}: {verdict}"));
        }
        println!(
            "{code:>6}  {:<14} {seen:>6}  {verdict}",
            format!("{label:?}")
        );
    }
    println!("\nmodifier codes in front of a key (left to right), slots:");
    for (shape, seen) in &shapes {
        println!("  [{}] {seen}", shape.join(" "));
    }
    let unused: Vec<u32> = keyboard::OBSERVED
        .iter()
        .map(|(c, _)| *c)
        .filter(|c| !pairs.keys().any(|(seen, _)| seen == c))
        .collect();
    if !unused.is_empty() {
        println!("\nin the table but in no profile now: {unused:?}");
    }

    if problems.is_empty() {
        println!(
            "\n{} distinct pairs, all in the table; every chord in an allowed order.",
            pairs.len()
        );
    } else {
        println!();
        for p in &problems {
            println!("PROBLEM  {p}");
        }
        std::process::exit(1);
    }
}

/// A left-hand modifier code, the only kind observed in a chord.
fn modifier(code: u32) -> Option<Modifier> {
    match code {
        160 => Some(Modifier::Shift),
        162 => Some(Modifier::Control),
        164 => Some(Modifier::Alt),
        _ => None,
    }
}
