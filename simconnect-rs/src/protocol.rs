//! SimConnect TCP wire protocol — packet framing, message IDs, builders, parsers.
//!
//! All integers are little-endian. Framing is:
//!
//! ```text
//! u32 size | u32 version | u32 msg_id | u32 reserved | payload…
//! ```
//!
//! Verified against MSFS 2024 (protocol version 0x05, sim build 12.2.282174).
//!
//! **Nothing in this module should be "cleaned up" against the published
//! SimConnect SDK headers.** Several constants and offsets here disagree with
//! the documented struct layouts and match what MSFS 2024 actually puts on the
//! wire. Each of those is covered by a test that states the observed value.

/// Protocol version for MSFS 2020 / 2024.
pub const SC_VERSION: u32 = 0x05;

// ── Client → Sim message IDs ─────────────────────────────────────────────────

pub const MSG_OPEN: u32 = 0x01;
pub const MSG_SUBSCRIBE_TO_SYSTEM_EVENT: u32 = 0x17;
pub const MSG_ADD_TO_DATA_DEFINITION: u32 = 0x0C;
pub const MSG_REQUEST_DATA_ON_SIM_OBJECT: u32 = 0x0E;

// ── Sim → Client message IDs ─────────────────────────────────────────────────

pub const RECV_EXCEPTION: u32 = 0x01;
pub const RECV_OPEN: u32 = 0x02;
pub const RECV_EVENT: u32 = 0x07;
pub const RECV_SIMOBJECT_DATA: u32 = 0x08;

// ── SimConnect constants ─────────────────────────────────────────────────────

/// `SIMCONNECT_PERIOD_VISUAL_FRAME` — deliver SimVar data every rendered frame.
pub const PERIOD_VISUAL_FRAME: u32 = 4;

/// `SIMCONNECT_DATATYPE_INT32`.
///
/// MSFS 2024 empirically returns these as `f32` — `SIM ON GROUND` comes back as
/// `1.0` — so the `i32` reinterpretation is non-zero when true and zero when
/// false. That is fine for booleans and is why [`crate::simvars`] only uses this
/// type for them.
pub const DATATYPE_INT32: u32 = 3;

/// `SIMCONNECT_DATATYPE_FLOAT64`.
pub const DATATYPE_FLOAT64: u32 = 4;

/// `SIMCONNECT_DATATYPE_STRING256` — fixed 256-byte null-padded buffer.
pub const DATATYPE_STRING256: u32 = 9;

/// Exception code raised when a data definition is rejected.
///
/// Worth naming because of what it does *not* look like: MSFS 2024 answers a
/// bundled multi-variable definition with this and then silently delivers
/// nothing for the entire group, for the rest of the session. See the
/// `one_define_id_per_variable` test in [`crate::simvars`].
pub const EXCEPTION_DATA_ERROR: u32 = 29;

// ── Minimum sim version for a reliable SimVar API ────────────────────────────

pub const MIN_VERSION_MAJOR: u32 = 1;
pub const MIN_VERSION_MINOR: u32 = 7;
pub const MIN_VERSION_BUILD: u32 = 9;

/// Whether a reported sim version is new enough to trust the SimVar API.
pub fn version_ok(major: u32, minor: u32, build: u32) -> bool {
    (major, minor, build) >= (MIN_VERSION_MAJOR, MIN_VERSION_MINOR, MIN_VERSION_BUILD)
}

// ── Packet building ──────────────────────────────────────────────────────────

/// Header length: size, version, msg_id, reserved.
pub const HEADER_LEN: usize = 16;

/// Length of the header actually read back off the wire before the payload.
///
/// Asymmetric with [`HEADER_LEN`] on purpose: outgoing packets carry a trailing
/// reserved word that the sim does not echo, so a receiver that strips 16 bytes
/// loses the first four bytes of every payload.
pub const RECV_HEADER_LEN: usize = 12;

