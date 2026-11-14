//! Turning `KEY_ELEV_TRIM_UP` into something a person can read.
//!
//! Three thousand `SHOUTING_SNAKE_CASE` identifiers is not a list anyone can
//! browse. Each action wants a title and a short line saying what kind of
//! control it is.
//!
//! # Where the sim's own strings are, and why we cannot have them
//!
//! MSFS keys them `INPUT.ACTION_<NAME>` with an `INPUT.ACTION_<NAME>_DESC`
//! sibling; a few leak into `Packages/fs-base/en-US.locPak`:
//!
//! ```text
//! INPUT.ACTION_KEY_CAMERA_TOGGLE_LOCKABLE      = "TOGGLE CAMERA LOCK"
//! INPUT.ACTION_KEY_CAMERA_TOGGLE_LOCKABLE_DESC = "Toggle the lock/ unlock of lockable camera controls"
//! ```
//!
//! The rest are **not on disk** (not in any `.locPak`, the cache trees or the
//! executable): MSFS 2024 streams its UI strings from Asobo's servers at
//! runtime.
//!
//! So this derives them instead, which works because the names are
//! systematic: nearly all start `KEY_`, and the final token is one of a small
//! set — `SET`, `TOGGLE`, `INC`/`DEC`, and so on.
//!
//! # What is claimed, and what is not
//!
//! The derived summary says **what kind of control it is** — stepped, absolute,
//! an axis, a toggle — because that follows from the naming convention with
//! confidence. It does not claim what the control *does to the aircraft*, or
//! how the sim behaves when you hold it, because the name does not say and
//! inventing it would put guesses next to facts.
//!
//! Where a name genuinely misleads, or where a real sentence earns its place,
//! [`CURATED`] overrides the derivation. That table is deliberately small and
//! is meant to grow from things people actually got wrong.

use std::fmt;

/// What sort of input an action expects, inferred from its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// `KEY_AXIS_ELEV_TRIM_SET` — a physical axis, absolute position.
    Axis,
    /// `KEY_ELEV_TRIM_UP`, `..._INC`, `..._DEC` — one increment per activation.
    Stepped,
    /// `KEY_..._TOGGLE` — flips between states.
    Toggle,
    /// `KEY_..._ON` / `_OFF` / `_SET` — drives to a stated state or value.
    Absolute,
    /// A plain action with no modifier in the name.
    Momentary,
}

impl InputKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InputKind::Axis => "axis",
            InputKind::Stepped => "stepped",
            InputKind::Toggle => "toggle",
            InputKind::Absolute => "absolute",
            InputKind::Momentary => "momentary",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            InputKind::Axis => "Analog axis — absolute position across its travel.",
            InputKind::Stepped => "Stepped — one increment per activation.",
            InputKind::Toggle => "Toggle — flips between states.",
            InputKind::Absolute => "Sets a stated value or state directly.",
            InputKind::Momentary => "Momentary action.",
        }
    }
}

/// Whether a description was written by hand or worked out from the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Curated,
    Derived,
}

/// A readable name and a line about the control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    /// `Elevator Trim Up`.
    pub title: String,
    /// One line about the kind of control, never about the aeroplane.
    pub summary: String,
    pub kind: InputKind,
    pub source: Source,
}

impl fmt::Display for Description {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} — {}", self.title, self.summary)
    }
}

/// Hand-written entries, for names that mislead or deserve a real sentence.
///
/// Keep it short. Every line here is a promise to keep it true, and a derived
/// description that is merely terse is better than a curated one that is
/// stale.
pub const CURATED: &[(&str, &str, &str)] = &[
    (
        "KEY_ELEV_TRIM_UP",
        "Elevator Trim Up",
        "Pitch trim towards nose-up. Stepped: the size of one step belongs to the aircraft, not to this binding.",
    ),
    (
        "KEY_ELEV_TRIM_DN",
        "Elevator Trim Down",
        "Pitch trim towards nose-down. Stepped: the size of one step belongs to the aircraft, not to this binding.",
    ),
    (
        "KEY_AXIS_ELEV_TRIM_SET",
        "Elevator Trim Axis",
        "Pitch trim from a physical axis, absolute across its travel. The analog alternative to the stepped trim keys.",
    ),
    (
        "KEY_ELEVATOR_TRIM_SET",
        "Elevator Trim Set",
        "Sets pitch trim to a given value rather than nudging it.",
    ),
    (
        "KEY_AXIS_RUDDER_SET",
        "Rudder Axis",
        "Rudder from a physical axis. On pedals this is the sliding or twisting axis, not a button.",
    ),
    (
        "KEY_AXIS_LEFT_BRAKE_SET",
        "Left Brake Axis",
        "Left toe brake as an analog axis, so partial braking is possible.",
    ),
    (
        "KEY_AXIS_RIGHT_BRAKE_SET",
        "Right Brake Axis",
        "Right toe brake as an analog axis, so partial braking is possible.",
    ),
    (
        "KEY_PARKING_BRAKES",
        "Parking Brake",
        "Toggles the parking brake.",
    ),
];

