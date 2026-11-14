//! Headless telemetry output bridges.
//!
//! * [`lnm`] — LittleNavMap's hand-reversed Qt `QDataStream` wire format, over
//!   TCP.
//! * [`efb`] — XGPS/XATT over UDP 49002, for Garmin Pilot and ForeFlight.
//!
//! Neither depends on `egui`, so both can be tested and shipped standalone.
//!
//! # The format is a pure function; the socket is not
//!
//! [`lnm::packet`] and [`efb::datagram`] turn one telemetry frame into bytes
//! and touch nothing else, so the encoding is testable with no sim, no network
//! and neither client installed. That matters because **neither protocol
//! reports an error**: LittleNavMap given a misplaced field draws the aeroplane
//! somewhere wrong, and an EFB given a malformed sentence silently ignores it.
//!
//! # What each is fed
//!
//! One `simconnect_tcp::Telemetry`, the same struct the status bar reads. A
//! bridge never opens its own connection to the sim: there is one telemetry
//! link in the application and everything downstream shares it, so a bridge
//! cannot be the reason the sim sees a second client.

pub mod efb;
pub mod lnm;
pub mod serve;

use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock seconds, for the timestamps LittleNavMap embeds.
///
/// Clamped at the epoch rather than erroring: a clock before 1970 is a broken
/// machine, and refusing to draw the aeroplane would not help.
pub fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
