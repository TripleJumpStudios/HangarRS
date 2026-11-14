//! The whole action catalogue, searchable, and what shares a control with what.
//!
//! A profile holds around two thousand actions, nearly all unbound, many in
//! contexts like `DEBUG`, `DEVMODE` and `3RD_PARTY` that the sim's settings
//! menu never shows. Making that searchable is most of what a bindings tab is
//! for.
//!
//! # Why there is no "conflicts" list
//!
//! Pointed at real profiles, a conflict detector reports findings that are
//! almost all fine: Asobo's keyboard defaults (`MENU` binds `f` to four
//! screen-reader actions, each live on a different screen) and deliberate
//! axis ganging (one lever driving two engines' throttles). A detector that
//! cries wolf trains people to ignore it. So this reports **shared bindings**,
//! neutrally, with the axis-ganging case labelled, and leaves the judgement
//! to the person who made them.

use crate::model::*;
use std::collections::BTreeMap;

/// One action, flattened, with the profile and context it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `inputprofile_1035869184`.
    pub profile_id: String,
    /// The name the sim shows for the profile.
    pub profile_name: String,
    /// The device, as its driver reports it.
    pub device: String,
    pub context: String,
    pub action: String,
    /// `Joystick Button 8`, `Control + Shift + t`. `None` when the slot is
    /// empty, which is the usual case.
    pub primary: Option<String>,
    pub secondary: Option<String>,
    /// Kept because the trim question wants them, and because an editor that
    /// cannot see them cannot preserve them.
    pub delay: String,
    pub flag: String,
}

impl Entry {
    pub fn is_bound(&self) -> bool {
        self.primary.is_some() || self.secondary.is_some()
    }

    /// Both labels, primary first, for display in one column.
    pub fn label(&self) -> String {
        match (&self.primary, &self.secondary) {
            (Some(p), Some(s)) => format!("{p}  /  {s}"),
            (Some(p), None) => p.clone(),
            (None, Some(s)) => format!("/  {s}"),
            (None, None) => String::new(),
        }
    }
}

/// Every action from every profile handed to it.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: Vec<Entry>,
}

/// Which actions to include in a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bound {
    /// Everything.
    #[default]
    Any,
    /// Only actions with something in a slot.
    Only,
    /// Only actions with both slots empty — the two thousand the settings menu
    /// never shows.
    Never,
}

/// A search over the catalogue. Every field is optional; an empty `Query`
/// matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Case-insensitive substring, matched against the action name, the
    /// binding labels and the context.
    pub text: String,
    pub context: Option<String>,
    /// One profile, by its `inputprofile_…` id.
    ///
    /// Not the same as filtering by device: several profiles can share one
    /// device name, and filtering by device returns all of them.
    pub profile: Option<String>,
    /// Every profile for one device.
    pub device: Option<String>,
    pub bound: Bound,
}

