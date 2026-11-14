//! The SimVar subscription table.
//!
//! # One `define_id` per variable
//!
//! Every entry below gets its own data definition and its own request.
//! Bundling several variables into one `define_id` makes MSFS 2024 answer with
//! [`EXCEPTION_DATA_ERROR`](crate::protocol::EXCEPTION_DATA_ERROR) once and then deliver **nothing** for that whole
//! group, for the rest of the session: no error, no retry, no partial data.
//!
//! The `one_define_id_per_variable` test enforces it.
//!
//! # Routing
//!
//! Each entry carries the function that writes its value into [`Telemetry`], so
//! a variable cannot be registered and then silently not routed anywhere.

use crate::protocol::{DATATYPE_FLOAT64, DATATYPE_INT32, DATATYPE_STRING256};
use crate::telemetry::{Datum, Telemetry};

/// One SimVar subscription.
pub struct SimVarDef {
    pub define_id: u32,
    pub request_id: u32,
    pub name: &'static str,
    pub units: &'static str,
    pub data_type: u32,
    /// Writes a delivered value into the snapshot.
    pub apply: fn(&mut Telemetry, Datum),
}

/// Define an `f64` SimVar; a datum of any other type is ignored.
macro_rules! f64_var {
    ($id:expr, $name:literal, $units:literal, $field:ident) => {
        SimVarDef {
            define_id: $id,
            request_id: $id,
            name: $name,
            units: $units,
            data_type: DATATYPE_FLOAT64,
            apply: |t, d| {
                if let Datum::F64(v) = d {
                    t.$field = v;
                }
            },
        }
    };
}

macro_rules! str_var {
    ($id:expr, $name:literal, $field:ident) => {
        SimVarDef {
            define_id: $id,
            request_id: $id,
            name: $name,
            units: "",
            data_type: DATATYPE_STRING256,
            apply: |t, d| {
                if let Datum::Str(s) = d {
                    t.$field = s;
                }
            },
        }
    };
}

/// All subscribed SimVars.
///
/// IDs start at 10 so they cannot collide with the camera-state definitions a
/// consumer of this crate might add alongside. Gaps in the numbering carry no
/// meaning.
pub const SIMVARS: &[SimVarDef] = &[
    // ── Position & motion (10–20, 49) ────────────────────────────────────────
    f64_var!(10, "PLANE ALTITUDE", "Feet", altitude_ft),
    f64_var!(11, "PLANE LATITUDE", "Degrees", latitude_deg),
    f64_var!(12, "PLANE LONGITUDE", "Degrees", longitude_deg),
    f64_var!(13, "AIRSPEED INDICATED", "Knots", airspeed_kt),
    SimVarDef {
        define_id: 14,
        request_id: 14,
        name: "SIM ON GROUND",
        units: "Bool",
        data_type: DATATYPE_INT32,
        // MSFS returns this as a float; reinterpreted as i32 it is non-zero when
        // true and zero when false, which is all a bool needs.
        apply: |t, d| {
            if let Datum::I32(v) = d {
                t.on_ground = v != 0;
            }
        },
    },
    f64_var!(
        15,
        "PLANE HEADING DEGREES MAGNETIC",
        "Degrees",
        heading_mag_deg
    ),
    f64_var!(
        16,
        "PLANE HEADING DEGREES TRUE",
        "Degrees",
        heading_true_deg
    ),
    f64_var!(17, "VERTICAL SPEED", "Feet per minute", vertical_speed_fpm),
    f64_var!(18, "GROUND VELOCITY", "Knots", ground_speed_kt),
    f64_var!(19, "PLANE BANK DEGREES", "Degrees", bank_deg),
    f64_var!(20, "PLANE PITCH DEGREES", "Degrees", pitch_deg),
    // ── Aircraft identity (21–28) ────────────────────────────────────────────
    str_var!(21, "TITLE", title),
    str_var!(22, "ATC MODEL", atc_model),
    str_var!(23, "ATC ID", atc_id),
    str_var!(24, "ATC TYPE", atc_type),
    str_var!(25, "ATC AIRLINE", atc_airline),
    str_var!(26, "ATC FLIGHT NUMBER", atc_flight_number),
    str_var!(27, "GPS WP PREV ID", gps_wp_prev_id),
    str_var!(28, "GPS WP NEXT ID", gps_wp_next_id),
    // ── Atmosphere (30–39) ───────────────────────────────────────────────────
    f64_var!(30, "AMBIENT WIND VELOCITY", "Knots", wind_velocity_kts),
    f64_var!(31, "AMBIENT WIND DIRECTION", "Degrees", wind_direction_deg),
    f64_var!(32, "AMBIENT TEMPERATURE", "Celsius", ambient_temp_c),
    f64_var!(33, "TOTAL AIR TEMPERATURE", "Celsius", total_air_temp_c),
    f64_var!(
        34,
        "SEA LEVEL PRESSURE",
        "Millibars",
        sea_level_pressure_mbar
    ),
    f64_var!(35, "AMBIENT VISIBILITY", "Meters", ambient_visibility_m),
    f64_var!(36, "MAGVAR", "Degrees", magvar_deg),
    f64_var!(37, "GROUND ALTITUDE", "Feet", ground_altitude_ft),
    f64_var!(38, "PLANE ALT ABOVE GROUND", "Feet", alt_above_ground_ft),
    f64_var!(
        39,
        "AUTOPILOT ALTITUDE LOCK VAR",
        "Feet",
        ap_altitude_lock_ft
    ),
    // ── Weights, fuel, powerplant (40–48) ────────────────────────────────────
    f64_var!(40, "TOTAL WEIGHT", "Pounds", total_weight_lbs),
    f64_var!(41, "MAX GROSS WEIGHT", "Pounds", max_gross_weight_lbs),
    f64_var!(42, "EMPTY WEIGHT", "Pounds", empty_weight_lbs),
    f64_var!(43, "FUEL TOTAL QUANTITY", "Gallons", fuel_total_qty_gal),
    f64_var!(
        44,
        "FUEL TOTAL QUANTITY WEIGHT",
        "Pounds",
        fuel_total_qty_lbs
    ),
    f64_var!(45, "ENG FUEL FLOW PPH:1", "Pounds per hour", fuel_flow_pph),
    f64_var!(46, "ENG FUEL FLOW GPH:1", "Gallons per hour", fuel_flow_gph),
    SimVarDef {
        define_id: 47,
        request_id: 47,
        name: "NUMBER OF ENGINES",
        units: "Number",
        data_type: DATATYPE_FLOAT64,
        apply: |t, d| {
            if let Datum::F64(v) = d {
                t.num_engines = v.clamp(0.0, 255.0) as u8;
            }
        },
    },
    SimVarDef {
        define_id: 48,
        request_id: 48,
        name: "ENGINE TYPE",
        units: "Number",
        data_type: DATATYPE_FLOAT64,
        apply: |t, d| {
            if let Datum::F64(v) = d {
                t.engine_type = v.clamp(0.0, 255.0) as u8;
            }
        },
    },
    // Registered last, hence the non-adjacent ID. Sending true heading in its
    // place is wrong by the crab angle in any crosswind.
    //
    // **The name is `GPS GROUND TRUE TRACK`.** `GPS GROUND TRACK` is not a
    // SimVar, and asking for it is answered with exception 7,
    // `NAME_UNRECOGNIZED` — which does not stop the other variables, so the
    // field simply keeps its default of 0.0 for ever. A wrong name and a
    // genuinely northbound aeroplane look identical.
    f64_var!(49, "GPS GROUND TRUE TRACK", "Degrees", ground_track_deg),
];

