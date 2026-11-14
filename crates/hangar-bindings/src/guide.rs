//! A printable cheat sheet of what is bound to what.
//!
//! The output is one self-contained HTML file: inline CSS, no scripts, no
//! external anything, so it can be emailed and renders the same anywhere.
//!
//! # Why there is no PDF writer
//!
//! The HTML carries a real print stylesheet, and every browser already has a
//! better PDF writer than a small Rust crate would give: hyphenation, widow
//! control, real font metrics, the reader's page size. `Ctrl-P → Save as PDF`
//! is the export.
//!
//! # What is left out
//!
//! A cheat sheet listing every action is not a cheat sheet. Two filters are on
//! by default:
//!
//! - **Unbound actions are omitted.** Most of each profile is unbound; those
//!   belong in the searchable catalogue, not on paper.
//! - **Developer contexts are omitted.** On the default keyboard profile
//!   `DEBUG` alone carries more bindings than every flight control combined.
//!
//! Both are options, because someone debugging a `DEVMODE` binding needs
//! exactly the document the defaults would leave out.

use crate::describe;
use crate::model::*;

/// Context-name prefixes treated as developer tooling rather than flying.
///
/// Matched by prefix rather than listed exactly, so `DEVMODECAMERA` and any
/// `EDITOR_LOCK_*` the sim adds later are covered without an update.
pub const DEVELOPER_CONTEXTS: &[&str] = &["DEBUG", "DEVMODE", "EDITOR"];

/// How much to put in the document.
#[derive(Debug, Clone)]
pub struct Options {
    /// Shown as the document title and the running header.
    pub title: String,
    /// Include actions with nothing bound to them.
    pub include_unbound: bool,
    /// Include `DEBUG`, `DEVMODE` and `EDITOR_*`.
    pub include_developer: bool,
    /// Print the one-line description under each action.
    pub include_descriptions: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            title: "MSFS 2024 control bindings".to_string(),
            include_unbound: false,
            include_developer: false,
            include_descriptions: true,
        }
    }
}

/// Render a cheat sheet for one or more profiles.
///
/// Each profile becomes a section that starts on a fresh page when printed, so
/// "print just the pedals" is a page range rather than a second export.
pub fn to_html<'a>(
    profiles: impl IntoIterator<Item = (&'a str, &'a Profile)>,
    options: &Options,
) -> String {
    let mut out = String::with_capacity(64 * 1024);

    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>");
    push_escaped(&mut out, &options.title);
    out.push_str("</title>\n<style>\n");
    out.push_str(STYLE);
    out.push_str("</style>\n</head>\n<body>\n");

    out.push_str("<header class=\"doc\">\n<h1>");
    push_escaped(&mut out, &options.title);
    out.push_str("</h1>\n");
    out.push_str("<p class=\"made\">Exported by HangarRS</p>\n</header>\n");

    let mut any = false;
    for (id, profile) in profiles {
        any = true;
        push_profile(&mut out, id, profile, options);
    }
    if !any {
        out.push_str("<p class=\"empty\">No profiles to show.</p>\n");
    }

    out.push_str("</body>\n</html>\n");
    out
}

fn push_profile(out: &mut String, id: &str, profile: &Profile, options: &Options) {
    out.push_str("<section class=\"profile\">\n<header>\n<h2>");
    push_escaped(out, &profile.friendly_name.name);
    out.push_str("</h2>\n<p class=\"device\">");
    push_escaped(out, &profile.device.name);
    out.push_str("</p>\n<p class=\"id\">");
    push_escaped(out, id);
    if profile.friendly_name.is_locked() {
        out.push_str(" · locked preset");
    }
    out.push_str("</p>\n</header>\n");

    let contexts: Vec<&Context> = profile
        .device
        .contexts
        .iter()
        .filter(|c| options.include_developer || !is_developer(&c.name))
        .filter(|c| c.actions.iter().any(|a| shown(a, options)))
        .collect();

    if contexts.is_empty() {
        out.push_str("<p class=\"empty\">Nothing bound in this profile.</p>\n");
    }

    for context in contexts {
        out.push_str("<section class=\"context\">\n<h3>");
        push_escaped(out, &context.name);
        out.push_str("</h3>\n<dl>\n");

        for action in context.actions.iter().filter(|a| shown(a, options)) {
            let described = describe::describe(&action.name);
            out.push_str("<dt>");
            push_escaped(out, &described.title);
            out.push_str("<span class=\"kind\">");
            push_escaped(out, described.kind.as_str());
            out.push_str("</span></dt>\n<dd>\n");

            if action.is_bound() {
                for (slot, binding) in action.bindings() {
                    out.push_str("<p class=\"binding\">");
                    if slot == Slot::Secondary {
                        out.push_str("<span class=\"slot\">alt</span>");
                    }
                    push_keys(out, binding);
                    out.push_str("</p>\n");
                }
            } else {
                out.push_str("<p class=\"unbound\">not bound</p>\n");
            }

            if options.include_descriptions {
                out.push_str("<p class=\"about\">");
                push_escaped(out, &described.summary);
                out.push_str("</p>\n");
            }
            out.push_str("</dd>\n");
        }

        out.push_str("</dl>\n</section>\n");
    }

    out.push_str("</section>\n");
}

