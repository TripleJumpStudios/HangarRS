//! Saved loadouts (sets of Community packages) and their exact package moves.
//!
//! Applying a profile writes to the live Community folder, so it is gated by
//! the Settings opt-in and a two-stage confirm. Managing profiles only writes
//! this app's own config and needs no gate.

use crate::app::HangarApp;
use hangar_catalog::switcher::Switcher;
use hangar_catalog::{apply_diff, plan_diff, Profile};
use hangar_ui as ui;
use std::collections::BTreeSet;

/// Shown while MSFS is up. The sim reads Community once, at startup.
pub const SIM_RUNNING: &str =
    "MSFS is running: a loadout applied now takes effect after it restarts.";

pub fn draw(app: &mut HangarApp, ui_: &mut egui::Ui) {
    let ui = ui_;
    let Some(env) = app.discovery.msfs() else {
        ui::group(ui, None, |ui| {
            ui.label(
                egui::RichText::new("MSFS was not found, so there is nothing to switch.")
                    .color(ui::theme::DIM),
            );
        });
        return;
    };
    let community = env.community.clone();

    toolbar(app, ui, &community);
    ui.add_space(ui::ROW_GAP);

    // Harmless but ineffective (see `switcher`): say so before the click.
    if app.sim_process_running() {
        ui.label(
            egui::RichText::new(SIM_RUNNING)
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::WARN),
        );
        ui.add_space(ui::ROW_GAP);
    }

    if let Some((message, ok)) = &app.profile_result {
        result_line(ui, message, *ok);
        ui.add_space(ui::ROW_GAP);
    }

    if app.profiles.is_empty() {
        ui::group(ui, None, |ui| {
            ui.label(
                egui::RichText::new("No loadouts yet. Capture the current selection to save one.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
        });
        return;
    }

    let names: Vec<String> = app
        .profiles
        .names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    if app
        .selected_profile
        .as_ref()
        .is_none_or(|selected| !names.contains(selected))
    {
        app.selected_profile = names.first().cloned();
    }
    ui.horizontal_wrapped(|ui| {
        for name in &names {
            if ui::segment(ui, name, app.selected_profile.as_ref() == Some(name)).clicked() {
                app.selected_profile = Some(name.clone());
                app.confirm_apply = None;
                app.confirm_delete_profile = None;
            }
        }
    });
    ui.add_space(ui::ROW_GAP);
    if let Some(name) = app.selected_profile.clone() {
        profile_card(app, ui, &name, &community);
    }
}

fn toolbar(app: &mut HangarApp, ui: &mut egui::Ui, community: &std::path::Path) {
    ui.horizontal(|ui| {
        ui::text_field(ui, &mut app.new_profile_name, "Loadout name", 220.0);

        let name = app.new_profile_name.trim().to_owned();
        let enabled = !name.is_empty();
        if ui::tool_button_enabled(
            ui,
            enabled,
            egui_phosphor::regular::CAMERA,
            "Save current selection",
            ui::ButtonKind::Primary,
        )
        .on_hover_text("Save everything currently enabled as a new loadout")
        .clicked()
        {
            let current = Switcher::new(community).enabled_names();
            let count = current.len();
            let result = app.profiles.create(Profile::from_enabled(&name, current));
            app.profile_result = Some(match result {
                Ok(()) => (format!("Captured {count} packages as {name:?}"), true),
                Err(e) => (e.to_string(), false),
            });
            app.new_profile_name.clear();
            if app.profile_result.as_ref().is_some_and(|(_, ok)| *ok) {
                app.selected_profile = Some(name);
            }
        }
    });
}

fn profile_card(app: &mut HangarApp, ui: &mut egui::Ui, name: &str, community: &std::path::Path) {
    let Some(profile) = app.profiles.get(name).cloned() else {
        return;
    };
    let switcher = Switcher::new(community);
    let current = switcher.enabled_names();
    let (to_enable, to_disable) = plan_diff(&current, &profile.packages);
    let is_current = to_enable.is_empty() && to_disable.is_empty();

    ui::group(ui, None, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&profile.name)
                            .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                            .color(ui::theme::TEXT_HI),
                    );
                    if is_current {
                        ui::state(ui, "matches current selection", ui::Tone::Ok);
                    }
                });
                ui.label(
                    egui::RichText::new(format!("{} packages", profile.packages.len()))
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIMMER),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                apply_control(
                    app,
                    ui,
                    &profile,
                    &to_enable,
                    &to_disable,
                    is_current,
                    community,
                );
            });
        });

        ui.add_space(10.0);
        ui::heading(ui, "Changes to apply");
        if is_current {
            ui.label(
                egui::RichText::new("This loadout matches the current selection.")
                    .color(ui::theme::DIM),
            );
        } else {
            ui.label(
                egui::RichText::new(format!(
                    "Enable {} · Disable {}",
                    to_enable.len(),
                    to_disable.len()
                ))
                .size(ui::theme::TEXT_MD)
                .color(ui::theme::TEXT_HI),
            );
            for name in &to_enable {
                ui.label(
                    egui::RichText::new(format!("Enable {name}"))
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::TEXT),
                );
            }
            for name in &to_disable {
                ui.label(
                    egui::RichText::new(format!("Disable {name}"))
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::TEXT),
                );
            }
        }
        ui.add_space(18.0);
        if app.confirm_delete_profile.as_deref() == Some(profile.name.as_str()) {
            ui.label(
                egui::RichText::new(format!(
                    "Delete saved loadout {:?}? Installed packages stay where they are.",
                    profile.name
                ))
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::WARN),
            );
            ui.horizontal(|ui| {
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::TRASH,
                    "Delete loadout",
                    ui::ButtonKind::Danger,
                )
                .clicked()
                {
                    let result = app.profiles.delete(&profile.name);
                    app.profile_result = Some(match result {
                        Ok(()) => (format!("Deleted {:?}", profile.name), true),
                        Err(e) => (e.to_string(), false),
                    });
                    app.confirm_delete_profile = None;
                    app.confirm_apply = None;
                    app.selected_profile = None;
                }
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::X,
                    "Cancel",
                    ui::ButtonKind::Ghost,
                )
                .clicked()
                {
                    app.confirm_delete_profile = None;
                }
            });
        } else if ui::tool_button(
            ui,
            egui_phosphor::regular::TRASH,
            "Delete saved loadout…",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            app.confirm_delete_profile = Some(profile.name.clone());
            app.confirm_apply = None;
        }
    });
}

