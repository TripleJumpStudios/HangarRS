//! Interactive primitives: buttons, state marks, chips, parameter cells.
//!
//! **House rule:** there are zero raw `ui.button` / `egui::Button::new` call
//! sites outside this crate. Everything interactive routes through here, which
//! is what stops panels drifting apart visually.

use crate::theme;
use egui::{Color32, CornerRadius, RichText, Stroke};

/// Indeterminate work: no percentage is implied when the worker has no total.
/// The spinner schedules repaints even when a background job produces no output.
pub fn activity(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(16.0).color(theme::ORANGE));
        ui.label(
            RichText::new(label)
                .size(theme::TEXT_BODY)
                .color(theme::TEXT),
        );
    });
}

/// Visual weight of a [`tool_button`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// Filled accent — one per toolbar, the action the user most likely wants.
    Primary,
    /// Panel fill with a border — the default.
    Normal,
    /// Transparent until hovered — secondary/toggle actions.
    Ghost,
    /// An inline navigation or secondary action.
    Link,
    /// Ghost, but currently on (accent text).
    GhostActive,
    /// Destructive action — outlined in the error colour.
    Danger,
}

/// A Phosphor glyph in the dedicated icon family.
///
/// Always wrap icon codepoints with this: they live in the Private Use Area,
/// which Inter also populates, so relying on the proportional fallback chain
/// silently renders the wrong glyph. See [`theme::ICONS`].
pub fn icon(glyph: &str) -> RichText {
    RichText::new(glyph).family(theme::icons())
}

/// Icon + label as one laid-out run, so the icon resolves in the icon family
/// while the label stays in the UI font.
///
/// Use for any widget that shows a glyph next to words — a plain
/// `format!("{icon} {text}")` puts the glyph in the proportional chain, where
/// Inter's PUA alternates intercept it.
pub fn icon_text(icon_glyph: &str, text: &str, fg: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    let fmt = |font| TextFormat {
        font_id: font,
        color: fg,
        valign: egui::Align::Center,
        ..Default::default()
    };
    if icon_glyph.is_empty() {
        job.append(text, 0.0, fmt(egui::FontId::new(14.0, theme::semibold())));
    } else {
        job.append(
            icon_glyph,
            0.0,
            fmt(egui::FontId::new(15.0, theme::icons())),
        );
        job.append(text, 8.0, fmt(egui::FontId::new(14.0, theme::semibold())));
    }
    job
}

/// Toolbar button: Phosphor icon + label, styled by [`ButtonKind`].
pub fn tool_button(ui: &mut egui::Ui, icon: &str, text: &str, kind: ButtonKind) -> egui::Response {
    let (fill, stroke, fg) = button_skin(kind);

    let response = ui.add(
        egui::Button::new(icon_text(icon, text, fg))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(theme::CONTROL_RADIUS))
            .min_size(egui::vec2(0.0, theme::CONTROL_H)),
    );
    focus_outline(ui, &response);
    response
}

/// Icon action with a keyboard/screen-reader label and a matching tooltip.
pub fn action_icon(ui: &mut egui::Ui, glyph: &str, label: &str) -> egui::Response {
    let response = icon_button(ui, glyph, ButtonKind::Ghost).on_hover_text(label);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response
}

/// A selectable list title. Truncation reserves space for neighboring actions.
pub fn item_title(ui: &mut egui::Ui, text: &str, selected: bool, width: f32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, theme::CONTROL_H), egui::Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            theme::CONTROL_RADIUS,
            if selected { theme::PANEL } else { theme::WELL },
        );
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        egui::FontId::new(theme::TEXT_MD, theme::semibold()),
        theme::TEXT_HI,
    );
    job.wrap.max_width = (width - 16.0).max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter().galley(
        egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT_HI,
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), text));
    focus_outline(ui, &response);
    response.on_hover_text(text)
}

/// Persistent disclosure for actual setup fields and diagnostic values.
pub fn disclosure(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    disclosure_open(ui, id, title, false, add);
}

pub fn disclosure_open(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    open: bool,
    add: impl FnOnce(&mut egui::Ui),
) {
    egui::CollapsingHeader::new(title)
        .id_salt(id)
        .default_open(open)
        .show(ui, add);
}

