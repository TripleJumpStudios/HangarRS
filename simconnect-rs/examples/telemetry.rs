//! Print live telemetry from a running MSFS 2024.
//!
//! `cargo run --example telemetry [seconds]`
//!
//! Read-only: it opens a SimConnect session, subscribes to the SimVars in
//! `simvars::SIMVARS`, prints what arrives, and exits. Nothing is sent to the
//! sim beyond the subscriptions themselves.

use simconnect_tcp::{Client, Config, Status};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "simconnect_tcp=info".into()),
        )
        .init();

    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let config = Config {
        app_name: "hangar-rs-probe".into(),
        ..Default::default()
    };
    println!("connecting to {} for {secs}s…", config.addr());

    let client = Client::spawn(config, None);
    let mut status = client.status();
    let deadline = Instant::now() + Duration::from_secs(secs);

    // Wait for the handshake, or give up.
    loop {
        if let Status::Connected {
            sim_name,
            sim_version,
        } = &*status.borrow_and_update()
        {
            println!("connected: {sim_name} {sim_version}\n");
            break;
        }
        if Instant::now() >= deadline {
            eprintln!("never connected — is MSFS running with SimConnect.xml in the prefix?");
            std::process::exit(1);
        }
        if tokio::time::timeout(Duration::from_millis(500), status.changed())
            .await
            .is_err()
        {
            continue;
        }
    }

    let mut frames = 0u32;
    let mut telemetry = client.telemetry();
    while Instant::now() < deadline {
        if tokio::time::timeout(Duration::from_millis(500), telemetry.changed())
            .await
            .is_err()
        {
            continue;
        }
        frames += 1;
        // One line a second is plenty; the sim sends per visual frame.
        if frames % 60 != 1 {
            continue;
        }
        let t = telemetry.borrow_and_update().clone();
        println!(
            "{:<28} {:>10.5},{:>11.5}  {:>7.0} ft  {:>5.0} kt IAS  {:>5.0} kt GS\n\
             {:<28} hdg true {:>5.1}  track {:>5.1}  (delta {:>+5.1})  vs {:>+6.0} fpm  {}",
            t.title,
            t.latitude_deg,
            t.longitude_deg,
            t.altitude_ft,
            t.airspeed_kt,
            t.ground_speed_kt,
            "",
            t.heading_true_deg,
            t.ground_track_deg,
            angle_delta(t.ground_track_deg, t.heading_true_deg),
            t.vertical_speed_fpm,
            if t.on_ground { "on ground" } else { "airborne" },
        );
    }

    let t = client.latest();
    println!("\n{frames} updates in {secs}s");
    println!(
        "stale? {}  (age {:?})",
        t.is_stale(Duration::from_secs(5)),
        t.age()
    );
    println!(
        "identity: title={:?} model={:?} id={:?} type={:?}",
        t.title, t.atc_model, t.atc_id, t.atc_type
    );
    println!(
        "atmosphere: wind {:.0} kt from {:.0}deg, OAT {:.0}C, QNH {:.0} mb",
        t.wind_velocity_kts, t.wind_direction_deg, t.ambient_temp_c, t.sea_level_pressure_mbar
    );
    println!(
        "aircraft: {} engine(s) type {}, fuel {:.0} gal, weight {:.0} lb",
        t.num_engines, t.engine_type, t.fuel_total_qty_gal, t.total_weight_lbs
    );
}

/// Signed smallest angle from `b` to `a`, in degrees.
fn angle_delta(a: f64, b: f64) -> f64 {
    let mut d = a - b;
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    d
}
