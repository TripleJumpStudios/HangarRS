//! `efbconnect` — the EFB bridge, without HangarRS.
//!
//! ```bash
//! efbconnect                        # broadcast to Garmin Pilot / ForeFlight
//! efbconnect --lnm                  # also serve LittleNavMap
//! efbconnect --rate 10 --name Bravo
//! ```
//!
//! Standalone on purpose. **HangarRS is never required to fly**, and a bridge
//! is the part of it most likely to end up on a machine that has no reason to
//! run a GUI — a spare laptop feeding an iPad, or a headless box beside the
//! sim. It shares every line of protocol code with the application and none of
//! its dependencies.
//!
//! It connects to the sim itself, which the in-application bridges do not:
//! inside HangarRS there is one telemetry link and the bridges read snapshots
//! of it, because two clients would show up as two connections in the sim.
//! Run this *and* HangarRS's own bridge and you get exactly that, so run one.

use std::sync::Arc;

use hangar_bridges::serve::{self, Source};
use hangar_bridges::{efb, lnm};
use simconnect_tcp::{Client, Config};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("usage: efbconnect [--lnm] [--rate HZ] [--name NAME] [--host HOST]");
        eprintln!();
        eprintln!(
            "  --lnm         also serve LittleNavMap on {}",
            lnm::DEFAULT_PORT
        );
        eprintln!("  --rate HZ     frames per second (default 5)");
        eprintln!("  --name NAME   the source name both apps display");
        eprintln!("  --host HOST   where the sim's SimConnect TCP server is");
        return;
    }

    let value = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let rate: u32 = value("--rate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RATE_HZ);
    let name = value("--name").unwrap_or_else(|| "HangarRS".into());
    let host = value("--host").unwrap_or_else(|| "127.0.0.1".into());
    let with_lnm = args.iter().any(|a| a == "--lnm");

    let client = Client::spawn(
        Config {
            host,
            app_name: "efbconnect".into(),
            ..Default::default()
        },
        None,
    );

    // The bridges read the latest frame rather than consuming a stream, so a
    // slow bridge can never hold the link up or make it lag behind the sim.
    let telemetry = client.telemetry();
    let source: Source = Arc::new(move || Some(telemetry.borrow().clone()));

    let mut status = client.status();
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            tracing::info!("sim link: {:?}", *status.borrow());
        }
    });

    tracing::info!(
        "broadcasting XGPS/XATT on {} at {rate} Hz as {name:?}",
        efb::DEFAULT_PORT
    );
    if with_lnm {
        let source = Arc::clone(&source);
        tokio::spawn(async move {
            if let Err(e) = serve::lnm_server(lnm::DEFAULT_PORT, source, rate).await {
                tracing::error!("LittleNavMap bridge stopped: {e}");
            }
        });
    }

    tokio::select! {
        result = serve::efb_broadcast(efb::DEFAULT_PORT, name, source, rate) => {
            if let Err(e) = result {
                tracing::error!("EFB bridge stopped: {e}");
            }
        }
        // Ctrl-C, so a bridge left running in a terminal stops when told
        // rather than needing to be killed.
        _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
    }
}

/// Five a second.
///
/// An EFB redraws its map far slower than the sim produces frames, and every
/// datagram is a broadcast every device on the network has to look at. Thirty
/// hertz would be six times the traffic for no visible difference.
const DEFAULT_RATE_HZ: u32 = 5;