/// Frame a payload as a SimConnect packet.
///
/// The `0xf000_0000` bit set on the message ID is required; without it the sim
/// ignores the packet without answering.
pub fn build_packet(msg_id: u32, payload: &[u8]) -> Vec<u8> {
    let total = (HEADER_LEN + payload.len()) as u32;
    let mut pkt = Vec::with_capacity(total as usize);
    pkt.extend_from_slice(&total.to_le_bytes());
    pkt.extend_from_slice(&SC_VERSION.to_le_bytes());
    pkt.extend_from_slice(&(0xf000_0000 | msg_id).to_le_bytes());
    pkt.extend_from_slice(&0u32.to_le_bytes());
    pkt.extend_from_slice(payload);
    pkt
}

/// Build the `MSG_OPEN` handshake payload.
///
/// The 280-byte length and every constant past offset 260 are load-bearing and
/// were established empirically. `"HK\0"` at offset 261 in particular is not
/// documented anywhere; a handshake without it is not answered.
pub fn build_open(app_name: &str) -> Vec<u8> {
    let mut payload = vec![0u8; 280];
    let name = app_name.as_bytes();
    let len = name.len().min(255);
    payload[..len].copy_from_slice(&name[..len]);
    payload[261..264].copy_from_slice(b"HK\0");
    payload[264..268].copy_from_slice(&11u32.to_le_bytes());
    payload[268..272].copy_from_slice(&0u32.to_le_bytes());
    payload[272..276].copy_from_slice(&62651u32.to_le_bytes());
    payload[276..280].copy_from_slice(&3u32.to_le_bytes());
    payload
}

/// Build a `MSG_SUBSCRIBE_TO_SYSTEM_EVENT` payload.
pub fn build_subscribe_event(client_event_id: u32, event_name: &str) -> Vec<u8> {
    let mut payload = vec![0u8; 260];
    payload[..4].copy_from_slice(&client_event_id.to_le_bytes());
    let name = event_name.as_bytes();
    let len = name.len().min(255);
    payload[4..4 + len].copy_from_slice(&name[..len]);
    payload
}

/// Build a `MSG_ADD_TO_DATA_DEFINITION` payload with an explicit data type.
///
/// One call per variable. Adding several data to the *same* `define_id` is what
/// triggers [`EXCEPTION_DATA_ERROR`] and kills the whole group.
pub fn build_add_to_data_definition(
    define_id: u32,
    datum_name: &str,
    units_name: &str,
    data_type: u32,
) -> Vec<u8> {
    let mut payload = vec![0u8; 528];
    payload[..4].copy_from_slice(&define_id.to_le_bytes());
    write_fixed_str(&mut payload[4..260], datum_name);
    write_fixed_str(&mut payload[260..516], units_name);
    payload[516..520].copy_from_slice(&data_type.to_le_bytes());
    payload
}

/// Build a `MSG_REQUEST_DATA_ON_SIM_OBJECT` payload.
pub fn build_request_data_on_sim_object(request_id: u32, define_id: u32, period: u32) -> Vec<u8> {
    let mut payload = vec![0u8; 32];
    payload[0..4].copy_from_slice(&request_id.to_le_bytes());
    payload[4..8].copy_from_slice(&define_id.to_le_bytes());
    payload[12..16].copy_from_slice(&period.to_le_bytes());
    payload
}

/// Copy a string into a fixed null-padded field, truncating on a char boundary.
fn write_fixed_str(field: &mut [u8], s: &str) {
    let cap = field.len() - 1; // always leave the terminating null
    let mut end = s.len().min(cap);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    field[..end].copy_from_slice(&s.as_bytes()[..end]);
}

// ── Packet parsing ───────────────────────────────────────────────────────────

/// Offset of the first datum in a `RECV_SIMOBJECT_DATA` payload.
///
/// The documented `SIMCONNECT_RECV_SIMOBJECT_DATA` struct puts the data
/// elsewhere; on the MSFS 2024 TCP wire, with the 12-byte receive header
/// stripped, the layout is:
///
/// ```text
/// [ 0.. 4] dwRequestID
/// [ 4.. 8] (reserved / flags)
/// [ 8..12] dwDefineID
/// [12..16] dwObjectID
/// [16..20] dwentrynumber
/// [20..24] dwoutof
/// [24..28] dwDefineCount
/// [28..  ] the datum
/// ```
pub const DATUM_OFFSET: usize = 28;

