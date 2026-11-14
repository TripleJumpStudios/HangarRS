//! The LittleNavConnect wire format.
//!
//! Qt `QDataStream` at version `Qt_5_5` with `SinglePrecision` floats: every
//! integer big-endian, every float 4-byte big-endian IEEE 754, every string a
//! `u8` length followed by UTF-8 with no terminator. The reference is atools'
//! `simconnectdata.cpp`, `simconnectaircraft.cpp` and
//! `simconnectuseraircraft.cpp` — this is a hand-reversal of what those write,
//! and the field **order** is the whole protocol. There are no names on the
//! wire, so a field inserted in the wrong place shifts everything after it and
//! LittleNavMap reads garbage without complaining.
//!
//! # The format is separate from the socket
//!
//! [`packet`] is a pure function from telemetry to bytes, so the thing that
//! has to be exactly right is testable with no network, no sim and no
//! LittleNavMap. Sending it is [`crate::serve`]'s problem.
//!
//! # Track is not heading
//!
//! `trackMagDeg` and `trackTrueDeg` are the last two floats before the
//! timestamps, and *heading* does not belong in either. They differ by the
//! crab angle, which is zero on the ground and nowhere else, so the error is
//! invisible exactly where it is easiest to test. The SimVar that belongs
//! there is `GPS GROUND TRUE TRACK`.

use simconnect_tcp::Telemetry;

const MAGIC_NUMBER: u32 = 0xF75E_0AF3;
const DATA_VERSION: u32 = 11;

const FLAG_IS_USER: u16 = 0x0010;
const FLAG_SIM_MSFS_2024: u16 = 0x2000;
const FLAG_ON_GROUND: u16 = 0x0001;

const CATEGORY_AIRPLANE: u8 = 0;

/// The port LittleNavConnect listens on for a simulator feed.
pub const DEFAULT_PORT: u16 = 51968;

/// Standard sea-level pressure, for when the sim has not reported one.
const STANDARD_PRESSURE_MBAR: f32 = 1013.25;
/// Ten kilometres, for when the sim has not reported visibility.
const DEFAULT_VISIBILITY_M: f32 = 10_000.0;