/// Explicit skins must retain a visible keyboard focus indicator.
pub(crate) fn focus_outline(ui: &egui::Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            CornerRadius::same(4),
            Stroke::new(1.0, theme::ORANGE),
            egui::StrokeKind::Outside,
        );
    }
}

/// [`tool_button`] that can be greyed out.
///
/// Wraps `add_enabled_ui` so a disabled action keeps the project's geometry
/// instead of falling back to a bare `egui::Button` of a different shape.
pub fn tool_button_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: &str,
    text: &str,
    kind: ButtonKind,
) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| tool_button(ui, icon, text, kind))
        .inner
}

/// Semantic colour of a [`state`] mark or a bare [`mark`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Ok,
    Warn,
    Err,
    Accent,
    Idle,
}

impl Tone {
    /// The palette colour this tone paints with.
    pub fn color(self) -> Color32 {
        match self {
            Tone::Ok => theme::OK,
            Tone::Warn => theme::WARN,
            Tone::Err => theme::ERR,
            Tone::Accent => theme::ORANGE,
            Tone::Idle => theme::DIM,
        }
    }

    /// The Phosphor glyph a [`state`] mark paints for this tone.
    ///
    /// All five are from the circle family, so a column of them reads as one
    /// set and sits in the same visual language as the nav rail's `PACKAGE` /
    /// `WRENCH` / `GEAR_SIX`. `WARNING_CIRCLE` rather than `WARNING_OCTAGON`
    /// for that reason alone.
    pub fn glyph(self) -> &'static str {
        use egui_phosphor::regular as icons;
        match self {
            // Present and correct — the direct counterpart to `X_CIRCLE`.
            Tone::Ok => icons::CHECK_CIRCLE,
            Tone::Warn => icons::WARNING_CIRCLE,
            Tone::Err => icons::X_CIRCLE,
            // Live / resident: the row-level echo of the rail's orange bar.
            Tone::Accent => icons::DOT_OUTLINE,
            // Present, nothing to say.
            Tone::Idle => icons::CIRCLE,
        }
    }
}

/// Phosphor glyph + word, both in the tone's colour — the row-level echo of
/// [`crate::shell::nav_item`], which pairs a glyph and a sentence-case label
/// the same way.
///
/// **The word is always drawn**, so colour is never the sole carrier of the
/// meaning. Returns the `Response` so a caller can hang detail on
/// `.on_hover_text`.
///
/// # Why not `icon_text`
///
/// `icon_text` lays the glyph and the label into one `LayoutJob`, so the
/// label's x-position follows the glyph's own advance width — and Phosphor's
/// glyphs do not share one, so a list mixing tones would go ragged down its
/// left edge. The fixed [`GLYPH_SLOT`] makes the alignment structural;
/// `every_tones_word_starts_at_the_same_x` holds it there.
pub fn state(ui: &mut egui::Ui, text: &str, tone: Tone) -> egui::Response {
    let color = tone.color();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(GLYPH_SLOT, GLYPH_SLOT), egui::Sense::hover());
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            tone.glyph(),
            egui::FontId::new(GLYPH_SIZE, theme::icons()),
            color,
        );
        ui.label(
            RichText::new(text)
                .font(egui::FontId::new(theme::TEXT_BODY, theme::semibold()))
                .color(color),
        );
    })
    .response
}

/// The glyph of a [`state`] mark with no word beside it.
///
/// For a **list**, and only for a list: for states that are simply how a
/// working library looks (Ready, Installed, Enabled…) the same word on every
/// row is noise. States a person has to act on keep their word via [`state`].
///
/// Colour is still not the sole carrier: [`Tone::glyph`] gives each tone a
/// different **shape**, and the caller is expected to hang the word on
/// `.on_hover_text`. Same [`GLYPH_SLOT`] as [`state`], so a column mixing the
/// two lines up down its left edge.
pub fn mark(ui: &mut egui::Ui, tone: Tone) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(GLYPH_SLOT, GLYPH_SLOT), egui::Sense::hover());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        tone.glyph(),
        egui::FontId::new(GLYPH_SIZE, theme::icons()),
        tone.color(),
    );
    response
}