/// Describe one action name.
pub fn describe(action: &str) -> Description {
    if let Some((_, title, summary)) = CURATED.iter().find(|(name, _, _)| *name == action) {
        return Description {
            title: (*title).to_string(),
            summary: (*summary).to_string(),
            kind: kind_of(action),
            source: Source::Curated,
        };
    }

    let kind = kind_of(action);
    Description {
        title: titlecase(action),
        summary: kind.summary().to_string(),
        kind,
        source: Source::Derived,
    }
}

/// Work out the input kind from the name's shape.
///
/// Order matters: `KEY_AXIS_ELEV_TRIM_SET` is an axis *and* ends in `SET`, and
/// the axis is the more useful thing to say.
fn kind_of(action: &str) -> InputKind {
    let upper = action.to_ascii_uppercase();
    if upper.contains("_AXIS_") || upper.ends_with("_AXIS") {
        return InputKind::Axis;
    }

    // `_EX1` is a suffix on top of the real verb, so look past it.
    let stem = upper.strip_suffix("_EX1").unwrap_or(&upper);
    let last = stem.rsplit('_').next().unwrap_or(stem);

    match last {
        "INC" | "INCR" | "INCREASE" | "DEC" | "DECR" | "DECREASE" | "UP" | "DN" | "DOWN"
        | "LEFT" | "RIGHT" | "SMALL" | "NEXT" | "PREV" | "PREVIOUS" => InputKind::Stepped,
        "TOGGLE" => InputKind::Toggle,
        "SET" | "ON" | "OFF" | "RESET" | "OPEN" | "CLOSE" | "SELECT" => InputKind::Absolute,
        _ => InputKind::Momentary,
    }
}

/// Expansions for tokens whose plain title case reads as noise.
///
/// Two kinds of entry: abbreviations MSFS shortens (`ELEV`, `DN`, `INCR`) and
/// initialisms that must stay upper case (`ADF`, `COM`, `VOR`). Anything not
/// listed is simply title-cased, which is right far more often than not.
const WORDS: &[(&str, &str)] = &[
    ("ELEV", "Elevator"),
    ("AIL", "Aileron"),
    ("RUD", "Rudder"),
    ("DN", "Down"),
    ("INCR", "Increase"),
    ("DECR", "Decrease"),
    ("INC", "Increase"),
    ("DEC", "Decrease"),
    ("PCT", "Percent"),
    ("POS", "Position"),
    ("ENG", "Engine"),
    ("PROP", "Propeller"),
    ("MIX", "Mixture"),
    ("GEAR", "Gear"),
    ("ALT", "Altitude"),
    ("HDG", "Heading"),
    ("VS", "Vertical Speed"),
    ("AP", "Autopilot"),
    ("ATC", "ATC"),
    ("ADF", "ADF"),
    ("COM", "COM"),
    ("NAV", "NAV"),
    ("DME", "DME"),
    ("OBS", "OBS"),
    ("VOR", "VOR"),
    ("ILS", "ILS"),
    ("GPS", "GPS"),
    ("EFB", "EFB"),
    ("VR", "VR"),
    ("UI", "UI"),
    ("SR", "Screen Reader"),
    ("HZ", "Hz"),
    ("EX1", "(extended)"),
    ("FRACT", "Fractional"),
    ("MAGNETO", "Magneto"),
    ("KOHLSMAN", "Altimeter"),
    ("PFD", "PFD"),
    ("MFD", "MFD"),
    ("FMS", "FMS"),
    ("CDI", "CDI"),
    ("EGT", "EGT"),
    ("IAS", "IAS"),
    ("TAS", "TAS"),
    ("APU", "APU"),
    ("GOV", "Governor"),
    ("G1000", "G1000"),
    ("G3000", "G3000"),
    ("G5000", "G5000"),
];

/// `KEY_ELEV_TRIM_UP` -> `Elevator Trim Up`.
///
/// Falls back to the raw name rather than returning nothing: an empty label
/// looks like a rendering bug.
fn titlecase(action: &str) -> String {
    let expanded = expand(action);
    if expanded.trim().is_empty() {
        action.to_string()
    } else {
        expanded
    }
}