/// Extract the sim version from a `RECV_OPEN` payload (header already stripped).
///
/// ```text
/// [  0..256] sim name
/// [256..260] app version major
/// [260..264] app version minor
/// [264..268] app build major
/// ```
pub fn parse_recv_open_version(payload: &[u8]) -> Option<(u32, u32, u32)> {
    Some((
        read_u32(payload, 256)?,
        read_u32(payload, 260)?,
        read_u32(payload, 264)?,
    ))
}

/// Extract the sim name from a `RECV_OPEN` payload.
pub fn parse_recv_open_name(payload: &[u8]) -> Option<String> {
    read_fixed_str(payload, 0, 256)
}

/// The request ID a `RECV_SIMOBJECT_DATA` payload answers.
pub fn parse_simobject_request_id(payload: &[u8]) -> Option<u32> {
    read_u32(payload, 0)
}

/// Extract the first `INT32` datum from a `RECV_SIMOBJECT_DATA` payload.
pub fn parse_simobject_data_i32(payload: &[u8]) -> Option<i32> {
    let b = payload.get(DATUM_OFFSET..DATUM_OFFSET + 4)?;
    Some(i32::from_le_bytes(b.try_into().ok()?))
}

/// Extract the first `FLOAT64` datum from a `RECV_SIMOBJECT_DATA` payload.
pub fn parse_simobject_data_f64(payload: &[u8]) -> Option<f64> {
    let b = payload.get(DATUM_OFFSET..DATUM_OFFSET + 8)?;
    Some(f64::from_le_bytes(b.try_into().ok()?))
}

/// Extract a `STRING256` datum — a null-padded buffer at [`DATUM_OFFSET`].
pub fn parse_simobject_data_string256(payload: &[u8]) -> Option<String> {
    read_fixed_str(payload, DATUM_OFFSET, 256)
}

/// Extract the exception code from a `RECV_EXCEPTION` payload.
pub fn parse_recv_exception(payload: &[u8]) -> Option<u32> {
    read_u32(payload, 0)
}

fn read_u32(payload: &[u8], at: usize) -> Option<u32> {
    let b = payload.get(at..at + 4)?;
    Some(u32::from_le_bytes(b.try_into().ok()?))
}

