//! What changed between two profiles.
//!
//! A text diff of these files is close to useless. Two profiles for the same
//! device share about 1,600 identical `<Action/>` lines, a rebind moves text
//! by one line in a 200 kB file, and a sim update that adds forty new unbound
//! actions looks exactly as alarming as someone wiping their bindings.
//!
//! So this compares meaning: an action gained a binding, lost one, or had it
//! changed; an axis curve was tuned; a context appeared. The output is a flat
//! list of [`Change`], each one printable as a sentence, ordered so that
//! structural changes come before the bindings inside them.
//!
//! Both sides are usually snapshots — [`crate::snapshot`] — or a snapshot
//! against what is on disk right now, which is how "did Steam Cloud replace my
//! bindings overnight?" gets answered.

use crate::model::*;
use std::collections::BTreeSet;
use std::fmt;

/// One difference, in the terms someone would describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The name shown in the sim's controls menu.
    ProfileRenamed {
        from: String,
        to: String,
    },
    /// The device the profile is for. Almost always means the two files are
    /// not versions of each other at all.
    DeviceChanged {
        from: String,
        to: String,
    },
    /// The `<Axes>` element itself appeared or disappeared.
    AxesSection {
        present: bool,
    },
    AxisAdded {
        axis: String,
    },
    AxisRemoved {
        axis: String,
    },
    /// One value on one axis' response curve.
    AxisTuned {
        axis: String,
        field: &'static str,
        from: String,
        to: String,
    },
    ContextAdded {
        context: String,
    },
    ContextRemoved {
        context: String,
    },
    /// An action name the other side does not have — a sim update, normally.
    ActionAdded {
        context: String,
        action: String,
    },
    ActionRemoved {
        context: String,
        action: String,
    },
    /// An action that had nothing in this slot now has something.
    Bound {
        context: String,
        action: String,
        slot: Slot,
        to: String,
    },
    Unbound {
        context: String,
        action: String,
        slot: Slot,
        from: String,
    },
    Rebound {
        context: String,
        action: String,
        slot: Slot,
        from: String,
        to: String,
    },
    /// `Delay`, `Flag` or `ValueEvent` on an action.
    ActionAttribute {
        context: String,
        action: String,
        attribute: &'static str,
        from: String,
        to: String,
    },
}

impl Change {
    /// True for the changes that are about what a control does, as opposed to
    /// the file's shape. These are the ones worth showing first and worth
    /// confirming before a write.
    pub fn is_binding(&self) -> bool {
        matches!(
            self,
            Change::Bound { .. } | Change::Unbound { .. } | Change::Rebound { .. }
        )
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::ProfileRenamed { from, to } => write!(f, "renamed {from:?} to {to:?}"),
            Change::DeviceChanged { from, to } => {
                write!(f, "device changed from {from:?} to {to:?}")
            }
            Change::AxesSection { present: true } => write!(f, "gained an <Axes> section"),
            Change::AxesSection { present: false } => write!(f, "lost its <Axes> section"),
            Change::AxisAdded { axis } => write!(f, "axis {axis} added"),
            Change::AxisRemoved { axis } => write!(f, "axis {axis} removed"),
            Change::AxisTuned {
                axis,
                field,
                from,
                to,
            } => write!(f, "axis {axis}: {field} {from} -> {to}"),
            Change::ContextAdded { context } => write!(f, "context {context} added"),
            Change::ContextRemoved { context } => write!(f, "context {context} removed"),
            Change::ActionAdded { context, action } => write!(f, "{context}/{action} added"),
            Change::ActionRemoved { context, action } => write!(f, "{context}/{action} removed"),
            Change::Bound {
                context,
                action,
                slot,
                to,
            } => write!(f, "{context}/{action} {} bound to {to}", slot.as_str()),
            Change::Unbound {
                context,
                action,
                slot,
                from,
            } => write!(
                f,
                "{context}/{action} {} unbound (was {from})",
                slot.as_str()
            ),
            Change::Rebound {
                context,
                action,
                slot,
                from,
                to,
            } => write!(f, "{context}/{action} {} {from} -> {to}", slot.as_str()),
            Change::ActionAttribute {
                context,
                action,
                attribute,
                from,
                to,
            } => write!(f, "{context}/{action} {attribute} {from} -> {to}"),
        }
    }
}

