//! Which profile belongs to which physical controller, and which do not.
//!
//! MSFS creates an input profile per device it sees, and on Linux it does not
//! always see the same device the same way twice. The result is profiles that
//! look like duplicates in the controls menu, only one of which the sim is
//! actually using — and no way, from inside the sim, to tell which.
//!
//! # Two things that look identical and are not
//!
//! Both appear as "two profiles with the same device name". They need
//! different answers, and conflating them is how someone deletes the wrong one.
//!
//! **Alternate profiles.** Same device, *same* `GUID`, two names. This is
//! MSFS's own feature — several named profiles for one stick, one selected at
//! a time. Nothing is wrong and nothing should be cleaned up.
//!
//! **Duplicate registrations.** Same device, *different* `GUID`s. The sim
//! enrolled one physical controller twice. Only one is live, and the other
//! collects bindings that never fire.
//!
//! # What makes the second case detectable
//!
//! `ProductID` is the USB product id in decimal, so two profiles for one
//! physical device are exactly the ones sharing it. The `GUID` is not reliably
//! a device identity: the same GUID has been observed for **both** a VKB
//! Gladiator (`ProductID` 544) and a Turtle Beach VelocityOne Rudder
//! (`ProductID` 28690). A GUID seen against more than one product id cannot
//! be identifying a device, so profiles carrying it are the suspect ones.
//!
//! # Read-only, and it stays that way
//!
//! This module reports. It does not merge profiles, delete them, rewrite them
//! or pick one for you. Losing bindings to a tidy-up is far worse than living
//! with a duplicate, and only the sim knows which registration it is using.

use crate::{Library, Loaded};
use hangar_core::input::InputDevice;
use std::collections::{BTreeMap, BTreeSet};

/// The GUID MSFS writes for a keyboard. Not a device identity.
pub const KEYBOARD_GUID: &str = "{0}";

/// What the profiles say about one physical device.
#[derive(Debug, Clone)]
pub struct DeviceView {
    /// The USB product id, as MSFS records it and the kernel reports it.
    pub product_id: u32,
    /// The device name from the profile — MSFS's mangled version.
    pub name: String,
    /// The matching connected device, if it is plugged in now.
    pub connected: Option<InputDevice>,
    /// Every profile claiming this product id, most-bound first.
    pub profiles: Vec<ProfileRef>,
    /// Distinct GUIDs across those profiles.
    pub registrations: BTreeSet<String>,
    pub status: Status,
}

/// One profile, summarised for this view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRef {
    pub id: String,
    pub name: String,
    pub guid: String,
    pub bound: usize,
    /// This profile's GUID is recorded against more than one product id, so it
    /// cannot be identifying a device.
    pub guid_is_shared: bool,
}

/// How a device's profiles look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// One profile, one registration. Nothing to say.
    Single,
    /// Several profiles, one registration — MSFS's alternate-profile feature.
    /// Normal, and not something to clean up.
    AlternateProfiles,
    /// Several registrations for one physical device. Only one is live.
    DuplicateRegistrations,
    /// No connected device carries this product id.
    NotConnected,
    /// A keyboard. It has no USB identity worth checking and never duplicates
    /// the way a stick does.
    Keyboard,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Single => "one profile",
            Status::AlternateProfiles => "alternate profiles",
            Status::DuplicateRegistrations => "registered twice",
            Status::NotConnected => "not connected",
            Status::Keyboard => "keyboard",
        }
    }

    /// Whether this is worth drawing attention to.
    pub fn needs_attention(self) -> bool {
        matches!(self, Status::DuplicateRegistrations | Status::NotConnected)
    }
}

impl DeviceView {
    /// The profile with the most bindings — the one most likely to be the live
    /// registration, and the one **not** to touch.
    ///
    /// A guess, and labelled as one wherever it is shown. The sim is the only
    /// thing that knows which registration it is reading.
    pub fn busiest(&self) -> Option<&ProfileRef> {
        self.profiles.first()
    }
}

