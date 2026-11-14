//! The Hangar tab: the library, divided into sections you can browse.
//!
//! Sections rather than internal kinds. Nobody looks for a Cessna under
//! "Package"; they look under Aircraft, and they look for Project Airports
//! under Apps alongside everything else they launch.

use crate::app::HangarApp;
use crate::apps;
use hangar_catalog::{CatalogSnapshot, Item, ItemKind, ItemState, Section};
use hangar_runtime::{proc, ProtonEnv, RunningSet};
use hangar_ui as ui;

pub fn draw(app: &mut HangarApp, ui_: &mut egui::Ui) {
    let ui = ui_;
    let catalog = app.discovery.catalog.clone();

    if catalog.is_empty() {
        ui::group(ui, None, |ui| {
            ui.label(
                egui::RichText::new(
                    "Nothing found. Check the Settings tab for what was discovered.",
                )
                .color(ui::theme::DIM),
            );
        });
        return;
    }

    let selected = app.hangar_selected.as_ref().and_then(|name| {
        catalog
            .items()
            .iter()
            .find(|item| &item.name == name)
            .cloned()
    });
    let narrow = ui.available_width() < 1050.0;
    if narrow {
        if let Some(item) = selected.as_ref() {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::ARROW_LEFT,
                "Back to library",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.hangar_selected = None;
            }
            if let Some((message, ok)) = app.actions.take_result() {
                action_result(app, ui, &message, ok);
                ui.add_space(ui::ROW_GAP);
            }
            detail(app, ui, item);
            return;
        }
    }

    search_row(app, ui, &catalog);
    scanned_line(app, ui);
    ui.add_space(8.0);
    section_tabs(app, ui, &catalog);
    ui.add_space(ui::ROW_GAP);

    if !narrow && selected.is_some() {
        let library_width = (ui.available_width() - 32.0) * 0.55;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(library_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(library_width);
                    library(app, ui, &catalog);
                },
            );
            ui.add_space(24.0);
            ui.vertical(|ui| {
                if let Some(item) = selected.as_ref() {
                    detail(app, ui, item);
                } else {
                    ui.label(
                        egui::RichText::new("Select an item to see its details.")
                            .color(ui::theme::DIM),
                    );
                }
            });
        });
    } else {
        library(app, ui, &catalog);
    }

    if !catalog.problems().is_empty() {
        ui.add_space(ui::ROW_GAP);
        problems(ui, &catalog);
    }
}

fn library(app: &mut HangarApp, ui: &mut egui::Ui, catalog: &CatalogSnapshot) {
    if let Some((message, ok)) = app.actions.take_result() {
        action_result(app, ui, &message, ok);
        ui.add_space(ui::ROW_GAP);
    }
    if !app.discovery.overlaps.is_empty() && app.hangar_section != Some(Section::Apps) {
        overlap_notice(app, ui);
        ui.add_space(ui::ROW_GAP);
    }
    match app.hangar_section {
        // "All" groups by section rather than running everything together, so
        // a flat list of 28 rows still reads as a library.
        None => {
            let mut drew_any = false;
            for section in Section::ALL {
                let items = catalog.section_view(Some(section), &app.hangar_search);
                if items.is_empty() {
                    continue;
                }
                drew_any = true;
                ui::section(ui, section.label());
                item_card(app, ui, &items);
                ui.add_space(ui::ROW_GAP);
            }
            if !drew_any {
                no_matches(app, ui, catalog);
            }
        }
        Some(section) => {
            let items = catalog.section_view(Some(section), &app.hangar_search);
            if items.is_empty() {
                no_matches(app, ui, catalog);
            } else {
                item_card(app, ui, &items);
            }
        }
    }
}

