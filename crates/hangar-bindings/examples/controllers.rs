//! Which profile belongs to which controller — and which profiles the sim is
//! probably ignoring.
//!
//! ```bash
//! cargo run -p hangar-bindings --example controllers
//! ```
//!
//! **Read-only, and deliberately so.** It reports; it never merges, deletes,
//! rewrites or picks: losing bindings to a tidy-up is far worse than living
//! with a duplicate, and only the sim knows which registration it reads.

use hangar_bindings as bindings;
use hangar_bindings::devices::Status;

fn main() {
    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("no Steam installation found: {e}");
            std::process::exit(2);
        }
    };

    let library = bindings::Library::discover(&steam);
    if library.is_empty() {
        eprintln!("no input profiles found");
        std::process::exit(2);
    }

    let controllers = hangar_core::input::controllers().unwrap_or_default();
    println!("Connected controllers ({}):", controllers.len());
    for device in &controllers {
        println!(
            "  {}  {:<52}  {}",
            device.usb_id(),
            device.tidy_name(),
            device.event_handler().unwrap_or("-")
        );
    }

    println!("\nProfiles by device:");
    for view in bindings::survey(&library, &controllers) {
        let mark = if view.status.needs_attention() {
            "!"
        } else {
            " "
        };
        println!(
            "\n{mark} {}  \u{2014}  product {} ({:#06x})  \u{2014}  {}",
            view.name,
            view.product_id,
            view.product_id,
            view.status.as_str()
        );
        if let Some(device) = &view.connected {
            println!(
                "    plugged in as {} ({})",
                device.usb_id(),
                device.tidy_name()
            );
        }

        for profile in &view.profiles {
            let shared = if profile.guid_is_shared {
                "   <- GUID also recorded for a different controller"
            } else {
                ""
            };
            println!(
                "    {:<26}  {:>3} bound  {}{}",
                profile.name, profile.bound, profile.guid, shared
            );
        }

        match view.status {
            Status::DuplicateRegistrations => {
                println!(
                    "    This controller is enrolled {} times. Only one is live; the sim reads",
                    view.registrations.len()
                );
                println!("    one of them and silently ignores the rest.");
                if let Some(busiest) = view.busiest() {
                    println!(
                        "    Most bindings: {:?} ({}). Likely the live one \u{2014} a guess, not a fact.",
                        busiest.name, busiest.bound
                    );
                }
            }
            Status::AlternateProfiles => {
                println!(
                    "    {} profiles for one registration. That is MSFS's own feature \u{2014} you",
                    view.profiles.len()
                );
                println!("    pick which is active in the sim. Nothing is wrong here.");
            }
            Status::NotConnected => {
                println!("    No connected controller reports this product id right now.");
            }
            Status::Keyboard | Status::Single => {}
        }
    }

    println!("\nNothing was changed. This command only reads.");
}