fn read_fixed_str(payload: &[u8], at: usize, len: usize) -> Option<String> {
    let raw = payload.get(at..at + len)?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    Some(String::from_utf8_lossy(&raw[..end]).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_header_is_size_version_flagged_id_reserved() {
        let pkt = build_packet(MSG_OPEN, &[0xAA, 0xBB]);
        assert_eq!(pkt.len(), HEADER_LEN + 2);
        assert_eq!(u32::from_le_bytes(pkt[0..4].try_into().unwrap()), 18);
        assert_eq!(
            u32::from_le_bytes(pkt[4..8].try_into().unwrap()),
            SC_VERSION
        );
        assert_eq!(
            u32::from_le_bytes(pkt[8..12].try_into().unwrap()),
            0xf000_0001,
            "the 0xf0000000 flag is required — without it the sim never answers"
        );
        assert_eq!(u32::from_le_bytes(pkt[12..16].try_into().unwrap()), 0);
        assert_eq!(&pkt[16..], &[0xAA, 0xBB]);
    }

    /// The handshake's undocumented constants. A payload without `"HK\0"` at
    /// offset 261 is not answered at all.
    #[test]
    fn open_payload_is_280_bytes_with_hk_at_261() {
        let p = build_open("hangar-rs");
        assert_eq!(p.len(), 280);
        assert_eq!(&p[..9], b"hangar-rs");
        assert_eq!(&p[261..264], b"HK\0");
        assert_eq!(u32::from_le_bytes(p[264..268].try_into().unwrap()), 11);
        assert_eq!(u32::from_le_bytes(p[268..272].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(p[272..276].try_into().unwrap()), 62651);
        assert_eq!(u32::from_le_bytes(p[276..280].try_into().unwrap()), 3);
    }

    /// An over-long app name must not run into the `"HK\0"` field.
    #[test]
    fn open_payload_truncates_a_long_app_name() {
        let p = build_open(&"x".repeat(400));
        assert_eq!(p.len(), 280);
        assert_eq!(p[255], 0, "the name field must stay null-terminated");
        assert_eq!(&p[261..264], b"HK\0");
    }

    /// **The offset-28 regression.**
    #[test]
    fn simobject_data_starts_at_offset_28() {
        assert_eq!(DATUM_OFFSET, 28);

        let mut payload = vec![0u8; DATUM_OFFSET + 8];
        payload[0..4].copy_from_slice(&42u32.to_le_bytes()); // dwRequestID
        payload[DATUM_OFFSET..DATUM_OFFSET + 8].copy_from_slice(&1234.5f64.to_le_bytes());

        assert_eq!(parse_simobject_request_id(&payload), Some(42));
        assert_eq!(parse_simobject_data_f64(&payload), Some(1234.5));
    }

    #[test]
    fn simobject_i32_and_string_read_from_the_same_offset() {
        let mut payload = vec![0u8; DATUM_OFFSET + 256];
        payload[DATUM_OFFSET..DATUM_OFFSET + 4].copy_from_slice(&1i32.to_le_bytes());
        assert_eq!(parse_simobject_data_i32(&payload), Some(1));

        let mut payload = vec![0u8; DATUM_OFFSET + 256];
        payload[DATUM_OFFSET..DATUM_OFFSET + 7].copy_from_slice(b"C172 G3");
        assert_eq!(
            parse_simobject_data_string256(&payload).as_deref(),
            Some("C172 G3")
        );
    }

    /// Short or truncated payloads must return `None`, never panic — a
    /// malformed packet from the sim should not take the client down.
    #[test]
    fn short_payloads_are_rejected_not_panicked_on() {
        for len in 0..DATUM_OFFSET + 8 {
            let payload = vec![0u8; len];
            let f64_ok = parse_simobject_data_f64(&payload).is_some();
            assert_eq!(f64_ok, len >= DATUM_OFFSET + 8);
            let _ = parse_simobject_data_i32(&payload);
            let _ = parse_simobject_data_string256(&payload);
            let _ = parse_recv_open_version(&payload);
            let _ = parse_recv_exception(&payload);
        }
    }

    #[test]
    fn recv_open_version_and_name_parse() {
        let mut p = vec![0u8; 272];
        p[..15].copy_from_slice(b"KittyHawk\0\0\0\0\0\0");
        p[256..260].copy_from_slice(&12u32.to_le_bytes());
        p[260..264].copy_from_slice(&2u32.to_le_bytes());
        p[264..268].copy_from_slice(&282174u32.to_le_bytes());
        assert_eq!(parse_recv_open_name(&p).as_deref(), Some("KittyHawk"));
        assert_eq!(parse_recv_open_version(&p), Some((12, 2, 282174)));
    }

    #[test]
    fn version_gate_compares_lexicographically() {
        assert!(version_ok(12, 2, 282174));
        assert!(version_ok(
            MIN_VERSION_MAJOR,
            MIN_VERSION_MINOR,
            MIN_VERSION_BUILD
        ));
        assert!(!version_ok(1, 7, 8));
        assert!(!version_ok(1, 6, 999));
        assert!(!version_ok(0, 99, 99));
        assert!(version_ok(2, 0, 0));
    }

    #[test]
    fn data_definition_places_name_units_and_type() {
        let p = build_add_to_data_definition(11, "PLANE LATITUDE", "Degrees", DATATYPE_FLOAT64);
        assert_eq!(p.len(), 528);
        assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 11);
        assert_eq!(&p[4..18], b"PLANE LATITUDE");
        assert_eq!(p[18], 0);
        assert_eq!(&p[260..267], b"Degrees");
        assert_eq!(
            u32::from_le_bytes(p[516..520].try_into().unwrap()),
            DATATYPE_FLOAT64
        );
    }

    /// Truncation must not split a UTF-8 sequence, or the sim receives an
    /// invalid name and answers with an exception instead of data.
    #[test]
    fn fixed_str_truncates_on_a_char_boundary() {
        let mut field = [0u8; 8];
        write_fixed_str(&mut field, "ααααα"); // 2 bytes each
        assert_eq!(field[7], 0);
        assert!(std::str::from_utf8(&field[..6]).is_ok());
    }
}
