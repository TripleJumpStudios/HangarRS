//! The telemetry snapshot published to consumers.

use std::time::{Duration, Instant};

/// How long after the last update the snapshot is considered stale.
///
/// It matters because the sim stops sending during a pause, a menu, or a loading
/// screen without closing the socket: a consumer that keeps broadcasting the
/// last frame is telling an EFB the aircraft is parked at the last known
/// position, which is worse than telling it nothing.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(5);

/// One value delivered for a SimVar.
#[derive(Debug, Clone, PartialEq)]
pub enum Datum {
    F64(f64),
    I32(i32),
    Str(String),
}

/// Everything this client reads from the sim.
///
/// Plain data with no interior mutability and no toolkit types — it crosses a
/// `watch` channel and is cloned by consumers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Telemetry {
    // ── Position & motion ────────────────────────────────────────────────────
    pub altitude_ft: f64,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub airspeed_kt: f64,
    pub on_ground: bool,
    pub heading_mag_deg: f64,
    pub heading_true_deg: f64,
    /// Track over the ground.
    ///
    /// **Not** the same as [`Self::heading_true_deg`], and the distinction is
    /// the reason this field exists: true heading in the XGPS track field is
    /// wrong by the crab angle in any crosswind. Anything drawing a ground track wants this one.
    pub ground_track_deg: f64,
    pub vertical_speed_fpm: f64,
    pub ground_speed_kt: f64,
    pub bank_deg: f64,
    pub pitch_deg: f64,

    // ── Aircraft identity ────────────────────────────────────────────────────
    pub title: String,
    pub atc_type: String,
    pub atc_model: String,
    pub atc_id: String,
    pub atc_airline: String,
    pub atc_flight_number: String,
    pub gps_wp_prev_id: String,
    pub gps_wp_next_id: String,

    // ── Atmosphere ───────────────────────────────────────────────────────────
    pub wind_velocity_kts: f64,
    pub wind_direction_deg: f64,
    pub ambient_temp_c: f64,
    pub total_air_temp_c: f64,
    pub sea_level_pressure_mbar: f64,
    pub ambient_visibility_m: f64,
    pub magvar_deg: f64,
    pub ground_altitude_ft: f64,
    pub alt_above_ground_ft: f64,
    pub ap_altitude_lock_ft: f64,

    // ── Weights & fuel ───────────────────────────────────────────────────────
    pub total_weight_lbs: f64,
    pub max_gross_weight_lbs: f64,
    pub empty_weight_lbs: f64,
    pub fuel_total_qty_gal: f64,
    pub fuel_total_qty_lbs: f64,
    pub fuel_flow_pph: f64,
    pub fuel_flow_gph: f64,

    // ── Aircraft properties ──────────────────────────────────────────────────
    pub num_engines: u8,
    pub engine_type: u8,

    /// When the sim last delivered any value. `None` before the first packet.
    ///
    /// Included in the derived [`PartialEq`], so two snapshots taken at
    /// different times never compare equal. A consumer that wants to know
    /// whether the *aircraft state* changed — rather than whether a frame
    /// arrived — should use [`Self::same_state_as`] instead of `==`.
    pub updated_at: Option<Instant>,
}

impl Telemetry {
    /// How long since the sim last sent anything.
    pub fn age(&self) -> Option<Duration> {
        self.updated_at.map(|t| t.elapsed())
    }

    /// Whether this snapshot is too old to act on.
    ///
    /// A snapshot that has never been filled is always stale.
    pub fn is_stale(&self, threshold: Duration) -> bool {
        match self.updated_at {
            Some(t) => t.elapsed() > threshold,
            None => true,
        }
    }

    /// The snapshot if it is fresh, otherwise `None`.
    ///
    /// The intended way to consume this: a bridge that must not transmit a
    /// frozen position calls this rather than reading fields directly.
    pub fn if_fresh(&self, threshold: Duration) -> Option<&Self> {
        (!self.is_stale(threshold)).then_some(self)
    }
}

/// Compare aircraft state while ignoring the update timestamp.
impl Telemetry {
    /// Field-wise equality excluding [`Self::updated_at`].
    pub fn same_state_as(&self, other: &Self) -> bool {
        let (mut a, mut b) = (self.clone(), other.clone());
        a.updated_at = None;
        b.updated_at = None;
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_never_updated_snapshot_is_stale() {
        let t = Telemetry::default();
        assert!(t.is_stale(DEFAULT_STALE_AFTER));
        assert_eq!(t.age(), None);
        assert!(t.if_fresh(DEFAULT_STALE_AFTER).is_none());
    }

    #[test]
    fn a_just_updated_snapshot_is_fresh() {
        let t = Telemetry {
            updated_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(!t.is_stale(DEFAULT_STALE_AFTER));
        assert!(t.if_fresh(DEFAULT_STALE_AFTER).is_some());
    }

    #[test]
    fn staleness_is_measured_against_the_threshold() {
        let t = Telemetry {
            updated_at: Instant::now().checked_sub(Duration::from_secs(10)),
            ..Default::default()
        };
        assert!(t.is_stale(DEFAULT_STALE_AFTER));
        assert!(!t.is_stale(Duration::from_secs(30)));
    }

    #[test]
    fn state_comparison_ignores_the_timestamp() {
        let a = Telemetry {
            altitude_ft: 3500.0,
            updated_at: Some(Instant::now()),
            ..Default::default()
        };
        let b = Telemetry {
            altitude_ft: 3500.0,
            updated_at: None,
            ..Default::default()
        };
        assert!(a.same_state_as(&b));

        let c = Telemetry {
            altitude_ft: 3501.0,
            ..b.clone()
        };
        assert!(!a.same_state_as(&c));
    }
}