/// Fixed box the state glyph is centred in, whatever it is.
pub const GLYPH_SLOT: f32 = 16.0;

/// Point size the glyph is painted at inside [`GLYPH_SLOT`].
const GLYPH_SIZE: f32 = 14.0;

/// A filled circle — status dots, legend markers.
pub fn dot(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size * 0.5, color);
}

/// On/off chip. Tinted with the supplied colour when on, flat grey when off.
pub fn toggle_chip(ui: &mut egui::Ui, text: &str, on: bool, color: Color32) -> egui::Response {
    let (fill, stroke, fg) = if on {
        (
            color.gamma_multiply(0.16),
            Stroke::new(1.0_f32, color.gamma_multiply(0.55)),
            color,
        )
    } else {
        (
            theme::WELL,
            Stroke::new(1.0_f32, theme::STROKE_SOFT),
            theme::DIMMER,
        )
    };
    ui.add(
        egui::Button::new(RichText::new(text).color(fg))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(7))
            .min_size(egui::vec2(74.0, 28.0)),
    )
}

/// Segmented-control button — filter selectors, the Hangar tab's kind switcher.
pub fn segment(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(if active {
            Color32::WHITE
        } else {
            theme::DIM
        }))
        .fill(if active {
            theme::ORANGE_DIM
        } else {
            Color32::TRANSPARENT
        })
        .stroke(if active {
            Stroke::new(1.0_f32, theme::ORANGE.gamma_multiply(0.6))
        } else {
            Stroke::NONE
        })
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(0.0, 28.0)),
    )
}

/// Local page destination with an underline; distinct from a filter chip.
pub fn view_tab(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    let response = ui.add(
        egui::Button::new(RichText::new(text).color(if active {
            theme::TEXT_HI
        } else {
            theme::DIM
        }))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(0.0, 36.0)),
    );
    if active {
        ui.painter().hline(
            response.rect.x_range(),
            response.rect.bottom(),
            Stroke::new(2.0, theme::ORANGE),
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            text,
        )
    });
    focus_outline(ui, &response);
    response
}

/// Compact selector for filters with too many choices for a segmented row.
///
/// The Bindings tab can hold several saved profiles and dozens of contexts.
/// Rendering either set as chips turns the filter into the largest thing on
/// the page, so those choices use one consistently-sized control instead.
pub fn select<T: Clone + PartialEq>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    selected: &mut T,
    selected_text: &str,
    width: f32,
    options: &[(T, String)],
) -> egui::Response {
    // available_width includes the next wrapped row; inspect the current row.
    if ui.layout().main_wrap() && ui.available_rect_before_wrap().width() < width {
        ui.end_row();
    }
    let width = width.min(ui.available_width());
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = theme::CONTROL_H;
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(RichText::new(selected_text).color(theme::TEXT))
            .width(width)
            .truncate()
            .show_ui(ui, |ui| {
                for (value, label) in options {
                    ui.selectable_value(selected, value.clone(), label);
                }
            })
            .response
    })
    .inner
}

/// Labelled slider + drag value. Returns true if the value changed.
pub fn param_cell(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    decimals: usize,
) -> bool {
    // Pinned rather than sized to content: a `DragValue` fits itself to the
    // text it happens to be showing, so "1.00" and "0.8" come out different
    // widths and the value boxes down a column of cells do not line up.
    const VALUE_W: f32 = 52.0;

    let mut changed = false;
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label)
                .size(theme::TEXT_BODY)
                .color(theme::DIM),
        );
        ui.add_space(1.0);
        ui.horizontal(|ui| {
            changed |= ui
                .add(egui::Slider::new(value, range.clone()).show_value(false))
                .changed();
            changed |= ui
                .add_sized(
                    egui::vec2(VALUE_W, ui.spacing().interact_size.y),
                    egui::DragValue::new(value)
                        .fixed_decimals(decimals)
                        .range(range)
                        .speed(0.001),
                )
                .changed();
        });
    });
    changed
}

/// Which end of a [`stepper`] was pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Dec,
    Inc,
}

