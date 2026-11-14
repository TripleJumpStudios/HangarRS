//! The Console tab — everything HangarRS has run, as it runs.
//!
//! The scrollback itself is [`crate::journal::Journal`], written to from every
//! place that starts a process: the Apps section's Launch, Stop and Restart, a
//! preflight's steps, and the installer's stage 4. A **running** app's output
//! reaches it only through `hangar_runtime::Sink`, because `runner::spawn`
//! drops the reader handles for a live process on purpose.
//!
//! # What this tab does not have
//!
//! Output from a process HangarRS **adopted** rather than started: a process
//! someone else started owns its stdout, and nothing outside it can read a pipe
//! it was handed at `exec`. An adopted app shows its lifecycle here, not its
//! console.

use crate::app::HangarApp;
use hangar_ui as ui;

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    let mut sources = app.journal.sources();
    // A deep link keeps its source even before its first event arrives.
    if let Some(source) = &app.console_source {
        if !sources.contains(source) {
            sources.push(source.clone());
        }
    }
    filters(app, ui, &sources);
    ui.add_space(12.0);
    let mut lines = app
        .journal
        .view(app.console_source.as_deref(), &app.console_search);
    if app.console_events_only {
        lines.retain(|line| {
            matches!(
                line.level,
                ui::ConsoleLevel::Note | ui::ConsoleLevel::Done | ui::ConsoleLevel::Fail
            )
        });
    }
    ui::group(ui, None, |ui| {
        let last = app.journal.last_at();
        ui::console_header(ui, lines.len(), &mut app.console_follow, |ui| {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::TRASH,
                "Clear",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.journal.clear();
                app.console_source = None;
            }
            // A scrollback cannot say on its own whether it is still receiving
            // — the tail of something that finished ten minutes ago looks
            // exactly like a live feed that has gone quiet.
            if let Some(at) = last {
                let age = at.elapsed();
                let (text, colour) = if age < std::time::Duration::from_secs(2) {
                    ("receiving".to_string(), ui::theme::OK)
                } else {
                    (format!("quiet {}s", age.as_secs()), ui::theme::DIM)
                };
                ui.label(
                    egui::RichText::new(text)
                        .size(ui::theme::TEXT_SM)
                        .color(colour),
                );
            }
        });

        if lines.is_empty() {
            ui.label(
                egui::RichText::new(if app.journal.is_empty() {
                    "Nothing has been run yet."
                } else {
                    "Nothing matches that filter."
                })
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
            );
            return;
        }

        // Sized to what is left rather than fixed, so the console grows with
        // the window instead of the page overflowing at small sizes. Floored
        // so it does not collapse to nothing on a short window.
        let height = (ui.available_height() - 24.0).max(120.0);
        ui::console_view(ui, "journal", height, &mut app.console_follow, &lines);
    });
}

/// One segment per source, plus a search box.
fn filters(app: &mut HangarApp, ui: &mut egui::Ui, sources: &[String]) {
    ui.horizontal_wrapped(|ui| {
        let mut options = vec![(None, "All sources".to_owned())];
        options.extend(
            sources
                .iter()
                .map(|source| (Some(source.clone()), source.clone())),
        );
        let selected = app
            .console_source
            .clone()
            .unwrap_or_else(|| "All sources".into());
        ui::select(
            ui,
            "activity-source",
            &mut app.console_source,
            &selected,
            200.0,
            &options,
        );
        ui::text_field(
            ui,
            &mut app.console_search,
            "Search activity",
            240.0_f32.min(ui.available_width()),
        );
    });
    ui.horizontal_wrapped(|ui| {
        for (label, events) in [("Events", true), ("All output", false)] {
            if ui::controls::view_tab(ui, label, app.console_events_only == events).clicked() {
                app.console_events_only = events;
            }
        }
    });
}
