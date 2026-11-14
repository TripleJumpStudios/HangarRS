//! The shape of an MSFS input profile.
//!
//! # Why the numbers are strings
//!
//! Every attribute value is stored **verbatim, as the file wrote it**, and the
//! typed view is an accessor on top: a profile read and written unchanged must
//! be byte-identical, because the file it goes back to is Steam Cloud userdata
//! and often the only copy of the user's binding work.
//!
//! Asobo writes `Delay="0.000000"`, not `Delay="0"`. A parse to `f32` and a
//! re-`format!` gets that right only by accident, and gets it wrong the first
//! time a value carries seven decimals or a leading `+`. Strings cannot drift.
//!
//! So: read [`Action::delay_secs`], write [`Action::set_delay_secs`], and the
//! formatting stays Asobo's problem rather than ours.

/// A parsed input profile — one file under Steam Cloud `remote/`.
///
/// There is no single root element: `<Version>`, `<FriendlyName>` and
/// `<Device>` are siblings at the top level, so the document is a fragment and
/// not well-formed XML. Any parser that insists on one root will reject every
/// real file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The `version` pseudo-attribute of the XML declaration — `1.0`.
    pub xml_version: String,
    /// The `encoding` pseudo-attribute — `UTF-8` on every observed profile.
    pub encoding: String,
    /// `<Version Num="-1"/>`. Not the XML version; MSFS's own schema counter,
    /// and `-1` in every file seen.
    pub version_num: String,
    pub friendly_name: FriendlyName,
    pub device: Device,
}

/// `<FriendlyName PlatformAvailability="1" Locked="false">…</FriendlyName>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendlyName {
    /// `1` is PC-only, `15` all platforms.
    pub platform_availability: String,
    /// `true` marks a vendor preset, which the sim will not let you edit.
    pub locked: String,
    /// The name shown in the sim's controls menu.
    pub name: String,
}

impl FriendlyName {
    pub fn is_locked(&self) -> bool {
        self.locked.eq_ignore_ascii_case("true")
    }

    /// True when the profile is offered on every platform, not just PC.
    pub fn all_platforms(&self) -> bool {
        self.platform_availability == "15"
    }
}

/// `<Device …>` — the one device a profile binds, and everything under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// As the driver reports it: `Turtle Beach VelocityOne Rudder`,
    /// `VKB-Sim (C) Alex Oz 2023 S-TECS MODERN THROTTLE MAX STEM`, `Keyboard`.
    pub name: String,
    pub guid: String,
    pub product_id: String,
    pub composite_id: String,
    pub hw_ver: String,
    /// `<AircraftInfo CategoryName="AIRPLANE"/>`, absent on keyboard profiles
    /// and on some device ones.
    pub aircraft_info: Option<AircraftInfo>,
    /// `None` when the file carries no `<Axes>` element at all, which is how
    /// keyboard profiles are written — distinct from a device that has the
    /// element with nothing in it.
    pub axes: Option<Vec<Axis>>,
    pub contexts: Vec<Context>,
}

impl Device {
    pub fn axes(&self) -> &[Axis] {
        self.axes.as_deref().unwrap_or(&[])
    }

    pub fn context(&self, name: &str) -> Option<&Context> {
        self.contexts.iter().find(|c| c.name == name)
    }

    /// Every action across every context, with the context it came from.
    pub fn actions(&self) -> impl Iterator<Item = (&Context, &Action)> {
        self.contexts
            .iter()
            .flat_map(|c| c.actions.iter().map(move |a| (c, a)))
    }

    /// Only the actions that are actually bound to something.
    pub fn bound(&self) -> impl Iterator<Item = (&Context, &Action)> {
        self.actions().filter(|(_, a)| a.is_bound())
    }
}

/// `<AircraftInfo CategoryName="AIRPLANE"/>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AircraftInfo {
    pub category_name: String,
}