fn expand(action: &str) -> String {
    let stripped = action
        .strip_prefix("KEY_")
        .unwrap_or(action)
        .replace("_AXIS_", "_Axis_");

    stripped
        .split('_')
        .filter(|t| !t.is_empty())
        .map(word_for)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One underscore-separated token, made readable.
///
/// MSFS glues an instance number straight onto the word — `MIXTURE3`,
/// `THROTTLE2`, `NAV1`. Splitting the digits off both reads better and lets
/// the abbreviation table match, which it cannot do against `NAV1`.
fn word_for(token: &str) -> String {
    let upper = token.to_ascii_uppercase();

    // The table wins over the digit rule, or `EX1` becomes "Ex 1" and
    // `G1000` becomes "G 1000".
    if let Some((_, word)) = WORDS.iter().find(|(k, _)| *k == upper) {
        return (*word).to_string();
    }

    let digits = token.len() - token.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 && digits < token.len() {
        let (word, number) = token.split_at(token.len() - digits);
        return format!("{} {number}", word_for(word));
    }

    let mut chars = token.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curated_entry_wins_over_the_derivation() {
        let d = describe("KEY_ELEV_TRIM_UP");
        assert_eq!(d.source, Source::Curated);
        assert_eq!(d.title, "Elevator Trim Up");
        assert!(d.summary.contains("nose-up"));
    }

    #[test]
    fn abbreviations_are_expanded_and_initialisms_are_not_mangled() {
        assert_eq!(titlecase("KEY_ELEV_TRIM_DN"), "Elevator Trim Down");
        assert_eq!(
            titlecase("KEY_COM_RADIO_WHOLE_INC"),
            "COM Radio Whole Increase"
        );
        assert_eq!(titlecase("KEY_ADF_1_DEC"), "ADF 1 Decrease");
        assert_eq!(titlecase("KEY_TOGGLE_JETWAY"), "Toggle Jetway");
    }

    #[test]
    fn an_instance_number_is_split_off_the_word() {
        // MSFS writes MIXTURE3, not MIXTURE_3, and "Mixture3" reads badly.
        assert_eq!(titlecase("KEY_MIXTURE3_INCR"), "Mixture 3 Increase");
        assert_eq!(
            titlecase("KEY_PROP_PITCH3_DECR"),
            "Propeller Pitch 3 Decrease"
        );
        // And it lets the abbreviation table reach a token it otherwise could not.
        assert_eq!(
            titlecase("KEY_TOGGLE_GPS_DRIVES_NAV1"),
            "Toggle GPS Drives NAV 1"
        );
    }

    #[test]
    fn a_name_that_is_only_digits_is_left_alone() {
        assert_eq!(titlecase("KEY_FLAPS_2"), "Flaps 2");
        assert_eq!(titlecase("KEY_VIEW_AUX_00"), "View Aux 00");
    }

    #[test]
    fn a_name_in_the_table_is_not_split_by_the_digit_rule() {
        // Both of these are single words that happen to end in digits, so the
        // abbreviation table has to be consulted before the digit rule.
        assert_eq!(
            titlecase("KEY_G1000_PFD_FLIGHTPLAN_BUTTON"),
            "G1000 PFD Flightplan Button"
        );
        assert_eq!(
            titlecase("KEY_THROTTLE2_INCREASE_SMALL_EX1"),
            "Throttle 2 Increase Small (extended)"
        );
    }

    #[test]
    fn the_input_kind_follows_the_naming_convention() {
        assert_eq!(kind_of("KEY_AXIS_ELEV_TRIM_SET"), InputKind::Axis);
        assert_eq!(kind_of("KEY_ELEV_TRIM_UP"), InputKind::Stepped);
        assert_eq!(kind_of("KEY_COM_RADIO_WHOLE_INC"), InputKind::Stepped);
        assert_eq!(kind_of("KEY_ANTI_ICE_TOGGLE"), InputKind::Toggle);
        assert_eq!(kind_of("KEY_ELEVATOR_TRIM_SET"), InputKind::Absolute);
        assert_eq!(kind_of("KEY_PAUSE_OFF"), InputKind::Absolute);
        assert_eq!(kind_of("KEY_ENGINE_PRIMER"), InputKind::Momentary);
    }

    #[test]
    fn an_axis_is_an_axis_even_though_it_also_ends_in_set() {
        // The trap: KEY_AXIS_ELEV_TRIM_SET matches both rules, and "axis" is
        // the more useful half.
        let d = describe("KEY_THROTTLE2_AXIS_SET_EX1");
        assert_eq!(d.kind, InputKind::Axis);
    }

    #[test]
    fn the_extended_suffix_does_not_hide_the_verb() {
        assert_eq!(kind_of("KEY_PROP_PITCH_DECREASE_EX1"), InputKind::Stepped);
        assert_eq!(kind_of("KEY_RUDDER_TRIM_SET_EX1"), InputKind::Absolute);
    }

    #[test]
    fn a_derived_description_says_nothing_about_the_aeroplane() {
        // It describes the control, not the effect. Anything more would be a
        // guess sitting next to facts.
        let d = describe("KEY_HELICOPTER_ENGINE_1_BEEP_TRIM_INCREASE");
        assert_eq!(d.source, Source::Derived);
        assert_eq!(d.kind, InputKind::Stepped);
        assert_eq!(d.summary, "Stepped — one increment per activation.");
        assert_eq!(d.title, "Helicopter Engine 1 Beep Trim Increase");
    }

    #[test]
    fn nothing_produces_an_empty_title() {
        for action in [
            "KEY_",
            "KEY_X",
            "MENU_FLY",
            "DEBUG_MENU_VALID",
            "KEY_AXIS_",
            "TOGGLE",
        ] {
            assert!(
                !describe(action).title.trim().is_empty(),
                "empty title for {action}"
            );
        }
    }
}