/// Group a library's profiles by the physical device they claim.
///
/// `devices` is what the kernel currently reports — normally
/// `hangar_core::input::controllers()`. Passing it in rather than reading it
/// here keeps this testable without a controller plugged in.
pub fn survey(library: &Library, devices: &[InputDevice]) -> Vec<DeviceView> {
    // A GUID against more than one product id is not identifying a device.
    let mut products_per_guid: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    for loaded in &library.profiles {
        if let Some(product) = product_id(loaded) {
            products_per_guid
                .entry(loaded.profile.device.guid.as_str())
                .or_default()
                .insert(product);
        }
    }

    let mut grouped: BTreeMap<u32, Vec<&Loaded>> = BTreeMap::new();
    for loaded in &library.profiles {
        if let Some(product) = product_id(loaded) {
            grouped.entry(product).or_default().push(loaded);
        }
    }

    let mut views: Vec<DeviceView> = grouped
        .into_iter()
        .map(|(product_id, mut loaded)| {
            // Most-bound first: the busiest profile is the useful default and
            // the one a person would least want to lose.
            loaded.sort_by(|a, b| {
                b.bound_count()
                    .cmp(&a.bound_count())
                    .then(a.id().cmp(b.id()))
            });

            let is_keyboard = loaded
                .iter()
                .all(|l| l.profile.device.guid == KEYBOARD_GUID);

            let registrations: BTreeSet<String> = loaded
                .iter()
                .map(|l| l.profile.device.guid.clone())
                .collect();

            let connected = devices
                .iter()
                .find(|d| u32::from(d.product) == product_id)
                .cloned();

            let status = if is_keyboard {
                Status::Keyboard
            } else if connected.is_none() {
                Status::NotConnected
            } else if registrations.len() > 1 {
                Status::DuplicateRegistrations
            } else if loaded.len() > 1 {
                Status::AlternateProfiles
            } else {
                Status::Single
            };

            DeviceView {
                product_id,
                name: loaded[0].device().to_string(),
                connected,
                profiles: loaded
                    .iter()
                    .map(|l| ProfileRef {
                        id: l.id().to_string(),
                        name: l.name().to_string(),
                        guid: l.profile.device.guid.clone(),
                        bound: l.bound_count(),
                        guid_is_shared: products_per_guid
                            .get(l.profile.device.guid.as_str())
                            .is_some_and(|products| products.len() > 1),
                    })
                    .collect(),
                registrations,
                status,
            }
        })
        .collect();

    // Things needing attention first, then by name, so the list is stable.
    views.sort_by(|a, b| {
        b.status
            .needs_attention()
            .cmp(&a.status.needs_attention())
            .then(a.name.cmp(&b.name))
    });
    views
}