/// One axis' response curve.
///
/// The attribute really is spelled `AxisSensitivy`. Correcting the typo on
/// write makes the sim ignore the value, silently, and the axis reverts to
/// linear — so it is preserved deliberately and asserted in a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Axis {
    /// `X`, `Y`, `Z`, `RX`, `RY`, `RZ`, `U`, `V`.
    pub name: String,
    pub sensitivity: String,
    pub sensitivity_minus: String,
    pub neutral: String,
    pub dead_zone: String,
    pub out_dead_zone: String,
    /// `-1` means the sim's default rate.
    pub response_rate: String,
}

impl Axis {
    /// True when every curve value is at its shipped default, so the axis has
    /// never been touched. Worth knowing before offering to tune one.
    pub fn is_default(&self) -> bool {
        self.sensitivity == "0"
            && self.sensitivity_minus == "0"
            && self.neutral == "0"
            && self.dead_zone == "0"
            && self.out_dead_zone == "0"
            && self.response_rate == "-1"
    }
}

/// `<Context ContextName="AIRCRAFT">` — one of MSFS's binding groups.
///
/// The list runs well past the settings menu: `DEBUG`, `DEVMODE` and
/// `3RD_PARTY` are all contexts, and the actions inside them are bindable
/// without ever appearing in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub name: String,
    pub actions: Vec<Action>,
}

impl Context {
    pub fn action(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.name == name)
    }

    pub fn action_mut(&mut self, name: &str) -> Option<&mut Action> {
        self.actions.iter_mut().find(|a| a.name == name)
    }
}

/// One bindable action.
///
/// A **self-closing `<Action/>` means unbound**, and that is the common case
/// by a long way: roughly two thousand actions per profile sit in the file
/// with nothing attached to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// `KEY_ELEV_TRIM_UP`, `KEY_AXIS_RUDDER_SET`, …
    pub name: String,
    pub value_event: String,
    /// Seconds. `0.000000` on every keyboard binding; `2.000000` on every
    /// device binding observed, which suggests it is a default the device
    /// binding UI writes rather than anything a person chose.
    pub delay: String,
    /// A bit field. `2` is overwhelmingly the common value; `8194` marks the
    /// global/menu actions, `1` and `2050` appear on aircraft controls.
    pub flag: String,
    pub primary: Option<Binding>,
    pub secondary: Option<Binding>,
}

impl Action {
    /// An unbound action, written back as `<Action …/>`.
    pub fn unbound(name: impl Into<String>) -> Self {
        Action {
            name: name.into(),
            value_event: format_msfs_f32(0.0),
            delay: format_msfs_f32(0.0),
            flag: "2".to_string(),
            primary: None,
            secondary: None,
        }
    }

    pub fn is_bound(&self) -> bool {
        self.primary.is_some() || self.secondary.is_some()
    }

    pub fn delay_secs(&self) -> Option<f32> {
        self.delay.parse().ok()
    }

    pub fn set_delay_secs(&mut self, secs: f32) {
        self.delay = format_msfs_f32(secs);
    }

    pub fn value_event_f32(&self) -> Option<f32> {
        self.value_event.parse().ok()
    }

    pub fn flag_bits(&self) -> Option<u32> {
        self.flag.parse().ok()
    }

    /// Put a binding in a slot.
    ///
    /// Binding the secondary slot on an action with an empty primary fills the
    /// **primary** instead: `<Secondary>` never appears without `<Primary>` in
    /// any observed profile, and MSFS's own UI cannot produce one, so writing
    /// that shape would be untested territory.
    pub fn bind(&mut self, slot: Slot, binding: Binding) -> Slot {
        let slot = match slot {
            Slot::Secondary if self.primary.is_none() => Slot::Primary,
            other => other,
        };
        match slot {
            Slot::Primary => self.primary = Some(binding),
            Slot::Secondary => self.secondary = Some(binding),
        }
        slot
    }

    /// Empty a slot.
    ///
    /// Clearing the primary while a secondary exists promotes the secondary,
    /// for the same reason [`Action::bind`] does not create a lone one.
    pub fn unbind(&mut self, slot: Slot) {
        match slot {
            Slot::Primary => {
                self.primary = self.secondary.take();
            }
            Slot::Secondary => self.secondary = None,
        }
    }

