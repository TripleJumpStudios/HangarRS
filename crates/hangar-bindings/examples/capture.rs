//! Check the joystick mapping against the real hardware, and capture a control.
//!
//! ```bash
//! cargo run -p hangar-bindings --example capture         # cross-check, no input needed
//! cargo run -p hangar-bindings --example capture live    # press things for 20 seconds
//! ```
//!
//! Read-only. It opens `/dev/input/event*` for reading and writes nothing
//! anywhere.
//!
//! The cross-check is the part that runs unattended: every control the real
//! profiles already bind must exist on the device those profiles are for. If a
//! profile binds `Joystick Button 75` and the stick only advertises 40 buttons,
//! the button-order assumption is wrong and this says so.

use hangar_bindings as bindings;
use hangar_bindings::joystick::Control;
use std::time::{Duration, Instant};

fn main() {
    let live = std::env::args().nth(1).as_deref() == Some("live");

    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("no Steam installation found: {e}");
            std::process::exit(2);
        }
    };
    let library = bindings::Library::discover(&steam);
    let devices = hangar_core::input::controllers().unwrap_or_default();

    if cross_check(&library, &devices) && live {
        capture_loop(&devices);
    }
}

/// Every bound control must exist on the device its profile is for.
fn cross_check(library: &bindings::Library, devices: &[hangar_core::input::InputDevice]) -> bool {
    println!("Cross-checking bound controls against the hardware\n");
    let mut all_ok = true;

    for view in bindings::survey(library, devices) {
        let Some(device) = &view.connected else {
            continue;
        };
        let buttons = device.buttons().len();
        let axes = device.axes().iter().filter(|c| **c < 16).count();

        println!(
            "{}\n  advertises {buttons} buttons, {axes} stick axes",
            device.tidy_name()
        );

        for profile_ref in &view.profiles {
            let Some(loaded) = library.get(&profile_ref.id) else {
                continue;
            };
            let listed_axes = loaded.profile.device.axes().len();
            let mut highest_button = 0u32;
            let mut used_axes = Vec::new();
            let mut unparsed = Vec::new();

            for (_, action) in loaded.profile.device.bound() {
                for (_, binding) in action.bindings() {
                    for key in &binding.keys {
                        match Control::from_label(&key.information) {
                            Some(Control::Button(n)) => highest_button = highest_button.max(n),
                            Some(control @ Control::Axis { .. }) => {
                                if !used_axes.contains(&control) {
                                    used_axes.push(control);
                                }
                            }
                            None => unparsed.push(key.information.clone()),
                        }
                        // The code in the file must match what we would write.
                        if let Some(control) = Control::from_label(&key.information) {
                            let expected = control.code().to_string();
                            if key.code != expected {
                                println!(
                                    "    MISMATCH {:?} is code {} in the file, we would write {}",
                                    key.information, key.code, expected
                                );
                                all_ok = false;
                            }
                        }
                    }
                }
            }

            print!("    {:<32}", profile_ref.name);
            if highest_button > 0 {
                let ok = highest_button as usize <= buttons;
                all_ok &= ok;
                print!(
                    "  highest button {highest_button} of {buttons} {}",
                    if ok { "OK" } else { "TOO HIGH" }
                );
            }
            if !used_axes.is_empty() {
                print!("  axes used {}", used_axes.len());
            }
            let axes_ok = listed_axes == axes || listed_axes == 0;
            all_ok &= axes_ok;
            print!(
                "  profile lists {listed_axes} axes {}",
                if axes_ok { "OK" } else { "DIFFERS" }
            );
            println!();

            for label in unparsed {
                println!("    unparsed label {label:?}");
                all_ok = false;
            }
        }
        println!();
    }

    if all_ok {
        println!("Everything the profiles bind exists on the hardware, and every code in the");
        println!("files matches what this mapping would write.\n");
    } else {
        println!(
            "Something does not line up \u{2014} see above. Do not write bindings until it does.\n"
        );
    }
    all_ok
}

fn capture_loop(devices: &[hangar_core::input::InputDevice]) {
    let (mut capture, problems) = bindings::Capture::start(devices);
    for (name, reason) in &problems {
        println!("could not open {name}: {reason}");
    }
    if capture.watching() == 0 {
        println!("no controllers could be opened");
        return;
    }

    println!(
        "Watching {} controller(s) for 20 seconds. Press buttons and move axes.\n",
        capture.watching()
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen: Vec<String> = Vec::new();

    while Instant::now() < deadline {
        if let Some(captured) = capture.poll() {
            let line = format!(
                "{:<24}  {:<28}  {}",
                captured
                    .device
                    .tidy_name()
                    .chars()
                    .take(24)
                    .collect::<String>(),
                captured.describe(),
                captured.confidence.as_str()
            );
            if !seen.contains(&line) {
                println!("  {line}");
                seen.push(line);
            }
        }
        if let Some(unmapped) = capture.last_unmapped.take() {
            println!("  {} \u{2014} {}", unmapped.device_name, unmapped.reason());
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    println!(
        "\n{} distinct control(s) seen. Nothing was written.",
        seen.len()
    );
}
