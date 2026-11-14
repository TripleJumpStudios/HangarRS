//! Flight: the saved launch list and the current session share one row layout.

use crate::app::HangarApp;
use crate::flight::{build_ordered, Flight, Placement, PlanEntry};
use crate::tab::Tab;
use egui_phosphor::regular as icons;
use hangar_catalog::{Item, ItemKind};
use hangar_runtime::preflight::{Action, Condition, State};
use hangar_ui as ui;
use ui::reorder::{SequenceEdit, SequenceRow};

/// The sim-session apps (name, configured exe) a drift reading was made for.
pub type DriftKey = Vec<(String, std::path::PathBuf)>;

/// One app whose configured program is not the copy the prefix installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drifted {
    pub app: String,
    pub warning: String,
    /// The installed copy's executable, when it could be found.
    pub installed: Option<std::path::PathBuf>,
}

#[derive(Default)]
pub struct ViewState {
    /// Version-drift findings, and the (app, exe) list they were read for, so
    /// the registry is read once per library change rather than per frame.
    pub drift: Option<(DriftKey, Vec<Drifted>)>,
    pub add_apps: bool,
    pub edit_order: bool,
    pub editing_next: bool,
    pub confirm_cancel: bool,
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    if app.flight.is_some() && !app.flight_view.editing_next {
        running(app, ui);
    } else {
        plan(app, ui, &launchable(app));
    }
}

fn launchable(app: &HangarApp) -> Vec<Item> {
    app.discovery
        .catalog
        .items()
        .iter()
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::CompanionApp | ItemKind::SimSessionApp | ItemKind::NativeTool
            )
        })
        .cloned()
        .collect()
}

/// Do not discard a session that still supervises apps, even after launch ends.
fn can_start(app: &HangarApp) -> bool {
    app.flight
        .as_ref()
        .is_none_or(|f| f.is_finished() && f.started_apps().is_empty())
}

fn observed(app: &HangarApp, item: &Item) -> bool {
    app.monitor
        .running()
        .get(&hangar_runtime::proc::match_key(&item.path))
        .is_some()
}