impl Catalog {
    /// Build from profiles already read, paired with the id they came from.
    pub fn from_profiles<'a>(profiles: impl IntoIterator<Item = (&'a str, &'a Profile)>) -> Self {
        let mut entries = Vec::new();
        for (id, profile) in profiles {
            for (context, action) in profile.device.actions() {
                entries.push(Entry {
                    profile_id: id.to_string(),
                    profile_name: profile.friendly_name.name.clone(),
                    device: profile.device.name.clone(),
                    context: context.name.clone(),
                    action: action.name.clone(),
                    primary: action.primary.as_ref().map(|b| b.label()),
                    secondary: action.secondary.as_ref().map(|b| b.label()),
                    delay: action.delay.clone(),
                    flag: action.flag.clone(),
                });
            }
        }
        Catalog { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every context name seen, sorted, deduplicated.
    pub fn contexts(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.entries.iter().map(|e| e.context.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Every device name seen, sorted, deduplicated.
    pub fn devices(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.entries.iter().map(|e| e.device.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Distinct action names across every profile.
    ///
    /// Lower than [`Catalog::len`] because the same action appears once per
    /// profile.
    pub fn distinct_actions(&self) -> usize {
        let mut names: Vec<&str> = self.entries.iter().map(|e| e.action.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names.len()
    }

    pub fn bound_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_bound()).count()
    }

    /// Everything matching a query, in catalogue order.
    pub fn search(&self, query: &Query) -> Vec<&Entry> {
        self.search_indices(query)
            .into_iter()
            .map(|i| &self.entries[i])
            .collect()
    }

    /// [`Catalog::search`] as positions in [`Catalog::entries`], for a caller
    /// that keeps the answer across frames without borrowing the catalogue.
    pub fn search_indices(&self, query: &Query) -> Vec<usize> {
        let needle = query.text.to_ascii_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| match query.bound {
                Bound::Any => true,
                Bound::Only => e.is_bound(),
                Bound::Never => !e.is_bound(),
            })
            .filter(|(_, e)| query.context.as_deref().is_none_or(|c| e.context == c))
            .filter(|(_, e)| query.profile.as_deref().is_none_or(|p| e.profile_id == p))
            .filter(|(_, e)| query.device.as_deref().is_none_or(|d| e.device == d))
            .filter(|(_, e)| needle.is_empty() || matches(e, &needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// How many matches fall in each context, for the same query.
    ///
    /// Seeing that "trim" is 4 in `AIRCRAFT` and 1 in `3RD_PARTY` says where to
    /// look without clicking through every context first.
    pub fn counts_by_context(&self, query: &Query) -> BTreeMap<&str, usize> {
        let mut counts = BTreeMap::new();
        let mut without_context = query.clone();
        without_context.context = None;
        for entry in self.search(&without_context) {
            *counts.entry(entry.context.as_str()).or_insert(0) += 1;
        }
        counts
    }
}

fn matches(entry: &Entry, needle: &str) -> bool {
    let haystacks = [
        Some(entry.action.as_str()),
        Some(entry.context.as_str()),
        entry.primary.as_deref(),
        entry.secondary.as_deref(),
    ];
    haystacks
        .into_iter()
        .flatten()
        .any(|h| h.to_ascii_lowercase().contains(needle))
}

/// One control driving more than one action in the same context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    pub profile_id: String,
    pub profile_name: String,
    pub context: String,
    /// The control, as the sim labels it.
    pub label: String,
    /// Every action it drives, in file order.
    pub uses: Vec<(String, Slot)>,
    pub kind: Kind,
}

/// What sort of sharing this is. Not a severity — a description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One physical axis feeding several `_AXIS_…_SET` actions. Ganging two
    /// engines to one throttle lever looks exactly like this and is a normal
    /// thing to want.
    Axis,
    /// A button or chord on several actions. Worth a look; still often
    /// deliberate, and Asobo's own keyboard defaults are full of it.
    Button,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Axis => "axis ganged",
            Kind::Button => "button shared",
        }
    }
}

/// Find every control bound to more than one action within a context.
///
/// Same context only. Whether two contexts are ever live at once is not
/// something this crate knows, and guessing would put invented findings next
/// to real ones.
pub fn shared(profile_id: &str, profile: &Profile) -> Vec<Shared> {
    let mut found = Vec::new();

    for context in &profile.device.contexts {
        // Keyed on the codes, not the labels: left and right Control are both
        // labelled "Control" and are not the same key.
        let mut by_keys: BTreeMap<Vec<String>, Vec<(String, Slot, String)>> = BTreeMap::new();

        for action in &context.actions {
            for (slot, binding) in action.bindings() {
                if binding.keys.is_empty() {
                    continue;
                }
                let codes: Vec<String> = binding.keys.iter().map(|k| k.code.clone()).collect();
                by_keys.entry(codes).or_default().push((
                    action.name.clone(),
                    slot,
                    binding.label(),
                ));
            }
        }

        for (_, uses) in by_keys {
            if uses.len() < 2 {
                continue;
            }
            let label = uses[0].2.clone();
            let kind = if uses.iter().all(|(action, _, l)| is_axis(action, l)) {
                Kind::Axis
            } else {
                Kind::Button
            };
            found.push(Shared {
                profile_id: profile_id.to_string(),
                profile_name: profile.friendly_name.name.clone(),
                context: context.name.clone(),
                label,
                uses: uses.into_iter().map(|(a, s, _)| (a, s)).collect(),
                kind,
            });
        }
    }

    found
}

/// Whether an action and its input both look like an axis.
///
/// Two signals rather than one. The action name is the stable half —
/// `KEY_THROTTLE2_AXIS_SET_EX1` — and the label is the direct half, because
/// what matters is that a *physical axis* feeds it. The label is English and
/// could be localised, so it is never trusted alone.
fn is_axis(action: &str, label: &str) -> bool {
    action.contains("AXIS") || label.contains("Axis")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn profile(name: &str, body: &str) -> Profile {
        let text = format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<Version Num=\"-1\"/>\n",
                "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">{}</FriendlyName>\n",
                "<Device DeviceName=\"STECS\" GUID=\"{{9E5}}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                "{}",
                "</Device>\n\n",
            ),
            name, body
        );
        parse(&text).expect("fixture parses")
    }

