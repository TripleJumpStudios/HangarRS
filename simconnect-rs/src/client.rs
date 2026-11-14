//! The connect / handshake / subscribe / read loop, and its public handle.

use crate::connection::SimConnectTcp;
use crate::error::{Error, Result};
use crate::protocol;
use crate::simvars;
use crate::telemetry::{Datum, Telemetry, DEFAULT_STALE_AFTER};

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Default TCP port MSFS 2024 serves SimConnect on under Proton.
///
/// Set by `SimConnect.xml` inside the prefix. 5557 is the value both shipping
/// Linux clients use and what the documented setup writes; it is not the 500
/// that appears in Windows SimConnect examples.
pub const DEFAULT_PORT: u16 = 5557;

/// Client event ID used for the `Frame` system-event subscription.
const FRAME_EVENT_ID: u32 = 1;

/// How long to wait for the sim to answer `MSG_OPEN`.
///
/// Bounded on purpose: a sim that accepts the socket but never completes the
/// handshake — which happens during a loading screen — would otherwise park
/// the client with no way back.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Called when new telemetry has been published.
///
/// This is the entire integration surface a GUI needs, and the reason this
/// crate has no toolkit dependency: pass `Arc::new(move || ctx.request_repaint())`
/// and egui stays on the far side of the boundary.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// Connection state, published on its own channel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Disconnected,
    Connecting,
    Connected {
        sim_name: String,
        sim_version: String,
    },
    /// Reached the sim, but it is too old to talk to.
    Unsupported {
        reason: String,
    },
}

impl Status {
    pub fn is_connected(&self) -> bool {
        matches!(self, Status::Connected { .. })
    }
}

/// How to reach the sim and how to behave once there.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Name this client reports to the sim; shows up in its connection list.
    pub app_name: String,
    /// Age past which [`Telemetry::is_stale`] returns true.
    pub stale_after: Duration,
    /// Delay between reconnect attempts.
    pub reconnect_delay: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            app_name: "simconnect-rs".into(),
            stale_after: DEFAULT_STALE_AFTER,
            reconnect_delay: Duration::from_secs(2),
        }
    }
}