fn plan(app: &mut HangarApp, ui: &mut egui::Ui, apps: &[Item]) {
    let placements = placements(app, apps);
    let entries = app.preflight.plan(&placements);
    let problem = plan_problem(app, apps);
    let can_start = can_start(app);
    ui::page_header(ui, "Flight", |ui| {
        if ui::tool_button_enabled(
            ui,
            problem.is_none() && can_start,
            icons::PLAY,
            "Start flight",
            ui::ButtonKind::Primary,
        )
        .clicked()
        {
            begin(app, apps);
        }
    });
    if app.flight_view.editing_next {
        ui.horizontal(|ui| {
            ui::heading(ui, "Next flight");
            if ui::tool_button(
                ui,
                icons::ARROW_LEFT,
                "Current session",
                ui::ButtonKind::Link,
            )
            .clicked()
            {
                app.flight_view.editing_next = false;
            }
        });
        if !can_start {
            ui.label("The current app session is still active.");
        }
        ui.add_space(12.0);
    }
    for drifted in drift_warnings(app, apps) {
        ui.label(egui::RichText::new(&drifted.warning).color(ui::theme::WARN));
        // The route is only switched by a person pressing this, never on its own.
        if let Some(target) = &drifted.installed {
            if ui::tool_button(
                ui,
                icons::ARROW_SQUARE_OUT,
                "Use the installed copy",
                ui::ButtonKind::Normal,
            )
            .on_hover_text(target.display().to_string())
            .clicked()
            {
                use_installed_copy(app, &drifted.app, target);
            }
        }
        ui.add_space(8.0);
    }
    if let Some(reason) = problem {
        ui.label(egui::RichText::new(reason).color(ui::theme::WARN));
        if app.discovery.msfs().is_none()
            && ui::tool_button(
                ui,
                icons::GEAR_SIX,
                "Simulator setup",
                ui::ButtonKind::Ghost,
            )
            .clicked()
        {
            app.settings_view = crate::settings::View::Simulator;
            app.active_tab = Tab::Settings;
        }
        ui.add_space(12.0);
    }
    ui.horizontal(|ui| {
        ui::heading(ui, "Launch order");
        ui.label(
            egui::RichText::new(format!("{} apps", entries.len().saturating_sub(1)))
                .small()
                .color(ui::theme::DIM),
        );
        if ui::tool_button(
            ui,
            icons::PENCIL_SIMPLE,
            if app.flight_view.edit_order {
                "Done"
            } else {
                "Edit order"
            },
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            app.flight_view.edit_order = !app.flight_view.edit_order;
        }
    });
    ui.add_space(8.0);
    let mut edit = None;
    for (index, entry) in entries.iter().enumerate() {
        let (title, detail, running) = match entry {
            PlanEntry::Simulator => ("MSFS 2024", "Simulator", app.sim_process_running()),
            PlanEntry::App(name) => {
                let item = apps
                    .iter()
                    .find(|i| &i.name == name)
                    .expect("plan uses current apps");
                (
                    item.display_title(),
                    if placements[name] == Placement::Before {
                        "Before MSFS"
                    } else {
                        "When MSFS is ready"
                    },
                    observed(app, item),
                )
            }
        };
        if let Some(change) = (SequenceRow {
            list: egui::Id::new("flight-order"),
            item: entry.clone(),
            index,
            total: entries.len(),
            title,
            detail,
            status: if running { "Running" } else { "Included" },
            tone: if running {
                ui::Tone::Ok
            } else {
                ui::Tone::Idle
            },
            editable: app.flight_view.edit_order,
            removable: matches!(entry, PlanEntry::App(_)),
            details: Some(app.flight_details.is_open(details_key(entry))),
        })
        .show_with(ui, |ui| {
            crate::flight_details::draw(app, ui, details_key(entry))
        }) {
            edit = Some((entry.clone(), change));
        }
    }
    if let Some((entry, change)) = edit {
        match change {
            SequenceEdit::Move { item, gap } => {
                if app.preflight.move_entry(&placements, &item, gap) {
                    app.save_preflight();
                }
            }
            SequenceEdit::Remove => {
                if let PlanEntry::App(name) = entry {
                    app.set_placement(&name, Placement::Off);
                }
            }
            SequenceEdit::Details => crate::flight_details::toggle(app, details_key(&entry)),
        }
    }
    if ui::tool_button(ui, icons::PLUS, "Add apps", ui::ButtonKind::Link).clicked() {
        app.flight_view.add_apps = !app.flight_view.add_apps;
    }
    if app.flight_view.add_apps {
        let excluded: Vec<_> = apps
            .iter()
            .filter(|i| app.placement_of(&i.name) == Placement::Off)
            .collect();
        if excluded.is_empty() {
            ui.label(egui::RichText::new("All available apps are included.").color(ui::theme::DIM));
        }
        for item in excluded {
            ui.horizontal(|ui| {
                if ui::tool_button(ui, icons::PLUS, "Add", ui::ButtonKind::Ghost).clicked() {
                    let placement = if hangar_runtime::readiness::probe_for(&item.name).is_some() {
                        Placement::Before
                    } else {
                        Placement::After
                    };
                    app.set_placement(&item.name, placement);
                }
                ui.add(egui::Label::new(item.display_title()).truncate())
                    .on_hover_text(item.display_title());
            });
        }
        if ui::tool_button(
            ui,
            icons::PACKAGE,
            "Manage apps in Hangar",
            ui::ButtonKind::Link,
        )
        .clicked()
        {
            app.active_tab = Tab::Hangar;
            app.hangar_section = Some(hangar_catalog::Section::Apps);
        }
    }
    ui.add_space(18.0);
    ui::divider(ui);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Loadout").color(ui::theme::DIM));
        let label = app.active_profile_label();
        if ui::tool_button(ui, icons::LIST_CHECKS, &label, ui::ButtonKind::Link).clicked() {
            app.active_tab = Tab::Profiles;
        }
    });
}

