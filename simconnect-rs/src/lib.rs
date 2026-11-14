//! SimConnect over TCP for Microsoft Flight Simulator 2024 on Linux.
//!
//! MSFS runs under Proton; its SimConnect server is reachable over TCP, not the
//! Windows named pipe. This crate speaks that wire protocol directly, so a
//! native Linux process can read telemetry without a Windows SDK, a Wine
//! bridge, or an in-prefix helper.
//!
//! ```no_run
//! # async fn demo() {
//! use simconnect_tcp::{Client, Config};
//!
//! let client = Client::spawn(Config::default(), None);
//! let mut telemetry = client.telemetry();
//!
//! while telemetry.changed().await.is_ok() {
//!     let t = telemetry.borrow().clone();
//!     println!("{:.4}, {:.4} at {:.0} ft", t.latitude_deg, t.longitude_deg, t.altitude_ft);
//! }
//! # }
//! ```
//!
//! # Setup
//!
//! MSFS only opens the port when `SimConnect.xml` is present in its Proton
//! prefix. Without it the connection is refused and this client retries
//! forever — correct behaviour, but it looks like a bug.
//!
//! # No GUI dependency
//!
//! Telemetry is published on a [`tokio::sync::watch`] channel, with an optional
//! [`WakeFn`] for a UI that needs nudging into a repaint. That is the whole
//! integration surface, so the crate tests headlessly.
//!
//! # Things that are load-bearing
//!
//! Each has a test naming it:
//!
//! * **The offset-28 datum layout** — [`protocol::DATUM_OFFSET`].
//! * **One `define_id` per variable.** Bundling makes MSFS 2024 answer with
//!   exception 29 and then silently deliver nothing for the whole group, for
//!   the rest of the session. See [`simvars`].
//! * **The 280-byte handshake** with `"HK\0"` at offset 261 — [`protocol::build_open`].
//! * **Exception throttling**, so one bad definition cannot flood the log.
//! * **Staleness suppression.** The sim stops sending during a pause or a menu
//!   without closing the socket; [`Telemetry::is_stale`] is how a consumer
//!   avoids broadcasting a frozen position.

pub mod client;
pub mod connection;
pub mod error;
pub mod protocol;
pub mod simvars;
pub mod telemetry;

pub use client::{Client, Config, Status, WakeFn, DEFAULT_PORT};
pub use error::{Error, Result};
pub use simvars::{SimVarDef, SIMVARS};
pub use telemetry::{Datum, Telemetry, DEFAULT_STALE_AFTER};