/// Every difference between two profiles, in a stable order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub changes: Vec<Change>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Just the changes to what controls do.
    pub fn bindings(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| c.is_binding())
    }

    /// A one-line summary: "3 bindings changed, 41 actions added".
    ///
    /// Written so the common uninteresting case — a sim update adding actions
    /// nobody has bound — cannot be mistaken for someone losing their setup.
    pub fn summary(&self) -> String {
        let bindings = self.bindings().count();
        let added = self
            .changes
            .iter()
            .filter(|c| matches!(c, Change::ActionAdded { .. }))
            .count();
        let removed = self
            .changes
            .iter()
            .filter(|c| matches!(c, Change::ActionRemoved { .. }))
            .count();
        let other = self.changes.len() - bindings - added - removed;

        let mut parts = Vec::new();
        if bindings > 0 {
            parts.push(format!("{bindings} binding{}", plural(bindings)));
        }
        if added > 0 {
            parts.push(format!("{added} action{} added", plural(added)));
        }
        if removed > 0 {
            parts.push(format!("{removed} action{} removed", plural(removed)));
        }
        if other > 0 {
            parts.push(format!("{other} other change{}", plural(other)));
        }
        if parts.is_empty() {
            "identical".to_string()
        } else {
            parts.join(", ")
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Compare two profiles.
///
/// Order of the result: identity, then axes, then contexts in the order `to`
/// has them, with anything only `from` had appended. Deterministic, so two
/// runs produce the same list and a test can assert on it.
pub fn compare(from: &Profile, to: &Profile) -> Diff {
    let mut changes = Vec::new();

    if from.friendly_name.name != to.friendly_name.name {
        changes.push(Change::ProfileRenamed {
            from: from.friendly_name.name.clone(),
            to: to.friendly_name.name.clone(),
        });
    }
    if from.device.name != to.device.name {
        changes.push(Change::DeviceChanged {
            from: from.device.name.clone(),
            to: to.device.name.clone(),
        });
    }

    compare_axes(&from.device, &to.device, &mut changes);
    compare_contexts(&from.device, &to.device, &mut changes);

    Diff { changes }
}

fn compare_axes(from: &Device, to: &Device, changes: &mut Vec<Change>) {
    match (&from.axes, &to.axes) {
        (None, Some(_)) => changes.push(Change::AxesSection { present: true }),
        (Some(_), None) => changes.push(Change::AxesSection { present: false }),
        _ => {}
    }

    for axis in to.axes() {
        let Some(before) = from.axes().iter().find(|a| a.name == axis.name) else {
            changes.push(Change::AxisAdded {
                axis: axis.name.clone(),
            });
            continue;
        };
        for (field, old, new) in [
            ("sensitivity", &before.sensitivity, &axis.sensitivity),
            (
                "sensitivity-",
                &before.sensitivity_minus,
                &axis.sensitivity_minus,
            ),
            ("neutral", &before.neutral, &axis.neutral),
            ("dead zone", &before.dead_zone, &axis.dead_zone),
            (
                "outer dead zone",
                &before.out_dead_zone,
                &axis.out_dead_zone,
            ),
            ("response rate", &before.response_rate, &axis.response_rate),
        ] {
            if old != new {
                changes.push(Change::AxisTuned {
                    axis: axis.name.clone(),
                    field,
                    from: old.clone(),
                    to: new.clone(),
                });
            }
        }
    }

    for axis in from.axes() {
        if !to.axes().iter().any(|a| a.name == axis.name) {
            changes.push(Change::AxisRemoved {
                axis: axis.name.clone(),
            });
        }
    }
}

fn compare_contexts(from: &Device, to: &Device, changes: &mut Vec<Change>) {
    for context in &to.contexts {
        let Some(before) = from.context(&context.name) else {
            changes.push(Change::ContextAdded {
                context: context.name.clone(),
            });
            // A brand-new context's bindings are worth listing even though the
            // context itself is new — that is where a restored profile's
            // bindings would show up.
            for action in context.actions.iter().filter(|a| a.is_bound()) {
                push_slot_changes(&context.name, action, None, Some(action), changes);
            }
            continue;
        };
        compare_actions(before, context, changes);
    }

    for context in &from.contexts {
        if to.context(&context.name).is_none() {
            changes.push(Change::ContextRemoved {
                context: context.name.clone(),
            });
        }
    }
}

fn compare_actions(from: &Context, to: &Context, changes: &mut Vec<Change>) {
    for action in &to.actions {
        let Some(before) = from.action(&action.name) else {
            changes.push(Change::ActionAdded {
                context: to.name.clone(),
                action: action.name.clone(),
            });
            if action.is_bound() {
                push_slot_changes(&to.name, action, None, Some(action), changes);
            }
            continue;
        };

        push_slot_changes(&to.name, action, Some(before), Some(action), changes);

        for (attribute, old, new) in [
            ("Delay", &before.delay, &action.delay),
            ("Flag", &before.flag, &action.flag),
            ("ValueEvent", &before.value_event, &action.value_event),
        ] {
            if old != new {
                changes.push(Change::ActionAttribute {
                    context: to.name.clone(),
                    action: action.name.clone(),
                    attribute,
                    from: old.clone(),
                    to: new.clone(),
                });
            }
        }
    }

    let present: BTreeSet<&str> = to.actions.iter().map(|a| a.name.as_str()).collect();
    for action in &from.actions {
        if !present.contains(action.name.as_str()) {
            changes.push(Change::ActionRemoved {
                context: from.name.clone(),
                action: action.name.clone(),
            });
        }
    }
}

/// Compare both slots of one action.
fn push_slot_changes(
    context: &str,
    named: &Action,
    from: Option<&Action>,
    to: Option<&Action>,
    changes: &mut Vec<Change>,
) {
    for slot in [Slot::Primary, Slot::Secondary] {
        let before = from.and_then(|a| slot_of(a, slot));
        let after = to.and_then(|a| slot_of(a, slot));
        match (before, after) {
            (None, Some(b)) => changes.push(Change::Bound {
                context: context.to_string(),
                action: named.name.clone(),
                slot,
                to: b.label(),
            }),
            (Some(a), None) => changes.push(Change::Unbound {
                context: context.to_string(),
                action: named.name.clone(),
                slot,
                from: a.label(),
            }),
            (Some(a), Some(b)) if a != b => changes.push(Change::Rebound {
                context: context.to_string(),
                action: named.name.clone(),
                slot,
                from: a.label(),
                to: b.label(),
            }),
            _ => {}
        }
    }
}

fn slot_of(action: &Action, slot: Slot) -> Option<&Binding> {
    match slot {
        Slot::Primary => action.primary.as_ref(),
        Slot::Secondary => action.secondary.as_ref(),
    }
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
                "<Device DeviceName=\"V1\" GUID=\"{{9E5}}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                "{}",
                "</Device>\n",
                "\n",
            ),
            name, body
        );
        parse(&text).expect("fixture parses")
    }

    fn action(name: &str, primary: Option<(&str, &str)>) -> String {
        match primary {
            None => format!(
                "\t\t<Action ActionName=\"{name}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n"
            ),
            Some((info, code)) => format!(
                concat!(
                    "\t\t<Action ActionName=\"{}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
                    "\t\t\t<Primary>\n",
                    "\t\t\t\t<KEY Information=\"{}\">{}</KEY>\n",
                    "\t\t\t</Primary>\n",
                    "\t\t</Action>\n",
                ),
                name, info, code
            ),
        }
    }

    fn aircraft(actions: &str) -> String {
        format!("\t<Context ContextName=\"AIRCRAFT\">\n{actions}\t</Context>\n")
    }

    #[test]
    fn an_unchanged_profile_has_no_changes() {
        let body = aircraft(&action(
            "KEY_ELEV_TRIM_UP",
            Some(("Joystick Button 8", "7")),
        ));
        let d = compare(&profile("A", &body), &profile("A", &body));
        assert!(d.is_empty(), "{:?}", d.changes);
        assert_eq!(d.summary(), "identical");
    }

    #[test]
    fn a_rebind_reads_as_a_rebind_not_two_edits() {
        let before = profile(
            "A",
            &aircraft(&action(
                "KEY_ELEV_TRIM_UP",
                Some(("Joystick Button 8", "7")),
            )),
        );
        let after = profile(
            "A",
            &aircraft(&action(
                "KEY_ELEV_TRIM_UP",
                Some(("Joystick Button 9", "8")),
            )),
        );
        let d = compare(&before, &after);
        assert_eq!(d.len(), 1);
        assert_eq!(
            d.changes[0].to_string(),
            "AIRCRAFT/KEY_ELEV_TRIM_UP Primary Joystick Button 8 -> Joystick Button 9"
        );
        assert_eq!(d.summary(), "1 binding");
    }

    #[test]
    fn binding_and_unbinding_are_told_apart() {
        let empty = profile("A", &aircraft(&action("KEY_FLAPS_UP", None)));
        let bound = profile(
            "A",
            &aircraft(&action("KEY_FLAPS_UP", Some(("Joystick Button 3", "2")))),
        );

        let gained = compare(&empty, &bound);
        assert_eq!(
            gained.changes,
            vec![Change::Bound {
                context: "AIRCRAFT".into(),
                action: "KEY_FLAPS_UP".into(),
                slot: Slot::Primary,
                to: "Joystick Button 3".into(),
            }]
        );

        let lost = compare(&bound, &empty);
        assert_eq!(
            lost.changes[0].to_string(),
            "AIRCRAFT/KEY_FLAPS_UP Primary unbound (was Joystick Button 3)"
        );
    }

    #[test]
    fn a_sim_update_adding_actions_does_not_look_like_lost_bindings() {
        // The case this whole module exists for: forty new unbound actions
        // must not read as alarming as one lost binding.
        let before = profile(
            "A",
            &aircraft(&action("KEY_FLAPS_UP", Some(("Joystick Button 3", "2")))),
        );
        let mut body = action("KEY_FLAPS_UP", Some(("Joystick Button 3", "2")));
        for n in 0..40 {
            body.push_str(&action(&format!("KEY_NEW_THING_{n}"), None));
        }
        let after = profile("A", &aircraft(&body));

        let d = compare(&before, &after);
        assert_eq!(d.bindings().count(), 0);
        assert_eq!(d.summary(), "40 actions added");
    }

    #[test]
    fn an_axis_tune_names_the_field_and_both_values() {
        let axes = |dead: &str| {
            format!(
                concat!(
                    "\t<Axes>\n",
                    "\t\t<Axis AxisName=\"X\" AxisSensitivy=\"0\" AxisSensitivyMinus=\"0\" AxisNeutral=\"0\"",
                    " AxisDeadZone=\"{}\" AxisOutDeadZone=\"0\" AxisResponseRate=\"-1\"/>\n",
                    "\t</Axes>\n",
                ),
                dead
            )
        };
        let d = compare(&profile("A", &axes("0")), &profile("A", &axes("5")));
        assert_eq!(d.changes[0].to_string(), "axis X: dead zone 0 -> 5");
    }

    #[test]
    fn a_delay_change_is_visible_even_though_it_is_not_a_binding() {
        let with_delay = |delay: &str| {
            aircraft(&format!(
                "\t\t<Action ActionName=\"KEY_ELEV_TRIM_UP\" ValueEvent=\"0.000000\" Delay=\"{delay}\" Flag=\"2\"/>\n"
            ))
        };
        let d = compare(
            &profile("A", &with_delay("0.000000")),
            &profile("A", &with_delay("2.000000")),
        );
        assert_eq!(
            d.changes[0].to_string(),
            "AIRCRAFT/KEY_ELEV_TRIM_UP Delay 0.000000 -> 2.000000"
        );
        assert_eq!(d.summary(), "1 other change");
    }

    #[test]
    fn a_rename_is_reported_and_is_not_a_binding_change() {
        let body = aircraft(&action("KEY_FLAPS_UP", None));
        let d = compare(&profile("Old name", &body), &profile("New name", &body));
        assert_eq!(
            d.changes[0].to_string(),
            "renamed \"Old name\" to \"New name\""
        );
        assert_eq!(d.bindings().count(), 0);
    }
}