fn details_key(entry: &PlanEntry) -> &str {
    match entry {
        PlanEntry::Simulator => crate::flight_details::SIMULATOR,
        PlanEntry::App(name) => name,
    }
}

fn placements(app: &HangarApp, apps: &[Item]) -> std::collections::BTreeMap<String, Placement> {
    apps.iter()
        .map(|item| (item.name.clone(), app.placement_of(&item.name)))
        .collect()
}

/// Sim-session apps whose configured program is not the copy the simulator
/// prefix's installer recorded — the two-copy update loop.
/// A warning, never a refusal: the launch route is unchanged.
fn drift_warnings(app: &mut HangarApp, apps: &[Item]) -> Vec<Drifted> {
    let wanted: Vec<(String, std::path::PathBuf)> = apps
        .iter()
        .filter(|i| i.kind == ItemKind::SimSessionApp)
        .map(|i| (i.name.clone(), i.path.clone()))
        .collect();
    if let Some((read_for, warnings)) = &app.flight_view.drift {
        if *read_for == wanted {
            return warnings.clone();
        }
    }
    // Renders and tests never read a real prefix's registry.
    if !app.observer.is_live() {
        return Vec::new();
    }
    let prefix = app.discovery.msfs().map(|m| m.install.prefix.clone());
    let warnings: Vec<Drifted> = prefix
        .map(|prefix| {
            wanted
                .iter()
                .filter_map(|(name, exe)| {
                    hangar_runtime::drift::check(exe, &prefix).map(|d| Drifted {
                        app: name.clone(),
                        warning: d.describe(name),
                        installed: d.installed_exe(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    app.flight_view.drift = Some((wanted, warnings.clone()));
    warnings
}

/// Point an app's config entry at the installed copy and save. The old path
/// goes to Activity, so the change is one edit from undone. Its saved Flight
/// choices follow it: the stable ID matches by name when the program moves.
pub fn use_installed_copy(app: &mut HangarApp, name: &str, target: &std::path::Path) {
    let Some(entry) = app.config.apps.iter_mut().find(|e| e.name == name) else {
        app.journal
            .fail(name, "no longer in the config; nothing changed".to_string());
        return;
    };
    let was = std::mem::replace(&mut entry.exe, target.to_owned());
    app.save_config();
    app.journal.done(
        name,
        format!("now starts {} (was {})", target.display(), was.display()),
    );
    tracing::info!(target: "hangar_launch", app = %name, exe = %target.display(), was = %was.display(), "route changed to the installed copy");
    app.flight_view.drift = None;
    app.rescan_requested = true;
}

/// Once MSFS and every app of the flight are down, go back to the
/// launch list. Activity keeps the record of the flight.
pub(crate) fn end_if_over(app: &mut HangarApp) {
    let Some(flight) = &app.flight else {
        return;
    };
    let running = app.monitor.running();
    let apps_running = flight.run().preflight().steps.iter().any(|step| {
        let Action::LaunchApp { label } = &step.action else {
            return false;
        };
        app.discovery
            .catalog
            .items()
            .iter()
            .find(|i| &i.name == label)
            .is_some_and(|i| {
                let key = hangar_runtime::proc::match_key(&i.path);
                running.is_running(&key) || app.actions.is_updating(&key)
            })
    });
    if flight.is_over(app.sim_process_running(), apps_running) {
        app.flight = None;
        app.flight_view.editing_next = false;
        app.flight_view.confirm_cancel = false;
        app.journal.note(
            "Preflight",
            "MSFS and the flight's apps have closed; ready for the next launch",
        );
    }
}

fn plan_problem(app: &HangarApp, apps: &[Item]) -> Option<String> {
    if let Some(item) = apps.iter().find(|item| {
        item.kind == ItemKind::SimSessionApp && app.placement_of(&item.name) == Placement::Before
    }) {
        return Some(format!(
            "Move {} below MSFS; it uses the running simulator.",
            item.display_title()
        ));
    }
    app.discovery
        .msfs()
        .is_none()
        .then(|| "MSFS 2024 was not found.".into())
}

pub(crate) fn begin(app: &mut HangarApp, apps: &[Item]) {
    if !can_start(app) {
        app.journal
            .note("Preflight", "The current app session is still active.");
        return;
    }
    // A dependency is a launch guard, never an invisible reorder of the plan.
    if let Some(reason) = plan_problem(app, apps) {
        app.journal.note("Preflight", reason);
        return;
    }
    if !app.actions.sessions.is_live() {
        app.journal
            .note("Preflight", "Launching is unavailable in this session.");
        return;
    }
    let preflight = build_ordered(&placements(app, apps), &app.preflight.order);
    let requests = apps
        .iter()
        .filter(|i| app.placement_of(&i.name) != Placement::Off)
        .map(|i| (i.name.clone(), crate::apps::request_for(i)))
        .collect();
    app.flight = Some(
        Flight::start(preflight, requests, app.proton.clone())
            .with_sessions(
                app.discovery
                    .msfs()
                    .zip(app.discovery.steam())
                    .and_then(|(msfs, steam)| hangar_runtime::ProtonEnv::for_msfs(msfs, steam)),
                app.actions.sessions.clone(),
            )
            .with_journal(&app.journal)
            .with_observer(app.observer.clone())
            .with_rows(&app.actions),
    );
    let drift = drift_warnings(app, apps);
    app.flight_view = ViewState::default();
    app.journal.note("Preflight", "started");
    for drifted in drift {
        app.journal.note("Preflight", drifted.warning);
    }
}

fn running(app: &mut HangarApp, ui: &mut egui::Ui) {
    let Some(flight) = &app.flight else {
        return;
    };
    let finished = flight.is_finished();
    let waiting = flight.waiting_on();
    let started = flight.started_apps();
    let torn_down = flight.torn_down().map(|t| t.to_vec());
    let watching = flight.is_watching_for_shutdown();
    let sim_gone_for = flight.sim_gone_for();
    let sim_replaced = flight.sim_replaced();
    let steps = flight.run().preflight().steps.clone();
    let states = flight.run().states().to_vec();
    let readiness = flight.app_readiness();
    let connections: Vec<String> = steps
        .iter()
        .map(|step| connection(flight, step.action.label()))
        .collect();
    let apps = launchable(app);
    // Away updating: followed by this flight, by a row that started it, or by
    // the sim-session runtime.
    let updating: Vec<Option<std::time::Duration>> = steps
        .iter()
        .map(|step| {
            let Action::LaunchApp { label } = &step.action else {
                return None;
            };
            flight.updating_for(label).or_else(|| {
                let item = apps.iter().find(|i| &i.name == label)?;
                let key = hangar_runtime::proc::match_key(&item.path);
                app.actions
                    .away(&key, std::time::Instant::now())
                    .map(|(_, away)| away)
                    .or_else(|| {
                        app.actions
                            .sessions
                            .updating(&key)
                            .map(|since| since.elapsed())
                    })
            })
        })
        .collect();
    // Still open after a Stop or the sim-exit close: it may be asking to confirm.
    let confirming: Vec<bool> = steps
        .iter()
        .map(|step| {
            let Action::LaunchApp { label } = &step.action else {
                return false;
            };
            apps.iter().find(|i| &i.name == label).is_some_and(|item| {
                app.actions
                    .sessions
                    .confirming(&hangar_runtime::proc::match_key(&item.path))
                    .is_some()
            })
        })
        .collect();
    ui::page_header(ui, "Flight", |ui| {
        if ui::tool_button(
            ui,
            icons::PENCIL_SIMPLE,
            "Edit next flight",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            app.flight_view.editing_next = true;
        }
    });
    let failed = states.iter().any(|s| matches!(s, State::Failed(_)));
    let skipped = states.iter().any(|s| matches!(s, State::Skipped));
    ui::heading(
        ui,
        if !finished {
            "Launch in progress"
        } else if failed {
            "Launch needs attention"
        } else if skipped {
            "Launch stopped"
        } else {
            "Launch complete"
        },
    );
    ui.add_space(8.0);
    for (index, step) in steps.iter().enumerate() {
        let item = if let Action::LaunchApp { label } = &step.action {
            apps.iter().find(|item| &item.name == label)
        } else {
            None
        };
        let title = match &step.action {
            Action::LaunchSim { .. } => "MSFS 2024",
            Action::LaunchApp { label } => item.map_or(label.as_str(), |i| i.display_title()),
            Action::ApplyProfile { profile } => profile,
        };
        let live = item.map_or_else(
            || matches!(step.action, Action::LaunchSim { .. }) && app.sim_process_running(),
            |i| observed(app, i),
        );
        let (status, tone) = match &states[index] {
            State::Pending => ("Included", ui::Tone::Idle),
            State::Waiting => ("Waiting", ui::Tone::Accent),
            State::Acting => ("Starting", ui::Tone::Accent),
            State::Failed(_) => ("Failed", ui::Tone::Err),
            State::Skipped => ("Skipped", ui::Tone::Idle),
            State::Done(_) if live => ("Running", ui::Tone::Ok),
            State::Done(_) => ("Not running", ui::Tone::Idle),
        };
        let detail = match &states[index] {
            State::Failed(_) => "Could not complete this step".to_owned(),
            State::Done(_) if matches!(step.action, Action::LaunchSim { .. }) => {
                "Launch requested through Steam".into()
            }
            // Running is process evidence only; without an adapter's fresh
            // evidence the connection is "unverified".
            State::Done(_) if live && !started.iter().any(|s| s == step.action.label()) => {
                format!("Started elsewhere · {}", connections[index])
            }
            State::Done(_) if live => {
                let mut c = connections[index].chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            }
            State::Done(_) => "Launch step completed".into(),
            State::Acting => "Starting now".into(),
            State::Skipped => "Not launched".into(),
            _ => waiting.filter(|(i, _, _)| *i == index).map_or_else(
                || condition_label(step.wait_for).to_owned(),
                |(_, condition, elapsed)| {
                    format!("{} · {}s", condition_label(condition), elapsed.as_secs())
                },
            ),
        };
        let syncing = readiness
            .iter()
            .find(|(name, _)| name == step.action.label())
            .filter(|(_, r)| {
                !r.is_ready() && waiting.is_some_and(|(_, c, _)| c == Condition::AppsReady)
            });
        let sync_detail = match updating[index] {
            Some(away) => Some(format!("Restarting after an update · {}s", away.as_secs())),
            None if confirming[index] => Some(hangar_runtime::sim_session::CONFIRM_NOTE.to_owned()),
            None => syncing.map(|(_, r)| r.describe()),
        };
        let details = match &step.action {
            Action::LaunchSim { .. } => Some(crate::flight_details::SIMULATOR),
            Action::LaunchApp { label } => Some(label.as_str()),
            Action::ApplyProfile { .. } => None,
        };
        let edit = SequenceRow {
            list: egui::Id::new("flight-session"),
            item: index,
            index,
            total: steps.len(),
            title,
            detail: sync_detail.as_deref().unwrap_or(&detail),
            status: if updating[index].is_some() {
                "Updating"
            } else if confirming[index] {
                "Closing"
            } else if syncing.is_some() {
                "Preparing scenery"
            } else {
                status
            },
            tone: if updating[index].is_some() || confirming[index] || syncing.is_some() {
                ui::Tone::Accent
            } else {
                tone
            },
            editable: false,
            removable: false,
            details: details.map(|key| app.flight_details.is_open(key)),
        }
        .show_with(ui, |ui| {
            if let State::Failed(reason) = &states[index] {
                ui.label(egui::RichText::new(reason).color(ui::theme::ERR));
                if let Some(item) = item.filter(|_| !finished) {
                    recovery(app, ui, item);
                }
            }
            if let Some(item) = item.filter(|_| finished) {
                ui.horizontal(|ui| {
                    // These shared controls log where they were pressed.
                    let source = std::mem::replace(&mut app.actions.source, "Flight");
                    crate::hangar::launch_controls(app, ui, item, live);
                    app.actions.source = source;
                    activity(app, ui, Some(&item.name));
                });
                ui.add_space(8.0);
            } else if matches!(states[index], State::Failed(_)) {
                let source = match &step.action {
                    Action::LaunchApp { label } => label.as_str(),
                    _ => "Preflight",
                };
                activity(app, ui, Some(source));
            }
            if let Some(key) = details {
                crate::flight_details::draw(app, ui, key);
            }
        });
        if let (Some(SequenceEdit::Details), Some(key)) = (edit, details) {
            crate::flight_details::toggle(app, key);
        }
    }
    if let Some((_, condition, _)) = waiting.filter(|(_, c, _)| c.is_overridable() && !finished) {
        ui.add_space(12.0);
        ui.label(if condition.is_manual() {
            "Continue when the apps above are ready."
        } else {
            "Continue skips the remaining scenery preparation wait."
        });
        if ui::tool_button(ui, icons::PLAY, "Continue", ui::ButtonKind::Primary).clicked() {
            if let Some(flight) = &mut app.flight {
                flight.proceed();
            }
        }
    }
    ui.add_space(18.0);
    if !finished || !started.is_empty() {
        let label = if finished {
            "End app session"
        } else {
            "Cancel launch"
        };
        if ui::tool_button(ui, icons::X, label, ui::ButtonKind::Ghost).clicked() {
            app.flight_view.confirm_cancel = true;
        }
    }
    if app.flight_view.confirm_cancel {
        ui::heading(ui, "Cancel this app session?");
        ui.label("MSFS and apps started elsewhere will stay open.");
        if !started.is_empty() {
            ui.label("Apps to close:");
            for name in &started {
                ui.add(egui::Label::new(name).truncate())
                    .on_hover_text(name);
            }
        }
        let pending_app =
            steps
                .iter()
                .zip(&states)
                .find_map(|(step, state)| match (&step.action, state) {
                    (Action::LaunchApp { label }, State::Acting) => Some(label),
                    _ => None,
                });
        if let Some(name) = pending_app {
            ui.label("Also closes this app if its pending launch creates a new instance:");
            ui.add(egui::Label::new(name).truncate())
                .on_hover_text(name);
        }
        ui.horizontal(|ui| {
            if ui::tool_button(ui, icons::X, "Confirm cancel", ui::ButtonKind::Danger).clicked() {
                if let Some(flight) = &mut app.flight {
                    flight.cancel();
                }
                app.flight_view.confirm_cancel = false;
            }
            if ui::tool_button(ui, "", "Keep session", ui::ButtonKind::Ghost).clicked() {
                app.flight_view.confirm_cancel = false;
            }
        });
    }
    if let Some((was, now)) = sim_replaced {
        ui.label(
            egui::RichText::new(format!(
                "MSFS restarted (pid {} → {}). Apps from the first start were not moved to it.",
                was.pid, now.pid
            ))
            .color(ui::theme::WARN),
        );
    }
    if let Some(closing) = torn_down {
        ui.label("Close requested for session apps; see Activity for results.");
        for name in closing {
            ui.add(egui::Label::new(&name).truncate())
                .on_hover_text(name);
        }
        activity(app, ui, None);
    } else if watching {
        ui.label(if let Some(gone) = sim_gone_for {
            format!(
                "MSFS closed · Closing session apps in {}s",
                crate::flight::SHUTDOWN_GRACE.saturating_sub(gone).as_secs()
            )
        } else {
            "Session apps close after MSFS exits.".into()
        });
    }
}

/// What is known about a running app's own connection, in a few words.
fn connection(flight: &crate::flight::Flight, label: &str) -> String {
    flight
        .connection_of(label)
        .unwrap_or_else(|| "connection unverified".to_owned())
}

/// How recent a check must be for Retry launch to trust it.
pub const RETRY_FRESH: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether a retry may go ahead: a check taken in the last [`RETRY_FRESH`]
/// found no instance. Anything else, including no check at all,
/// asks for Check again first.
pub fn retry_allowed(app: &HangarApp, name: &str) -> bool {
    app.flight_details
        .last_check(name)
        .is_some_and(|(verdict, at)| {
            verdict == hangar_runtime::observe::Verdict::NotRunning
                && std::time::SystemTime::now()
                    .duration_since(at)
                    .is_ok_and(|age| age < RETRY_FRESH)
        })
}

/// A failed app step mid-flight: look first, read-only; retry only once
/// nothing is running. Retrying goes through the same resolver as a launch.
fn recovery(app: &mut HangarApp, ui: &mut egui::Ui, item: &hangar_catalog::Item) {
    ui.horizontal(|ui| {
        if ui::tool_button(
            ui,
            icons::ARROWS_CLOCKWISE,
            "Check again",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            crate::flight_details::check(app, &item.name);
        }
        let allowed = retry_allowed(app, &item.name) && !app.actions.is_busy(&item.name);
        let retry = ui::tool_button_enabled(
            ui,
            allowed,
            icons::PLAY,
            "Retry launch",
            ui::ButtonKind::Normal,
        );
        let retry = if allowed {
            retry
        } else {
            retry.on_disabled_hover_text(
                "Check again first; retry is offered once nothing is running.",
            )
        };
        if retry.clicked() {
            if let Some(env) = crate::hangar::launch_env(app, item) {
                let ctx = ui.ctx().clone();
                let source = std::mem::replace(&mut app.actions.source, "Flight");
                crate::apps::spawn_launch(&app.actions, env, item, &app.journal, move || {
                    ctx.request_repaint()
                });
                app.actions.source = source;
            }
        }
    });
}

fn condition_label(condition: Condition) -> &'static str {
    match condition {
        Condition::Nothing => "Ready to launch",
        Condition::SimStarted => "Waiting for MSFS to start",
        Condition::SimConnectReady => "Waiting for MSFS connection",
        Condition::TelemetryFlowing => "Waiting for flight data",
        Condition::AppsReady => "Waiting for scenery preparation",
        Condition::Manual => "Waiting for you to continue",
    }
}

pub(crate) fn activity(app: &mut HangarApp, ui: &mut egui::Ui, source: Option<&str>) {
    if ui::tool_button(ui, icons::LIST_BULLETS, "Activity", ui::ButtonKind::Link).clicked() {
        app.console_source = source.map(str::to_owned);
        app.console_search.clear();
        app.active_tab = Tab::Console;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The Flight row of an app still open after a Stop
    /// reads "Closing" with the one-sentence reason, and fits a narrow window.
    #[test]
    fn a_flight_row_waiting_on_a_confirmation_says_closing_and_fits() {
        for confirming in [false, true] {
            for width in [640.0, 1400.0] {
                let (status, detail, tone) = if confirming {
                    (
                        "Closing",
                        hangar_runtime::sim_session::CONFIRM_NOTE,
                        ui::Tone::Accent,
                    )
                } else {
                    ("Running", "Launch step completed", ui::Tone::Ok)
                };
                let (text, right) = drawn(width, |ui| {
                    SequenceRow {
                        list: egui::Id::new("flight-session"),
                        item: 0usize,
                        index: 0,
                        total: 3,
                        title: "BeyondATC",
                        detail,
                        status,
                        tone,
                        editable: false,
                        removable: false,
                        details: Some(false),
                    }
                    .show(ui);
                });
                assert!(text.contains(status), "{text}");
                assert_eq!(text.contains("own window"), confirming, "{text}");
                assert!(right <= width, "{right} > {width}");
                assert!(text.lines().all(|l| l.chars().count() <= 160));
            }
        }
    }
}