fn detail(app: &mut HangarApp, ui: &mut egui::Ui, item: &Item) {
    crate::install::refresh_history(&mut app.install);
    ui::heading(ui, item.display_title());
    ui.add_space(8.0);
    ui::state(
        ui,
        item.state.label(),
        if needs_attention(&item.state) {
            ui::Tone::Err
        } else {
            ui::Tone::Accent
        },
    );
    ui.add_space(10.0);
    for (key, value) in [
        ("Type", item.kind.label().to_owned()),
        ("Maker", item.creator.clone()),
        ("Version", item.version.clone()),
        (
            "Size",
            item.size_bytes
                .map(hangar_msfs::manifest::format_size)
                .unwrap_or_default(),
        ),
    ] {
        if !value.trim().is_empty() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(key)
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIM),
                );
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(value)
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::TEXT),
                );
            });
        }
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Location")
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::DIM),
    );
    ui.label(
        egui::RichText::new(item.path.display().to_string())
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::TEXT),
    );
    if !item.args.is_empty() {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Launch arguments")
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );
        ui.label(
            egui::RichText::new(item.args.join(" "))
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::TEXT),
        );
    }
    let installs: Vec<_> = app
        .install
        .history
        .iter()
        .filter(|record| {
            record.landed.iter().any(|landed| {
                landed.name == item.name
                    || landed.name == item.display_title()
                    || landed.detail == item.path.display().to_string()
            })
        })
        .take(3)
        .collect();
    if !installs.is_empty() {
        ui.add_space(12.0);
        ui::heading(ui, "Install history");
        for record in installs {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {}",
                    crate::install::when(&record.at),
                    record.verdict
                ))
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::TEXT),
            );
        }
    }
    if item.kind == hangar_catalog::ItemKind::Package {
        set_membership(app, ui, &item.name);
    }
    if is_launchable(item) {
        ui.add_space(14.0);
        let running = app
            .monitor
            .running()
            .get(&proc::match_key(&item.path))
            .is_some();
        launch_controls(app, ui, item, running);
        if ui::tool_button(
            ui,
            egui_phosphor::regular::LIST_BULLETS,
            "Activity",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            app.console_source = Some(item.name.clone());
            app.console_search.clear();
            app.active_tab = crate::tab::Tab::Console;
        }
    }
    crate::remove::draw(app, ui, item);
}

/// Which loadouts hold this package, each one a click to change.
///
/// Writes only this app's own profile files, like saving a set on the
/// Loadouts tab, so it needs no Community gate: nothing moves until a set
/// is applied there.
fn set_membership(app: &mut HangarApp, ui: &mut egui::Ui, package: &str) {
    ui.add_space(12.0);
    ui.label(
        egui::RichText::new("In loadouts")
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::DIM),
    );
    let names: Vec<String> = app
        .profiles
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        ui.label(
            egui::RichText::new("No loadouts yet.")
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
        );
        return;
    }
    let mut toggled = None;
    ui.horizontal_wrapped(|ui| {
        for name in &names {
            let on = app.profiles.get(name).is_some_and(|p| p.contains(package));
            if ui::toggle_chip(ui, name, on, ui::theme::ORANGE)
                .on_hover_text(if on {
                    "In this loadout"
                } else {
                    "Not in this loadout"
                })
                .clicked()
            {
                toggled = Some((name.clone(), !on));
            }
        }
    });
    if let Some((name, on)) = toggled {
        let (message, ok) = toggle_membership(app, &name, package, on);
        app.hangar_set_error = (!ok).then_some(message);
    }
    if let Some(error) = &app.hangar_set_error {
        ui.label(
            egui::RichText::new(error)
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::ERR),
        );
    }
}

/// Add `package` to the set `name`, or take it out, and save the set.
pub(crate) fn toggle_membership(
    app: &mut HangarApp,
    name: &str,
    package: &str,
    on: bool,
) -> (String, bool) {
    let Some(mut profile) = app.profiles.get(name).cloned() else {
        return (format!("{name:?} is no longer saved"), false);
    };
    profile.set(package, on);
    match app.profiles.save(profile) {
        Ok(()) if on => (format!("Added {package} to {name:?}"), true),
        Ok(()) => (format!("Removed {package} from {name:?}"), true),
        Err(e) => (e.to_string(), false),
    }
}

/// When the library was last built.
///
/// The library is a snapshot, not a watch: anything that changes Community
/// from outside leaves the screen stale until the next rescan (on focus and
/// when this tab opens). This line says when that was.
///
/// Outside any `ui.horizontal`, so it wraps rather than extending a row.
fn scanned_line(app: &HangarApp, ui: &mut egui::Ui) {
    let text = match app.last_scan {
        Some(at) => format!("Rescanned {}.", crate::app::scanned_ago(at.elapsed())),
        // Not "never": the startup scan is `Discovery`'s.
        None => "Scanned when HangarRS started.".to_string(),
    };
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text)
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::DIMMER),
    );
}