/// `ProductID` as a number. MSFS writes it in decimal for controllers.
fn product_id(loaded: &Loaded) -> Option<u32> {
    loaded.profile.device.product_id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn profile(name: &str, device: &str, guid: &str, product: &str, bound: usize) -> String {
        let mut actions = String::new();
        for n in 0..3 {
            if n < bound {
                actions.push_str(&format!(
                    concat!(
                        "\t\t<Action ActionName=\"KEY_A{}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
                        "\t\t\t<Primary>\n\t\t\t\t<KEY Information=\"B{}\">{}</KEY>\n\t\t\t</Primary>\n",
                        "\t\t</Action>\n",
                    ),
                    n, n, n
                ));
            } else {
                actions.push_str(&format!(
                    "\t\t<Action ActionName=\"KEY_A{n}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n"
                ));
            }
        }
        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<Version Num=\"-1\"/>\n",
                "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">{}</FriendlyName>\n",
                "<Device DeviceName=\"{}\" GUID=\"{}\" ProductID=\"{}\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                "\t<Context ContextName=\"AIRCRAFT\">\n{}\t</Context>\n",
                "</Device>\n\n",
            ),
            name, device, guid, product, actions
        )
    }

    /// Modelled on a real setup: three sticks, seven profiles.
    fn real_shaped_library() -> Library {
        let files = [
            // Gladiator, registered twice under different GUIDs.
            (
                "inputprofile_0242070832",
                profile(
                    "My General Settings",
                    "Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA",
                    "{9E573ED3-7734-11D2-8D4A-23903FB6BDF7}",
                    "544",
                    1,
                ),
            ),
            (
                "inputprofile_2019199616",
                profile(
                    "My General Camera",
                    "Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA",
                    "{BAAADAD7-7E59-11F1-8003-000044455354}",
                    "544",
                    2,
                ),
            ),
            // S-TECS: two profiles, one registration.
            (
                "inputprofile_1507820032",
                profile(
                    "My General Settings",
                    "VKB-Sim S-TECS",
                    "{979FF95F-7D7C-11F1-8002-000044455354}",
                    "302",
                    1,
                ),
            ),
            (
                "inputprofile_1674509312",
                profile(
                    "My General Settings STECS MAX",
                    "VKB-Sim S-TECS",
                    "{979FF95F-7D7C-11F1-8002-000044455354}",
                    "302",
                    3,
                ),
            ),
            // Pedals: one profile — carrying the same GUID as a Gladiator one.
            (
                "inputprofile_1035869184",
                profile(
                    "My General V1 Pedals",
                    "Turtle Beach VelocityOne Rudder",
                    "{9E573ED3-7734-11D2-8D4A-23903FB6BDF7}",
                    "28690",
                    2,
                ),
            ),
            // Keyboard, twice.
            (
                "inputprofile_0106087416",
                profile("My new general profile", "Keyboard", "{0}", "34891", 3),
            ),
            (
                "inputprofile_1098663168",
                profile("My new general profile_1", "Keyboard", "{0}", "34891", 3),
            ),
        ];

        let mut library = Library::default();
        for (id, xml) in files {
            let p = parse(&xml).expect("fixture parses");
            library.profiles.push(Loaded {
                summary: crate::ProfileSummary {
                    path: id.into(),
                    id: id.to_string(),
                    friendly_name: p.friendly_name.name.clone(),
                    device_name: p.device.name.clone(),
                    locked: false,
                    bytes: 0,
                },
                profile: p,
            });
        }
        library
    }

    fn plugged_in() -> Vec<InputDevice> {
        [(0x231d, 0x0220u16), (0x231d, 0x012e), (0x10f5, 0x7012)]
            .into_iter()
            .map(|(vendor, product)| InputDevice {
                name: format!("device {product:04x}"),
                vendor,
                product,
                handlers: vec!["event0".into(), "js0".into()],
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn a_device_registered_twice_is_told_apart_from_two_profiles_for_one_device() {
        let views = survey(&real_shaped_library(), &plugged_in());

        let gladiator = views.iter().find(|v| v.product_id == 544).unwrap();
        assert_eq!(gladiator.status, Status::DuplicateRegistrations);
        assert_eq!(gladiator.registrations.len(), 2);

        // Same shape on the surface — two profiles, one device name — and a
        // completely different answer.
        let stecs = views.iter().find(|v| v.product_id == 302).unwrap();
        assert_eq!(stecs.status, Status::AlternateProfiles);
        assert_eq!(stecs.registrations.len(), 1);
        assert!(!stecs.status.needs_attention());
    }

    #[test]
    fn a_guid_seen_against_two_products_is_marked_as_not_a_device_identity() {
        // The real tell: {9E573ED3-…} is recorded for both the Gladiator and
        // the pedals, which are different controllers.
        let views = survey(&real_shaped_library(), &plugged_in());
        let gladiator = views.iter().find(|v| v.product_id == 544).unwrap();

        let suspect = gladiator
            .profiles
            .iter()
            .find(|p| p.guid.starts_with("{9E573ED3"))
            .unwrap();
        assert!(suspect.guid_is_shared);

        let genuine = gladiator
            .profiles
            .iter()
            .find(|p| p.guid.starts_with("{BAAADAD7"))
            .unwrap();
        assert!(!genuine.guid_is_shared);
    }

    #[test]
    fn the_busiest_profile_comes_first() {
        let views = survey(&real_shaped_library(), &plugged_in());
        let gladiator = views.iter().find(|v| v.product_id == 544).unwrap();
        let busiest = gladiator.busiest().unwrap();
        assert_eq!(busiest.name, "My General Camera");
        assert_eq!(busiest.bound, 2);
    }

    #[test]
    fn a_keyboard_is_not_reported_as_a_duplicate_or_as_unplugged() {
        // Two keyboard profiles, GUID {0}, and no USB device to match. Neither
        // fact is a problem.
        let views = survey(&real_shaped_library(), &plugged_in());
        let keyboard = views.iter().find(|v| v.product_id == 34891).unwrap();
        assert_eq!(keyboard.status, Status::Keyboard);
        assert!(!keyboard.status.needs_attention());
    }

    #[test]
    fn unplugging_a_stick_is_reported_rather_than_guessed_at() {
        let views = survey(&real_shaped_library(), &[]);
        let gladiator = views.iter().find(|v| v.product_id == 544).unwrap();
        assert_eq!(gladiator.status, Status::NotConnected);
        assert!(gladiator.connected.is_none());
        // Still a keyboard, still fine.
        let keyboard = views.iter().find(|v| v.product_id == 34891).unwrap();
        assert_eq!(keyboard.status, Status::Keyboard);
    }

    #[test]
    fn what_needs_attention_is_listed_first() {
        let views = survey(&real_shaped_library(), &plugged_in());
        assert!(
            views[0].status.needs_attention(),
            "the duplicate should sort to the top, got {:?}",
            views[0].status
        );
    }
}