/// Each key as its own chip, joined by `+`, the way a chord is read.
fn push_keys(out: &mut String, binding: &Binding) {
    if binding.keys.is_empty() {
        out.push_str("<span class=\"unbound\">empty</span>");
        return;
    }
    for (i, key) in binding.keys.iter().enumerate() {
        if i > 0 {
            out.push_str("<span class=\"plus\">+</span>");
        }
        out.push_str("<kbd>");
        // MSFS writes some labels with a trailing space — `Joystick L-Axis X `.
        // It is preserved on disk and trimmed for display, which are different
        // jobs and only collide if you forget which one you are doing.
        push_escaped(out, key.information.trim());
        out.push_str("</kbd>");
    }
}

fn shown(action: &Action, options: &Options) -> bool {
    options.include_unbound || action.is_bound()
}

fn is_developer(context: &str) -> bool {
    DEVELOPER_CONTEXTS.iter().any(|p| context.starts_with(p))
}

fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
}

/// The whole stylesheet, inlined.
///
/// Light by default because it is a document to print. The dark block is for
/// reading it on screen before printing; `@media print` forces white back,
/// since a dark page costs a cartridge and reads worse on paper.
const STYLE: &str = r#"
:root {
  --bg: #ffffff;
  --fg: #16191d;
  --muted: #5c6570;
  --rule: #dfe3e8;
  --chip-bg: #f2f4f7;
  --chip-fg: #16191d;
  --chip-edge: #c9d0d8;
  --accent: #1f5f8b;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --bg: #14171a;
    --fg: #e6e9ec;
    --muted: #9aa4ae;
    --rule: #2a3037;
    --chip-bg: #232930;
    --chip-fg: #e6e9ec;
    --chip-edge: #3a424b;
    --accent: #7fb6dc;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0 auto;
  padding: 2rem 1.5rem 4rem;
  max-width: 60rem;
  background: var(--bg);
  color: var(--fg);
  font: 15px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
}
header.doc { border-bottom: 2px solid var(--fg); padding-bottom: .6rem; margin-bottom: 2rem; }
h1 { font-size: 1.5rem; margin: 0; letter-spacing: -0.01em; }
.made { margin: .25rem 0 0; color: var(--muted); font-size: .8rem; }
.profile { margin-bottom: 3rem; }
.profile > header { margin-bottom: 1.25rem; }
h2 { font-size: 1.2rem; margin: 0; color: var(--accent); }
.device { margin: .15rem 0 0; font-weight: 600; }
.id { margin: .1rem 0 0; color: var(--muted); font-size: .78rem;
      font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.context { break-inside: avoid; margin-bottom: 1.5rem; }
h3 { font-size: .74rem; text-transform: uppercase; letter-spacing: .09em;
     color: var(--muted); margin: 0 0 .5rem; padding-bottom: .3rem;
     border-bottom: 1px solid var(--rule); }
dl { margin: 0; display: grid; grid-template-columns: minmax(12rem, 1fr) minmax(12rem, 1.1fr);
     gap: .45rem 1.5rem; align-items: baseline; }
dt { font-weight: 600; }
dd { margin: 0; }
.kind { margin-left: .5rem; font-weight: 400; font-size: .7rem; color: var(--muted);
        text-transform: uppercase; letter-spacing: .06em; }
.binding { margin: 0 0 .2rem; }
.about { margin: .1rem 0 0; color: var(--muted); font-size: .8rem; }
.slot { margin-right: .4rem; font-size: .68rem; text-transform: uppercase;
        letter-spacing: .06em; color: var(--muted); }
kbd {
  display: inline-block; padding: .1rem .4rem;
  background: var(--chip-bg); color: var(--chip-fg);
  border: 1px solid var(--chip-edge); border-radius: 4px;
  font: 500 .82rem/1.4 ui-monospace, SFMono-Regular, Menlo, monospace;
}
.plus { margin: 0 .25rem; color: var(--muted); font-size: .8rem; }
.unbound, .empty { color: var(--muted); font-style: italic; }
@media (max-width: 40rem) { dl { grid-template-columns: 1fr; gap: .1rem; }
                            dd { margin-bottom: .6rem; } }
@media print {
  :root { --bg: #fff; --fg: #000; --muted: #444; --rule: #bbb;
          --chip-bg: #fff; --chip-fg: #000; --chip-edge: #666; --accent: #000; }
  body { max-width: none; padding: 0; font-size: 10.5pt; }
  .profile { break-before: page; }
  .profile:first-of-type { break-before: auto; }
  .made { display: none; }
}
"#;

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
                "<Device DeviceName=\"V1 &amp; Co\" GUID=\"{{9E5}}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                "{}",
                "</Device>\n\n",
            ),
            name, body
        );
        parse(&text).expect("fixture parses")
    }

    fn ctx(name: &str, actions: &str) -> String {
        format!("\t<Context ContextName=\"{name}\">\n{actions}\t</Context>\n")
    }

    fn bound(name: &str, info: &str, code: &str) -> String {
        format!(
            concat!(
                "\t\t<Action ActionName=\"{}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
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

    fn sample() -> Profile {
        profile(
            "Pedals",
            &format!(
                "{}{}",
                ctx(
                    "AIRCRAFT",
                    &format!(
                        "{}{}",
                        bound("KEY_ELEV_TRIM_UP", "Joystick Button 8", "7"),
                        unbound("KEY_FLAPS_UP"),
                    )
                ),
                ctx("DEBUG", &bound("KEY_DEBUG_ENTER", "Return", "13")),
            ),
        )
    }

    #[test]
    fn unbound_actions_and_developer_contexts_stay_out_by_default() {
        let p = sample();
        let html = to_html([("inputprofile_1", &p)], &Options::default());
        assert!(html.contains("Elevator Trim Up"));
        assert!(!html.contains("Flaps Up"), "unbound action leaked in");
        assert!(!html.contains("DEBUG"), "developer context leaked in");
    }

    #[test]
    fn both_filters_can_be_turned_off() {
        let p = sample();
        let html = to_html(
            [("inputprofile_1", &p)],
            &Options {
                include_unbound: true,
                include_developer: true,
                ..Default::default()
            },
        );
        assert!(html.contains("Flaps Up"));
        assert!(html.contains("DEBUG"));
        assert!(html.contains("not bound"));
    }

    #[test]
    fn the_document_is_self_contained() {
        // Community-shareable means one file. Anything fetched is a thing that
        // renders wrong on the machine it matters on.
        let p = sample();
        let html = to_html([("inputprofile_1", &p)], &Options::default());
        for fetch in ["<script", "src=", "href=", "@import", "http://", "https://"] {
            assert!(!html.contains(fetch), "document reaches out: {fetch}");
        }
    }

    #[test]
    fn text_from_the_profile_is_escaped() {
        // Device names and labels come out of a file, so they are untrusted
        // input as far as the document is concerned.
        // Written as entities, because that is how MSFS would have to store
        // them — and parsing them back out is the other half of the test.
        let p = profile(
            "Pedals &lt;&amp;&gt; &quot;quoted&quot;",
            &ctx("AIRCRAFT", &bound("KEY_FLAPS_UP", "A &amp; B", "65")),
        );
        assert_eq!(p.friendly_name.name, "Pedals <&> \"quoted\"");

        let html = to_html([("id", &p)], &Options::default());
        assert!(html.contains("Pedals &lt;&amp;&gt; &quot;quoted&quot;"));
        assert!(html.contains("V1 &amp; Co"));
        assert!(html.contains("A &amp; B"));
        assert!(!html.contains("<&>"));
    }

    #[test]
    fn a_trailing_space_in_a_label_is_trimmed_for_display_only() {
        // The file keeps `Joystick L-Axis X ` and must; the printed page
        // should not show a hanging gap inside the chip.
        let p = profile(
            "Throttle",
            &ctx(
                "AIRCRAFT",
                &bound("KEY_THROTTLE1_AXIS_SET_EX1", "Joystick L-Axis X ", "1026"),
            ),
        );
        let html = to_html([("id", &p)], &Options::default());
        assert!(html.contains("<kbd>Joystick L-Axis X</kbd>"));
    }

    #[test]
    fn a_profile_with_nothing_bound_says_so_rather_than_rendering_blank() {
        let p = profile("Empty", &ctx("AIRCRAFT", &unbound("KEY_FLAPS_UP")));
        let html = to_html([("id", &p)], &Options::default());
        assert!(html.contains("Nothing bound in this profile"));
    }

    #[test]
    fn developer_contexts_are_matched_by_prefix() {
        assert!(is_developer("DEBUG"));
        assert!(is_developer("DEVMODE"));
        assert!(is_developer("DEVMODECAMERA"));
        assert!(is_developer("EDITOR_LOCK_ROTATION"));
        assert!(!is_developer("AIRCRAFT"));
        assert!(!is_developer("COCKPIT_CAMERA"));
    }
}