fn search_row(app: &mut HangarApp, ui: &mut egui::Ui, catalog: &CatalogSnapshot) {
    ui.horizontal(|ui| {
        ui.label(ui::icon(egui_phosphor::regular::MAGNIFYING_GLASS).color(ui::theme::DIM));
        let width = (ui.available_width() - 150.0).max(180.0);
        ui::text_field(
            ui,
            &mut app.hangar_search,
            &format!("Search {} items", catalog.len()),
            width,
        );
        if !app.hangar_search.is_empty()
            && ui::icon_button(ui, egui_phosphor::regular::X, ui::ButtonKind::Ghost)
                .on_hover_text("Clear the search")
                .clicked()
        {
            app.hangar_search.clear();
        }

        ui.horizontal(|ui| {
            let showing_all = app.hangar_section.is_none();
            if ui::toggle_chip(ui, "All", showing_all, ui::theme::ORANGE)
                .on_hover_text("Show every section at once")
                .clicked()
            {
                // Coming back from All lands on the first section that has
                // something in it, rather than an empty one.
                app.hangar_section = if showing_all {
                    first_populated(catalog, &app.hangar_search)
                } else {
                    None
                };
            }
        });
    });
}

/// One tab per section, with counts that follow the search.
fn section_tabs(app: &mut HangarApp, ui: &mut egui::Ui, catalog: &CatalogSnapshot) {
    let counts = catalog.counts_by_section(&app.hangar_search);
    ui.horizontal_wrapped(|ui| {
        for section in Section::ALL {
            // A section with nothing in it is still shown while searching, so
            // the counts read as a complete answer to "where are the matches"
            // rather than tabs appearing and disappearing under the cursor.
            let n = counts.get(&section).copied().unwrap_or(0);
            if n == 0 && app.hangar_search.trim().is_empty() {
                continue;
            }
            let selected = app.hangar_section == Some(section);
            let label = format!("{}  {n}", section.label());
            if ui::segment(ui, &label, selected).clicked() {
                app.hangar_section = Some(section);
            }
        }
    });
}

fn first_populated(catalog: &CatalogSnapshot, search: &str) -> Option<Section> {
    let counts = catalog.counts_by_section(search);
    Section::ALL
        .into_iter()
        .find(|s| counts.get(s).is_some_and(|n| *n > 0))
}

fn item_card(app: &mut HangarApp, ui: &mut egui::Ui, items: &[&Item]) {
    let running = app.monitor.running();
    ui::group(ui, None, |ui| {
        for (n, item) in items.iter().enumerate() {
            if n > 0 {
                ui.add_space(6.0);
            }
            row(app, ui, item, &running);
        }
    });
}

/// Whether this kind of item can be started and stopped.
///
/// An `EXE.xml` add-on counts: the sim starts it at sim start, but it is an
/// ordinary Windows program the rest of the time (TDS' installer leaves a
/// Start Menu shortcut to it). `runner::already_running` keeps a second copy
/// from starting.
fn is_launchable(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::CompanionApp
            | ItemKind::SimSessionApp
            | ItemKind::NativeTool
            | ItemKind::SimAddonExe
    )
}

/// The prefix an item is started in.
///
/// **Not always `app.proton`**, the companion prefix: an `EXE.xml` add-on lives
/// in the sim's own (TDS reads `C:\ProgramData\TDS\`, which exists only there).
pub(crate) fn launch_env(app: &HangarApp, item: &Item) -> Option<ProtonEnv> {
    if matches!(item.kind, ItemKind::SimAddonExe | ItemKind::SimSessionApp) {
        return ProtonEnv::for_msfs(app.discovery.msfs()?, app.discovery.steam()?);
    }
    app.proton.clone()
}

/// Whether the executable is where the catalog said it was.
///
/// A configured app records this in its state at scan time. An `EXE.xml` add-on
/// does not — its state says whether the *sim* has it enabled — so the file is
/// checked here.
fn exe_missing(item: &Item) -> bool {
    item.state == ItemState::MissingExecutable
        || (item.kind == ItemKind::SimAddonExe && !item.path.is_file())
}