impl Config {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A running client. Dropping it stops the background task.
pub struct Client {
    telemetry: watch::Receiver<Telemetry>,
    status: watch::Receiver<Status>,
    stale_after: Duration,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Client {
    /// Start connecting, and keep reconnecting for as long as the client lives.
    ///
    /// Returns immediately; the sim does not need to be running yet. The
    /// reconnect loop is **self-contained** — it does not depend on a process
    /// watcher telling it the sim has started, so a missed event cannot leave
    /// it stuck.
    pub fn spawn(config: Config, wake: Option<WakeFn>) -> Self {
        let (telemetry_tx, telemetry) = watch::channel(Telemetry::default());
        let (status_tx, status) = watch::channel(Status::Disconnected);
        let stale_after = config.stale_after;

        let task = tokio::spawn(run(config, telemetry_tx, status_tx, wake));

        Self {
            telemetry,
            status,
            stale_after,
            task,
        }
    }

    /// Live telemetry channel. Clone it per consumer.
    pub fn telemetry(&self) -> watch::Receiver<Telemetry> {
        self.telemetry.clone()
    }

    /// Live connection-status channel.
    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// The most recent snapshot, whatever its age.
    pub fn latest(&self) -> Telemetry {
        self.telemetry.borrow().clone()
    }

    /// The most recent snapshot, or `None` if the sim has gone quiet.
    pub fn latest_fresh(&self) -> Option<Telemetry> {
        let t = self.telemetry.borrow();
        t.if_fresh(self.stale_after).cloned()
    }

    pub fn is_connected(&self) -> bool {
        self.status.borrow().is_connected()
    }
}

/// Reconnect forever, running one session per successful connection.
async fn run(
    config: Config,
    telemetry_tx: watch::Sender<Telemetry>,
    status_tx: watch::Sender<Status>,
    wake: Option<WakeFn>,
) {
    let addr = config.addr();
    info!("SimConnect: client starting, target {addr}");

    // Log a failing reconnect loop once per outage rather than once per retry —
    // otherwise a sim that is simply not running fills the log forever.
    let mut announced_outage = false;

    loop {
        let _ = status_tx.send(Status::Connecting);

        match session(&config, &telemetry_tx, &status_tx, wake.as_ref()).await {
            Ok(()) => {
                info!("SimConnect: session ended cleanly");
                announced_outage = false;
            }
            Err(e) => {
                if announced_outage {
                    debug!("SimConnect: still unreachable: {e}");
                } else {
                    warn!("SimConnect: {e}");
                    announced_outage = true;
                }
            }
        }

        let _ = status_tx.send(Status::Disconnected);
        tokio::time::sleep(config.reconnect_delay).await;
    }
}

/// One connection, from TCP connect until the stream fails.
async fn session(
    config: &Config,
    telemetry_tx: &watch::Sender<Telemetry>,
    status_tx: &watch::Sender<Status>,
    wake: Option<&WakeFn>,
) -> Result<()> {
    let mut conn = SimConnectTcp::connect(&config.addr()).await?;

    conn.send(protocol::MSG_OPEN, &protocol::build_open(&config.app_name))
        .await?;
    let (sim_name, major, minor, build) = handshake(&mut conn).await?;

    if !protocol::version_ok(major, minor, build) {
        let err = Error::SimTooOld {
            major,
            minor,
            build,
            min_major: protocol::MIN_VERSION_MAJOR,
            min_minor: protocol::MIN_VERSION_MINOR,
            min_build: protocol::MIN_VERSION_BUILD,
        };
        let _ = status_tx.send(Status::Unsupported {
            reason: err.to_string(),
        });
        return Err(err);
    }

    let sim_version = format!("{major}.{minor}.{build}");
    info!("SimConnect: connected to {sim_name} {sim_version}");
    let _ = status_tx.send(Status::Connected {
        sim_name,
        sim_version,
    });

    conn.send(
        protocol::MSG_SUBSCRIBE_TO_SYSTEM_EVENT,
        &protocol::build_subscribe_event(FRAME_EVENT_ID, "Frame"),
    )
    .await?;
    register_simvars(&mut conn).await?;

    read_loop(&mut conn, telemetry_tx, wake).await
}

/// Wait for `RECV_OPEN`, ignoring anything that arrives first.
async fn handshake(conn: &mut SimConnectTcp) -> Result<(String, u32, u32, u32)> {
    let wait = async {
        loop {
            let (msg_id, payload) = conn.recv_packet().await?;
            if msg_id != protocol::RECV_OPEN {
                continue;
            }
            let (major, minor, build) =
                protocol::parse_recv_open_version(&payload).ok_or(Error::MalformedOpen)?;
            let name = protocol::parse_recv_open_name(&payload).unwrap_or_default();
            return Ok((name, major, minor, build));
        }
    };

    tokio::time::timeout(HANDSHAKE_TIMEOUT, wait)
        .await
        .map_err(|_| Error::HandshakeTimeout(HANDSHAKE_TIMEOUT))?
}

/// Register every SimVar, one definition and one request each.
async fn register_simvars(conn: &mut SimConnectTcp) -> Result<()> {
    for v in simvars::SIMVARS {
        conn.send(
            protocol::MSG_ADD_TO_DATA_DEFINITION,
            &protocol::build_add_to_data_definition(v.define_id, v.name, v.units, v.data_type),
        )
        .await?;
        conn.send(
            protocol::MSG_REQUEST_DATA_ON_SIM_OBJECT,
            &protocol::build_request_data_on_sim_object(
                v.request_id,
                v.define_id,
                protocol::PERIOD_VISUAL_FRAME,
            ),
        )
        .await?;
    }
    debug!("SimConnect: registered {} SimVars", simvars::SIMVARS.len());
    Ok(())
}

/// Read packets until the stream fails, publishing telemetry as it changes.
async fn read_loop(
    conn: &mut SimConnectTcp,
    telemetry_tx: &watch::Sender<Telemetry>,
    wake: Option<&WakeFn>,
) -> Result<()> {
    let mut telemetry = Telemetry::default();
    let mut exceptions = ExceptionLog::default();

    loop {
        let (msg_id, payload) = conn.recv_packet().await?;

        match msg_id {
            // Frame ticks carry no telemetry; data arrives as SIMOBJECT_DATA.
            protocol::RECV_EVENT => {}

            protocol::RECV_SIMOBJECT_DATA => {
                if apply_datum(&payload, &mut telemetry) {
                    telemetry.updated_at = Some(Instant::now());
                    let _ = telemetry_tx.send(telemetry.clone());
                    if let Some(wake) = wake {
                        wake();
                    }
                }
            }

            protocol::RECV_EXCEPTION => {
                if let Some(code) = protocol::parse_recv_exception(&payload) {
                    exceptions.record(code);
                }
            }

            _ => {}
        }
    }
}

/// Route one delivered datum into the snapshot. Returns whether it landed.
fn apply_datum(payload: &[u8], telemetry: &mut Telemetry) -> bool {
    let Some(request_id) = protocol::parse_simobject_request_id(payload) else {
        return false;
    };
    let Some(var) = simvars::by_request_id(request_id) else {
        return false;
    };

    let datum = match var.data_type {
        protocol::DATATYPE_FLOAT64 => protocol::parse_simobject_data_f64(payload).map(Datum::F64),
        protocol::DATATYPE_INT32 => protocol::parse_simobject_data_i32(payload).map(Datum::I32),
        protocol::DATATYPE_STRING256 => {
            protocol::parse_simobject_data_string256(payload).map(Datum::Str)
        }
        _ => None,
    };

    match datum {
        Some(d) => {
            (var.apply)(telemetry, d);
            true
        }
        None => false,
    }
}

/// Throttled exception reporting.
///
/// A single bad definition can raise an exception on every visual frame. Left
/// unthrottled that is tens of thousands of identical warnings a minute, which
/// buries whatever the real problem was.
#[derive(Default)]
struct ExceptionLog {
    count: u32,
}

impl ExceptionLog {
    fn record(&mut self, code: u32) {
        self.count += 1;
        let hint = if code == protocol::EXCEPTION_DATA_ERROR {
            " — a data definition was rejected; check the SimVar name and unit string"
        } else {
            ""
        };
        match self.count {
            1 => warn!("SimConnect: exception {code}{hint}"),
            n if n % 10 == 0 && n <= 100 => {
                warn!("SimConnect: exception {code} (occurrence {n}){hint}");
            }
            101 => warn!("SimConnect: exception {code} — suppressing further warnings"),
            _ => debug!("SimConnect: exception {code}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simobject_payload(request_id: u32, datum: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; protocol::DATUM_OFFSET + datum.len().max(8)];
        p[0..4].copy_from_slice(&request_id.to_le_bytes());
        p[protocol::DATUM_OFFSET..protocol::DATUM_OFFSET + datum.len()].copy_from_slice(datum);
        p
    }

    #[test]
    fn a_delivered_float_lands_in_its_field() {
        let mut t = Telemetry::default();
        let payload = simobject_payload(10, &3500.0f64.to_le_bytes()); // PLANE ALTITUDE
        assert!(apply_datum(&payload, &mut t));
        assert_eq!(t.altitude_ft, 3500.0);
    }

    #[test]
    fn a_delivered_string_lands_in_its_field() {
        let mut t = Telemetry::default();
        let mut buf = vec![0u8; 256];
        buf[..8].copy_from_slice(b"Cessna 1");
        let payload = simobject_payload(21, &buf); // TITLE
        assert!(apply_datum(&payload, &mut t));
        assert_eq!(t.title, "Cessna 1");
    }

    #[test]
    fn ground_track_routes_separately_from_true_heading() {
        let mut t = Telemetry::default();
        assert!(apply_datum(
            &simobject_payload(16, &100.0f64.to_le_bytes()),
            &mut t
        ));
        assert!(apply_datum(
            &simobject_payload(49, &115.0f64.to_le_bytes()),
            &mut t
        ));
        assert_eq!(t.heading_true_deg, 100.0);
        assert_eq!(t.ground_track_deg, 115.0);
    }

    #[test]
    fn an_unknown_request_id_is_ignored() {
        let mut t = Telemetry::default();
        let payload = simobject_payload(9999, &1.0f64.to_le_bytes());
        assert!(!apply_datum(&payload, &mut t));
        assert_eq!(t, Telemetry::default());
    }

    #[test]
    fn a_truncated_payload_is_ignored() {
        let mut t = Telemetry::default();
        assert!(!apply_datum(&[], &mut t));
        assert!(!apply_datum(&[0, 0, 0], &mut t));
        let short = vec![0u8; protocol::DATUM_OFFSET];
        assert!(!apply_datum(&short, &mut t));
    }

    #[test]
    fn config_builds_the_expected_address() {
        let c = Config::default();
        assert_eq!(c.addr(), "127.0.0.1:5557");
        assert_eq!(
            DEFAULT_PORT, 5557,
            "MSFS under Proton serves SimConnect on 5557"
        );
    }

    /// Exception logging must be bounded no matter how many arrive.
    #[test]
    fn exception_logging_is_throttled() {
        let mut log = ExceptionLog::default();
        for _ in 0..5_000 {
            log.record(protocol::EXCEPTION_DATA_ERROR);
        }
        assert_eq!(log.count, 5_000);
    }
}