    /// The bindings that exist, primary first.
    pub fn bindings(&self) -> impl Iterator<Item = (Slot, &Binding)> {
        self.primary
            .iter()
            .map(|b| (Slot::Primary, b))
            .chain(self.secondary.iter().map(|b| (Slot::Secondary, b)))
    }
}

/// Which of an action's two binding slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Primary,
    Secondary,
}

impl Slot {
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::Primary => "Primary",
            Slot::Secondary => "Secondary",
        }
    }
}

/// What is in one slot: one key, or several forming a chord.
///
/// `<Secondary>` never appears without `<Primary>` and never before it in any
/// real profile, which is why the two are separate fields rather than a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub keys: Vec<Key>,
}

impl Binding {
    pub fn single(key: Key) -> Self {
        Binding { keys: vec![key] }
    }

    pub fn is_chord(&self) -> bool {
        self.keys.len() > 1
    }

    /// The human-readable form, as the sim's own labels spell it:
    /// `Control + Shift + Alt + t`, `Joystick Button 8`.
    pub fn label(&self) -> String {
        self.keys
            .iter()
            .map(|k| k.information.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// `<KEY Information="Joystick Button 8">7</KEY>`
///
/// `Information` is the label MSFS shows; the element text is the raw code the
/// sim matches on. Both are needed — the label alone is ambiguous (left and
/// right Control are both `Control`, codes 162 and 163) and the code alone is
/// unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub information: String,
    pub code: String,
}

impl Key {
    pub fn new(information: impl Into<String>, code: impl Into<String>) -> Self {
        Key {
            information: information.into(),
            code: code.into(),
        }
    }

    pub fn code_num(&self) -> Option<i64> {
        self.code.parse().ok()
    }
}

/// Format a float the way MSFS writes them: always six decimal places.
pub fn format_msfs_f32(v: f32) -> String {
    format!("{v:.6}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_are_written_with_asobos_six_decimals() {
        assert_eq!(format_msfs_f32(0.0), "0.000000");
        assert_eq!(format_msfs_f32(2.0), "2.000000");
        assert_eq!(format_msfs_f32(0.5), "0.500000");
    }

    #[test]
    fn a_chord_reads_the_way_the_sim_spells_it() {
        let b = Binding {
            keys: vec![
                Key::new("Control", "162"),
                Key::new("Shift", "160"),
                Key::new("t", "84"),
            ],
        };
        assert_eq!(b.label(), "Control + Shift + t");
        assert!(b.is_chord());
    }

    #[test]
    fn a_lone_secondary_cannot_be_created() {
        // No real profile has one, and MSFS's UI cannot make one.
        let mut a = Action::unbound("KEY_FLAPS_UP");
        let landed = a.bind(Slot::Secondary, Binding::single(Key::new("j", "74")));
        assert_eq!(landed, Slot::Primary);
        assert!(a.primary.is_some());
        assert!(a.secondary.is_none());
    }

    #[test]
    fn clearing_the_primary_promotes_the_secondary() {
        let mut a = Action::unbound("KEY_FORCE_SAFE_BOOT");
        a.bind(Slot::Primary, Binding::single(Key::new("Control", "162")));
        a.bind(Slot::Secondary, Binding::single(Key::new("Control", "163")));

        a.unbind(Slot::Primary);
        assert_eq!(
            a.primary.as_ref().map(|b| b.keys[0].code.clone()),
            Some("163".to_string()),
            "the secondary should have moved up"
        );
        assert!(a.secondary.is_none());

        a.unbind(Slot::Primary);
        assert!(!a.is_bound());
    }

    #[test]
    fn an_unbound_action_stays_unbound() {
        let a = Action::unbound("KEY_ELEV_TRIM_UP");
        assert!(!a.is_bound());
        assert_eq!(a.delay, "0.000000");
    }
}
