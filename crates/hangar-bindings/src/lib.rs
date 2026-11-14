//! MSFS 2024 control bindings.
//!
//! Bindings are **not** in the Proton prefix. They are Steam Cloud userdata:
//! `~/.local/share/Steam/userdata/<id>/2537590/remote/inputprofile_<10 digits>`,
//! plain UTF-8 XML. That location matters three ways — they are cloud-synced,
//! they sit outside the prefix (so every prefix-level backup misses them), and
//! `remotecache.vdf` beside them tracks sync state and parses with the VDF
//! reader `hangar-steam` already has.
//!
//! # The gate
//!
//! Every write to `remote/` sits behind one check:
//!
//! ```text
//! cargo run -p hangar-bindings --example roundtrip
//! ```
//!
//! Read each real profile, write it back out, compare bytes. Unless that is
//! clean on every profile, **nothing writes to `remote/`**: these files often
//! hold the only copy of a lot of binding work, and a file that is
//! semantically right but textually different is a file nobody has tested.
//!
//! # What the format is like
//!
//! Regular to the point of being boring, which is what makes a byte-exact
//! writer feasible. Fixed attribute order, tab indentation, LF endings, no
//! entities, one `<Device>` per file. Two shapes catch people out: there is
//! **no root element** — `<Version>`, `<FriendlyName>` and `<Device>` are
//! siblings, so the file is a fragment rather than well-formed XML — and a
//! **self-closing `<Action/>` means unbound**, which is the state of roughly
//! two thousand actions in every profile.
//!
//! Asobo's axis attribute really is spelled **`AxisSensitivy`**. Preserve the
//! typo on write or the sim ignores the value.

pub mod capture;
pub mod catalog;
pub mod describe;
pub mod devices;
pub mod diff;
pub mod discover;
pub mod guide;
pub mod joystick;
pub mod keyboard;
pub mod library;
pub mod model;
pub mod parse;
pub mod save;
pub mod snapshot;
pub mod write;

pub use capture::{Capture, Captured};
pub use catalog::{Catalog, Query};
pub use describe::{describe, Description, InputKind};
pub use devices::{survey, DeviceView};
pub use diff::{compare, Change, Diff};
pub use discover::{list, peek, remote_dirs, ProfileSummary, MSFS_2024_APP_ID};
pub use guide::to_html;
pub use joystick::Control;
pub use keyboard::KeyCapture;
pub use library::{Library, Loaded};
pub use model::{
    Action, AircraftInfo, Axis, Binding, Context, Device, FriendlyName, Key, Profile, Slot,
};
pub use parse::{parse, read};
pub use save::{Approval, Destination, Refusal, Saved};
pub use snapshot::{Manifest, Snapshot, SnapshotEntry};
pub use write::to_xml;

use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BindingsError {
    #[error(transparent)]
    Steam(#[from] hangar_steam::SteamError),

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is malformed: {reason}")]
    Malformed { path: PathBuf, reason: String },

    /// A parse failure before the file it came from is known — [`parse`] on a
    /// string in memory. [`read`] upgrades these to `Malformed`.
    #[error("malformed input profile: {reason}")]
    Parse { reason: String },

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A snapshot could not be taken, or does not match what it claims.
    ///
    /// Separate from a plain write failure because a snapshot that is present
    /// but wrong is more dangerous than one that is missing: it is the thing
    /// someone would restore from.
    #[error("snapshot problem at {path}: {reason}")]
    Snapshot { path: PathBuf, reason: String },

    /// A profile did not survive parse-and-write unchanged.
    ///
    /// Typed, and never merged into a generic error, because this is the one
    /// failure that must stop a write rather than be logged and stepped over.
    #[error("{path} does not round-trip: {reason}")]
    RoundTrip { path: PathBuf, reason: String },
}

pub type Result<T> = std::result::Result<T, BindingsError>;

impl BindingsError {
    pub fn malformed(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        BindingsError::Malformed {
            path: path.into(),
            reason: reason.into(),
        }
    }

    pub fn parse(reason: impl Into<String>) -> Self {
        BindingsError::Parse {
            reason: reason.into(),
        }
    }
}

/// Where a round-trip first went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Byte offset of the first difference.
    pub offset: usize,
    /// 1-based line number containing it.
    pub line: usize,
    /// The line as the file has it.
    pub original: String,
    /// The line as this crate would write it.
    pub written: String,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {} (byte {}):\n  file:    {:?}\n  written: {:?}",
            self.line, self.offset, self.original, self.written
        )
    }
}

/// Compare two versions of a profile and describe the first difference.
///
/// Returns `None` when they are identical. A whole-file diff would be noise:
/// the writer is a reproduction, so the first divergence is the bug and
/// everything after it is downstream of the same mistake.
pub fn first_divergence(original: &str, written: &str) -> Option<Divergence> {
    if original == written {
        return None;
    }

    let offset = original
        .as_bytes()
        .iter()
        .zip(written.as_bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| original.len().min(written.len()));

    let line = original[..offset].matches('\n').count() + 1;
    Some(Divergence {
        offset,
        line,
        original: nth_line(original, line),
        written: nth_line(written, line),
    })
}

