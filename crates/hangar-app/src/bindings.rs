//! The Bindings tab: everything MSFS can bind, not just what is bound.
//!
//! Actions the sim's settings menu never shows (`3RD_PARTY`, and whole
//! contexts besides) are in the profile files and bindable; making them
//! searchable is most of what this tab is for.
//!
//! Rows carry a readable title from `hangar_bindings::describe`, which derives
//! it: MSFS keeps its own strings on its servers, not in the install.

use crate::app::HangarApp;
use hangar_bindings::catalog::{Bound, Query};
use hangar_bindings::{describe, Library};
use hangar_ui as ui;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The hover note on a Bind action that has nothing to capture for.
pub(crate) const BIND_UNAVAILABLE: &str = "That profile is no longer loaded.";

/// Whether the Bind action is offered at all.
///
/// Four traps, each held by a test:
///
/// 1. A **keyboard** profile is captured from this window's own key events,
///    never from evdev, and only the keys whose codes are in
///    `hangar_bindings::keyboard::OBSERVED` can become a binding.
/// 2. Capture watches only the profile's own device, or a press on one stick
///    could be written into another device's profile; it refuses at once when
///    that device is not plugged in.
/// 3. The card is drawn at the top of the page and scrolls itself into view,
///    since Bind is often pressed far down the list.
/// 4. While a keyboard capture waits, it takes every key event, so a press
///    cannot also type into the search or press a button; Escape cancels.
const REBIND_ENABLED: bool = true;

/// What a rebind for one profile listens to.
#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    /// Key presses in this window. MSFS records one keyboard for all of them
    /// (`GUID="{0}"`, a `ProductID` no USB keyboard here reports), so there is
    /// no single device to pick.
    Keyboard,
    /// These controllers, all of them the profile's own product.
    Controllers(Vec<hangar_core::input::InputDevice>),
}

/// Which plugged-in devices a rebind for this profile may listen to.
///
/// Only the profile's own device, matched by product ID like the controllers
/// card. A keyboard profile listens to this window's keys instead.
pub(crate) fn capture_target(
    device: &hangar_bindings::model::Device,
    plugged: &[hangar_core::input::InputDevice],
) -> Result<Target, String> {
    if device.guid == hangar_bindings::devices::KEYBOARD_GUID {
        return Ok(Target::Keyboard);
    }
    let product: Option<u32> = device.product_id.trim().parse().ok();
    let matching: Vec<_> = plugged
        .iter()
        .filter(|d| product == Some(u32::from(d.product)))
        .cloned()
        .collect();
    if matching.is_empty() {
        return Err(format!("{} is not plugged in.", device.name.trim()));
    }
    Ok(Target::Controllers(matching))
}

/// The code MSFS writes for each key egui can report, by **physical** key.
///
/// Physical, not logical: holding Shift turns the logical `1` into `!`, while
/// the code MSFS stores names the key, not the character. Every code here is
/// in `keyboard::OBSERVED` (a test holds that); a key not listed is refused
/// with a plain note rather than given a code.
///
/// Observed codes no egui key reaches, so they cannot be captured: Pause (19,
/// egui has no `Pause`), the numpad digits (96–105), `KP_Subtract` (109) and
/// `KP_Delete` (110) — egui-winit 0.35 folds the numpad digits and minus into
/// the main-row keys and drops the numpad decimal. `KP_Add` (107) survives as
/// `Plus`, the only physical key egui gives that name. Escape (27) cancels.
const KEY_CODES: &[(egui::Key, u32)] = &[
    (egui::Key::Backspace, 8),
    (egui::Key::Tab, 9),
    (egui::Key::Enter, 13),
    (egui::Key::Space, 32),
    (egui::Key::PageUp, 33),
    (egui::Key::PageDown, 34),
    (egui::Key::End, 35),
    (egui::Key::Home, 36),
    (egui::Key::ArrowLeft, 37),
    (egui::Key::ArrowUp, 38),
    (egui::Key::ArrowRight, 39),
    (egui::Key::ArrowDown, 40),
    (egui::Key::Delete, 46),
    (egui::Key::Num0, 48),
    (egui::Key::Num1, 49),
    (egui::Key::Num2, 50),
    (egui::Key::Num3, 51),
    (egui::Key::Num4, 52),
    (egui::Key::Num5, 53),
    (egui::Key::Num6, 54),
    (egui::Key::Num7, 55),
    (egui::Key::Num8, 56),
    (egui::Key::Num9, 57),
    (egui::Key::A, 65),
    (egui::Key::B, 66),
    (egui::Key::C, 67),
    (egui::Key::D, 68),
    (egui::Key::E, 69),
    (egui::Key::F, 70),
    (egui::Key::G, 71),
    (egui::Key::H, 72),
    (egui::Key::I, 73),
    (egui::Key::J, 74),
    (egui::Key::K, 75),
    (egui::Key::L, 76),
    (egui::Key::M, 77),
    (egui::Key::N, 78),
    (egui::Key::O, 79),
    (egui::Key::P, 80),
    (egui::Key::Q, 81),
    (egui::Key::R, 82),
    (egui::Key::S, 83),
    (egui::Key::T, 84),
    (egui::Key::U, 85),
    (egui::Key::V, 86),
    (egui::Key::W, 87),
    (egui::Key::X, 88),
    (egui::Key::Y, 89),
    (egui::Key::Z, 90),
    (egui::Key::Plus, 107),
    (egui::Key::F1, 112),
    (egui::Key::F2, 113),
    (egui::Key::F3, 114),
    (egui::Key::F4, 115),
    (egui::Key::F5, 116),
    (egui::Key::F6, 117),
    (egui::Key::F7, 118),
    (egui::Key::F8, 119),
    (egui::Key::F9, 120),
    (egui::Key::F10, 121),
    (egui::Key::F11, 122),
    (egui::Key::Equals, 187),
    (egui::Key::Comma, 188),
    (egui::Key::Minus, 189),
    (egui::Key::OpenBracket, 219),
    (egui::Key::Backslash, 220),
    (egui::Key::CloseBracket, 221),
    (egui::Key::Quote, 222),
];

/// A physical key as keyboard capture reads it, if it has an observed code.
fn key_input(key: egui::Key) -> Option<hangar_bindings::keyboard::Input> {
    use hangar_bindings::keyboard::{Input, Modifier, Side};
    let modifier = match key {
        egui::Key::ShiftLeft => Some((Modifier::Shift, Side::Left)),
        egui::Key::ShiftRight => Some((Modifier::Shift, Side::Right)),
        egui::Key::ControlLeft => Some((Modifier::Control, Side::Left)),
        egui::Key::ControlRight => Some((Modifier::Control, Side::Right)),
        egui::Key::AltLeft => Some((Modifier::Alt, Side::Left)),
        egui::Key::AltRight => Some((Modifier::Alt, Side::Right)),
        _ => None,
    };
    if let Some((m, side)) = modifier {
        return Some(Input::Modifier(m, side));
    }
    KEY_CODES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, code)| Input::Key(*code))
}

/// Main-row keys a numpad key is indistinguishable from in egui: the digits,
/// `-` and Enter. The card says so, because the sim tells them apart.
fn has_numpad_twin(code: u32) -> bool {
    matches!(code, 13 | 48..=57 | 189)
}

/// Copy, cut and paste arrive as their own events, never as a key.
const NOT_A_KEY: &str =
    "Control + C, X and V arrive as copy, cut and paste, so they cannot be captured.";

/// The filter lists, which depend only on the library.
///
/// `Catalog::devices` and `contexts` sort ten thousand names each; asked for
/// every frame by the filter row, they lag the tab while the window resizes.
pub(crate) struct Lists {
    library: Arc<Library>,
    devices: Vec<(Option<String>, String)>,
    contexts: Vec<String>,
    profiles: Vec<(Option<String>, String)>,
}

/// One search's answer, kept until the library or the filters change.
///
/// The library is held, not just its address, so a reloaded library can never
/// land at the old address and be mistaken for it.
pub(crate) struct Hits {
    library: Arc<Library>,
    query: Query,
    indices: Vec<usize>,
    by_context: BTreeMap<String, usize>,
}

/// The cached derived state the Assignments view reads each frame.
#[derive(Default)]
pub(crate) struct Lookup {
    lists: Option<Arc<Lists>>,
    hits: Option<Arc<Hits>>,
}

fn lists(app: &mut HangarApp, library: &Arc<Library>) -> Arc<Lists> {
    let cache = &mut app.binding_tools.lookup;
    if let Some(lists) = &cache.lists {
        if Arc::ptr_eq(&lists.library, library) {
            return Arc::clone(lists);
        }
    }
    let mut devices = vec![(None, "All devices".to_owned())];
    devices.extend(
        library
            .catalog
            .devices()
            .into_iter()
            .map(|device| (Some(device.to_owned()), short_device(device))),
    );
    let lists = Arc::new(Lists {
        library: Arc::clone(library),
        devices,
        contexts: library
            .catalog
            .contexts()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        profiles: profile_options(library),
    });
    cache.lists = Some(Arc::clone(&lists));
    lists
}