/// Apply a loadout: the action behind the second click. Its guards are here,
/// not on the button — the write toggle, and nothing else changing Community
/// (`HangarApp::community_busy`: an install, a placement or a Remove).
pub(crate) fn apply_loadout(app: &mut HangarApp, profile: &Profile, community: &std::path::Path) {
    app.confirm_apply = None;
    let refused = if !app.writes_allowed {
        Some("Writing to the Community folder is switched off. Turn it on in Settings.")
    } else {
        app.community_busy()
    };
    if let Some(why) = refused {
        app.profile_result = Some((format!("{}: {why}", profile.name), false));
        return;
    }
    let switcher = Switcher::new(community);
    let current: BTreeSet<String> = switcher.enabled_names();
    let report = apply_diff(&switcher, &current, &profile.packages);
    app.profile_result = Some((
        format!("{}: {}", profile.name, report.summary()),
        report.is_clean(),
    ));
    app.rescan_requested = true;
}

/// The apply button, its gate, and its confirm.
fn apply_control(
    app: &mut HangarApp,
    ui: &mut egui::Ui,
    profile: &Profile,
    to_enable: &[String],
    to_disable: &[String],
    is_current: bool,
    community: &std::path::Path,
) {
    if is_current {
        return;
    }

    if !app.writes_allowed {
        ui::tool_button_enabled(
            ui,
            false,
            egui_phosphor::regular::LOCK,
            "Apply loadout",
            ui::ButtonKind::Normal,
        )
        .on_hover_text("Writing to the Community folder is switched off. Turn it on in Settings.");
        return;
    }

    let awaiting = app.confirm_apply.as_deref() == Some(profile.name.as_str());
    if awaiting {
        // Second stage: the button now says exactly what it is about to do.
        if ui::tool_button(
            ui,
            egui_phosphor::regular::CHECK,
            &format!("Enable {} · Disable {}", to_enable.len(), to_disable.len()),
            ui::ButtonKind::Danger,
        )
        .clicked()
        {
            apply_loadout(app, profile, community);
        }
        if ui::tool_button(ui, "", "Cancel", ui::ButtonKind::Ghost).clicked() {
            app.confirm_apply = None;
        }
    } else if ui::tool_button(
        ui,
        egui_phosphor::regular::ARROWS_LEFT_RIGHT,
        "Apply loadout",
        ui::ButtonKind::Normal,
    )
    .clicked()
    {
        app.confirm_apply = Some(profile.name.clone());
    }
}

fn result_line(ui: &mut egui::Ui, message: &str, ok: bool) {
    ui::group(ui, None, |ui| {
        ui::state(
            ui,
            if ok { "done" } else { "failed" },
            if ok { ui::Tone::Ok } else { ui::Tone::Err },
        );
        ui.add_space(2.0);
        // Outside the mark's horizontal: a failure message can be any length,
        // and a label in a `horizontal` extends the row instead of wrapping.
        ui.label(
            egui::RichText::new(message)
                .size(ui::theme::TEXT_BODY)
                .color(if ok { ui::theme::TEXT } else { ui::theme::ERR }),
        );
    });
}
