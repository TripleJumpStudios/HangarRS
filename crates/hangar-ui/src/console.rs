//! The one console scrollback component.
//!
//! One component, used by every console view, so they cannot drift apart.
//!
//! The type is a **view model**, deliberately owned by the design system rather
//! than by `hangar-core`: the headless crates emit their own log records and
//! `hangar-app` converts once, when a line is pushed into the ring buffer, not
//! once per frame. That is what keeps egui out of the crates below the app.

use crate::theme;
use egui::{Color32, RichText};

/// How one console line should read.
///
/// Note that [`ConsoleLevel::Err`] is *not* styled as a failure. Wine writes a
/// great deal of harmless noise to stderr — `fixme:` chatter on every launch —
/// and colouring all of it red trains the eye to ignore the colour that
/// actually matters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConsoleLevel {
    /// HangarRS narrating what it is about to do.
    Note,
    /// Child process stdout.
    Out,
    /// Child process stderr — noisy under Wine, so styled as incidental.
    Err,
    /// A recognised failure signature, called out by name.
    Fail,
    /// A step that completed.
    Done,
}

/// Palette colour for a level. The single definition — there is no second one.
pub fn level_color(level: ConsoleLevel) -> Color32 {
    match level {
        ConsoleLevel::Note => theme::DIM,
        ConsoleLevel::Out => theme::TEXT,
        ConsoleLevel::Err => theme::DIMMER,
        ConsoleLevel::Fail => theme::ERR,
        ConsoleLevel::Done => theme::OK,
    }
}

/// One line of scrollback.
#[derive(Clone, Debug)]
pub struct ConsoleLine {
    pub level: ConsoleLevel,
    pub text: String,
}

impl ConsoleLine {
    pub fn new(level: ConsoleLevel, text: impl Into<String>) -> Self {
        Self {
            level,
            text: text.into(),
        }
    }

    pub fn note(text: impl Into<String>) -> Self {
        Self::new(ConsoleLevel::Note, text)
    }
    pub fn out(text: impl Into<String>) -> Self {
        Self::new(ConsoleLevel::Out, text)
    }
    pub fn err(text: impl Into<String>) -> Self {
        Self::new(ConsoleLevel::Err, text)
    }
    pub fn fail(text: impl Into<String>) -> Self {
        Self::new(ConsoleLevel::Fail, text)
    }
    pub fn done(text: impl Into<String>) -> Self {
        Self::new(ConsoleLevel::Done, text)
    }
}

/// Monospaced scrollback in a recessed well, with optional stick-to-bottom.
///
/// Rows are drawn through `show_rows` rather than laid out in full: a Wine
/// console runs to thousands of lines during an install, and laying every one
/// of them out each frame stutters.
///
/// `follow` is read *and written*: scrolling up by hand clears it, so the view
/// stops yanking itself back to the bottom while the user is reading.
pub fn console_view(
    ui: &mut egui::Ui,
    id_source: &str,
    height: f32,
    follow: &mut bool,
    lines: &[ConsoleLine],
) {
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);

    egui::Frame::new()
        .fill(theme::WELL)
        .stroke(egui::Stroke::new(1.0_f32, theme::STROKE_SOFT))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let out = egui::ScrollArea::vertical()
                .id_salt(id_source)
                .max_height(height)
                .auto_shrink([false, false])
                .stick_to_bottom(*follow)
                .show_rows(ui, row_h, lines.len(), |ui, range| {
                    for line in &lines[range] {
                        ui.label(
                            RichText::new(&line.text)
                                .monospace()
                                .size(theme::TEXT_BODY)
                                .color(level_color(line.level)),
                        );
                    }
                });

            // Dragging the scrollbar or wheeling up breaks the follow. Compared
            // against a row of slack so that arriving at the bottom by scrolling
            // re-arms it rather than leaving the view stuck one line short.
            let hidden_below = out.content_size.y - out.state.offset.y - out.inner_rect.height();
            if ui.rect_contains_pointer(out.inner_rect)
                && ui.input(|i| i.smooth_scroll_delta.y != 0.0)
            {
                *follow = hidden_below <= row_h;
            }
        });
}

/// Header row for a console: line count on the left, caller-drawn actions on
/// the right. Kept separate so a modal and a full tab can share the scrollback
/// without sharing a toolbar.
pub fn console_header(
    ui: &mut egui::Ui,
    lines: usize,
    follow: &mut bool,
    right: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        crate::layout::heading(ui, &format!("{lines} lines"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui);
            if crate::controls::toggle_chip(ui, "Follow", *follow, theme::ORANGE).clicked() {
                *follow = !*follow;
            }
        });
    });
    ui.add_space(6.0);
}