    fn bound(name: &str, info: &str, code: &str) -> String {
        format!(
            concat!(
                "\t\t<Action ActionName=\"{}\" ValueEvent=\"0.000000\" Delay=\"2.000000\" Flag=\"2\">\n",
                "\t\t\t<Primary>\n\t\t\t\t<KEY Information=\"{}\">{}</KEY>\n\t\t\t</Primary>\n",
                "\t\t</Action>\n",
            ),
            name, info, code
        )
    }

    fn unbound(name: &str) -> String {
        format!(
            "\t\t<Action ActionName=\"{name}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n"
        )
    }

    fn context(name: &str, actions: &str) -> String {
        format!("\t<Context ContextName=\"{name}\">\n{actions}\t</Context>\n")
    }

    fn sample() -> Profile {
        profile(
            "STECS MAX",
            &format!(
                "{}{}",
                context(
                    "AIRCRAFT",
                    &format!(
                        "{}{}{}",
                        bound("KEY_ELEV_TRIM_UP", "Joystick Button 8", "7"),
                        bound("KEY_ELEV_TRIM_DN", "Joystick Button 9", "8"),
                        unbound("KEY_ELEV_TRIM_SET"),
                    )
                ),
                context("3RD_PARTY", &unbound("KEY_THIRD_PARTY_TRIM_HELPER")),
            ),
        )
    }

    #[test]
    fn the_catalogue_counts_what_is_there_and_what_is_not() {
        let p = sample();
        let catalog = Catalog::from_profiles([("inputprofile_1", &p)]);
        assert_eq!(catalog.len(), 4);
        assert_eq!(catalog.bound_count(), 2);
        assert_eq!(catalog.contexts(), vec!["3RD_PARTY", "AIRCRAFT"]);
        assert_eq!(catalog.devices(), vec!["STECS"]);
    }

    #[test]
    fn searching_reaches_the_actions_the_settings_menu_never_shows() {
        let p = sample();
        let catalog = Catalog::from_profiles([("inputprofile_1", &p)]);

        let all_trim = catalog.search(&Query {
            text: "trim".into(),
            ..Default::default()
        });
        assert_eq!(all_trim.len(), 4, "including the 3RD_PARTY one");

        let unbound_trim = catalog.search(&Query {
            text: "trim".into(),
            bound: Bound::Never,
            ..Default::default()
        });
        assert_eq!(unbound_trim.len(), 2);
    }

    #[test]
    fn search_indices_point_at_exactly_what_search_returns() {
        let p = sample();
        let catalog = Catalog::from_profiles([("inputprofile_1", &p)]);
        for query in [
            Query::default(),
            Query {
                text: "trim".into(),
                bound: Bound::Never,
                ..Default::default()
            },
        ] {
            let by_index: Vec<&Entry> = catalog
                .search_indices(&query)
                .into_iter()
                .map(|i| &catalog.entries()[i])
                .collect();
            assert_eq!(by_index, catalog.search(&query));
        }
    }