/// Look up the variable a `RECV_SIMOBJECT_DATA` request ID belongs to.
pub fn by_request_id(request_id: u32) -> Option<&'static SimVarDef> {
    SIMVARS.iter().find(|v| v.request_id == request_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// **The exception-29 guard.**
    ///
    /// Two variables sharing a `define_id` is exactly the bundling that makes
    /// MSFS 2024 drop the entire group silently. This assertion is the reason
    /// the table is a table.
    #[test]
    fn one_define_id_per_variable() {
        let mut seen = HashSet::new();
        for v in SIMVARS {
            assert!(
                seen.insert(v.define_id),
                "define_id {} is used twice ({}). MSFS 2024 answers a bundled \
                 definition with exception 29 and then delivers nothing for the \
                 whole group, for the rest of the session.",
                v.define_id,
                v.name
            );
        }
        assert_eq!(seen.len(), SIMVARS.len());
    }

    #[test]
    fn request_ids_are_unique_and_resolvable() {
        let mut seen = HashSet::new();
        for v in SIMVARS {
            assert!(
                seen.insert(v.request_id),
                "duplicate request_id {}",
                v.request_id
            );
            let found = by_request_id(v.request_id).expect("request_id must resolve");
            assert_eq!(found.name, v.name);
        }
        assert!(by_request_id(9999).is_none());
    }

    /// Every registered variable must actually write somewhere. A var that is
    /// subscribed but routed nowhere costs a definition slot and delivers
    /// nothing — invisible without this check.
    #[test]
    fn every_variable_routes_into_telemetry() {
        for v in SIMVARS {
            let datum = match v.data_type {
                DATATYPE_FLOAT64 => Datum::F64(1.0),
                DATATYPE_INT32 => Datum::I32(1),
                DATATYPE_STRING256 => Datum::Str("x".into()),
                other => panic!("{} has unknown data type {other}", v.name),
            };
            let mut t = Telemetry::default();
            (v.apply)(&mut t, datum);
            assert!(
                t != Telemetry::default(),
                "{} is registered but writes nothing into Telemetry",
                v.name
            );
        }
    }

    /// A datum of the wrong type must be ignored rather than coerced — a
    /// mismatched delivery should leave the field alone, not zero it.
    #[test]
    fn a_mismatched_datum_is_ignored() {
        let v = by_request_id(11).unwrap(); // PLANE LATITUDE, f64
        let mut t = Telemetry {
            latitude_deg: 51.5,
            ..Default::default()
        };
        (v.apply)(&mut t, Datum::Str("nonsense".into()));
        assert_eq!(t.latitude_deg, 51.5);
    }

    #[test]
    fn ground_track_is_registered_and_distinct_from_true_heading() {
        let track = SIMVARS
            .iter()
            .find(|v| v.name == "GPS GROUND TRUE TRACK")
            .expect(
                "GPS GROUND TRUE TRACK must be registered — see the note on Telemetry::ground_track_deg",
            );

        let mut t = Telemetry::default();
        (track.apply)(&mut t, Datum::F64(275.0));
        assert_eq!(t.ground_track_deg, 275.0);
        assert_eq!(
            t.heading_true_deg, 0.0,
            "ground track must not be written into the true-heading field"
        );
    }

    #[test]
    fn names_and_units_fit_their_wire_fields() {
        for v in SIMVARS {
            assert!(v.name.len() < 256, "{} name too long for the field", v.name);
            assert!(
                v.units.len() < 256,
                "{} units too long for the field",
                v.name
            );
            assert!(!v.name.is_empty());
        }
    }
}