/// Square icon-only button, same height and radius as [`tool_button`].
///
/// Exists so an icon action stops falling back to `ui.small_button`, which
/// takes egui's defaults — a different height and corner radius from every
/// other button in the app.
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, kind: ButtonKind) -> egui::Response {
    let (fill, stroke, fg) = button_skin(kind);
    let response = ui.add(
        egui::Button::new(
            RichText::new(glyph)
                .family(theme::icons())
                .size(15.0)
                .color(fg),
        )
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(theme::CONTROL_RADIUS))
        .min_size(egui::vec2(theme::CONTROL_H, theme::CONTROL_H)),
    );
    focus_outline(ui, &response);
    response
}

/// A `−  value  +` cluster. The caller draws the middle, so the same control
/// serves an editable value and a read-only mirror.
///
/// Returns which end was pressed, if any.
pub fn stepper(ui: &mut egui::Ui, middle: impl FnOnce(&mut egui::Ui)) -> Option<Step> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // U+2212 MINUS SIGN, not a hyphen: it matches the "+" optically.
        if step_button(ui, "\u{2212}").clicked() {
            out = Some(Step::Dec);
        }
        middle(ui);
        if step_button(ui, "+").clicked() {
            out = Some(Step::Inc);
        }
    });
    out
}

/// One end of a [`stepper`]. Text rather than an icon — Phosphor has no
/// plus/minus that reads as cleanly at this size.
fn step_button(ui: &mut egui::Ui, glyph: &str) -> egui::Response {
    let (fill, stroke, fg) = button_skin(ButtonKind::Normal);
    ui.add(
        egui::Button::new(
            RichText::new(glyph)
                .font(egui::FontId::new(15.0, theme::semibold()))
                .color(fg),
        )
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(theme::CONTROL_RADIUS))
        .min_size(egui::vec2(theme::CONTROL_H, theme::CONTROL_H)),
    )
}

/// Single-line text input on the project's control geometry.
pub fn text_field(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) -> egui::Response {
    ui.add_sized(
        egui::vec2(width, theme::CONTROL_H),
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .margin(egui::Margin::symmetric(8, 6))
            .background_color(theme::WELL),
    )
}