fn hits(app: &mut HangarApp, library: &Arc<Library>) -> Arc<Hits> {
    let q = query(app, library);
    let cache = &mut app.binding_tools.lookup;
    if let Some(hits) = &cache.hits {
        if Arc::ptr_eq(&hits.library, library) && hits.query == q {
            return Arc::clone(hits);
        }
    }
    let hits = Arc::new(Hits {
        library: Arc::clone(library),
        indices: library.catalog.search_indices(&q),
        by_context: library
            .catalog
            .counts_by_context(&q)
            .into_iter()
            .map(|(context, count)| (context.to_owned(), count))
            .collect(),
        query: q,
    });
    cache.hits = Some(Arc::clone(&hits));
    hits
}

/// How many rows to draw before saying "and N more".
///
/// A bare search matches over ten thousand rows; drawing them all costs a
/// layout pass each for results nobody scrolls to.
const MAX_ROWS: usize = 300;

/// A rebind in progress.
///
/// Three states, and the order matters: watching for a control, holding one
/// that has been caught and needs confirming, then gone. Nothing is written
/// until the middle state is confirmed, and the whole thing is dropped on
/// cancel so the device handles close.
pub struct Rebind {
    pub profile_id: String,
    pub context: String,
    pub action: String,
    pub slot: hangar_bindings::Slot,
    /// Readable title, so the prompt says what is being bound.
    pub title: String,
    /// What was there before, for the confirm prompt.
    pub was: Option<String>,
    capture: hangar_bindings::Capture,
    problems: Vec<(String, String)>,
    /// Filled once something has been caught. Confirming writes it.
    pub caught: Option<hangar_bindings::Captured>,
    /// Set after a write attempt, successful or refused.
    pub outcome: Option<(String, bool)>,
    /// Whether the card has been scrolled into view yet.
    revealed: bool,
    /// Keyboard profiles: key presses read from this window, and nothing
    /// else. `None` for a controller.
    keys: Option<hangar_bindings::KeyCapture>,
    /// A key or combination caught; confirming writes it.
    pub caught_keys: Option<hangar_bindings::keyboard::Caught>,
    /// Why the last key could not be bound. Capture keeps waiting.
    pub key_note: Option<String>,
}

impl Rebind {
    /// A rebind watching nothing, for render tests.
    ///
    /// `Capture::start(&[])` opens no devices, so a test never depends on what
    /// is plugged in.
    #[cfg(test)]
    pub(crate) fn detached(title: &str, context: &str, action: &str) -> Self {
        let (capture, problems) = hangar_bindings::Capture::start(&[]);
        Rebind {
            profile_id: "inputprofile_0000000000".into(),
            context: context.into(),
            action: action.into(),
            slot: hangar_bindings::Slot::Primary,
            title: title.into(),
            was: Some("Joystick Button 8".into()),
            capture,
            problems,
            caught: None,
            outcome: None,
            revealed: false,
            keys: None,
            caught_keys: None,
            key_note: None,
        }
    }

    /// A keyboard rebind, waiting, for render tests.
    #[cfg(test)]
    pub(crate) fn detached_keyboard(title: &str, context: &str, action: &str) -> Self {
        let mut rebind = Self::detached(title, context, action);
        rebind.was = Some("Control + space".into());
        rebind.keys = Some(hangar_bindings::KeyCapture::new());
        rebind
    }

    /// Whether this rebind reads the keyboard.
    pub(crate) fn is_keyboard(&self) -> bool {
        self.keys.is_some()
    }

    fn start(
        entry: &hangar_bindings::catalog::Entry,
        device: Option<&hangar_bindings::model::Device>,
        title: String,
        was: Option<String>,
        live: bool,
    ) -> Self {
        let plugged = if live {
            hangar_core::input::controllers().unwrap_or_default()
        } else {
            Vec::new()
        };
        let target = device.map_or_else(
            || Err("That profile is no longer loaded.".to_string()),
            |device| capture_target(device, &plugged),
        );
        let (devices, keys, refused) = match target {
            Ok(Target::Controllers(devices)) => (devices, None, None),
            // Window events only: nothing outside the process is opened, so
            // an inert session captures keys exactly as a live one does.
            Ok(Target::Keyboard) => (Vec::new(), Some(hangar_bindings::KeyCapture::new()), None),
            Err(why) => (Vec::new(), None, Some((why, false))),
        };
        let (capture, problems) = hangar_bindings::Capture::start(&devices);
        Rebind {
            profile_id: entry.profile_id.clone(),
            context: entry.context.clone(),
            action: entry.action.clone(),
            slot: hangar_bindings::Slot::Primary,
            title,
            was,
            capture,
            problems,
            caught: None,
            // Refused before watching anything: said at once, not waited on.
            outcome: refused,
            revealed: false,
            keys,
            caught_keys: None,
            key_note: None,
        }
    }
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    let library = app.bindings.clone();
    crate::binding_tools::navigation(app, ui);

    if library.is_empty() && library.problems.is_empty() {
        ui::group(ui, None, |ui| {
            ui.label(egui::RichText::new("No input profiles found.").color(ui::theme::DIM));
        });
        return;
    }

    let locked = editing_locked(app);
    let has_edit_notice = edit_notice(ui, locked);
    let has_controller_notice = controllers(app, ui);
    if has_edit_notice || has_controller_notice {
        ui.add_space(12.0);
    }

    if app.rebind.is_some() {
        rebind_card(app, ui);
        ui.add_space(ui::ROW_GAP);
    }

    filters(app, ui, &library);
    ui.add_space(ui::ROW_GAP);

    if app.binding_tools.view == crate::binding_tools::View::Assignments {
        results(app, ui, &library);
    } else {
        crate::binding_tools::draw(app, ui);
    }

    if !library.problems.is_empty() {
        ui.add_space(ui::ROW_GAP);
        problems(ui, &library);
    }
}

/// Show the one condition that currently prevents an edit.
///
/// Steam being open is the normal state, so this is information, not an error.
fn edit_notice(ui: &mut egui::Ui, locked: bool) -> bool {
    if locked {
        ui.label(
            egui::RichText::new("Viewing only · Close Steam to edit")
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
        );
    }
    locked
}

/// The rebind flow: watch, propose, confirm, write.
///
/// Capture only reads devices, so it runs whatever Steam is doing. Mapping an
/// evdev code to an MSFS control is partly inference, so what was caught is
/// shown before anything is written, and the write goes through the same gates
/// as every other profile write. A keyboard rebind reads this window's key
/// events instead, and is confirmed the same way.
fn rebind_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    // Capture is polled per frame, and a joystick press is not an egui event.
    ui.ctx().request_repaint();

    let mut cancel = false;
    let mut confirm = false;

    if let Some(rebind) = &mut app.rebind {
        if rebind.caught.is_none() && rebind.outcome.is_none() {
            rebind.caught = rebind.capture.poll();
        }
        cancel = read_keys(ui.ctx(), rebind);
    }
    if cancel {
        app.rebind = None;
        return;
    }

    let Some(rebind) = &app.rebind else {
        return;
    };
    let reveal = !rebind.revealed;

    let card = ui::group(ui, Some("Rebind"), |ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("{}  \u{00b7}  {}", rebind.title, rebind.context))
                    .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                    .color(ui::theme::TEXT_HI),
            )
            .truncate(),
        );
        if let Some(was) = &rebind.was {
            ui.label(
                egui::RichText::new(format!("currently {}", was.trim()))
                    .size(ui::theme::TEXT_SM)
                    .color(ui::theme::DIMMER),
            );
        }
        ui.add_space(6.0);

        for (name, reason) in &rebind.problems {
            ui.label(
                egui::RichText::new(format!("could not open {name}: {reason}"))
                    .size(ui::theme::TEXT_SM)
                    .color(ui::theme::ERR),
            );
        }

        let proposal = match (&rebind.caught, &rebind.caught_keys) {
            (Some(caught), _) => Some(Proposal::Control(caught)),
            (None, Some(keys)) => Some(Proposal::Keys(keys)),
            (None, None) => None,
        };

        match (&rebind.outcome, proposal) {
            // Done, one way or the other.
            (Some((message, ok)), _) => {
                ui::state(
                    ui,
                    if *ok { "written" } else { "refused" },
                    if *ok { ui::Tone::Ok } else { ui::Tone::Warn },
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(message)
                        .size(ui::theme::TEXT_BODY)
                        .color(if *ok {
                            ui::theme::TEXT
                        } else {
                            ui::theme::WARN
                        }),
                );
                ui.add_space(6.0);
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::X,
                    "Close",
                    ui::ButtonKind::Normal,
                )
                .clicked()
                {
                    cancel = true;
                }
            }

            // Something was caught; it needs agreeing with.
            (None, Some(proposal)) => {
                let (kind, what, source) = match proposal {
                    Proposal::Control(caught) => (
                        caught.confidence.as_str(),
                        caught.describe(),
                        format!(
                            "read from {} \u{2014} does that match what you just moved?",
                            caught.device.tidy_name()
                        ),
                    ),
                    Proposal::Keys(keys) => (
                        if keys.binding.is_chord() {
                            "keys"
                        } else {
                            "key"
                        },
                        keys.describe(),
                        "read from this window \u{2014} does that match what you pressed?"
                            .to_string(),
                    ),
                };
                ui.horizontal(|ui| {
                    ui::state(ui, kind, ui::Tone::Accent);
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(what)
                                .monospace()
                                .size(ui::theme::TEXT_MD)
                                .color(ui::theme::ORANGE),
                        )
                        .truncate(),
                    );
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(source)
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIM),
                );
                if let Proposal::Keys(keys) = proposal {
                    if keys.key_code().is_some_and(has_numpad_twin) {
                        ui.label(
                            egui::RichText::new(
                                "A numpad key reads the same here; this writes the main-row key.",
                            )
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::WARN),
                        );
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui::tool_button(
                        ui,
                        egui_phosphor::regular::CHECK,
                        "Yes, bind it",
                        ui::ButtonKind::Primary,
                    )
                    .clicked()
                    {
                        confirm = true;
                    }
                    if ui::tool_button(
                        ui,
                        egui_phosphor::regular::X,
                        "Cancel",
                        ui::ButtonKind::Normal,
                    )
                    .clicked()
                    {
                        cancel = true;
                    }
                });
            }

            // Still waiting.
            (None, None) => {
                let waiting = if rebind.is_keyboard() {
                    "Press a key or combination. Escape cancels.".to_string()
                } else if rebind.capture.watching() == 0 {
                    "No controllers could be opened \u{2014} reading them needs the \
                     `input` group, or a desktop session's udev ACL."
                        .to_string()
                } else {
                    format!(
                        "Press a button or sweep an axis. Watching {} controller(s).",
                        rebind.capture.watching()
                    )
                };
                ui.label(
                    egui::RichText::new(waiting)
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::DIM),
                );
                if let Some(note) = &rebind.key_note {
                    ui.label(
                        egui::RichText::new(note)
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::WARN),
                    );
                }
                ui.add_space(6.0);
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::X,
                    "Cancel",
                    ui::ButtonKind::Normal,
                )
                .clicked()
                {
                    cancel = true;
                }
            }
        }
        ui.min_rect()
    });

    // Bind is often pressed far down a scrolled list; bring the card up.
    if reveal {
        ui.scroll_to_rect(card, Some(egui::Align::TOP));
        if let Some(rebind) = &mut app.rebind {
            rebind.revealed = true;
        }
    }

    if confirm {
        commit_rebind(app);
    }
    if cancel {
        // Dropping it closes the device handles.
        app.rebind = None;
    }
}

