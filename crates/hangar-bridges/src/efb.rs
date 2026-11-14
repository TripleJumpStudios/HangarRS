//! XGPS/XATT — the X-Plane datagram format Garmin Pilot and ForeFlight read.
//!
//! Two ASCII sentences per datagram, broadcast to UDP 49002:
//!
//! ```text
//! XGPS<app>,<lon>,<lat>,<alt_m>,<track_true>,<ground_speed_m_s>
//! XATT<app>,<heading_true>,<pitch>,<roll>
//! ```
//!
//! # Field 5 of XGPS is track, not heading
//!
//! This is the one thing to get right: it is not `PLANE HEADING DEGREES
//! TRUE`. Heading is where the nose points; track is where the aeroplane is going, and in any
//! crosswind they differ by the crab angle. An EFB fed heading draws the
//! ownship symbol pointing correctly and moving along the wrong line, which
//! looks plausible right up until it matters.
//!
//! XATT's first field genuinely *is* heading — that one describes attitude,
//! and the nose is what it is about. So the two sentences carry different
//! numbers, and a bridge that passes the same value to both is wrong in one
//! of them whichever value it picks.
//!
//! # Units are not the sim's
//!
//! XGPS wants **metres** and **metres per second**; the sim reports feet and
//! knots. The conversions are exact rather than approximate: a foot is
//! 0.3048 m by definition, and a knot is one nautical mile per hour where the
//! nautical mile is exactly 1852 m.

use simconnect_tcp::Telemetry;

/// The port both apps listen on. Not configurable at their end.
pub const DEFAULT_PORT: u16 = 49002;

/// Exactly, by the international definition since 1959.
const METRES_PER_FOOT: f64 = 0.3048;
/// A nautical mile is exactly 1852 m.
const METRES_PER_NAUTICAL_MILE: f64 = 1852.0;
const SECONDS_PER_HOUR: f64 = 3600.0;

pub fn feet_to_metres(ft: f64) -> f64 {
    ft * METRES_PER_FOOT
}

/// **Multiplied before dividing, deliberately.** `1852 / 3600` is not
/// representable in binary floating point, so folding it into a single
/// constant makes 3600 kt come back as 1852.0000000000002. Multiplying first
/// keeps the exact cases exact.
pub fn knots_to_metres_per_second(kt: f64) -> f64 {
    kt * METRES_PER_NAUTICAL_MILE / SECONDS_PER_HOUR
}

/// The datagram for one telemetry frame.
///
/// `app` is the name the EFB shows as the traffic source. Both sentences carry
/// it, and neither app cares what it says as long as it is stable.
pub fn datagram(tel: &Telemetry, app: &str) -> String {
    format!(
        "XGPS{app},{lon:.6},{lat:.6},{alt:.1},{track:.1},{speed:.1}\n\
         XATT{app},{heading:.1},{pitch:.1},{roll:.1}\n",
        app = app,
        lon = tel.longitude_deg,
        lat = tel.latitude_deg,
        alt = feet_to_metres(tel.altitude_ft),
        // Track, not heading. See the module note.
        track = tel.ground_track_deg,
        speed = knots_to_metres_per_second(tel.ground_speed_kt),
        // Heading. XATT is about where the nose points, so here it is right.
        heading = tel.heading_true_deg,
        pitch = tel.pitch_deg,
        roll = tel.bank_deg,
    )
}

#[cfg(test)]
mod tests {
    /// 49002 is not configurable at the EFB's end, so a config default that
    /// drifted from it would broadcast to a port no tablet is listening on.
    #[test]
    fn the_config_default_matches_this_crate() {
        assert_eq!(
            hangar_core::config::EfbBridge::default().port,
            super::DEFAULT_PORT
        );
    }

    use super::*;

    /// Telemetry with heading and track deliberately different, which is what
    /// a crosswind produces and what every field test on the ground misses.
    fn crabbing() -> Telemetry {
        Telemetry {
            latitude_deg: 47.123456,
            longitude_deg: -122.987654,
            altitude_ft: 5000.0,
            ground_speed_kt: 120.0,
            heading_true_deg: 90.0,
            ground_track_deg: 82.5,
            pitch_deg: 2.5,
            bank_deg: -15.0,
            ..Default::default()
        }
    }

    fn fields(sentence: &str) -> Vec<&str> {
        sentence.split(',').collect()
    }

    #[test]
    fn xgps_carries_track_and_xatt_carries_heading() {
        let out = datagram(&crabbing(), "HangarRS");
        let mut lines = out.lines();

        let xgps = fields(lines.next().expect("XGPS"));
        assert_eq!(xgps[0], "XGPSHangarRS");
        assert_eq!(
            xgps[4], "82.5",
            "field 5 is ground track — heading here is the bug both previous \
             implementations shipped"
        );

        let xatt = fields(lines.next().expect("XATT"));
        assert_eq!(xatt[0], "XATTHangarRS");
        assert_eq!(
            xatt[1], "90.0",
            "XATT is about the nose, so heading is right"
        );
    }

    #[test]
    fn position_is_six_decimal_places() {
        let out = datagram(&crabbing(), "X");
        let xgps = fields(out.lines().next().expect("XGPS"));
        assert_eq!(xgps[1], "-122.987654");
        assert_eq!(xgps[2], "47.123456");
    }

    /// Feet to metres and knots to metres per second, both exact by
    /// definition rather than approximated.
    #[test]
    fn altitude_and_speed_are_converted_to_metric() {
        let out = datagram(&crabbing(), "X");
        let xgps = fields(out.lines().next().expect("XGPS"));
        assert_eq!(xgps[3], "1524.0", "5000 ft is exactly 1524 m");
        assert_eq!(xgps[5], "61.7", "120 kt is 61.73 m/s");
    }

    #[test]
    fn exact_conversions() {
        assert_eq!(feet_to_metres(5000.0), 1524.0);
        assert_eq!(knots_to_metres_per_second(3600.0), 1852.0);
        assert_eq!(feet_to_metres(0.0), 0.0);
    }

    #[test]
    fn both_sentences_end_in_a_newline() {
        let out = datagram(&crabbing(), "X");
        assert_eq!(out.lines().count(), 2);
        assert!(out.ends_with('\n'));
        assert_eq!(out.matches('\n').count(), 2);
    }

    #[test]
    fn attitude_carries_its_sign() {
        let out = datagram(&crabbing(), "X");
        let xatt = fields(out.lines().nth(1).expect("XATT"));
        assert_eq!(xatt[2], "2.5");
        assert_eq!(xatt[3], "-15.0", "a left bank is negative and stays so");
    }
}
