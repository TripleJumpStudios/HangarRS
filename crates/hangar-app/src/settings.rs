//! The Settings tab: what HangarRS found, and what it is allowed to touch.

use crate::app::HangarApp;
use hangar_ui as ui;
use std::path::Path;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum View {
    #[default]
    General,
    Simulator,
    Storage,
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    if app.settings_view != View::Simulator {
        let problem = match (&app.discovery.steam, &app.discovery.msfs) {
            (Err(e), _) | (_, Err(e)) => Some(e.clone()),
            _ => None,
        };
        if let Some(reason) = problem {
            ui.label(egui::RichText::new(reason).color(ui::theme::WARN));
            if ui::tool_button(
                ui,
                egui_phosphor::regular::GEAR_SIX,
                "Simulator setup",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.settings_view = View::Simulator;
            }
            ui.add_space(12.0);
        }
    }
    match app.settings_view {
        View::General => write_gate(app, ui),
        View::Storage => {
            crate::replacements::draw(app, ui);
            if !app.writes_allowed {
                ui.add_space(12.0);
                ui.label("Package changes are off.");
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::GEAR_SIX,
                    "Permissions",
                    ui::ButtonKind::Ghost,
                )
                .clicked()
                {
                    app.settings_view = View::General;
                }
            }
        }
        View::Simulator => {
            match (&app.discovery.steam, &app.discovery.msfs) {
                (Err(e), _) => problem(ui, "Steam was not found", e),
                (Ok(_), Err(e)) => problem(ui, "MSFS 2024 was not found", e),
                _ => {}
            }
            steam_card(app, ui);
            ui.add_space(ui::ROW_GAP);
            ui::group(ui, Some("MSFS 2024"), |ui| msfs_rows(app, ui));
            ui.add_space(ui::ROW_GAP);
            ui::group(ui, Some("Proton"), |ui| proton_rows(app, ui));
            ui.add_space(ui::ROW_GAP);
            ui::controls::disclosure(ui, "package-paths", "Package folders", |ui| {
                package_rows(app, ui)
            });
            ui::controls::disclosure(ui, "binding-paths", "Control profile folders", |ui| {
                binding_rows(app, ui)
            });
        }
    }
}

/// Room kept at the right of a gate row for its On/Off chip.
///
/// The chip is 74 px plus spacing. Without the reservation the text beside it
/// wraps to the whole remaining width (measured inside a `horizontal` before
/// the right-to-left cluster is placed) and the chip lands past the page edge.
const GATE_CHIP_W: f32 = 92.0;