/// What the card is asking to be agreed with.
#[derive(Clone, Copy)]
enum Proposal<'a> {
    Control(&'a hangar_bindings::Captured),
    Keys(&'a hangar_bindings::keyboard::Caught),
}

/// Feed this frame's key events to a waiting keyboard capture.
///
/// Returns `true` when Escape asks to cancel.
///
/// **Only while waiting.** Every key event (and typed text, and copy, cut and
/// paste) is taken out of the frame's input, so a press cannot also type into
/// the search or reach egui's zoom shortcuts, which run at the end of the
/// frame. Once something is caught, or for a controller rebind, nothing is
/// taken. Focus is dropped too, so Space or Enter cannot press a button that
/// was drawn earlier in the frame.
///
/// Escape cancels rather than binds. Three Asobo defaults do bind it
/// (`KEY_MENU_SR_BACK`, `KEY_PAUSE_OFF`, `KEY_PAUSE_TOGGLE`), so this is a
/// deliberate choice.
fn read_keys(ctx: &egui::Context, rebind: &mut Rebind) -> bool {
    use hangar_bindings::keyboard::Unbindable;
    if rebind.caught_keys.is_some() || rebind.outcome.is_some() {
        return false;
    }
    let Some(capture) = rebind.keys.as_mut() else {
        return false;
    };
    let taken: Vec<egui::Event> = ctx.input_mut(|i| {
        let (taken, kept) = std::mem::take(&mut i.events).into_iter().partition(|e| {
            matches!(
                e,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::Copy
                    | egui::Event::Cut
                    | egui::Event::Paste(_)
            )
        });
        i.events = kept;
        taken
    });
    ctx.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });

    for event in taken {
        match event {
            egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                ..
            } => {
                if pressed && (key == egui::Key::Escape || physical_key == Some(egui::Key::Escape))
                {
                    return true;
                }
                if repeat {
                    continue;
                }
                // Physical only: a numpad decimal reports no physical key and
                // a logical `Delete`, and must not be written as Delete.
                let Some(input) = physical_key.and_then(key_input) else {
                    if pressed {
                        let name = physical_key.unwrap_or(key).name().to_string();
                        rebind.key_note = Some(Unbindable::NoEvidence { name }.to_string());
                    }
                    continue;
                };
                let answer = if pressed {
                    capture.press(input)
                } else {
                    capture.release(input)
                };
                match answer {
                    Some(Ok(caught)) => {
                        rebind.caught_keys = Some(caught);
                        rebind.key_note = None;
                        break;
                    }
                    Some(Err(why)) => rebind.key_note = Some(why.to_string()),
                    None => {}
                }
            }
            egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => {
                rebind.key_note = Some(NOT_A_KEY.to_string());
            }
            _ => {}
        }
    }
    false
}

/// Apply the caught control and write, through the ordinary gate.
///
/// The library is reloaded afterwards on success: what is on screen came from
/// the files, and the files just changed.
fn commit_rebind(app: &mut HangarApp) {
    if !app.actions.sessions.is_live() {
        if let Some(rebind) = &mut app.rebind {
            rebind.outcome = Some(("Editing is unavailable in this session.".into(), false));
        }
        return;
    }

    let Some(rebind) = &mut app.rebind else {
        return;
    };
    let (binding, what) = match (&rebind.caught, &rebind.caught_keys) {
        (Some(caught), _) => (
            hangar_bindings::Binding::single(caught.control.to_key()),
            caught.describe(),
        ),
        (None, Some(keys)) => (keys.binding.clone(), keys.describe()),
        (None, None) => return,
    };

    let Some(loaded) = app.bindings.get(&rebind.profile_id) else {
        rebind.outcome = Some(("that profile is no longer loaded".into(), false));
        return;
    };
    let Some(store) = hangar_core::paths::bindings_dir() else {
        rebind.outcome = Some(("no XDG data directory for the snapshot".into(), false));
        return;
    };

    let outcome = write_binding(
        &loaded.profile,
        &loaded.summary.path,
        &store,
        &rebind.context,
        &rebind.action,
        rebind.slot,
        binding,
        &what,
    );
    let ok = outcome.is_ok();
    let stale = outcome.as_ref().is_err_and(|why| why == CHANGED_ON_DISK);
    rebind.outcome = Some(match outcome {
        Ok(message) | Err(message) => (message, ok),
    });
    if ok || stale {
        // The files changed, so what is on screen is stale. After a refusal
        // the next Bind then edits what is on disk now.
        if let Some(dir) = app.bindings.remote_dir.clone() {
            app.bindings = std::sync::Arc::new(hangar_bindings::Library::load(dir));
            app.controllers = None;
        }
    }
}

/// Why a Bind wrote nothing because the file moved on under it.
pub(crate) const CHANGED_ON_DISK: &str = "This profile changed on disk since HangarRS read it, \
     so nothing was written. It has been read again; bind once more.";

/// Put `binding` in one action of a copy of `profile` and write it to `path`
/// through `save::save` — Steam check, snapshot first into `store`, round
/// trip, atomic write, read back. Refused with [`CHANGED_ON_DISK`] unless
/// `profile` is still exactly what `path` holds.
///
/// Separate from [`commit_rebind`] so a test can run the real gate on a
/// scratch copy; the session's live check stays on the action that calls it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_binding(
    profile: &hangar_bindings::Profile,
    path: &std::path::Path,
    store: &std::path::Path,
    context: &str,
    action: &str,
    slot: hangar_bindings::Slot,
    binding: hangar_bindings::Binding,
    what: &str,
) -> Result<String, String> {
    use hangar_bindings::save::{self, Approval};

    // The copy being edited must still be the file on disk. Profiles round-trip
    // byte-identical, so at load the two match; if they no longer do, something
    // else (the sim's own Controls menu, Steam Cloud) changed the file since it
    // was read, and writing this copy would silently revert that. Checked
    // before `save` runs, so a refusal leaves no snapshot and writes nothing.
    let on_disk =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if on_disk != hangar_bindings::to_xml(profile).as_bytes() {
        return Err(CHANGED_ON_DISK.into());
    }

    let mut profile = profile.clone();
    let Some(target) = profile
        .device
        .contexts
        .iter_mut()
        .find(|c| c.name == context)
        .and_then(|c| c.action_mut(action))
    else {
        return Err("that action is no longer in the profile".into());
    };
    target.bind(slot, binding);

    let approval = Approval::given(format!("bind {context}/{action} to {what}"));
    match save::save(&profile, path, store, &approval) {
        Ok(Ok(saved)) => Ok(format!(
            "{what} \u{2014} snapshot taken first at {}",
            saved.snapshot_dir.display()
        )),
        Ok(Err(refusal)) => Err(refusal.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// What the profiles say about each physical controller.
///
/// Duplicates in the profile list are **not a naming quirk**: MSFS enrols a
/// controller more than once on Linux, and only one registration is live.
///
/// Two shapes look identical and mean opposite things: the same device with the
/// *same* GUID is MSFS's alternate-profile feature and is fine; with
/// *different* GUIDs it is one controller enrolled twice.
///
/// Reports only, never merges or deletes: only the sim knows which
/// registration it reads, and a wrong guess costs someone their bindings.
fn controllers(app: &mut HangarApp, ui: &mut egui::Ui) -> bool {
    let survey = controller_survey(app);
    if survey.is_empty() {
        return false;
    }

    let wrong: Vec<_> = survey
        .iter()
        .filter(|view| view.status.needs_attention())
        .collect();
    if wrong.is_empty() {
        return false;
    }

    ui::group(ui, None, |ui| {
        for (n, view) in wrong.iter().enumerate() {
            if n > 0 {
                ui::divider(ui);
            }
            controller_row(ui, view);
        }
    });
    true
}

/// One device.
///
/// A header row, then one consequence **outside** the horizontal, because egui
/// labels only wrap where the available width is finite. Healthy devices never
/// reach this function.
fn controller_row(ui: &mut egui::Ui, view: &hangar_bindings::DeviceView) {
    use hangar_bindings::devices::Status;

    let tone = match view.status {
        Status::DuplicateRegistrations => ui::Tone::Warn,
        Status::NotConnected => ui::Tone::Err,
        Status::AlternateProfiles => ui::Tone::Idle,
        Status::Single | Status::Keyboard => ui::Tone::Ok,
    };
    ui.horizontal(|ui| {
        ui::state(ui, view.status.as_str(), tone);
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(short_device(&view.name))
                    .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                    .color(ui::theme::TEXT_HI),
            )
            .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{:#06x}", view.product_id))
                    .monospace()
                    .size(ui::theme::TEXT_SM)
                    .color(ui::theme::DIMMER),
            );
        });
    });

    ui.label(
        egui::RichText::new(match view.status {
            Status::DuplicateRegistrations => format!(
                "Only one registration is live; check {} saved profiles in the simulator before editing.",
                view.profiles.len()
            ),
            _ => format!(
                "No connected controller reports this product; its {} bindings remain saved.",
                total_bound(view)
            ),
        })
        .size(ui::theme::TEXT_BODY)
        .color(ui::theme::DIM),
    );
}