fn u8v(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}
fn u16v(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn i16v(buf: &mut Vec<u8>, v: i16) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn u32v(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn i32v(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn i64v(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn f32v(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

/// `u8` length then UTF-8 bytes.
///
/// **Truncated on a byte boundary that is also a character boundary.** An
/// aircraft title is user-supplied and can carry anything; slicing UTF-8 at
/// byte 255 mid-character panics, and a panic here would take down a bridge
/// that is only meant to be drawing an aeroplane on a map.
fn string(buf: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(255);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    u8v(buf, end as u8);
    buf.extend_from_slice(&s.as_bytes()[..end]);
}

/// An empty `QHash<QString, QVariant>` — a `u16` count of zero.
fn empty_props(buf: &mut Vec<u8>) {
    u16v(buf, 0);
}

/// `QDateTime` in UTC: Julian day, milliseconds since midnight, spec byte.
///
/// `0x01` is `Qt::UTC`. 2440588 is the Julian day number of 1970-01-01.
fn qdatetime_utc(buf: &mut Vec<u8>, unix_secs: u64) {
    i64v(buf, (unix_secs / 86_400) as i64 + 2_440_588);
    i32v(buf, ((unix_secs % 86_400) * 1000) as i32);
    u8v(buf, 0x01);
}

fn aircraft(buf: &mut Vec<u8>, tel: &Telemetry) {
    u32v(buf, 1); // objectId — the user's aircraft is 1
    u8v(buf, 0); // dataFlags — strings present

    let mut flags = FLAG_IS_USER | FLAG_SIM_MSFS_2024;
    if tel.on_ground {
        flags |= FLAG_ON_GROUND;
    }
    u16v(buf, flags);

    string(buf, &tel.title);
    string(buf, &tel.atc_model);
    string(buf, &tel.atc_id);
    string(buf, &tel.atc_type);
    string(buf, &tel.atc_airline);
    string(buf, &tel.atc_flight_number);
    string(buf, &tel.gps_wp_prev_id);
    string(buf, &tel.gps_wp_next_id);

    let alt_ft = tel.altitude_ft as f32;
    let ias_kt = tel.airspeed_kt as f32;

    f32v(buf, tel.longitude_deg as f32);
    f32v(buf, tel.latitude_deg as f32);
    f32v(buf, alt_ft);
    f32v(buf, tel.heading_true_deg as f32);
    f32v(buf, tel.heading_mag_deg as f32);
    f32v(buf, tel.ground_speed_kt as f32);
    f32v(buf, ias_kt);
    f32v(buf, tel.vertical_speed_fpm as f32);
    f32v(buf, alt_ft); // indicatedAltitudeFt
    f32v(buf, ias_kt); // trueAirspeedKts
    f32v(buf, 0.0); // machSpeed — not read from the sim

    u8v(buf, tel.num_engines.max(1));
    u16v(buf, 0); // wingSpanFt
    u16v(buf, 0); // modelRadiusFt
    u16v(buf, 0); // deckHeight
    u8v(buf, CATEGORY_AIRPLANE);
    u8v(buf, tel.engine_type);
    i16v(buf, -1); // transponderCode — none reported
    empty_props(buf);
}

fn user_aircraft(buf: &mut Vec<u8>, tel: &Telemetry, unix_secs: u64) {
    aircraft(buf, tel);

    f32v(buf, tel.wind_velocity_kts as f32);
    f32v(buf, tel.wind_direction_deg as f32);
    f32v(buf, tel.alt_above_ground_ft as f32);
    f32v(buf, tel.ground_altitude_ft as f32);
    f32v(buf, tel.ap_altitude_lock_ft as f32);
    f32v(buf, tel.ambient_temp_c as f32);
    f32v(buf, tel.total_air_temp_c as f32);
    f32v(
        buf,
        or_default(tel.sea_level_pressure_mbar, STANDARD_PRESSURE_MBAR),
    );

    // Ice percentages. SimConnect exposes these, `simconnect-tcp` does not
    // read them yet, and zero is what "no ice" looks like — so this is a
    // truthful placeholder rather than a missing field.
    for _ in 0..8 {
        u8v(buf, 0);
    }

    f32v(buf, tel.total_weight_lbs as f32);
    f32v(buf, tel.max_gross_weight_lbs as f32);
    f32v(buf, tel.empty_weight_lbs as f32);
    f32v(buf, tel.fuel_total_qty_gal as f32);
    f32v(buf, tel.fuel_total_qty_lbs as f32);
    f32v(buf, tel.fuel_flow_pph as f32);
    f32v(buf, tel.fuel_flow_gph as f32);

    f32v(buf, tel.magvar_deg as f32);
    f32v(
        buf,
        or_default(tel.ambient_visibility_m, DEFAULT_VISIBILITY_M),
    );

    // Track, not heading. See the module note.
    f32v(buf, track_mag(tel) as f32);
    f32v(buf, tel.ground_track_deg as f32);

    qdatetime_utc(buf, unix_secs);
    qdatetime_utc(buf, unix_secs);
}

/// Magnetic ground track.
///
/// SimConnect reports track true; magnetic is true minus variation, the same
/// relationship the sim's own two heading variables have. Wrapped to
/// `[0, 360)` because a compass has no negative numbers on it.
pub fn track_mag(tel: &Telemetry) -> f64 {
    let track = tel.ground_track_deg - tel.magvar_deg;
    track.rem_euclid(360.0)
}

fn or_default(value: f64, fallback: f32) -> f32 {
    if value > 0.0 {
        value as f32
    } else {
        fallback
    }
}

/// One LittleNavConnect packet.
///
/// `unix_secs` is the wall-clock time of the frame; it appears in the header
/// and in both embedded `QDateTime`s.
pub fn packet(tel: &Telemetry, packet_id: u32, unix_secs: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);

    u32v(&mut buf, MAGIC_NUMBER);
    u32v(&mut buf, 0); // packetSize, patched below
    u32v(&mut buf, DATA_VERSION);
    u32v(&mut buf, packet_id);
    u32v(&mut buf, unix_secs as u32);

    u8v(&mut buf, 1); // hasUser
    user_aircraft(&mut buf, tel, unix_secs);

    u16v(&mut buf, 0); // numAi — no traffic
    u16v(&mut buf, 0); // numMetar

    // The size field counts everything after itself, so the magic number and
    // the field itself are excluded.
    let payload = (buf.len() - 8) as u32;
    buf[4..8].copy_from_slice(&payload.to_be_bytes());

    buf
}

#[cfg(test)]
mod tests {
    /// The config default and this crate's port are two literals in two
    /// crates; if they disagreed, LittleNavMap would be pointed at a port
    /// nothing is serving.
    #[test]
    fn the_config_default_matches_this_crate() {
        assert_eq!(
            hangar_core::config::LnmBridge::default().port,
            super::DEFAULT_PORT
        );
    }

    use super::*;

    fn flying() -> Telemetry {
        Telemetry {
            latitude_deg: 47.5,
            longitude_deg: -122.3,
            altitude_ft: 5000.0,
            airspeed_kt: 110.0,
            ground_speed_kt: 120.0,
            heading_true_deg: 90.0,
            heading_mag_deg: 105.0,
            ground_track_deg: 82.5,
            magvar_deg: -15.0,
            vertical_speed_fpm: 500.0,
            on_ground: false,
            num_engines: 1,
            engine_type: 0,
            title: "Commander 114".into(),
            ..Default::default()
        }
    }

    fn be_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
    }

    fn be_f32(bytes: &[u8], at: usize) -> f32 {
        f32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
    }

    #[test]
    fn the_header_is_magic_size_version_id_and_time() {
        let bytes = packet(&flying(), 7, 1_700_000_000);
        assert_eq!(be_u32(&bytes, 0), MAGIC_NUMBER);
        assert_eq!(be_u32(&bytes, 8), DATA_VERSION);
        assert_eq!(be_u32(&bytes, 12), 7);
        assert_eq!(be_u32(&bytes, 16), 1_700_000_000);
    }

    /// The size field counts everything after itself, so a reader that trusts
    /// it and a writer that miscounts disagree about where the packet ends.
    #[test]
    fn the_size_field_excludes_the_magic_number_and_itself() {
        let bytes = packet(&flying(), 1, 0);
        assert_eq!(be_u32(&bytes, 4) as usize, bytes.len() - 8);
    }

    #[test]
    fn the_user_flag_and_ground_flag_are_set_from_the_frame() {
        let airborne = packet(&flying(), 1, 0);
        // objectId(4) + dataFlags(1) start at 21; flags is the u16 after them.
        let flags_at = 21 + 4 + 1;
        let flags = u16::from_be_bytes(airborne[flags_at..flags_at + 2].try_into().unwrap());
        assert_eq!(flags & FLAG_IS_USER, FLAG_IS_USER);
        assert_eq!(flags & FLAG_SIM_MSFS_2024, FLAG_SIM_MSFS_2024);
        assert_eq!(flags & FLAG_ON_GROUND, 0);

        let mut parked = flying();
        parked.on_ground = true;
        let bytes = packet(&parked, 1, 0);
        let flags = u16::from_be_bytes(bytes[flags_at..flags_at + 2].try_into().unwrap());
        assert_eq!(flags & FLAG_ON_GROUND, FLAG_ON_GROUND);
    }

    /// The last two floats before the timestamps. Heading in both is only
    /// correct with no wind.
    #[test]
    fn the_track_fields_carry_track_and_not_heading() {
        let tel = flying();
        let bytes = packet(&tel, 1, 0);

        // Two QDateTimes at the end, 13 bytes each; the two track floats sit
        // immediately before them.
        let end = bytes.len() - 4 /* numAi + numMetar */ - 26;
        let track_true = be_f32(&bytes, end - 4);
        let track_mag_written = be_f32(&bytes, end - 8);

        assert_eq!(track_true, 82.5, "trackTrueDeg is ground track");
        assert_ne!(
            track_true, tel.heading_true_deg as f32,
            "and it is not heading — that was the bug"
        );
        assert_eq!(track_mag_written, 97.5, "82.5 true less -15 variation");
    }

    #[test]
    fn magnetic_track_wraps_onto_the_compass() {
        let mut tel = flying();
        tel.ground_track_deg = 5.0;
        tel.magvar_deg = 15.0;
        assert_eq!(track_mag(&tel), 350.0, "and never -10");

        tel.ground_track_deg = 350.0;
        tel.magvar_deg = -20.0;
        assert_eq!(track_mag(&tel), 10.0);
    }

    /// An aircraft title is user-supplied and can be anything at all.
    #[test]
    fn a_long_multibyte_title_does_not_panic_or_split_a_character() {
        let mut tel = flying();
        tel.title = "✈".repeat(200); // 600 bytes
        let bytes = packet(&tel, 1, 0);

        // The title is the first string, right after objectId, dataFlags and
        // flags.
        let len_at = 21 + 4 + 1 + 2;
        let len = bytes[len_at] as usize;
        assert!(len <= 255);
        assert_eq!(len % 3, 0, "a three-byte character was not cut in half");
        std::str::from_utf8(&bytes[len_at + 1..len_at + 1 + len]).expect("still valid UTF-8");
    }

    #[test]
    fn an_empty_string_is_a_zero_length_and_nothing_else() {
        let mut tel = flying();
        tel.title = String::new();
        let bytes = packet(&tel, 1, 0);
        assert_eq!(bytes[21 + 4 + 1 + 2], 0);
    }

    /// Missing weather readings become the standard values rather than zero,
    /// because zero pressure and zero visibility are both meaningful and both
    /// wrong.
    #[test]
    fn absent_pressure_and_visibility_fall_back_rather_than_sending_zero() {
        assert_eq!(or_default(0.0, STANDARD_PRESSURE_MBAR), 1013.25);
        assert_eq!(or_default(995.0, STANDARD_PRESSURE_MBAR), 995.0);
        assert_eq!(or_default(0.0, DEFAULT_VISIBILITY_M), 10_000.0);
    }

    /// The whole protocol is field order, so the length is a fingerprint of
    /// it: nothing added, removed or resized without this changing.
    #[test]
    fn the_packet_is_the_length_the_field_list_adds_up_to() {
        let mut tel = flying();
        tel.title = String::new();
        tel.atc_model = String::new();
        tel.atc_id = String::new();
        tel.atc_type = String::new();
        tel.atc_airline = String::new();
        tel.atc_flight_number = String::new();
        tel.gps_wp_prev_id = String::new();
        tel.gps_wp_next_id = String::new();

        // Counted from the field list rather than from what the code
        // produced, or this test only asserts that nothing changed:
        //
        //   header      magic 4 + size 4 + version 4 + id 4 + time 4   =  20
        //   hasUser                                                    =   1
        //   aircraft    objectId 4, dataFlags 1, flags 2               =   7
        //               8 empty strings, one length byte each          =   8
        //               11 f32                                        =  44
        //               engines 1, span 2, radius 2, deck 2            =   7
        //               category 1, engineType 1, transponder 2        =   4
        //               empty props                                    =   2
        //   user        8 f32 weather                                 =  32
        //               8 ice bytes                                    =   8
        //               7 f32 weight and fuel                         =  28
        //               magvar 4, visibility 4                        =   8
        //               trackMag 4, trackTrue 4                       =   8
        //               2 QDateTime, 13 each                          =  26
        //   trailer     numAi 2 + numMetar 2                          =   4
        //                                                               ───
        //                                                               207
        let bytes = packet(&tel, 1, 0);
        assert_eq!(
            bytes.len(),
            207,
            "if this changed, a field moved — which LittleNavMap will not report"
        );
    }
}
