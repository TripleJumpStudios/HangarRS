//! Writing an input profile back out.
//!
//! The contract this module owes is narrow and absolute: **a profile parsed
//! and serialised without being changed must come back byte-identical.** Not
//! semantically equivalent — identical. These files are Steam Cloud userdata,
//! often the only copy, and only the sim can tell whether a subtly reformatted
//! file still works.
//!
//! So the layout below is not a pretty-printer's opinion. It reproduces what
//! real profiles contain:
//!
//! - tabs for indentation, LF line endings, no CRLF anywhere
//! - fixed attribute order, the same in every element of a given kind
//! - `<Action …/>` self-closing when unbound, a start/end pair when bound
//! - the file ends `</Device>\n\n` — with that trailing blank line
//!
//! [`crate::roundtrip_ok`] is the check, and `examples/roundtrip.rs` runs it
//! against the real profiles. Nothing writes to `remote/` unless it passes.

use crate::model::*;

/// Serialise a profile to the exact bytes MSFS writes.
pub fn to_xml(profile: &Profile) -> String {
    // Real profiles run 130–200 kB; one allocation beats several thousand.
    let mut out = String::with_capacity(256 * 1024);

    out.push_str("<?xml version=\"");
    push_attr_value(&mut out, &profile.xml_version);
    out.push_str("\" encoding=\"");
    push_attr_value(&mut out, &profile.encoding);
    out.push_str("\"?>\n");

    out.push_str("<Version Num=\"");
    push_attr_value(&mut out, &profile.version_num);
    out.push_str("\"/>\n");

    let fname = &profile.friendly_name;
    out.push_str("<FriendlyName PlatformAvailability=\"");
    push_attr_value(&mut out, &fname.platform_availability);
    out.push_str("\" Locked=\"");
    push_attr_value(&mut out, &fname.locked);
    out.push_str("\">");
    push_text(&mut out, &fname.name);
    out.push_str("</FriendlyName>\n");

    push_device(&mut out, &profile.device);

    // The trailing blank line is real: every observed file ends
    // `</Device>\n\n`, and dropping it fails the byte comparison.
    out.push('\n');

    out
}

fn push_device(out: &mut String, device: &Device) {
    out.push_str("<Device DeviceName=\"");
    push_attr_value(out, &device.name);
    out.push_str("\" GUID=\"");
    push_attr_value(out, &device.guid);
    out.push_str("\" ProductID=\"");
    push_attr_value(out, &device.product_id);
    out.push_str("\" CompositeID=\"");
    push_attr_value(out, &device.composite_id);
    out.push_str("\" HWVer=\"");
    push_attr_value(out, &device.hw_ver);
    out.push_str("\">\n");

    if let Some(info) = &device.aircraft_info {
        out.push_str("\t<AircraftInfo CategoryName=\"");
        push_attr_value(out, &info.category_name);
        out.push_str("\"/>\n");
    }

    if let Some(axes) = &device.axes {
        out.push_str("\t<Axes>\n");
        for axis in axes {
            push_axis(out, axis);
        }
        out.push_str("\t</Axes>\n");
    }

    for context in &device.contexts {
        push_context(out, context);
    }

    out.push_str("</Device>\n");
}

fn push_axis(out: &mut String, axis: &Axis) {
    out.push_str("\t\t<Axis AxisName=\"");
    push_attr_value(out, &axis.name);
    // Asobo's typo, kept on purpose: spelling it correctly makes the sim
    // ignore the value and quietly revert the axis to linear.
    out.push_str("\" AxisSensitivy=\"");
    push_attr_value(out, &axis.sensitivity);
    out.push_str("\" AxisSensitivyMinus=\"");
    push_attr_value(out, &axis.sensitivity_minus);
    out.push_str("\" AxisNeutral=\"");
    push_attr_value(out, &axis.neutral);
    out.push_str("\" AxisDeadZone=\"");
    push_attr_value(out, &axis.dead_zone);
    out.push_str("\" AxisOutDeadZone=\"");
    push_attr_value(out, &axis.out_dead_zone);
    out.push_str("\" AxisResponseRate=\"");
    push_attr_value(out, &axis.response_rate);
    out.push_str("\"/>\n");
}

fn push_context(out: &mut String, context: &Context) {
    out.push_str("\t<Context ContextName=\"");
    push_attr_value(out, &context.name);
    out.push_str("\">\n");
    for action in &context.actions {
        push_action(out, action);
    }
    out.push_str("\t</Context>\n");
}

fn push_action(out: &mut String, action: &Action) {
    out.push_str("\t\t<Action ActionName=\"");
    push_attr_value(out, &action.name);
    out.push_str("\" ValueEvent=\"");
    push_attr_value(out, &action.value_event);
    out.push_str("\" Delay=\"");
    push_attr_value(out, &action.delay);
    out.push_str("\" Flag=\"");
    push_attr_value(out, &action.flag);

    if !action.is_bound() {
        // The unbound form. This is most of the file.
        out.push_str("\"/>\n");
        return;
    }

    out.push_str("\">\n");
    if let Some(primary) = &action.primary {
        push_binding(out, "Primary", primary);
    }
    if let Some(secondary) = &action.secondary {
        push_binding(out, "Secondary", secondary);
    }
    out.push_str("\t\t</Action>\n");
}

fn push_binding(out: &mut String, slot: &str, binding: &Binding) {
    out.push_str("\t\t\t<");
    out.push_str(slot);
    out.push_str(">\n");
    for key in &binding.keys {
        out.push_str("\t\t\t\t<KEY Information=\"");
        push_attr_value(out, &key.information);
        out.push_str("\">");
        push_text(out, &key.code);
        out.push_str("</KEY>\n");
    }
    out.push_str("\t\t\t</");
    out.push_str(slot);
    out.push_str(">\n");
}

/// Escape only what a double-quoted attribute value actually requires.
///
/// Deliberately *not* `quick_xml::escape::escape`, which also rewrites `'` to
/// `&apos;` and `>` to `&gt;`. Neither needs escaping here, and either would
/// turn a faithful copy into a different file the first time a device name
/// contained an apostrophe. Real profiles carry no entities at all.
fn push_attr_value(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
}

/// Element text — `<KEY>` codes and the friendly name. Same reasoning.
fn push_text(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_characters_that_have_to_be_escaped_are() {
        let mut s = String::new();
        push_attr_value(&mut s, "Alex's \"VKB\" <thing> & co");
        assert_eq!(s, "Alex's &quot;VKB&quot; &lt;thing> &amp; co");
    }

    #[test]
    fn the_typo_survives_a_write() {
        let axis = Axis {
            name: "X".into(),
            sensitivity: "0".into(),
            sensitivity_minus: "0".into(),
            neutral: "0".into(),
            dead_zone: "0".into(),
            out_dead_zone: "0".into(),
            response_rate: "-1".into(),
        };
        let mut s = String::new();
        push_axis(&mut s, &axis);
        assert!(
            s.contains("AxisSensitivy=\""),
            "the typo was corrected: {s}"
        );
        assert!(
            !s.contains("AxisSensitivity=\""),
            "the typo was corrected: {s}"
        );
    }
}