fn nth_line(text: &str, line: usize) -> String {
    text.lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("<past end of file>")
        .to_string()
}

/// Read a profile, write it back out, and confirm the bytes are unchanged.
///
/// The write gate, and a standing regression test.
/// Run it against every real profile before any write path is enabled, and
/// again whenever the model gains a field.
pub fn roundtrip_ok(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let original = std::fs::read_to_string(path).map_err(|source| BindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let profile = parse::parse(&original).map_err(|e| match e {
        BindingsError::Parse { reason } => BindingsError::Malformed {
            path: path.to_path_buf(),
            reason,
        },
        other => other,
    })?;
    let written = write::to_xml(&profile);

    match first_divergence(&original, &written) {
        None => Ok(()),
        Some(d) => Err(BindingsError::RoundTrip {
            path: path.to_path_buf(),
            reason: d.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_does_not_diverge() {
        assert_eq!(first_divergence("same\n", "same\n"), None);
    }

    #[test]
    fn a_divergence_names_the_line_and_shows_both_sides() {
        let a = "one\ntwo\nthree\n";
        let b = "one\nTWO\nthree\n";
        let d = first_divergence(a, b).expect("these differ");
        assert_eq!(d.line, 2);
        assert_eq!(d.original, "two");
        assert_eq!(d.written, "TWO");
    }

    #[test]
    fn a_truncated_write_diverges_at_the_cut() {
        let d = first_divergence("one\ntwo\n", "one\n").expect("these differ");
        assert_eq!(d.line, 2);
        assert_eq!(d.written, "<past end of file>");
    }

    /// A device profile: `<AircraftInfo>`, `<Axes>`, an unbound action, a
    /// bound one, and a `Delay` that is not zero.
    ///
    /// Written with explicit `\t` and `\n` rather than a raw string, because
    /// the whitespace *is* the thing under test and an editor that trims
    /// trailing tabs would silently weaken it.
    const DEVICE_PROFILE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Version Num=\"-1\"/>\n",
        "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">My General V1 Pedals</FriendlyName>\n",
        "<Device DeviceName=\"Turtle Beach VelocityOne Rudder\" GUID=\"{9E573ED3-7734-11D2-8D4A-23903FB6BDF7}\"",
        " ProductID=\"28690\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
        "\t<AircraftInfo CategoryName=\"AIRPLANE\"/>\n",
        "\t<Axes>\n",
        "\t\t<Axis AxisName=\"X\" AxisSensitivy=\"0\" AxisSensitivyMinus=\"0\" AxisNeutral=\"0\"",
        " AxisDeadZone=\"0\" AxisOutDeadZone=\"0\" AxisResponseRate=\"-1\"/>\n",
        "\t</Axes>\n",
        "\t<Context ContextName=\"AIRCRAFT\">\n",
        "\t\t<Action ActionName=\"KEY_ADF_1_DEC\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
        "\t\t<Action ActionName=\"KEY_ELEV_TRIM_UP\" ValueEvent=\"0.000000\" Delay=\"2.000000\" Flag=\"1\">\n",
        "\t\t\t<Primary>\n",
        "\t\t\t\t<KEY Information=\"Joystick Button 8\">7</KEY>\n",
        "\t\t\t</Primary>\n",
        "\t\t</Action>\n",
        "\t</Context>\n",
        "</Device>\n",
        "\n",
    );

    /// A keyboard profile: no `<AircraftInfo>`, no `<Axes>` at all, a chord,
    /// and an action with both slots filled.
    const KEYBOARD_PROFILE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Version Num=\"-1\"/>\n",
        "<FriendlyName PlatformAvailability=\"15\" Locked=\"false\">My new general profile</FriendlyName>\n",
        "<Device DeviceName=\"Keyboard\" GUID=\"{6F1D2B61-D5A0-11CF-BFC7-444553540000}\"",
        " ProductID=\"0\" CompositeID=\"0\" HWVer=\"0.0.0.0\">\n",
        "\t<Context ContextName=\"GLOBAL\">\n",
        "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
        "\t\t\t<Primary>\n",
        "\t\t\t\t<KEY Information=\"Control\">162</KEY>\n",
        "\t\t\t\t<KEY Information=\"Shift\">160</KEY>\n",
        "\t\t\t\t<KEY Information=\"Alt\">164</KEY>\n",
        "\t\t\t\t<KEY Information=\"t\">84</KEY>\n",
        "\t\t\t</Primary>\n",
        "\t\t</Action>\n",
        "\t\t<Action ActionName=\"KEY_FORCE_SAFE_BOOT\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
        "\t\t\t<Primary>\n",
        "\t\t\t\t<KEY Information=\"Control\">162</KEY>\n",
        "\t\t\t</Primary>\n",
        "\t\t\t<Secondary>\n",
        "\t\t\t\t<KEY Information=\"Control\">163</KEY>\n",
        "\t\t\t</Secondary>\n",
        "\t\t</Action>\n",
        "\t</Context>\n",
        "</Device>\n",
        "\n",
    );

    fn assert_roundtrips(fixture: &str) {
        let profile = parse(fixture).expect("fixture parses");
        let written = to_xml(&profile);
        if let Some(d) = first_divergence(fixture, &written) {
            panic!("fixture did not round-trip:\n{d}");
        }
    }

    #[test]
    fn a_device_profile_roundtrips() {
        assert_roundtrips(DEVICE_PROFILE);
    }

    #[test]
    fn a_keyboard_profile_roundtrips() {
        assert_roundtrips(KEYBOARD_PROFILE);
    }

    #[test]
    fn a_missing_axes_element_is_not_the_same_as_an_empty_one() {
        // Keyboard profiles carry no <Axes> at all. Writing an empty one back
        // would be a different file, so the distinction has to survive.
        let keyboard = parse(KEYBOARD_PROFILE).expect("fixture parses");
        assert!(keyboard.device.axes.is_none());
        assert!(keyboard.device.aircraft_info.is_none());
        assert!(!to_xml(&keyboard).contains("<Axes"));

        let device = parse(DEVICE_PROFILE).expect("fixture parses");
        assert_eq!(device.device.axes().len(), 1);
        assert!(device.device.axes()[0].is_default());
    }

    #[test]
    fn a_self_closing_action_is_unbound_and_stays_that_way() {
        let profile = parse(DEVICE_PROFILE).expect("fixture parses");
        let aircraft = profile
            .device
            .context("AIRCRAFT")
            .expect("AIRCRAFT context");

        let unbound = aircraft.action("KEY_ADF_1_DEC").expect("the ADF action");
        assert!(!unbound.is_bound());

        let bound = aircraft
            .action("KEY_ELEV_TRIM_UP")
            .expect("the trim action");
        assert!(bound.is_bound());
        assert_eq!(bound.delay_secs(), Some(2.0));
        assert_eq!(
            bound.primary.as_ref().map(|b| b.label()),
            Some("Joystick Button 8".to_string())
        );

        // One bound, one not, out of two.
        assert_eq!(profile.device.bound().count(), 1);
        assert_eq!(profile.device.actions().count(), 2);
    }

    #[test]
    fn a_chord_keeps_its_order_and_its_codes() {
        let profile = parse(KEYBOARD_PROFILE).expect("fixture parses");
        let global = profile.device.context("GLOBAL").expect("GLOBAL context");
        let jetway = global
            .action("KEY_TOGGLE_JETWAY")
            .expect("the jetway action");
        let primary = jetway.primary.as_ref().expect("a primary binding");
        assert!(primary.is_chord());
        assert_eq!(primary.label(), "Control + Shift + Alt + t");
        assert_eq!(
            primary
                .keys
                .iter()
                .map(|k| k.code_num())
                .collect::<Vec<_>>(),
            vec![Some(162), Some(160), Some(164), Some(84)]
        );
    }

    #[test]
    fn left_and_right_control_are_told_apart_by_code_not_label() {
        // Both slots say "Control". Only the code distinguishes them, which is
        // why Key keeps both halves.
        let profile = parse(KEYBOARD_PROFILE).expect("fixture parses");
        let action = profile
            .device
            .context("GLOBAL")
            .and_then(|c| c.action("KEY_FORCE_SAFE_BOOT"))
            .expect("the safe-boot action");
        let slots: Vec<_> = action
            .bindings()
            .map(|(slot, b)| (slot, b.label(), b.keys[0].code.clone()))
            .collect();
        assert_eq!(
            slots,
            vec![
                (Slot::Primary, "Control".to_string(), "162".to_string()),
                (Slot::Secondary, "Control".to_string(), "163".to_string()),
            ]
        );
    }

    #[test]
    fn an_attribute_we_do_not_model_is_refused_not_dropped() {
        let doctored = DEVICE_PROFILE.replace(
            "<Axis AxisName=\"X\"",
            "<Axis AxisName=\"X\" AxisFutureThing=\"7\"",
        );
        let err = parse(&doctored).expect_err("an unknown attribute must not parse");
        let msg = err.to_string();
        assert!(msg.contains("AxisFutureThing"), "unhelpful error: {msg}");
    }

    #[test]
    fn correcting_asobos_typo_would_break_the_parse() {
        // The point of the test is that nobody can "fix" the spelling in one
        // place without the other place failing loudly.
        let corrected = DEVICE_PROFILE.replace("AxisSensitivy=", "AxisSensitivity=");
        assert!(parse(&corrected).is_err());
    }
}