fn row(app: &mut HangarApp, ui: &mut egui::Ui, item: &Item, running: &RunningSet) {
    // What `/proc` says wins over what the catalog recorded at scan time: an app
    // can be started or stopped from anywhere, and the file being present says
    // nothing about whether it is up.
    let live = is_launchable(item)
        .then(|| running.get(&proc::match_key(&item.path)))
        .flatten();
    // Still open after a Stop: it may be asking to confirm.
    let confirming = live.is_some()
        && app
            .actions
            .sessions
            .confirming(&proc::match_key(&item.path))
            .is_some();

    ui.push_id(&item.name, |ui| {
        ui.horizontal(|ui| {
            if live.is_some() {
                ui::mark(ui, ui::Tone::Ok).on_hover_text("Running");
            } else {
                state_mark(ui, &item.state);
            }
            let reserved = if is_launchable(item) { 120.0 } else { 94.0 };
            let room = (ui.available_width() - reserved).max(80.0);
            if ui::controls::item_title(
                ui,
                item.display_title(),
                app.hangar_selected.as_deref() == Some(&item.name),
                room,
            )
            .on_hover_text(path_hint(item))
            .clicked()
            {
                app.hangar_selected = Some(item.name.clone());
            }
            if is_launchable(item) {
                // Frequent launch stays in the list; session management lives in details.
                if live.is_some() {
                    running_state(ui, confirming);
                } else if app.actions.is_updating(&proc::match_key(&item.path)) {
                    // Away restarting after an update: Launch would start a
                    // second copy beside the one about to come back.
                    ui::state(ui, "Updating", ui::Tone::Accent)
                        .on_hover_text("Restarting after an update");
                } else {
                    launch_controls(app, ui, item, false);
                }
            } else if let Some(size) = item.size_bytes {
                ui.label(
                    egui::RichText::new(hangar_msfs::manifest::format_size(size))
                        .small()
                        .color(ui::theme::DIM),
                );
            }
        })
        .response
        .on_hover_text(path_hint(item));
        if confirming {
            closing_note(ui);
        }
        if !session_ready(item, app.sim_process_running()) && live.is_none() {
            ui.label(
                egui::RichText::new("Start the simulator first.")
                    .small()
                    .color(ui::theme::DIM),
            );
        }
    });
}

/// A running app's state word: "Closing" while a Stop waits on its exit.
fn running_state(ui: &mut egui::Ui, confirming: bool) {
    if confirming {
        ui::state(ui, "Closing", ui::Tone::Accent)
            .on_hover_text(hangar_runtime::sim_session::CONFIRM_NOTE);
    } else {
        ui::state(ui, "Running", ui::Tone::Ok);
    }
}

/// The one sentence under a row whose Stop is waiting on a confirmation.
fn closing_note(ui: &mut egui::Ui) {
    ui.label(
        egui::RichText::new(hangar_runtime::sim_session::CONFIRM_NOTE)
            .small()
            .color(ui::theme::DIM),
    );
}

/// The row's one line: what it is, who made it, and which version.
///
/// The package directory name is on the hover, not the row. Parts that would
/// repeat the title are skipped: an app's directory name is its title.
fn row_parts(item: &Item) -> (String, Vec<String>) {
    let title = item.display_title().to_owned();
    let mut rest: Vec<String> = Vec::new();
    if !item.creator.trim().is_empty() {
        rest.push(item.creator.trim().to_owned());
    }
    if !item.version.trim().is_empty() {
        rest.push(format!("v{}", item.version.trim()));
    }
    // With neither, the kind; inside Apps the three kinds have different
    // lifecycles.
    if rest.is_empty() {
        rest.push(item.kind.label().to_owned());
    }
    (title, rest)
}

/// [`row_parts`] as one string — what the row reads as, for a test.
#[cfg(test)]
fn row_line_text(item: &Item) -> String {
    let (title, rest) = row_parts(item);
    format!("{title}  \u{00b7}  {}", rest.join("  \u{00b7}  "))
}