fn total_bound(view: &hangar_bindings::DeviceView) -> usize {
    view.profiles.iter().map(|p| p.bound).sum()
}

/// Cached so a frame does not rescan devices; expires because a stick can be
/// plugged in while the app is open.
fn controller_survey(app: &mut HangarApp) -> std::sync::Arc<Vec<hangar_bindings::DeviceView>> {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

    if let Some((taken, survey)) = &app.controllers {
        if taken.elapsed() < STALE_AFTER {
            return survey.clone();
        }
    }
    let devices = hangar_core::input::controllers().unwrap_or_default();
    let survey = std::sync::Arc::new(hangar_bindings::survey(&app.bindings, &devices));
    app.controllers = Some((std::time::Instant::now(), survey.clone()));
    survey
}

/// Is Steam up? Cached, because the answer costs a `/proc` scan and the panel
/// asks once a frame.
///
/// Two seconds: closing Steam shows promptly, without walking `/proc` every
/// frame.
pub(crate) fn editing_locked(app: &mut HangarApp) -> bool {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

    if let Some((checked, answer)) = app.steam_check {
        if checked.elapsed() < STALE_AFTER {
            return answer;
        }
    }
    let answer = hangar_steam::is_running();
    app.steam_check = Some((std::time::Instant::now(), answer));
    answer
}

fn filters(app: &mut HangarApp, ui: &mut egui::Ui, library: &Arc<Library>) {
    let lists = lists(app, library);
    ui.horizontal_wrapped(|ui| {
        let devices = &lists.devices;
        let device_label = selected_label(&app.binding_tools.device, devices);
        let previous = app.binding_tools.device.clone();
        ui::select(
            ui,
            "controls-device",
            &mut app.binding_tools.device,
            &device_label,
            260.0,
            devices,
        );
        if previous != app.binding_tools.device {
            app.bindings_profile = None;
        }
        let options: Vec<_> = lists
            .profiles
            .iter()
            .filter(|(id, _)| {
                id.as_deref().is_none_or(|id| {
                    library.get(id).is_some_and(|p| {
                        app.binding_tools
                            .device
                            .as_deref()
                            .is_none_or(|device| device == p.device())
                    })
                })
            })
            .cloned()
            .collect();
        let selected = selected_label(&app.bindings_profile, &options);
        ui::select(
            ui,
            "bindings-profile",
            &mut app.bindings_profile,
            &selected,
            300.0,
            &options,
        );
        if app.binding_tools.view != crate::binding_tools::View::Backups {
            let options = context_options(&hits(app, library), &lists);
            let selected = selected_label(&app.bindings_context, &options);
            ui::select(
                ui,
                "bindings-context",
                &mut app.bindings_context,
                &selected,
                190.0,
                &options,
            );
        }
    });
    if app.binding_tools.view == crate::binding_tools::View::Assignments {
        ui.add_space(8.0);
        ui::text_field(
            ui,
            &mut app.bindings_search,
            "Search actions or controls",
            ui.available_width().min(520.0),
        );
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [
                ("Assigned", Bound::Only),
                ("All actions", Bound::Any),
                ("Unbound", Bound::Never),
            ] {
                if ui::segment(ui, label, app.bindings_bound == value).clicked() {
                    app.bindings_bound = value;
                }
            }
            let matches = hits(app, library).indices.len();
            ui.label(
                egui::RichText::new(format!("{matches} results"))
                    .small()
                    .color(ui::theme::DIM),
            );
        });
    }
}

pub(crate) fn profile_options(library: &Library) -> Vec<(Option<String>, String)> {
    let mut options = vec![(None, "All profiles".to_string())];
    options.extend(library.profiles.iter().map(|loaded| {
        let ambiguous = library
            .profiles
            .iter()
            .filter(|other| other.name() == loaded.name())
            .count()
            > 1;
        let label = match (ambiguous, loaded.profile.device.product_id.parse::<u32>()) {
            (true, Ok(product)) => format!(
                "{} \u{00b7} {:#06x}  ({})",
                loaded.name(),
                product,
                loaded.bound_count()
            ),
            _ => format!("{}  ({})", loaded.name(), loaded.bound_count()),
        };
        (Some(loaded.id().to_string()), label)
    }));
    options
}

fn context_options(hits: &Hits, lists: &Lists) -> Vec<(Option<String>, String)> {
    let mut options = vec![(None, "All contexts".to_string())];
    options.extend(lists.contexts.iter().map(|context| {
        let count = hits.by_context.get(context).copied().unwrap_or(0);
        (Some(context.clone()), format!("{context}  ({count})"))
    }));
    options
}

fn selected_label<T: PartialEq>(selected: &T, options: &[(T, String)]) -> String {
    options
        .iter()
        .find(|(value, _)| value == selected)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| options[0].1.clone())
}

/// Longest device hint shown beside an ambiguous profile name. Wide enough for
/// `Turtle Beach VelocityOne Rudder`, which is 31 characters.
const MAX_DEVICE_CHARS: usize = 34;

/// Drop the vendor boilerplate MSFS records in front of a device name.
///
/// e.g. `VKB-Sim (C) Alex Oz 2023 S-TECS MODERN THROTTLE MAX STEM`: the part
/// that identifies the stick is everything after the year.
fn short_device(device: &str) -> String {
    let tail = device
        .split_whitespace()
        .skip_while(|token| !is_year(token))
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    let name = if tail.is_empty() { device } else { &tail };
    if name.chars().count() > MAX_DEVICE_CHARS {
        format!(
            "{}\u{2026}",
            name.chars().take(MAX_DEVICE_CHARS - 1).collect::<String>()
        )
    } else {
        name.to_string()
    }
}

fn is_year(token: &str) -> bool {
    token.len() == 4 && token.chars().all(|c| c.is_ascii_digit())
}

fn query(app: &HangarApp, _library: &Library) -> Query {
    Query {
        text: app.bindings_search.clone(),
        context: app.bindings_context.clone(),
        // By profile, not only by device: two profiles can share a device name.
        profile: app.bindings_profile.clone(),
        device: app.binding_tools.device.clone(),
        bound: app.bindings_bound,
    }
}

fn results(app: &mut HangarApp, ui: &mut egui::Ui, library: &Arc<Library>) {
    let found = hits(app, library);
    let entries = library.catalog.entries();
    let hits: Vec<_> = found.indices.iter().map(|&i| &entries[i]).collect();

    if hits.is_empty() {
        ui::group(ui, None, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Nothing matches {:?} in this view.",
                    app.bindings_search
                ))
                .color(ui::theme::DIM),
            );
        });
        return;
    }

    ui::group(ui, None, |ui| {
        // Rows are one height, so a row outside the visible area is reserved
        // rather than laid out; laying out all of them lags resizing badly.
        let mut row_h: Option<f32> = None;
        for (n, entry) in hits.iter().take(MAX_ROWS).enumerate() {
            if n > 0 {
                ui.add_space(8.0);
            }
            if let Some(h) = row_h {
                let rect =
                    egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), h));
                if !ui.is_rect_visible(rect) {
                    ui.allocate_space(rect.size());
                    continue;
                }
            }
            let top = ui.cursor().min.y;
            row(app, ui, entry);
            let drawn = ui.min_rect().bottom() - top;
            row_h = Some(row_h.map_or(drawn, |h| h.max(drawn)));
        }
        if hits.len() > MAX_ROWS {
            ui::divider(ui);
            ui.label(
                egui::RichText::new(format!(
                    "\u{2026} and {} more. Narrow the search to see them.",
                    hits.len() - MAX_ROWS
                ))
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
            );
        }
    });
}

/// Room kept at the right of a result row for the binding label and the Bind
/// button.
///
/// **The trap.** The middle column truncates to `available_width`, which inside
/// a `horizontal` is measured *before* the right-to-left cluster is placed, so
/// the row comes out wider than the page and the next row's width grows again,
/// compounding down the list. Reserving the tail up front makes the middle
/// column's width a property of the page, not of the row above.
const ROW_TAIL_W: f32 = 200.0;