/// Fill / stroke / foreground for a [`ButtonKind`], shared by every button
/// primitive so a new kind cannot be styled differently in two places.
fn button_skin(kind: ButtonKind) -> (Color32, Stroke, Color32) {
    match kind {
        ButtonKind::Primary => (theme::ORANGE, Stroke::NONE, theme::BG),
        ButtonKind::Normal => (
            theme::PANEL_HI,
            Stroke::new(1.0_f32, theme::STROKE),
            theme::TEXT,
        ),
        ButtonKind::Ghost => (Color32::TRANSPARENT, Stroke::NONE, theme::DIM),
        ButtonKind::Link => (Color32::TRANSPARENT, Stroke::NONE, theme::ORANGE),
        ButtonKind::GhostActive => (
            theme::ORANGE.gamma_multiply(0.12),
            Stroke::new(1.0_f32, theme::ORANGE.gamma_multiply(0.5)),
            theme::ORANGE,
        ),
        ButtonKind::Danger => (
            theme::ERR.gamma_multiply(0.10),
            Stroke::new(1.0_f32, theme::ERR.gamma_multiply(0.55)),
            theme::ERR,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One headless frame with the real fonts installed — the glyph metrics are
    /// the whole point of these tests, so egui's defaults will not do.
    fn frame(build: impl FnMut(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, build);
    }

    /// **The reason [`GLYPH_SLOT`] is fixed.** Phosphor's glyphs do not share an
    /// advance width, so laying the mark out as one `LayoutJob` would put the
    /// word at a different x for every tone, and a column of rows would go
    /// ragged down its left edge.
    #[test]
    fn every_tones_word_starts_at_the_same_x() {
        let seen: RefCell<Vec<(Tone, f32)>> = RefCell::new(Vec::new());

        frame(|ui| {
            for tone in [Tone::Ok, Tone::Warn, Tone::Err, Tone::Accent, Tone::Idle] {
                ui.horizontal(|ui| {
                    let before = ui.cursor().min.x;
                    let resp = state(ui, "installed", tone);
                    // The word sits one fixed slot plus one fixed gap in.
                    seen.borrow_mut().push((tone, resp.rect.left() - before));
                });
            }
        });

        let seen = seen.into_inner();
        assert_eq!(seen.len(), 5, "every tone should have drawn");
        let (_, first) = seen[0];
        for (tone, left) in &seen {
            assert!(
                (left - first).abs() < 0.5,
                "{tone:?} starts {:.2} px from the others",
                (left - first).abs()
            );
        }
    }

    /// The marks must also be the same *width* per word, or a right-aligned
    /// value column goes ragged instead of the left one.
    #[test]
    fn every_tone_occupies_the_same_box() {
        let seen: RefCell<Vec<(Tone, egui::Vec2)>> = RefCell::new(Vec::new());

        frame(|ui| {
            for tone in [Tone::Ok, Tone::Warn, Tone::Err, Tone::Accent, Tone::Idle] {
                let resp = state(ui, "installed", tone);
                seen.borrow_mut().push((tone, resp.rect.size()));
            }
        });

        let seen = seen.into_inner();
        let (_, first) = seen[0];
        for (tone, size) in &seen {
            assert!(
                (size.x - first.x).abs() < 0.5 && (size.y - first.y).abs() < 0.5,
                "{tone:?} is {size:?}, others are {first:?}"
            );
        }
    }

    /// A [`mark`] and a [`state`] can share a column, so the glyph has to
    /// occupy the same box in both, whatever the tone.
    #[test]
    fn a_bare_mark_is_the_same_fixed_slot_a_state_uses() {
        let marks: RefCell<Vec<(Tone, egui::Vec2)>> = RefCell::new(Vec::new());
        let states: RefCell<Vec<(Tone, f32)>> = RefCell::new(Vec::new());

        frame(|ui| {
            for tone in [Tone::Ok, Tone::Warn, Tone::Err, Tone::Accent, Tone::Idle] {
                ui.horizontal(|ui| {
                    marks.borrow_mut().push((tone, mark(ui, tone).rect.size()));
                });
                states
                    .borrow_mut()
                    .push((tone, state(ui, "running", tone).rect.width()));
            }
        });

        for (tone, size) in marks.into_inner() {
            assert!(
                (size.x - GLYPH_SLOT).abs() < 0.5 && (size.y - GLYPH_SLOT).abs() < 0.5,
                "{tone:?} allocated {size:?}, not the fixed slot — a column of bare \
                 marks would go ragged on the font's advance widths"
            );
        }

        // Same word, so any difference between tones is the glyph's doing —
        // and there must not be one.
        let states = states.into_inner();
        let (_, first) = states[0];
        for (tone, width) in &states {
            assert!(
                (width - first).abs() < 0.5,
                "{tone:?} is {width:.2} px wide, others are {first:.2}"
            );
        }
    }

    #[test]
    fn a_selector_uses_the_shared_control_geometry() {
        let rect = RefCell::new(egui::Rect::NOTHING);
        frame(|ui| {
            let mut selected = 0;
            let options = [(0, "All profiles".to_string()), (1, "Joystick".to_string())];
            *rect.borrow_mut() = select(
                ui,
                "geometry-test",
                &mut selected,
                "All profiles",
                220.0,
                &options,
            )
            .rect;
        });

        let rect = rect.into_inner();
        assert!((rect.width() - 220.0).abs() < 0.5);
        assert!((rect.height() - theme::CONTROL_H).abs() < 0.5);
    }
}

#[cfg(test)]
mod keyboard_actions {
    use super::*;

    #[test]
    fn list_titles_and_icon_actions_activate_from_the_keyboard() {
        for title in [false, true] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut activated = false;
            for frame in 0..3 {
                let events = if frame == 0 {
                    vec![]
                } else {
                    vec![egui::Event::Key {
                        key: egui::Key::Space,
                        physical_key: None,
                        pressed: frame == 1,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]
                };
                let _ = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let response = if title {
                            item_title(
                                ui,
                                "An unusually long application title for keyboard selection",
                                false,
                                240.0,
                            )
                        } else {
                            action_icon(ui, egui_phosphor::regular::ARROW_UP, "Move up")
                        };
                        if frame == 0 {
                            response.request_focus();
                        }
                        activated |= response.clicked();
                    },
                );
            }
            assert!(activated, "keyboard did not activate title={title}");
        }
    }
}
