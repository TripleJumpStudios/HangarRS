//! Getting the bytes to the client.
//!
//! Kept apart from the formats on purpose: [`lnm::packet`] and
//! [`efb::datagram`] are pure and tested, and everything that can fail for
//! reasons outside this process lives here.
//!
//! # Two protocols, two shapes
//!
//! **LittleNavMap connects to us.** LittleNavConnect is a client: it opens a
//! TCP connection to a simulator feed and reads packets until the socket
//! closes. So this listens, and a disconnect is ordinary — LittleNavMap being
//! closed and reopened must not end the bridge.
//!
//! **An EFB never connects at all.** Garmin Pilot and ForeFlight listen on UDP
//! 49002 for broadcasts and say nothing back. There is no session, no
//! acknowledgement, and no way to discover whether anything is listening — a
//! send to a broadcast address succeeds whether or not an iPad is awake. So
//! the EFB side can never report "connected", and it must not pretend to.
//!
//! # Stale telemetry is not sent
//!
//! Both bridges stop transmitting when the frame stops being updated rather
//! than repeating the last one. An EFB showing an ownship frozen mid-air looks
//! like a working feed of a stationary aeroplane; showing nothing looks like
//! what it is.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use simconnect_tcp::Telemetry;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, UdpSocket};

use crate::{efb, lnm};

/// How old a frame may be before a bridge stops sending it.
///
/// The sim delivers at roughly 30 Hz, so a second is many missed frames — long
/// enough not to flicker on one dropped update, short enough that a person
/// notices the ownship stop rather than watching it drift on stale data.
pub const STALE_AFTER: Duration = Duration::from_secs(1);

/// Where a bridge gets its telemetry.
///
/// A closure rather than a channel or a lock, so the caller keeps whatever
/// shape it already has — the application has one telemetry link and hands out
/// snapshots of it. Returning `None` means "nothing to send", which is
/// different from sending zeroes.
pub type Source = Arc<dyn Fn() -> Option<Telemetry> + Send + Sync>;

/// Serve LittleNavMap on `port` until the future is dropped.
///
/// One client at a time, which is what LittleNavConnect is: a second
/// connection replaces the first rather than being refused, because the
/// common case for one arriving is that LittleNavMap was restarted and the
/// old socket has not been noticed as dead yet.
pub async fn lnm_server(port: u16, source: Source, rate_hz: u32) -> std::io::Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    tracing::info!("LittleNavMap bridge listening on {port}");

    loop {
        let (mut stream, peer) = listener.accept().await?;
        tracing::info!("LittleNavMap connected from {peer}");

        let mut ticker = ticker(rate_hz);
        let mut packet_id: u32 = 0;

        loop {
            ticker.tick().await;
            let Some(telemetry) = fresh(&source) else {
                continue;
            };

            packet_id = packet_id.wrapping_add(1);
            let bytes = lnm::packet(&telemetry, packet_id, crate::unix_secs());
            if let Err(e) = stream.write_all(&bytes).await {
                // Ordinary: LittleNavMap was closed. Wait for the next one.
                tracing::info!("LittleNavMap disconnected: {e}");
                break;
            }
        }
    }
}

/// Broadcast XGPS/XATT until the future is dropped.
///
/// Never reports a client, because there is no way to have one: see the module
/// note.
pub async fn efb_broadcast(
    port: u16,
    app: String,
    source: Source,
    rate_hz: u32,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))).await?;
    socket.set_broadcast(true)?;
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);
    tracing::info!("EFB bridge broadcasting to {destination} at {rate_hz} Hz");

    let mut ticker = ticker(rate_hz);
    loop {
        ticker.tick().await;
        let Some(telemetry) = fresh(&source) else {
            continue;
        };

        let datagram = efb::datagram(&telemetry, &app);
        if let Err(e) = socket.send_to(datagram.as_bytes(), destination).await {
            // A broadcast can fail transiently — no route while a network
            // interface is coming up, for one — and that is not a reason to
            // stop for good.
            tracing::warn!("EFB broadcast failed: {e}");
        }
    }
}

/// A frame, if there is one and it is recent enough to mean anything.
fn fresh(source: &Source) -> Option<Telemetry> {
    source().filter(|t| !t.is_stale(STALE_AFTER))
}

/// **Missed ticks are skipped, not queued.** The default behaviour catches up
/// by firing back to back, which after any pause sends a burst of frames that
/// are all equally out of date. For a position feed the only interesting frame
/// is the current one.
fn ticker(rate_hz: u32) -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(period(rate_hz));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker
}

/// How long between frames.
///
/// Separate from [`ticker`] because `tokio::time::interval` panics outside a
/// runtime, so folding the arithmetic into it makes the arithmetic untestable
/// without one. A rate of zero is clamped rather than dividing by zero — it
/// can only come from a config file, and refusing to start a bridge over a
/// typo is worse than running it at 1 Hz.
fn period(rate_hz: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(rate_hz.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn frame(updated_at: Option<Instant>) -> Telemetry {
        Telemetry {
            latitude_deg: 47.5,
            updated_at,
            ..Default::default()
        }
    }

    #[test]
    fn a_stale_frame_is_not_sent() {
        let old = Instant::now() - Duration::from_secs(5);
        let source: Source = Arc::new(move || Some(frame(Some(old))));
        assert!(
            fresh(&source).is_none(),
            "an ownship frozen mid-air reads as a working feed of a \
             stationary aeroplane"
        );
    }

    #[test]
    fn a_recent_frame_is_sent() {
        let source: Source = Arc::new(|| Some(frame(Some(Instant::now()))));
        assert!(fresh(&source).is_some());
    }

    /// Never updated is not the same as updated long ago, and neither is
    /// something to broadcast.
    #[test]
    fn a_frame_that_never_arrived_is_not_sent() {
        let source: Source = Arc::new(|| Some(frame(None)));
        assert!(fresh(&source).is_none());

        let source: Source = Arc::new(|| None);
        assert!(fresh(&source).is_none());
    }

    #[test]
    fn a_zero_rate_is_clamped_rather_than_dividing_by_zero() {
        assert_eq!(period(0), Duration::from_secs(1));
        assert_eq!(period(1), Duration::from_secs(1));
        assert_eq!(period(10), Duration::from_millis(100));
        assert_eq!(period(30), Duration::from_secs_f64(1.0 / 30.0));
    }
}