fn row(app: &mut HangarApp, ui: &mut egui::Ui, entry: &hangar_bindings::catalog::Entry) {
    let described = describe::describe(&entry.action);

    ui.horizontal(|ui| {
        // Never more than half the row: on a very narrow page the tail giving
        // way is better than the title disappearing entirely.
        let tail = ROW_TAIL_W.min(ui.available_width() * 0.5);
        let middle = (ui.available_width() - tail).max(40.0);

        ui.allocate_ui_with_layout(
            egui::vec2(middle, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width(middle);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&described.title)
                            .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                            .color(ui::theme::TEXT_HI),
                    )
                    .truncate(),
                )
                .on_hover_text(format!("{}\n{}", entry.action, described.summary));
                // Truncated, not wrapped, so the list stays one line per row.
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(if app.bindings_profile.is_none() {
                            short_device(&entry.device)
                        } else {
                            entry.context.clone()
                        })
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIMMER),
                    )
                    .truncate(),
                );
            },
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Capture only reads, so it is offered whatever Steam is doing;
            // the write at the end is what the gate stops. Keyboard and
            // controller profiles alike.
            let capturable = app.bindings.get(&entry.profile_id).is_some();
            if bind_button(ui, capturable).clicked() {
                let title = describe::describe(&entry.action).title;
                let was = entry.primary.clone();
                let device = app
                    .bindings
                    .get(&entry.profile_id)
                    .map(|loaded| &loaded.profile.device);
                app.rebind = Some(Rebind::start(
                    entry,
                    device,
                    title,
                    was,
                    app.actions.sessions.is_live(),
                ));
            }
            ui.add_space(8.0);

            let label = entry.label();
            if label.is_empty() {
                ui.label(
                    egui::RichText::new("not bound")
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIMMER),
                );
            } else {
                // Trimmed for display only: MSFS writes some labels with a
                // trailing space, and the file keeps it.
                ui.label(
                    egui::RichText::new(label.trim())
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::ORANGE),
                );
            }
            ui.add_space(10.0);
        });
    });
}

/// The row's Bind action; disabled when its profile is gone, with a one-line
/// note on hover.
fn bind_button(ui: &mut egui::Ui, capturable: bool) -> egui::Response {
    ui::tool_button_enabled(
        ui,
        REBIND_ENABLED && capturable,
        egui_phosphor::regular::CROSSHAIR,
        "Bind",
        ui::ButtonKind::Ghost,
    )
    .on_disabled_hover_text(BIND_UNAVAILABLE)
}