    #[test]
    fn a_search_can_be_driven_from_the_binding_label() {
        let p = sample();
        let catalog = Catalog::from_profiles([("inputprofile_1", &p)]);
        let hits = catalog.search(&Query {
            text: "button 8".into(),
            ..Default::default()
        });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].action, "KEY_ELEV_TRIM_UP");
    }

    #[test]
    fn two_profiles_sharing_a_device_are_still_told_apart() {
        // Real case: "My General Settings" and "My General Camera" are both on
        // `Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA`. Filtering by
        // device returns both; filtering by profile must not.
        let settings = sample();
        let camera = sample();
        let catalog = Catalog::from_profiles([
            ("inputprofile_0242070832", &settings),
            ("inputprofile_2019199616", &camera),
        ]);

        let by_device = catalog.search(&Query {
            device: Some("STECS".into()),
            ..Default::default()
        });
        assert_eq!(by_device.len(), 8, "both profiles are on that device");

        let by_profile = catalog.search(&Query {
            profile: Some("inputprofile_0242070832".into()),
            ..Default::default()
        });
        assert_eq!(by_profile.len(), 4);
        assert!(by_profile
            .iter()
            .all(|e| e.profile_id == "inputprofile_0242070832"));
    }

    #[test]
    fn counts_say_which_context_to_look_in() {
        let p = sample();
        let catalog = Catalog::from_profiles([("inputprofile_1", &p)]);
        let counts = catalog.counts_by_context(&Query {
            text: "trim".into(),
            ..Default::default()
        });
        assert_eq!(counts.get("AIRCRAFT"), Some(&3));
        assert_eq!(counts.get("3RD_PARTY"), Some(&1));
    }

    #[test]
    fn two_engines_on_one_lever_is_reported_as_ganging_not_as_a_fault() {
        let p = profile(
            "STECS MAX",
            &context(
                "AIRCRAFT",
                &format!(
                    "{}{}",
                    bound("KEY_THROTTLE2_AXIS_SET_EX1", "Joystick L-Axis X ", "1026"),
                    bound("KEY_THROTTLE3_AXIS_SET_EX1", "Joystick L-Axis X ", "1026"),
                ),
            ),
        );
        let shared = shared("inputprofile_1", &p);
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].kind, Kind::Axis);
        assert_eq!(shared[0].uses.len(), 2);
        assert_eq!(shared[0].label, "Joystick L-Axis X ");
    }

    #[test]
    fn a_button_on_two_actions_is_reported_as_a_button() {
        let p = profile(
            "Keyboard",
            &context(
                "MENU",
                &format!(
                    "{}{}",
                    bound("KEY_MENU_SR_FAVORITES", "f", "70"),
                    bound("KEY_MENU_SR_PLANE_STATS", "f", "70"),
                ),
            ),
        );
        let shared = shared("inputprofile_1", &p);
        assert_eq!(shared[0].kind, Kind::Button);
        assert_eq!(shared[0].kind.as_str(), "button shared");
    }

    #[test]
    fn the_same_key_in_two_contexts_is_not_sharing() {
        let p = profile(
            "Keyboard",
            &format!(
                "{}{}",
                context("MENU", &bound("KEY_MENU_BACK", "f", "70")),
                context("AIRCRAFT", &bound("KEY_FLAPS_UP", "f", "70")),
            ),
        );
        assert!(shared("inputprofile_1", &p).is_empty());
    }

    #[test]
    fn left_and_right_control_are_not_the_same_control() {
        // Same label, different codes. Keying on the label would invent a
        // finding here.
        let p = profile(
            "Keyboard",
            &context(
                "GLOBAL",
                &format!(
                    "{}{}",
                    bound("KEY_ONE", "Control", "162"),
                    bound("KEY_TWO", "Control", "163"),
                ),
            ),
        );
        assert!(shared("inputprofile_1", &p).is_empty());
    }
}
