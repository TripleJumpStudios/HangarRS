//! Flight details: where each app is expected to run, and where it is running.
//!
//! Read-only. A check reads the process table
//! once, compares, and records the verdict in Activity; it never launches,
//! adopts or changes a route. Checks run on request, never per frame.

use crate::app::HangarApp;
use egui_phosphor::regular as icons;
use hangar_catalog::ItemKind;
use hangar_runtime::observe::{
    self, Comparison, Continuity, Expected, Identity, Observation, Route, Verdict,
};
use hangar_ui as ui;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

/// The details key for the simulator row; app rows use the app's name.
pub const SIMULATOR: &str = "#simulator";

/// Injection point for reading the process table.
///
/// `Observer::live` reads `/proc`. The inert form reads nothing and reports
/// what it was given — nothing running, unless a test supplies an observation.
#[derive(Clone, Default)]
pub struct Observer {
    live: bool,
    /// Shared so a test can change what a running Flight sees between ticks.
    given: Option<Arc<std::sync::Mutex<Observation>>>,
}

impl Observer {
    pub fn live() -> Self {
        Self {
            live: true,
            given: None,
        }
    }

    /// Whether this reads the real machine. Other read-only machine checks
    /// on the Flight page (the version-drift warning) follow the same switch.
    pub fn is_live(&self) -> bool {
        self.live
    }

    #[cfg(test)]
    pub fn given(observation: Observation) -> Self {
        Self {
            live: false,
            given: Some(Arc::new(std::sync::Mutex::new(observation))),
        }
    }

    /// Replace what a `given` observer reports.
    #[cfg(test)]
    pub fn set(&self, observation: Observation) {
        if let Some(given) = &self.given {
            *given.lock().unwrap_or_else(|e| e.into_inner()) = observation;
        }
    }

    pub(crate) fn observe(&self, keys: &[String]) -> Observation {
        if self.live {
            observe::observe(std::path::Path::new(observe::PROC), keys)
        } else {
            self.given
                .as_ref()
                .map(|g| g.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_else(|| Observation::empty(keys))
        }
    }
}

struct Open {
    name: String,
    expected: Expected,
    comparison: Comparison,
}

#[derive(Default)]
pub struct Details {
    open: Option<Open>,
    /// The instance seen at each app's last check, to spot a recycled PID.
    previous: BTreeMap<String, Identity>,
}

impl Details {
    pub fn is_open(&self, name: &str) -> bool {
        self.open.as_ref().is_some_and(|o| o.name == name)
    }

    /// The verdict of the open check for `name`, and when it was taken.
    pub fn last_check(&self, name: &str) -> Option<(Verdict, SystemTime)> {
        self.open
            .as_ref()
            .filter(|o| o.name == name)
            .map(|o| (o.comparison.verdict, o.comparison.taken))
    }
}

/// What the configured route requires; `None` for an app no longer listed.
fn expected(app: &HangarApp, name: &str) -> Option<Expected> {
    let sim_prefix = app.discovery.msfs().map(|m| m.install.prefix.as_path());
    if name == SIMULATOR {
        return Some(Expected::new(
            Route::Simulator,
            hangar_runtime::sim::MSFS_2024_EXE,
            sim_prefix,
        ));
    }
    let item = app
        .discovery
        .catalog
        .items()
        .iter()
        .find(|i| i.name == name)?;
    let (route, prefix) = match item.kind {
        ItemKind::SimSessionApp => (Route::SimSession, sim_prefix),
        ItemKind::NativeTool => (Route::Native, None),
        _ => (
            Route::Companion,
            app.proton.as_ref().map(|p| p.wineprefix.as_path()),
        ),
    };
    Some(Expected::new(
        route,
        hangar_runtime::proc::match_key(&item.path),
        prefix,
    ))
}

pub fn toggle(app: &mut HangarApp, name: &str) {
    if app.flight_details.is_open(name) {
        app.flight_details.open = None;
    } else {
        check(app, name);
    }
}

/// Observe once, compare, remember the instance and record the result.
pub fn check(app: &mut HangarApp, name: &str) {
    let Some(expected) = expected(app, name) else {
        app.flight_details.open = None;
        return;
    };
    let observation = app.observer.observe(std::slice::from_ref(&expected.key));
    let previous = app.flight_details.previous.get(name).copied();
    let comparison = observe::compare(&expected, &observation, previous);
    match comparison.instance {
        Some(identity) => app
            .flight_details
            .previous
            .insert(name.to_owned(), identity),
        None => app.flight_details.previous.remove(name),
    };
    let source = if name == SIMULATOR { "Preflight" } else { name };
    app.journal.note(
        source,
        format!(
            "Environment check: {}{}",
            comparison.verdict.label(),
            comparison
                .instance
                .map_or_else(String::new, |i| format!(" · PID {}", i.pid))
        ),
    );
    app.flight_details.open = Some(Open {
        name: name.to_owned(),
        expected,
        comparison,
    });
}

fn tone(verdict: Verdict) -> egui::Color32 {
    match verdict {
        Verdict::Matches => ui::theme::OK,
        Verdict::NotRunning => ui::theme::DIM,
        Verdict::WrongPrefix | Verdict::OtherSession => ui::theme::ERR,
        Verdict::Ambiguous(_) | Verdict::Unverified(_) => ui::theme::WARN,
    }
}

fn notes(c: &Comparison) -> Vec<String> {
    let mut out = Vec::new();
    match c.continuity {
        Continuity::First | Continuity::Same => {}
        Continuity::Replaced { was } => out.push(format!(
            "A different process since the last check (was PID {}).",
            was.pid
        )),
        Continuity::PidReused { was } => out.push(format!(
            "PID {} now belongs to a newer process; the earlier one exited.",
            was.pid
        )),
        Continuity::Gone { was } => {
            out.push(format!("PID {} from the last check has exited.", was.pid))
        }
    }
    if c.helpers > 0 {
        out.push(format!("{} helper processes not counted.", c.helpers));
    }
    if !c.similar.is_empty() {
        out.push(format!(
            "Similar names not counted: {}",
            c.similar.join(", ")
        ));
    }
    out
}

fn age(taken: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(taken)
        .map_or(0, |d| d.as_secs());
    if secs < 60 {
        format!("Checked {secs}s ago")
    } else {
        format!("Checked {} min ago", secs / 60)
    }
}

fn cell(ui: &mut egui::Ui, width: f32, text: &str, color: egui::Color32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_max_width(width);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(ui::theme::TEXT_SM)
                        .color(color),
                )
                .truncate(),
            )
            .on_hover_text(text);
        },
    );
}