fn problems(ui: &mut egui::Ui, library: &Library) {
    ui::group(ui, Some("Could not be read"), |ui| {
        for (id, reason) in &library.problems {
            ui.label(
                egui::RichText::new(format!("{id}  \u{2014}  {reason}"))
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::ERR),
            );
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "These are still backed up by a snapshot; they just cannot be edited.",
            )
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::DIMMER),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch library at real scale: seven profiles, 10,752 actions, one in
    /// nine bound.
    fn big_library() -> Arc<Library> {
        big_library_with_guid("{0}")
    }

    /// `big_library` registered under `guid`: `{0}` is MSFS's keyboard GUID.
    fn big_library_with_guid(guid: &str) -> Arc<Library> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-controls-rows-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let contexts = [
            "AIRCRAFT",
            "MENU",
            "DEBUG",
            "DEVMODE",
            "3RD_PARTY",
            "CAMERA",
            "ATC",
            "INSTRUMENT",
            "PLANE",
            "DRONE",
            "EFB",
            "TOOLBAR",
        ];
        for p in 0..7 {
            let mut body = String::new();
            for (ci, context) in contexts.iter().enumerate() {
                body.push_str(&format!("\t<Context ContextName=\"{context}\">\n"));
                for a in 0..128 {
                    let name = format!("KEY_SOME_LONGER_ACTION_NAME_{ci}_{a}_SET");
                    if a % 9 == 0 {
                        body.push_str(&format!(
                            concat!(
                                "\t\t<Action ActionName=\"{}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
                                "\t\t\t<Primary>\n\t\t\t\t<KEY Information=\"Joystick Button {}\">{}</KEY>\n",
                                "\t\t\t</Primary>\n\t\t</Action>\n"
                            ),
                            name,
                            a + 1,
                            a
                        ));
                    } else {
                        body.push_str(&format!(
                            "\t\t<Action ActionName=\"{name}\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n"
                        ));
                    }
                }
                body.push_str("\t</Context>\n");
            }
            let xml = format!(
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<Version Num=\"-1\"/>\n",
                    "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">Profile {}</FriendlyName>\n",
                    "<Device DeviceName=\"Sim (C) Alex Oz 2023 Device {}\" GUID=\"{}\" ProductID=\"{}\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                    "{}</Device>\n\n"
                ),
                p,
                p,
                guid,
                500 + p,
                body
            );
            std::fs::write(dir.join(format!("inputprofile_000000000{p}")), xml).unwrap();
        }
        let library = Arc::new(Library::load(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(library.catalog.len(), 10_752);
        library
    }

    /// An app on the Controls tab that reads nothing from this machine while
    /// drawing and cannot write the real config — the same resets the app's
    /// own render tests apply.
    fn controls_app(library: Arc<Library>) -> (HangarApp, tokio::runtime::Runtime) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let discovery = crate::env::Discovery {
            overlaps: Vec::new(),
            steam: Err("none".into()),
            msfs: Err("none".into()),
            catalog: Default::default(),
            bindings: library,
        };
        let mut app = {
            let _guard = runtime.enter();
            HangarApp::new(
                discovery,
                crate::env::TelemetryLink::idle(),
                hangar_runtime::AppMonitor::inert(Default::default()),
            )
        };
        app.preflight = crate::flight::Settings::default();
        app.config = hangar_core::config::Config::default();
        app.config_problem = None;
        app.config_path = Some(std::env::temp_dir().join(format!(
            "hangar-controls-config-{}.toml",
            std::process::id()
        )));
        app.writes_allowed = app.config.toggles.community_writes;
        app.install.history_read = true;
        app.file_drops = crate::env::FileDrops::Available;
        app.auto_rescan = false;
        // Primed far ahead so no frame walks /proc for Steam or controllers.
        let later = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        app.steam_check = Some((later, false));
        app.controllers = Some((later, Arc::new(Vec::new())));
        app.bindings_bound = Bound::Only;
        app.active_tab = crate::tab::Tab::Bindings;
        (app, runtime)
    }

    fn texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn collect(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((
                    t.galley.job.text.clone(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut out);
        }
        out
    }

    fn input(size: egui::Vec2, time: f64, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(time),
            events,
            ..Default::default()
        }
    }

    fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn assignments_lay_out_only_the_rows_on_screen() {
        // Rows off screen are reserved, not laid out.
        let library = big_library();
        let (mut app, _runtime) = controls_app(Arc::clone(&library));
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let hits = library.catalog.search(&query(&app, &library)).len();
        assert!(hits > MAX_ROWS, "the fixture must overflow the row cap");
        // The first frame builds the font atlas; it is not what resizing costs.
        let _ = ctx.run_ui(input(egui::vec2(1000.0, 800.0), 0.0, vec![]), |ui| {
            app.draw(ui)
        });
        let mut slowest = 0.0f64;
        for (n, width) in [760.0f32, 900.0, 1100.0, 1280.0, 1600.0].iter().enumerate() {
            let started = std::time::Instant::now();
            let output = ctx.run_ui(
                input(egui::vec2(*width, 800.0), 1.0 + n as f64, vec![]),
                |ui| app.draw(ui),
            );
            slowest = slowest.max(started.elapsed().as_secs_f64() * 1000.0);
            assert!(app.page_overflow <= 0.0, "overflow at {width}");
            let binds = texts(&output)
                .iter()
                .filter(|(text, _)| text.contains("Bind"))
                .count();
            assert!(binds > 0, "no row drawn at {width}");
            assert!(
                binds < 40,
                "{binds} rows laid out at {width}; only the visible ones should be"
            );
        }
        eprintln!("slowest Controls frame while resizing: {slowest:.2} ms");
    }

    #[test]
    fn the_search_answer_is_kept_until_the_filters_or_library_change() {
        let library = big_library();
        let (mut app, _runtime) = controls_app(Arc::clone(&library));
        let first = hits(&mut app, &library);
        assert!(
            Arc::ptr_eq(&first, &hits(&mut app, &library)),
            "recomputed per call"
        );
        let lists_first = lists(&mut app, &library);
        assert!(Arc::ptr_eq(&lists_first, &lists(&mut app, &library)));
        assert_eq!(
            first.indices.len(),
            library.catalog.search(&query(&app, &library)).len()
        );

        app.bindings_search = "_0_1".into();
        let narrowed = hits(&mut app, &library);
        assert!(!Arc::ptr_eq(&first, &narrowed));
        assert_eq!(
            narrowed.indices.len(),
            library.catalog.search(&query(&app, &library)).len()
        );
        assert!(narrowed.indices.len() < first.indices.len());

        // A reloaded library is a new answer even with the same filters.
        let reloaded = Arc::new((*library).clone());
        assert!(!Arc::ptr_eq(&narrowed, &hits(&mut app, &reloaded)));
        assert!(!Arc::ptr_eq(&lists_first, &lists(&mut app, &reloaded)));
    }

    #[test]
    fn the_bind_action_is_present_disabled_and_explains_itself() {
        // A plain egui frame: the disabled Bind next to an enabled control at
        // the same place, so the simulated click is shown to land.
        fn frame(
            ctx: &egui::Context,
            time: f64,
            events: Vec<egui::Event>,
            enabled_control: bool,
        ) -> (egui::Response, egui::FullOutput) {
            let mut response = None;
            let output = ctx.run_ui(input(egui::vec2(400.0, 200.0), time, events), |ui| {
                response = Some(if enabled_control {
                    ui::tool_button(
                        ui,
                        egui_phosphor::regular::CROSSHAIR,
                        "Bind",
                        ui::ButtonKind::Ghost,
                    )
                } else {
                    bind_button(ui, false)
                });
            });
            (response.unwrap(), output)
        }
        for enabled_control in [true, false] {
            let ctx = egui::Context::default();
            ui::theme::install(&ctx);
            let (button, output) = frame(&ctx, 0.0, vec![], enabled_control);
            assert!(texts(&output).iter().any(|(t, _)| t.contains("Bind")));
            assert_eq!(button.enabled(), enabled_control);
            let at = button.rect.center();
            frame(
                &ctx,
                0.1,
                vec![egui::Event::PointerMoved(at), press(at, true)],
                enabled_control,
            );
            let (clicked, _) = frame(&ctx, 0.2, vec![press(at, false)], enabled_control);
            assert_eq!(
                clicked.clicked(),
                enabled_control,
                "enabled={enabled_control}"
            );
            if !enabled_control {
                // A fresh pointer that rests on the button.
                let ctx = egui::Context::default();
                ui::theme::install(&ctx);
                frame(&ctx, 0.0, vec![], false);
                frame(&ctx, 0.1, vec![egui::Event::PointerMoved(at)], false);
                let mut shown = false;
                for step in 0..6 {
                    let (_, output) = frame(&ctx, 1.0 + step as f64, vec![], false);
                    shown |= texts(&output).iter().any(|(t, _)| t == BIND_UNAVAILABLE);
                }
                assert!(shown, "the hover note never appeared");
            }
        }
    }

    #[test]
    fn clicking_bind_on_a_keyboard_row_opens_no_devices_and_writes_nothing() {
        let library = big_library();
        let (mut app, _runtime) = controls_app(library);
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let size = egui::vec2(1280.0, 800.0);
        let output = ctx.run_ui(input(size, 0.0, vec![]), |ui| app.draw(ui));
        let (_, rect) = texts(&output)
            .into_iter()
            .find(|(text, _)| text.contains("Bind"))
            .expect("a Bind action is drawn");
        let at = rect.center();
        let _ = ctx.run_ui(
            input(
                size,
                0.1,
                vec![egui::Event::PointerMoved(at), press(at, true)],
            ),
            |ui| app.draw(ui),
        );
        let _ = ctx.run_ui(input(size, 0.2, vec![press(at, false)]), |ui| app.draw(ui));
        // The fixture's GUID is `{0}`, a keyboard: it listens to this window.
        let rebind = app
            .rebind
            .as_ref()
            .expect("a keyboard Bind starts a rebind");
        assert!(rebind.is_keyboard());
        assert_eq!(
            rebind.capture.watching(),
            0,
            "a keyboard rebind opened a device"
        );
        assert!(rebind.problems.is_empty());
        assert!(rebind.outcome.is_none(), "{:?}", rebind.outcome);
        assert!(app.page_overflow <= 0.0);
    }

    /// A keyboard profile listens to this window's keys,
    /// and a controller profile listens only to its own device.
    #[test]
    fn capture_listens_only_to_the_profiles_own_device() {
        use hangar_bindings::model::Device;
        use hangar_core::input::InputDevice;
        let device = |guid: &str, product: &str| Device {
            name: "VKB-Sim S-TECS MODERN THROTTLE MAX STEM ".into(),
            guid: guid.into(),
            product_id: product.into(),
            composite_id: "0".into(),
            hw_ver: "1.0.0.0".into(),
            aircraft_info: None,
            axes: None,
            contexts: vec![],
        };
        let stick = |product: u16| InputDevice {
            product,
            ..InputDevice::default()
        };
        let plugged = [stick(544), stick(302), stick(544)];

        assert_eq!(
            capture_target(&device("{0}", "34891"), &plugged),
            Ok(Target::Keyboard)
        );
        // Not even a plugged-in device with the keyboard's product ID.
        assert_eq!(
            capture_target(&device("{0}", "544"), &plugged),
            Ok(Target::Keyboard)
        );

        let Ok(Target::Controllers(throttle)) =
            capture_target(&device("{979FF95F}", "544"), &plugged)
        else {
            panic!("the throttle's own device is plugged in");
        };
        assert_eq!(throttle.len(), 2);
        assert!(throttle.iter().all(|d| d.product == 544));

        let unplugged = capture_target(&device("{979FF95F}", "28690"), &plugged).unwrap_err();
        assert_eq!(
            unplugged,
            "VKB-Sim S-TECS MODERN THROTTLE MAX STEM is not plugged in."
        );
    }

    /// Bind pressed after scrolling down the list: the card
    /// comes into view and says why it cannot listen (inert: nothing plugged
    /// in), instead of waiting off screen.
    #[test]
    fn bind_far_down_the_list_brings_the_card_into_view() {
        let library = big_library_with_guid("{979FF95F-7D7C-11F1-8002-000044455354}");
        let (mut app, _runtime) = controls_app(library);
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let size = egui::vec2(1280.0, 800.0);
        let mut time = 0.0;
        let mut frame = |app: &mut HangarApp, events: Vec<egui::Event>| {
            time += 0.1;
            ctx.run_ui(input(size, time, events), |ui| app.draw(ui))
        };
        frame(&mut app, vec![]);
        let centre = egui::pos2(640.0, 500.0);
        for _ in 0..8 {
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(centre),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -600.0),
                        modifiers: Default::default(),
                        phase: egui::TouchPhase::Move,
                    },
                ],
            );
        }
        // Let the smooth scroll settle, so the press and release land on the
        // same row.
        for _ in 0..40 {
            frame(&mut app, vec![]);
        }
        let output = frame(&mut app, vec![]);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let visible: Vec<_> = texts(&output)
            .into_iter()
            .filter(|(text, r)| text.contains("Bind") && screen.contains_rect(*r))
            .collect();
        let (_, rect) = visible[visible.len() / 2].clone();
        let at = rect.center();
        frame(
            &mut app,
            vec![egui::Event::PointerMoved(at), press(at, true)],
        );
        frame(&mut app, vec![press(at, false)]);
        let rebind = app
            .rebind
            .as_ref()
            .expect("an enabled Bind starts a rebind");
        let (message, ok) = rebind.outcome.clone().expect("refused at once");
        assert!(!ok);
        assert!(message.ends_with("is not plugged in."), "{message}");

        let mut seen = false;
        for _ in 0..10 {
            let output = frame(&mut app, vec![]);
            seen |= texts(&output)
                .iter()
                .any(|(t, r)| t.contains("is not plugged in.") && screen.contains_rect(*r));
        }
        assert!(seen, "the rebind card never came into view");
        assert!(app.page_overflow <= 0.0);
    }

    #[test]
    fn the_vendor_blob_is_dropped_from_a_device_name() {
        // Both sticks report a copyright notice and a year before the name
        // that actually identifies them.
        assert_eq!(
            short_device("Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA"),
            "VKBsim Gladiator EVO R FSM.GA"
        );
        assert_eq!(
            short_device("VKB-Sim (C) Alex Oz 2023 S-TECS MODERN THROTTLE MAX STEM"),
            "S-TECS MODERN THROTTLE MAX STEM"
        );
    }

    #[test]
    fn a_name_without_a_year_is_left_alone() {
        assert_eq!(
            short_device("Turtle Beach VelocityOne Rudder"),
            "Turtle Beach VelocityOne Rudder"
        );
        assert_eq!(short_device("Keyboard"), "Keyboard");
    }

    #[test]
    fn something_absurdly_long_is_cut_rather_than_breaking_the_row() {
        let long = short_device(&"A".repeat(80));
        assert_eq!(long.chars().count(), MAX_DEVICE_CHARS);
        assert!(long.ends_with('\u{2026}'));
    }

    /// A synthetic keyboard profile in the real files' shape: `GUID="{0}"`,
    /// no `<Axes>`, one bound action. No personal ids.
    const KEYBOARD_FIXTURE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Version Num=\"-1\"/>\n",
        "<FriendlyName PlatformAvailability=\"15\" Locked=\"false\">Scratch keyboard</FriendlyName>\n",
        "<Device DeviceName=\"Keyboard\" GUID=\"{0}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
        "\t<Context ContextName=\"PLANE\">\n",
        "\t\t<Action ActionName=\"KEY_PARKING_BRAKES\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
        "\t\t\t<Primary>\n",
        "\t\t\t\t<KEY Information=\"Control\">162</KEY>\n",
        "\t\t\t\t<KEY Information=\"space\">32</KEY>\n",
        "\t\t\t</Primary>\n",
        "\t\t</Action>\n",
        "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
        "\t</Context>\n",
        "</Device>\n",
        "\n",
    );

    /// A key event as egui-winit sends it: the physical key set.
    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    fn click_text(
        ctx: &egui::Context,
        app: &mut HangarApp,
        size: egui::Vec2,
        time: &mut f64,
        label: &str,
    ) {
        let output = ctx.run_ui(input(size, *time, vec![]), |ui| app.draw(ui));
        let (_, rect) = texts(&output)
            .into_iter()
            .find(|(text, _)| text.contains(label))
            .unwrap_or_else(|| panic!("{label:?} is not drawn"));
        let at = rect.center();
        *time += 0.1;
        let _ = ctx.run_ui(
            input(
                size,
                *time,
                vec![egui::Event::PointerMoved(at), press(at, true)],
            ),
            |ui| app.draw(ui),
        );
        *time += 0.1;
        let _ = ctx.run_ui(input(size, *time, vec![press(at, false)]), |ui| {
            app.draw(ui)
        });
        *time += 0.1;
    }

    #[test]
    fn every_capturable_key_writes_an_observed_code() {
        use hangar_bindings::keyboard::{self, Input};
        let mut reached = std::collections::BTreeSet::new();
        for (egui_key, code) in KEY_CODES {
            assert!(
                keyboard::label(*code).is_some(),
                "{egui_key:?} would write code {code}, which no profile has"
            );
            assert_eq!(key_input(*egui_key), Some(Input::Key(*code)));
            assert!(reached.insert(*code), "code {code} is reached twice");
        }
        for modifier in [
            egui::Key::ShiftLeft,
            egui::Key::ShiftRight,
            egui::Key::ControlLeft,
            egui::Key::ControlRight,
            egui::Key::AltLeft,
            egui::Key::AltRight,
        ] {
            let Some(Input::Modifier(m, side)) = key_input(modifier) else {
                panic!("{modifier:?} is not read as a modifier");
            };
            reached.insert(m.code(side));
        }
        // Observed, and out of reach: Pause, Escape (it cancels), the numpad
        // digits, KP_Subtract and KP_Delete. Nothing else.
        let unreachable: Vec<u32> = keyboard::OBSERVED
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| !reached.contains(c))
            .collect();
        assert_eq!(
            unreachable,
            vec![19, 27, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 109, 110]
        );
        // Keys with no observed code are not read at all.
        for egui_key in [
            egui::Key::Insert,
            egui::Key::F12,
            egui::Key::Semicolon,
            egui::Key::Slash,
            egui::Key::Backtick,
            egui::Key::Period,
            egui::Key::SuperLeft,
        ] {
            assert_eq!(key_input(egui_key), None, "{egui_key:?}");
        }
    }

    #[test]
    fn a_keyboard_bind_captures_a_combination_and_writes_it_to_a_scratch_copy() {
        let root =
            std::env::temp_dir().join(format!("hangar-controls-keyboard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let remote = root.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        let path = remote.join("inputprofile_0000000001");
        std::fs::write(&path, KEYBOARD_FIXTURE).unwrap();
        let library = Arc::new(Library::load(&remote));
        assert!(library.problems.is_empty(), "{:?}", library.problems);

        let (mut app, _runtime) = controls_app(Arc::clone(&library));
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let size = egui::vec2(1280.0, 800.0);
        let mut time = 0.0;
        click_text(&ctx, &mut app, size, &mut time, "Bind");
        assert!(app.rebind.as_ref().is_some_and(Rebind::is_keyboard));

        // Control down, then j, with the text it types: a chord, and the
        // search field gets nothing.
        for events in [
            vec![key(egui::Key::ControlLeft, true)],
            vec![
                key(egui::Key::J, true),
                egui::Event::Text("j".into()),
                key(egui::Key::J, false),
            ],
        ] {
            time += 0.1;
            let _ = ctx.run_ui(input(size, time, events), |ui| app.draw(ui));
        }
        let caught = app
            .rebind
            .as_ref()
            .and_then(|r| r.caught_keys.clone())
            .expect("Control + j was caught");
        assert_eq!(caught.describe(), "Control + j (codes 162 + 74)");
        assert!(app.bindings_search.is_empty(), "{:?}", app.bindings_search);

        time += 0.1;
        let output = ctx.run_ui(input(size, time, vec![]), |ui| app.draw(ui));
        let shown = texts(&output);
        assert!(shown
            .iter()
            .any(|(t, _)| t == "Control + j (codes 162 + 74)"));
        assert!(!shown.iter().any(|(t, _)| t.contains("numpad")));

        // Confirming in an inert session: the guard on the action refuses and
        // the file is untouched.
        click_text(&ctx, &mut app, size, &mut time, "Yes, bind it");
        assert_eq!(
            app.rebind.as_ref().and_then(|r| r.outcome.clone()),
            Some(("Editing is unavailable in this session.".to_string(), false))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), KEYBOARD_FIXTURE);

        // The same binding through the real gate, on the scratch copy.
        let loaded = library.get("inputprofile_0000000001").unwrap();
        let store = root.join("snapshots");
        let message = write_binding(
            &loaded.profile,
            &path,
            &store,
            "PLANE",
            "KEY_PARKING_BRAKES",
            hangar_bindings::Slot::Primary,
            caught.binding.clone(),
            &caught.describe(),
        )
        .expect("a scratch write passes the gate");
        assert!(
            message.starts_with("Control + j (codes 162 + 74)"),
            "{message}"
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            KEYBOARD_FIXTURE.replace(
                "\t\t\t\t<KEY Information=\"space\">32</KEY>\n",
                "\t\t\t\t<KEY Information=\"j\">74</KEY>\n"
            )
        );
        hangar_bindings::roundtrip_ok(&path).expect("the written file round-trips");
        let snapshot = hangar_bindings::snapshot::list(&store);
        assert_eq!(snapshot.len(), 1, "one snapshot, taken first");
        assert_eq!(
            std::fs::read_to_string(snapshot[0].dir.join("inputprofile_0000000001")).unwrap(),
            KEYBOARD_FIXTURE
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A synthetic joystick profile in the real files' shape. No personal ids.
    const JOYSTICK_FIXTURE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Version Num=\"-1\"/>\n",
        "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">Scratch stick</FriendlyName>\n",
        "<Device DeviceName=\"Scratch Stick\" GUID=\"{11111111-2222-3333-4444-555555555555}\" ProductID=\"501\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
        "\t<Context ContextName=\"PLANE\">\n",
        "\t\t<Action ActionName=\"KEY_PARKING_BRAKES\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
        "\t\t\t<Primary>\n",
        "\t\t\t\t<KEY Information=\"Joystick Button 1\">0</KEY>\n",
        "\t\t\t</Primary>\n",
        "\t\t</Action>\n",
        "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
        "\t</Context>\n",
        "</Device>\n",
        "\n",
    );

    /// FINAL_REVIEW S1: the sim's own Controls menu changed the file after
    /// HangarRS read it. Bind must refuse — no snapshot, not one byte written,
    /// the in-sim edit kept — and, once the library is read again, bind on the
    /// fresh copy without losing that edit. Joystick and keyboard rows alike.
    #[test]
    fn bind_refuses_a_profile_that_changed_on_disk_and_keeps_the_newer_file() {
        let bound_jetway = |code_line: &str| {
            format!(
                concat!(
                    "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
                    "\t\t\t<Primary>\n",
                    "{}",
                    "\t\t\t</Primary>\n",
                    "\t\t</Action>\n"
                ),
                code_line
            )
        };
        let unbound_jetway = "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n";
        let cases = [
            (
                "joystick",
                JOYSTICK_FIXTURE,
                // What the sim wrote: Jetway on button 5.
                bound_jetway("\t\t\t\t<KEY Information=\"Joystick Button 5\">4</KEY>\n"),
                // What Bind proposes: Parking Brakes on button 3.
                hangar_bindings::Binding::single(hangar_bindings::Key::new(
                    "Joystick Button 3",
                    "2",
                )),
                (
                    "\t\t\t\t<KEY Information=\"Joystick Button 1\">0</KEY>\n",
                    "\t\t\t\t<KEY Information=\"Joystick Button 3\">2</KEY>\n",
                ),
            ),
            (
                "keyboard",
                KEYBOARD_FIXTURE,
                bound_jetway("\t\t\t\t<KEY Information=\"g\">71</KEY>\n"),
                hangar_bindings::Binding::single(hangar_bindings::Key::new("j", "74")),
                (
                    "\t\t\t\t<KEY Information=\"Control\">162</KEY>\n\t\t\t\t<KEY Information=\"space\">32</KEY>\n",
                    "\t\t\t\t<KEY Information=\"j\">74</KEY>\n",
                ),
            ),
        ];
        for (kind, fixture, sim_edit, binding, (old_keys, new_keys)) in cases {
            let root = std::env::temp_dir().join(format!(
                "hangar-controls-stale-{kind}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let remote = root.join("remote");
            std::fs::create_dir_all(&remote).unwrap();
            let path = remote.join("inputprofile_0000000001");
            std::fs::write(&path, fixture).unwrap();
            let library = Library::load(&remote);
            assert!(
                library.problems.is_empty(),
                "{kind}: {:?}",
                library.problems
            );
            let stale = library.get("inputprofile_0000000001").unwrap();

            // The sim writes the file after HangarRS read it.
            let newer = fixture.replace(unbound_jetway, &sim_edit);
            assert_ne!(newer, fixture, "{kind}: the edit applies");
            std::fs::write(&path, &newer).unwrap();

            let store = root.join("snapshots");
            let refused = write_binding(
                &stale.profile,
                &path,
                &store,
                "PLANE",
                "KEY_PARKING_BRAKES",
                hangar_bindings::Slot::Primary,
                binding.clone(),
                "the new binding",
            );
            assert_eq!(refused, Err(CHANGED_ON_DISK.to_string()), "{kind}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), newer, "{kind}");
            assert!(
                hangar_bindings::snapshot::list(&store).is_empty(),
                "{kind}: a refusal takes no snapshot"
            );

            // Read again, the same Bind writes, and the sim's edit survives.
            let fresh = Library::load(&remote);
            let fresh = fresh.get("inputprofile_0000000001").unwrap();
            write_binding(
                &fresh.profile,
                &path,
                &store,
                "PLANE",
                "KEY_PARKING_BRAKES",
                hangar_bindings::Slot::Primary,
                binding,
                "the new binding",
            )
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                newer.replace(old_keys, new_keys),
                "{kind}"
            );
            hangar_bindings::roundtrip_ok(&path).expect("the written file round-trips");
            let snapshots = hangar_bindings::snapshot::list(&store);
            assert_eq!(snapshots.len(), 1, "{kind}: one snapshot, taken first");
            assert_eq!(
                std::fs::read_to_string(snapshots[0].dir.join("inputprofile_0000000001")).unwrap(),
                newer,
                "{kind}: the snapshot holds the sim's version"
            );

            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn escape_cancels_a_keyboard_capture_instead_of_binding() {
        let (mut app, _runtime) = controls_app(big_library());
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let size = egui::vec2(1280.0, 800.0);
        app.rebind = Some(Rebind::detached_keyboard(
            "Parking Brakes",
            "PLANE",
            "KEY_PARKING_BRAKES",
        ));
        let _ = ctx.run_ui(input(size, 0.0, vec![]), |ui| app.draw(ui));
        // Even with a modifier already held.
        let _ = ctx.run_ui(
            input(
                size,
                0.1,
                vec![
                    key(egui::Key::ShiftLeft, true),
                    key(egui::Key::Escape, true),
                ],
            ),
            |ui| app.draw(ui),
        );
        assert!(app.rebind.is_none(), "Escape did not cancel");
    }

    #[test]
    fn a_key_with_no_observed_code_is_refused_and_capture_keeps_waiting() {
        let (mut app, _runtime) = controls_app(big_library());
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let size = egui::vec2(1280.0, 800.0);
        app.rebind = Some(Rebind::detached_keyboard(
            "Parking Brakes",
            "PLANE",
            "KEY_PARKING_BRAKES",
        ));
        let mut time = 0.0;
        let mut frame = |app: &mut HangarApp, events: Vec<egui::Event>| {
            time += 0.1;
            ctx.run_ui(input(size, time, events), |ui| app.draw(ui))
        };
        frame(&mut app, vec![]);

        let note = |app: &HangarApp| app.rebind.as_ref().and_then(|r| r.key_note.clone());
        let caught = |app: &HangarApp| app.rebind.as_ref().and_then(|r| r.caught_keys.clone());

        frame(&mut app, vec![key(egui::Key::Insert, true)]);
        assert_eq!(
            note(&app).as_deref(),
            Some("Insert has no example in any keyboard profile, so its code would be a guess.")
        );
        // The numpad decimal: no physical key, a logical Delete. Not Delete.
        frame(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Delete,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(caught(&app).is_none(), "the numpad decimal became Delete");
        // A right-hand modifier with a key, and an unseen combination.
        frame(
            &mut app,
            vec![key(egui::Key::ShiftRight, true), key(egui::Key::T, true)],
        );
        assert!(note(&app).unwrap().starts_with("Use the left Shift"));
        frame(
            &mut app,
            vec![
                key(egui::Key::T, false),
                key(egui::Key::ShiftRight, false),
                key(egui::Key::ControlLeft, true),
                key(egui::Key::AltLeft, true),
                key(egui::Key::T, true),
            ],
        );
        assert!(note(&app).unwrap().starts_with("Control + Alt"));
        // Copy arrives as copy, not as Control + c.
        frame(&mut app, vec![egui::Event::Copy]);
        assert_eq!(note(&app).as_deref(), Some(NOT_A_KEY));
        assert!(caught(&app).is_none());

        let output = frame(&mut app, vec![]);
        assert!(texts(&output).iter().any(|(t, _)| t == NOT_A_KEY));
        assert!(texts(&output)
            .iter()
            .any(|(t, _)| t == "Press a key or combination. Escape cancels."));

        // And it is still listening: everything up, then a lone right Alt.
        frame(
            &mut app,
            vec![
                key(egui::Key::T, false),
                key(egui::Key::AltLeft, false),
                key(egui::Key::ControlLeft, false),
                key(egui::Key::AltRight, true),
                key(egui::Key::AltRight, false),
            ],
        );
        assert_eq!(
            caught(&app).map(|c| c.describe()).as_deref(),
            Some("right Alt (code 165)")
        );
    }

    #[test]
    fn keys_are_taken_only_while_a_keyboard_capture_waits() {
        fn left_after(rebind: &mut Rebind) -> (usize, usize) {
            let ctx = egui::Context::default();
            let mut left = (0, 0);
            let _ = ctx.run_ui(
                input(
                    egui::vec2(400.0, 200.0),
                    0.0,
                    vec![
                        egui::Event::PointerMoved(egui::pos2(5.0, 5.0)),
                        key(egui::Key::Minus, true),
                        egui::Event::Text("-".into()),
                    ],
                ),
                |ui| {
                    read_keys(ui.ctx(), rebind);
                    left = ui.input(|i| {
                        let keys = i
                            .events
                            .iter()
                            .filter(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
                            .count();
                        (keys, i.events.len() - keys)
                    });
                },
            );
            left
        }

        let mut waiting = Rebind::detached_keyboard("Zoom", "CAMERA", "KEY_ZOOM_OUT");
        assert_eq!(left_after(&mut waiting), (0, 1), "the pointer is not taken");
        assert!(waiting.caught_keys.is_some(), "minus was caught");

        // Caught already: Control + minus would zoom egui, and is left alone.
        assert_eq!(left_after(&mut waiting), (2, 1));
        // A controller rebind never touches key events.
        let mut stick = Rebind::detached("Zoom", "CAMERA", "KEY_ZOOM_OUT");
        assert_eq!(left_after(&mut stick), (2, 1));
    }

    #[test]
    fn the_keyboard_rebind_card_renders_in_each_state_without_overflow() {
        use hangar_bindings::keyboard::{binding, Modifier, Side, Stroke};
        let chord =
            |held: Vec<(Modifier, Side)>, key: u32| binding(&Stroke::Chord { held, key }).unwrap();

        // What the state must draw, and how to make it.
        type State<'a> = (&'static str, Box<dyn Fn() -> Rebind + 'a>);
        let states: Vec<State<'_>> = vec![
            (
                "Press a key or combination. Escape cancels.",
                Box::new(|| {
                    Rebind::detached_keyboard("Parking Brakes", "PLANE", "KEY_PARKING_BRAKES")
                }),
            ),
            (
                NOT_A_KEY,
                Box::new(|| {
                    let mut r =
                        Rebind::detached_keyboard("Parking Brakes", "PLANE", "KEY_PARKING_BRAKES");
                    r.key_note = Some(NOT_A_KEY.into());
                    r
                }),
            ),
            (
                "Control + Shift + Alt + t (codes 162 + 160 + 164 + 84)",
                Box::new(move || {
                    let mut r =
                        Rebind::detached_keyboard("Parking Brakes", "PLANE", "KEY_PARKING_BRAKES");
                    r.caught_keys = Some(chord(
                        vec![
                            (Modifier::Control, Side::Left),
                            (Modifier::Shift, Side::Left),
                            (Modifier::Alt, Side::Left),
                        ],
                        84,
                    ));
                    r
                }),
            ),
            (
                "A numpad key reads the same here; this writes the main-row key.",
                Box::new(move || {
                    let mut r =
                        Rebind::detached_keyboard("Parking Brakes", "PLANE", "KEY_PARKING_BRAKES");
                    r.caught_keys = Some(chord(vec![], 53));
                    r
                }),
            ),
            (
                "Editing is unavailable in this session.",
                Box::new(|| {
                    let mut r =
                        Rebind::detached_keyboard("Parking Brakes", "PLANE", "KEY_PARKING_BRAKES");
                    r.caught_keys = Some(chord(vec![], 84));
                    r.outcome = Some(("Editing is unavailable in this session.".into(), false));
                    r
                }),
            ),
        ];

        for (expected, make) in states {
            let (mut app, _runtime) = controls_app(big_library());
            let ctx = egui::Context::default();
            ui::theme::install(&ctx);
            app.rebind = Some(make());
            for (n, size) in [
                egui::vec2(1600.0, 1000.0),
                egui::vec2(1280.0, 800.0),
                egui::vec2(1181.0, 791.0),
                egui::vec2(960.0, 640.0),
            ]
            .into_iter()
            .enumerate()
            {
                let output = ctx.run_ui(input(size, n as f64, vec![]), |ui| app.draw(ui));
                assert!(
                    app.page_overflow <= 0.0,
                    "overflow at {size:?} for {expected:?}"
                );
                if n < 2 {
                    assert!(
                        texts(&output).iter().any(|(t, _)| t == expected),
                        "{expected:?} not drawn at {size:?}"
                    );
                }
            }
            assert!(app.rebind.is_some(), "the card closed itself");
        }
    }
}