/// A row's tooltip: what it is, and where it lives (for a companion app, the
/// prefix is the useful fact).
fn path_hint(item: &Item) -> String {
    let (_, metadata) = row_parts(item);
    let mut lines = vec![item.kind.label().to_owned(), metadata.join(" · ")];
    // The directory name is the one used on disk and in a profile.
    if item.name != item.display_title() {
        lines.push(item.name.clone());
    }
    lines.push(home_of(item));
    lines.join("\n")
}

/// Where an item lives, shortened to the part that identifies it.
fn home_of(item: &Item) -> String {
    let path = item.path.display().to_string();
    // These paths run to ~140 characters; the tail is the half that says
    // anything.
    match path.char_indices().nth_back(56) {
        Some((at, _)) => format!("…{}", &path[at..]),
        None => path,
    }
}

fn session_ready(item: &Item, sim_running: bool) -> bool {
    item.kind != ItemKind::SimSessionApp || sim_running
}

/// Start / stop, and the reason a button is unavailable.
pub(crate) fn launch_controls(app: &mut HangarApp, ui: &mut egui::Ui, item: &Item, running: bool) {
    let busy = app.actions.is_busy(&item.name);

    if running {
        // Stop works on processes HangarRS never started.
        if ui::tool_button_enabled(
            ui,
            !busy,
            egui_phosphor::regular::STOP_CIRCLE,
            "Stop",
            ui::ButtonKind::Danger,
        )
        .clicked()
        {
            let ctx = ui.ctx().clone();
            apps::spawn_stop(
                &app.actions,
                launch_env(app, item),
                item,
                &app.journal,
                move || ctx.request_repaint(),
            );
        }

        // The only way back to a window closed while the app kept running.
        let env = launch_env(app, item);
        let can_restart = !busy && env.is_some() && session_ready(item, app.sim_process_running());
        let restart = ui::tool_button_enabled(
            ui,
            can_restart,
            egui_phosphor::regular::ARROW_CLOCKWISE,
            "Restart",
            ui::ButtonKind::Normal,
        );
        if can_restart {
            let restart = restart.on_hover_text(
                "Closing a window does not always stop the app. Restarting is the way back.",
            );
            if restart.clicked() {
                if let Some(env) = env {
                    let ctx = ui.ctx().clone();
                    apps::spawn_restart(&app.actions, env, item, &app.journal, move || {
                        ctx.request_repaint()
                    });
                }
            }
        }
        return;
    }

    let missing = exe_missing(item);
    let env = launch_env(app, item);
    let ready = session_ready(item, app.sim_process_running());
    let can_launch = !busy && !missing && env.is_some() && ready;
    let button = ui::tool_button_enabled(
        ui,
        can_launch,
        egui_phosphor::regular::PLAY,
        "Launch",
        ui::ButtonKind::Normal,
    );

    if !ready {
        button.on_hover_text("Start the simulator first.");
    } else if missing {
        button.on_hover_text("The executable is no longer where it was configured.");
    } else if env.is_none() {
        button.on_hover_text(
            "No Proton build is configured for MSFS, so nothing can run in a prefix.",
        );
    } else if button.clicked() {
        if let Some(env) = env {
            let ctx = ui.ctx().clone();
            apps::spawn_launch(&app.actions, env, item, &app.journal, move || {
                ctx.request_repaint()
            });
        }
    }
}

fn action_result(app: &HangarApp, ui: &mut egui::Ui, message: &str, ok: bool) {
    ui::group(ui, None, |ui| {
        ui.horizontal(|ui| {
            ui::state(
                ui,
                if ok { "done" } else { "failed" },
                if ok { ui::Tone::Ok } else { ui::Tone::Err },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::icon_button(ui, egui_phosphor::regular::X, ui::ButtonKind::Ghost)
                    .on_hover_text("Dismiss")
                    .clicked()
                {
                    app.actions.clear_result();
                }
            });
        });
        // Outside the `horizontal`: a label there extends the row instead of
        // wrapping, and the message can be a whole error string.
        ui.label(
            egui::RichText::new(message)
                .size(ui::theme::TEXT_BODY)
                .color(if ok { ui::theme::TEXT } else { ui::theme::ERR }),
        );
    });
}

