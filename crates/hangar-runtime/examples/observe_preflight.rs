//! Watch a preflight happen, without taking part in it.
//!
//! ```bash
//! cargo run -p hangar-runtime --example observe_preflight
//! ```
//!
//! **Launches nothing and changes nothing.** It reads the same things the
//! Preflight tab reads — the readiness probe, `/proc` for the sim, and the
//! SimConnect port — and prints a line whenever any of them changes. Useful for
//! watching a real run and confirming the gate opens where it should.
//!
//! Run it *before* starting the preflight: the readiness watch records the log's
//! length when it begins, exactly as a real flight does, so anything written
//! before that belongs to a previous run.

use hangar_runtime::readiness::{self, Watch};
use hangar_runtime::sim;
use std::time::{Duration, Instant};

fn main() {
    let home = std::env::var("HOME").expect("HOME");
    let roaming = std::path::PathBuf::from(home).join(
        ".local/share/msfs-manager/companion-prefix/pfx/drive_c/users/steamuser/AppData/Roaming",
    );

    let probe = readiness::probe_for("Project Airports").expect("a shipped probe");
    let watch = Watch::begin("Project Airports", probe, &roaming);

    println!("Observing. Nothing here starts or stops anything.");
    if let Some(ago) = watch.since_last_sync() {
        println!(
            "  Project Airports last synced {:.0} minutes ago",
            ago.as_secs() as f64 / 60.0
        );
    }
    println!(
        "  grace window {}s, from the app's first log line",
        readiness::SYNC_GRACE.as_secs()
    );
    println!(
        "\n{:>8}  {:<12} {:<10} Project Airports",
        "elapsed", "sim", "simconnect"
    );

    let start = Instant::now();
    let mut last = String::new();

    loop {
        let sim_up = sim::is_running();
        let port = simconnect_open();
        let state = watch.poll();

        let line = format!(
            "{:<12} {:<10} {}{}",
            if sim_up { "running" } else { "-" },
            if port { "open" } else { "-" },
            if state.is_ready() { "READY  " } else { "" },
            state.describe()
        );

        // Only speak when something changes, so the transitions stand out.
        if line != last {
            println!("{:>7.0}s  {line}", start.elapsed().as_secs_f32());
            last = line;
        }

        std::thread::sleep(Duration::from_millis(400));
    }
}

/// Is anything listening on the SimConnect port?
///
/// A weaker signal than the handshake the Preflight tab waits on — the sim
/// accepts the socket during its loading screen — but it is the earliest sign
/// the sim is getting somewhere, which is what makes it worth watching.
fn simconnect_open() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:5557".parse().expect("a valid address"),
        Duration::from_millis(200),
    )
    .is_ok()
}