/// The expected/observed block under a row, when that row's details are open.
pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui, name: &str) {
    let Some(open) = app.flight_details.open.as_ref().filter(|o| o.name == name) else {
        return;
    };
    let c = &open.comparison;
    let verdict = c.verdict;
    let route = match open.expected.route {
        Route::Companion => "Companion prefix",
        Route::SimSession => "Simulator session",
        Route::Native => "Linux app",
        Route::Simulator => "Steam",
    };
    // The stable ID its saved Flight choices are kept under.
    let route = match app.preflight.id_of(name) {
        Some(id) => format!("{route} · {id}"),
        None => route.to_owned(),
    };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(verdict.label()).color(tone(verdict)));
        ui.label(
            egui::RichText::new(format!("· {route}"))
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );
    });
    ui.label(
        egui::RichText::new(verdict.explain())
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::DIM),
    );
    let width = ui.available_width().min(ui::theme::ROW_MAX_WIDTH);
    let fact = 72.0;
    let value = ((width - fact - 16.0) / 2.0).max(40.0);
    ui.horizontal(|ui| {
        cell(ui, fact, "", ui::theme::DIM);
        cell(ui, value, "Expected", ui::theme::DIM);
        cell(ui, value, "Observed", ui::theme::DIM);
    });
    for row in &c.rows {
        let color = match row.agrees {
            Some(true) => ui::theme::OK,
            Some(false) => ui::theme::ERR,
            None => ui::theme::TEXT_HI,
        };
        ui.horizontal(|ui| {
            cell(ui, fact, row.fact, ui::theme::DIM);
            cell(ui, value, &row.expected, ui::theme::TEXT_HI);
            cell(ui, value, &row.observed, color);
        });
    }
    for note in notes(c) {
        ui.add(
            egui::Label::new(
                egui::RichText::new(&note)
                    .size(ui::theme::TEXT_SM)
                    .color(ui::theme::DIM),
            )
            .truncate(),
        )
        .on_hover_text(note);
    }
    let taken = c.taken;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(age(taken))
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );
        if ui::tool_button(
            ui,
            icons::ARROWS_CLOCKWISE,
            "Check again",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            check(app, name);
        }
        let source = (name != SIMULATOR).then_some(name);
        crate::preflight::activity(app, ui, source);
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inert_observer_reports_nothing_running() {
        let obs = Observer::default().observe(&["BATC.exe".into()]);
        assert!(obs.apps.values().all(|f| f.exact.is_empty()));
    }

    #[test]
    fn notes_name_replacement_reuse_helpers_and_similar_names() {
        let was = Identity { pid: 7, start: 1 };
        let mut c = observe::compare(
            &Expected::new(Route::Native, "x", None),
            &Observation::empty(&["x".into()]),
            Some(was),
        );
        assert_eq!(c.continuity, Continuity::Gone { was });
        c.helpers = 2;
        c.similar = vec!["MyX".into()];
        let text = notes(&c).join(" ");
        assert!(
            text.contains("PID 7") && text.contains("2 helper") && text.contains("MyX"),
            "{text}"
        );
        c.continuity = Continuity::PidReused { was };
        assert!(notes(&c)[0].contains("newer process"));
    }
}