/// The mark at the head of a row.
///
/// **The word is drawn only when it is news.** Ordinary states get the glyph
/// (a different shape per tone, not colour alone) with the word on the tooltip.
/// `Dangling` and `MissingExecutable` keep theirs because a person has to act
/// on them. `Running` is not an `ItemState`; [`row`] draws it from `/proc`.
fn state_mark(ui: &mut egui::Ui, state: &ItemState) {
    let tone = match state {
        ItemState::Linked { .. } | ItemState::SimEnabled | ItemState::Ready => ui::Tone::Ok,
        // Loaded, but nothing can switch it — deliberately not the same tone.
        ItemState::Resident => ui::Tone::Accent,
        ItemState::Available | ItemState::SimDisabled => ui::Tone::Idle,
        ItemState::Dangling { .. } | ItemState::MissingExecutable => ui::Tone::Err,
    };
    if needs_attention(state) {
        ui::state(ui, state.label(), tone);
    } else {
        ui::mark(ui, tone).on_hover_text(state.label());
    }
}

/// The states whose word is worth the width it costs.
fn needs_attention(state: &ItemState) -> bool {
    matches!(
        state,
        ItemState::Dangling { .. } | ItemState::MissingExecutable
    )
}

fn no_matches(app: &mut HangarApp, ui: &mut egui::Ui, catalog: &CatalogSnapshot) {
    let elsewhere: usize = catalog.counts_by_section(&app.hangar_search).values().sum();

    ui::group(ui, None, |ui| {
        if app.hangar_search.trim().is_empty() {
            let what = app
                .hangar_section
                .map(Section::description)
                .unwrap_or("Nothing here yet.");
            ui.label(
                egui::RichText::new(what)
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            return;
        }
        ui.label(
            egui::RichText::new(format!(
                "Nothing here matches {:?}.",
                app.hangar_search.trim()
            ))
            .size(ui::theme::TEXT_BODY)
            .color(ui::theme::DIM),
        );
        if elsewhere > 0 {
            ui.add_space(6.0);
            if ui::tool_button(
                ui,
                egui_phosphor::regular::MAGNIFYING_GLASS,
                &format!("{elsewhere} elsewhere \u{2014} show all sections"),
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.hangar_section = None;
            }
        }
    });
}

/// Two installed packages covering the same thing.
///
/// **The sim loads both and picks by its own ordering**, and the two rows can
/// carry unrelated-looking titles.
///
/// Reported, never fixed: which to remove is a person's call, and two authors'
/// versions of one airport can both be legitimate.
fn overlap_notice(app: &HangarApp, ui: &mut egui::Ui) {
    ui::group(ui, Some("Two packages, one place"), |ui| {
        for (_, overlap) in &app.discovery.overlaps {
            ui.horizontal(|ui| {
                ui::mark(ui, ui::Tone::Warn);
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(overlap.shared.join(", "))
                            .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                            .color(ui::theme::TEXT_HI),
                    )
                    .truncate(),
                );
            });
            ui.add_space(2.0);
            // Titles and versions, as the list shows them.
            for name in &overlap.packages {
                let item = app
                    .discovery
                    .catalog
                    .items()
                    .iter()
                    .find(|i| &i.name == name);
                let line = match item {
                    Some(i) if !i.version.trim().is_empty() => {
                        format!("{}  \u{00b7}  v{}", i.display_title(), i.version.trim())
                    }
                    Some(i) => i.display_title().to_owned(),
                    None => name.clone(),
                };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(line)
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIM),
                    )
                    .truncate(),
                )
                .on_hover_text(name.as_str());
            }
            ui.add_space(8.0);
        }
        ui.label(
            egui::RichText::new("The sim loads both and picks one by its own ordering.")
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::WARN),
        );
    });
}