/// A gate: a description on the left, an On/Off chip held to the right.
fn gate_row(
    ui: &mut egui::Ui,
    describe: impl FnOnce(&mut egui::Ui),
    control: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        let chip = GATE_CHIP_W.min(ui.available_width() * 0.5);
        let prose = (ui.available_width() - chip).max(40.0);
        ui.allocate_ui_with_layout(
            egui::vec2(prose, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width(prose);
                describe(ui);
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

fn steam_card(app: &HangarApp, ui: &mut egui::Ui) {
    ui::group(ui, Some("Steam"), |ui| steam_rows(app, ui));
}

fn steam_rows(app: &HangarApp, ui: &mut egui::Ui) {
    let Some(steam) = app.discovery.steam() else {
        return;
    };
    path_row(ui, "Install", &steam.steam_root);
    ui::kv_row(ui, "Packaging", |ui| {
        let (label, tone) = if steam.is_flatpak {
            ("Flatpak", ui::Tone::Warn)
        } else {
            ("Native", ui::Tone::Ok)
        };
        ui::state(ui, label, tone);
    });
    for lib in &steam.libraries {
        let label = if lib.is_steam_root {
            "Library (root)"
        } else {
            "Library"
        };
        path_row(ui, label, &lib.path);
    }
}

fn proton_rows(app: &HangarApp, ui: &mut egui::Ui) {
    let Some(msfs) = app.discovery.msfs() else {
        return;
    };
    match &msfs.compat_tool {
        None => {
            ui.label(
                egui::RichText::new(
                    "No compatibility tool is configured for MSFS in Steam. \
                     Read-only features work; nothing can be launched.",
                )
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::WARN),
            );
        }
        Some(tool) => {
            ui::kv_row(ui, "Version", |ui| mono(ui, &tool.display_name));
            path_row(ui, "Proton script", &tool.proton_script);
            path_row(ui, "Wine binary", &tool.wine_binary);
        }
    }
}

fn msfs_rows(app: &HangarApp, ui: &mut egui::Ui) {
    let Some(msfs) = app.discovery.msfs() else {
        return;
    };
    path_row(ui, "Install", &msfs.install.install_dir);
    path_row(ui, "Steam library", &msfs.install.library_root);
    path_row(ui, "Prefix", &msfs.install.prefix);
    if let Some(proton) = &app.proton {
        path_row(ui, "Companion prefix", &proton.compat_data);
    }
}

fn package_rows(app: &HangarApp, ui: &mut egui::Ui) {
    let Some(msfs) = app.discovery.msfs() else {
        return;
    };
    path_row(ui, "UserCfg.opt", &msfs.user_cfg);
    path_row(ui, "Packages root", &msfs.packages_root);
    for folder in &msfs.communities {
        let active = folder.path == msfs.community;
        ui::kv_row(
            ui,
            if active {
                "Community (active)"
            } else {
                "Community"
            },
            |ui| {
                ui.horizontal(|ui| {
                    if active {
                        ui::state(ui, "managed", ui::Tone::Ok);
                    }
                    mono(ui, &file_name(&folder.path));
                });
            },
        );
    }
    for official in msfs.official_folders() {
        ui::kv_row(ui, "Official", |ui| mono(ui, &file_name(&official)));
    }
}

fn binding_rows(app: &HangarApp, ui: &mut egui::Ui) {
    let (Some(steam), Some(msfs)) = (app.discovery.steam(), app.discovery.msfs()) else {
        return;
    };
    let dirs = msfs.binding_dirs(steam);
    if dirs.is_empty() {
        ui.label(
            egui::RichText::new("No Steam Cloud data found for MSFS 2024.")
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
        );
    }
    for dir in &dirs {
        path_row(ui, "Cloud remote", dir);
        let count = std::fs::read_dir(dir)
            .map(|e| {
                e.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("inputprofile_"))
                    .count()
            })
            .unwrap_or(0);
        ui::kv_row(ui, "Input profiles", |ui| {
            mono(ui, &count.to_string());
        });
    }
}

/// The one switch that lets this app modify the Community folder.
///
/// Default off. Applying a profile moves package directories in and out of the
/// live folder: atomic and reversible, but a real change to installed content.
fn write_gate(app: &mut HangarApp, ui: &mut egui::Ui) {
    ui::group(ui, Some("Community folder"), |ui| {
        gate_row(
            ui,
            |ui| {
                ui.label(
                    egui::RichText::new("Allow package changes")
                        .size(ui::theme::TEXT_MD)
                        .color(ui::theme::TEXT),
                );
                ui.label(
                    egui::RichText::new(
                        "Allow installs, updates, and moving packages into or out of Community.",
                    )
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
                );
            },
            |ui| {
                let on = app.writes_allowed;
                if ui::toggle_chip(ui, if on { "On" } else { "Off" }, on, ui::theme::WARN).clicked()
                {
                    app.writes_allowed = !on;
                    app.remove.follow_writes(app.writes_allowed);
                    app.config.toggles.community_writes = app.writes_allowed;
                    app.save_config();
                    // Turning the gate off must also drop any pending confirm,
                    // or a click on a now-hidden button could still fire.
                    if on {
                        app.confirm_apply = None;
                    }
                }
            },
        );
    });
}

fn problem(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui::group(ui, None, |ui| {
        // Outside any `horizontal`: a label there extends the row instead of
        // wrapping, and a title is often a sentence.
        ui::state(ui, "problem", ui::Tone::Err);
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(title)
                .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                .color(ui::theme::TEXT_HI),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(detail)
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
        );
    });
}

/// A path row that says so when the path is not there.
///
/// The value is elided to the width available, not to a character count: a
/// right-aligned run that does not fit grows leftwards over its own label.
fn path_row(ui: &mut egui::Ui, key: &str, path: &Path) {
    let full = path.display().to_string();
    let exists = path.exists();

    ui::kv_row(ui, key, |ui| {
        // Right-to-left: the first widget added is the rightmost.
        let font = egui::FontId::monospace(ui::theme::TEXT_SM);
        let reserved = if exists { 0.0 } else { MISSING_MARK_WIDTH };
        let room = (ui.available_width() - reserved).max(0.0);

        ui.label(
            egui::RichText::new(ui::elide_to_width(ui, &full, &font, room))
                .monospace()
                .size(ui::theme::TEXT_SM)
                .color(if exists {
                    ui::theme::TEXT
                } else {
                    ui::theme::ERR
                }),
        )
        .on_hover_text(&full);

        if !exists {
            ui::state(ui, "missing", ui::Tone::Err);
        }
    });
}

/// Width the "missing" mark needs, reserved so it cannot squeeze the path.
///
/// Generous on purpose: too much costs a few characters of path, too little
/// lets the run grow leftwards over its label.
const MISSING_MARK_WIDTH: f32 = 84.0;

fn mono(ui: &mut egui::Ui, s: &str) {
    ui.label(
        egui::RichText::new(s)
            .monospace()
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::TEXT),
    );
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