fn problems(ui: &mut egui::Ui, catalog: &CatalogSnapshot) {
    ui::group(ui, Some("Problems"), |ui| {
        for p in catalog.problems() {
            ui.horizontal(|ui| {
                ui::state(ui, "unreadable", ui::Tone::Err);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(p.path.display().to_string())
                            .monospace()
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::TEXT),
                    );
                    ui.label(
                        egui::RichText::new(&p.reason)
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIMMER),
                    );
                });
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Text drawn by `draw` in a window of `width`, and the rightmost x any of it reaches.
    fn drawn(width: f32, draw: impl FnMut(&mut egui::Ui)) -> (String, f32) {
        fn walk(shape: &egui::Shape, text: &mut String, right: &mut f32) {
            match shape {
                egui::Shape::Text(t) => {
                    text.push_str(&t.galley.job.text);
                    text.push('\n');
                    *right = right.max(t.pos.x + t.galley.rect.width());
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, text, right)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        let mut draw = draw;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 400.0),
                )),
                ..Default::default()
            },
            |ui| draw(ui),
        );
        let (mut text, mut right) = (String::new(), 0.0);
        for shape in &output.shapes {
            walk(&shape.shape, &mut text, &mut right);
        }
        (text, right)
    }

    /// A running row whose Stop is waiting on the app's own
    /// confirmation says so in one sentence; otherwise it reads as before.
    #[test]
    fn a_row_waiting_on_a_confirmation_says_closing_and_fits() {
        for confirming in [false, true] {
            for width in [320.0, 760.0] {
                let (text, right) = drawn(width, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("BeyondATC");
                        running_state(ui, confirming);
                    });
                    if confirming {
                        closing_note(ui);
                    }
                });
                assert_eq!(text.contains("Closing"), confirming, "{text}");
                assert_eq!(text.contains("Running"), !confirming, "{text}");
                assert_eq!(
                    text.contains(hangar_runtime::sim_session::CONFIRM_NOTE),
                    confirming,
                    "{text}"
                );
                assert!(right <= width, "{right} > {width}");
                assert!(text.lines().all(|l| l.chars().count() <= 160));
            }
        }
    }

    fn item(name: &str, title: &str, path: &str) -> Item {
        Item {
            name: name.into(),
            kind: ItemKind::CompanionApp,
            state: ItemState::Ready,
            title: title.into(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: PathBuf::from(path),
            community: None,
            args: Vec::new(),
        }
    }

    /// TDS' GTNXi integration is an `EXE.xml` entry and its exe is a real
    /// Windows program with a Start Menu shortcut, so the kind must be
    /// launchable or the tab draws no button at all.
    #[test]
    fn an_exe_xml_addon_is_launchable_and_a_package_is_not() {
        let mut addon = item("TDS", "TDS", "/pfx/drive_c/ProgramData/TDS/x.exe");
        addon.kind = ItemKind::SimAddonExe;
        addon.state = ItemState::SimEnabled;
        assert!(is_launchable(&addon));

        let mut pkg = item("koak", "Oakland", "/x/koak");
        pkg.kind = ItemKind::Package;
        pkg.state = ItemState::Resident;
        assert!(!is_launchable(&pkg), "a package is content, not a program");
    }

    #[test]
    fn session_apps_require_the_sim_but_companion_apps_do_not() {
        let mut app = item("BeyondATC", "BeyondATC", "/companion/BeyondATC.exe");
        assert!(session_ready(&app, false));
        app.kind = ItemKind::SimSessionApp;
        assert!(is_launchable(&app));
        assert!(!session_ready(&app, false));
        assert!(session_ready(&app, true));
        assert_eq!(row_line_text(&app), "BeyondATC  ·  Sim session app");
    }

    /// An add-on's state says whether the *sim* has it enabled, which is a
    /// different question from whether the file is there — so offering Launch
    /// on that state alone would offer to run something that is gone.
    #[test]
    fn an_addon_whose_exe_is_gone_is_not_offered() {
        let mut addon = item("TDS", "TDS", "/nonexistent/TDS.exe");
        addon.kind = ItemKind::SimAddonExe;
        addon.state = ItemState::SimEnabled;
        assert!(exe_missing(&addon));

        let here = std::env::current_exe().expect("this test binary exists");
        let mut present = item("TDS", "TDS", &here.display().to_string());
        present.kind = ItemKind::SimAddonExe;
        present.state = ItemState::SimEnabled;
        assert!(!exe_missing(&present));
    }

    /// An app's directory name is its title, so a naive line prints the same
    /// words twice.
    #[test]
    fn a_row_line_never_repeats_the_title() {
        let app = item(
            "BeyondATC",
            "BeyondATC",
            "/prefix/drive_c/BeyondATC/BeyondATC.exe",
        );
        let (title, rest) = row_parts(&app);
        assert_eq!(title, "BeyondATC");
        assert!(
            !rest.iter().any(|p| p.contains("BeyondATC")),
            "nothing of the title survives into the rest: {rest:?}"
        );
    }

    /// What the line says when the creator and version are empty. The kind is
    /// short, it is true of every item, and inside Apps it is the fact that
    /// distinguishes the three lifecycles.
    #[test]
    fn a_row_with_nothing_distinguishing_says_its_kind() {
        let app = item(
            "BeyondATC",
            "BeyondATC",
            "/prefix/drive_c/BeyondATC/BeyondATC.exe",
        );
        assert_eq!(row_line_text(&app), "BeyondATC  \u{00b7}  Companion app");

        let mut tool = item("efbconnect", "efbconnect", "/usr/bin/efbconnect");
        tool.kind = ItemKind::NativeTool;
        assert_eq!(row_line_text(&tool), "efbconnect  \u{00b7}  Native tool");
    }

    /// The shape asked for: title, then maker, then version, on one line.
    #[test]
    fn a_package_line_is_title_then_maker_then_version() {
        let mut pkg = item("koak", "Oakland International Airport", "/x/koak");
        pkg.kind = ItemKind::Package;
        pkg.creator = "FreakyD".into();
        pkg.version = "1.2.0".into();

        assert_eq!(
            row_line_text(&pkg),
            "Oakland International Airport  \u{00b7}  FreakyD  \u{00b7}  v1.2.0"
        );
    }

    /// The directory name is on the hover, not the row.
    #[test]
    fn the_directory_name_moved_to_the_hover() {
        let mut pkg = item("koak", "Oakland International Airport", "/x/koak");
        pkg.kind = ItemKind::Package;
        pkg.creator = "FreakyD".into();

        assert!(!row_line_text(&pkg).contains("koak"));
        assert!(path_hint(&pkg).contains("koak"), "{}", path_hint(&pkg));
    }

    /// No dangling separators when there is nothing to separate.
    #[test]
    fn missing_creator_and_version_are_simply_left_out() {
        let mut pkg = item("koak", "Oakland", "/x/koak");
        pkg.kind = ItemKind::Package;
        let (title, rest) = row_parts(&pkg);
        assert_eq!(title, "Oakland");
        assert_eq!(
            rest,
            vec!["Package".to_string()],
            "the kind is the fallback"
        );
    }

    /// Long paths keep their tail, and elision must not split a character.
    #[test]
    fn a_long_path_is_shortened_from_the_left() {
        let long = item(
            "App",
            "App",
            "/mnt/steam/SteamLibrary/steamapps/compatdata/2537590/pfx/drive_c/users/steamuser/AppData/Local/Programs/thing/App.exe",
        );
        let hint = path_hint(&long);
        assert!(hint.contains('…'));
        assert!(hint.ends_with("App.exe"));

        let unicode = item("A", "A", &"é".repeat(90));
        assert!(
            !path_hint(&unicode).is_empty(),
            "must not panic on a char boundary"
        );
    }

    /// A tooltip that only repeated the sub-line would be worth nothing, so it
    /// says the two things the row itself no longer does.
    #[test]
    fn a_row_hint_names_the_kind_and_where_it_lives() {
        let app = item(
            "BeyondATC",
            "BeyondATC",
            "/prefix/drive_c/BeyondATC/BeyondATC.exe",
        );
        let hint = path_hint(&app);
        assert!(hint.starts_with("Companion app"), "{hint:?}");
        assert!(hint.contains("BeyondATC.exe"), "{hint:?}");
    }

    /// Only the states a person has to act on keep their word.
    #[test]
    fn only_the_states_worth_acting_on_keep_their_word() {
        for quiet in [
            ItemState::Ready,
            ItemState::Resident,
            ItemState::Available,
            ItemState::SimEnabled,
            ItemState::SimDisabled,
        ] {
            assert!(!needs_attention(&quiet), "{quiet:?} should be a bare mark");
        }
        assert!(needs_attention(&ItemState::MissingExecutable));
        assert!(needs_attention(&ItemState::Dangling {
            target: PathBuf::from("/gone"),
        }));
    }
}
