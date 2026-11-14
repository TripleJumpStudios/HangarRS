//! A folder or a `.zip` dropped on the window, reviewed, then placed.
//!
//! # The shape of the sheet
//!
//! One row per package found, each with its own checkbox, because one archive
//! often carries several. Each row says what the package is, what it declares,
//! and the verdict: New, an Update of something named, or an Overlap with
//! something by a different author. Then **one confirm that names how many will
//! be placed and how many replaced**, as when applying a profile.
//!
//! # What is guarded, and where
//!
//! The guard is on the action, never on the button: the sheet draws and the
//! confirm is clickable in an inert state; [`place`] refuses. A guard nothing
//! can reach is a guard no test exercises.
//!
//! Reading is not a write, so an inert state reads and the whole reviewing half
//! renders in a test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hangar_install::drop::{
    self, Approval, Confidence, Found, GateRequest, Reading, Verdict, Verification,
};
use hangar_install::gate::{Outcome, Refusal, Report};
use hangar_install::record;
use hangar_msfs::manifest::format_size;
use hangar_ui as ui;

use crate::app::HangarApp;

/// One line of what a placement did, and whether it went in.
///
/// A sentence and a tone per package, so a partial success reads as both.
pub type OutcomeLine = (String, bool);

/// What a finished placement reported, once. `None` until it has.
pub type Outcomes = Arc<Mutex<Option<Vec<OutcomeLine>>>>;

/// One package found in a drop, with what is proposed for it.
#[derive(Clone)]
pub struct Row {
    pub found: Found,
    pub verdict: Verdict,
    /// Whether this one is included in the confirm. Defaults on, except for an
    /// overlap — replacing another author's work is not something to arrive
    /// pre-ticked.
    pub include: bool,
}

/// What the read produced.
pub enum Sheet {
    /// Packages to review.
    Rows(Vec<Row>),
    /// An `.exe`. Handed to the seven stages instead.
    Installer(PathBuf),
    /// Refused, with why. Every refusal names what was dropped.
    Refused(String),
}

/// Everything the drop half of the Install tab remembers.
pub struct DropState {
    /// False for [`DropState::inert`], which is what `HangarApp::new` gets.
    /// The whole reviewing half still works; only [`place`] is refused.
    live: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// What was dropped.
    pub source: Option<PathBuf>,
    /// Which Community folder was explicitly chosen for this source.
    pub community: Option<PathBuf>,
    pub sheet: Arc<Mutex<Option<Sheet>>>,
    pub reading: Arc<Mutex<bool>>,
    /// How many packages the chosen folder held when the sheet was read.
    ///
    /// Counted once, at read time, never walked per frame.
    pub installed_count: usize,
    /// Where the reading task leaves [`Self::installed_count`], since it runs
    /// off the UI thread.
    pub counted: Arc<Mutex<usize>>,
    /// Whether the sim was running when the sheet was read. A snapshot for
    /// display; the gate is run again against the live answer before any write.
    pub sim_running: bool,
    /// Second stage of the confirm.
    pub confirm: bool,
    pub placing: Arc<Mutex<bool>>,
    /// One line per package, and whether it went in.
    pub outcome: Outcomes,
}

impl Default for DropState {
    fn default() -> Self {
        Self::inert()
    }
}

impl DropState {
    /// A state that can never place anything. Reads, proposes and draws.
    pub fn inert() -> Self {
        Self {
            live: false,
            wake: Arc::new(|| {}),
            source: None,
            community: None,
            sheet: Arc::new(Mutex::new(None)),
            reading: Arc::new(Mutex::new(false)),
            installed_count: 0,
            counted: Arc::new(Mutex::new(0)),
            sim_running: false,
            confirm: false,
            placing: Arc::new(Mutex::new(false)),
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    /// A state that can. Built by `main.rs`, once there is a window.
    pub fn live(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            live: true,
            wake: Arc::new(wake),
            ..Self::inert()
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Forget the last drop entirely.
    pub fn clear(&mut self) {
        self.source = None;
        self.community = None;
        self.confirm = false;
        *self.sheet.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn is_reading(&self) -> bool {
        *self.reading.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_placing(&self) -> bool {
        *self.placing.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Take a dropped folder or archive.
///
/// Read-only whatever the state, like `InstallState::select` next door.
pub fn accept(app: &mut HangarApp, path: PathBuf) {
    app.drop_install.clear();
    // The installer cards and the drop sheet are alternatives; only one shows.
    app.install.forget();
    app.drop_install.source = Some(path);
    // The destination is chosen per drop, even with one Community folder.
}

/// Every Community folder discovery found.
pub fn communities(app: &HangarApp) -> Vec<PathBuf> {
    app.discovery
        .msfs()
        .map(|m| m.communities.iter().map(|c| c.path.clone()).collect())
        .unwrap_or_default()
}

/// Read the drop and propose an identity for everything in it.
///
/// Blocking: it walks a directory or reads an archive's central directory, and
/// reads every installed manifest to propose against. None of that belongs on
/// the UI thread.
fn start_read(app: &mut HangarApp) {
    let (Some(source), Some(community)) = (
        app.drop_install.source.clone(),
        app.drop_install.community.clone(),
    ) else {
        return;
    };

    app.drop_install.sim_running = hangar_runtime::sim::is_running();
    *app.drop_install
        .reading
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = true;
    *app.drop_install
        .sheet
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let sheet = Arc::clone(&app.drop_install.sheet);
    let reading = Arc::clone(&app.drop_install.reading);
    let counted = Arc::clone(&app.drop_install.counted);
    let wake = Arc::clone(&app.drop_install.wake);

    tokio::task::spawn_blocking(move || {
        let staging = drop::staging_root(&community);
        let read = match drop::read(&source, &staging) {
            Err(e) => Sheet::Refused(e.to_string()),
            Ok(Reading::Installer(exe)) => Sheet::Installer(exe),
            Ok(Reading::Packages(found)) => {
                let installed = drop::installed_in(&community);
                *counted.lock().unwrap_or_else(|e| e.into_inner()) = installed.len();
                Sheet::Rows(
                    found
                        .into_iter()
                        .map(|found| {
                            let verdict = drop::propose(&found, &installed);
                            let include = !matches!(verdict, Verdict::Overlaps { .. });
                            Row {
                                found,
                                verdict,
                                include,
                            }
                        })
                        .collect(),
                )
            }
        };
        *sheet.lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
        *reading.lock().unwrap_or_else(|e| e.into_inner()) = false;
        wake();
    });
}

/// The gate, against what the sheet currently holds.
pub fn gate_now(app: &HangarApp, rows: &[Row], sim_running: bool) -> Report {
    let discovered = communities(app);
    let need: u64 = rows
        .iter()
        .filter(|r| r.include)
        .map(|r| r.found.bytes)
        .sum();
    drop::gate(GateRequest {
        writes_allowed: app.writes_allowed,
        sim_running,
        discovered: &discovered,
        chosen: app.drop_install.community.as_deref(),
        need_bytes: need,
    })
}

/// **The only place in the application that places a dropped package.**
///
/// Refused unless the state is live and the gate — re-run here, against the
/// sim as it is at this instant rather than as the sheet found it — is clear.
/// Refused with a line rather than a panic: the caller is a click handler.
pub(crate) fn place(app: &mut HangarApp, rows: Vec<Row>) {
    if !app.drop_install.is_live() {
        tracing::warn!("an inert install state was asked to place a drop; refused");
        set_outcome(
            &app.drop_install.outcome,
            vec![("this build cannot write to Community".into(), false)],
        );
        return;
    }
    // Nothing else may be changing Community while a package goes in.
    if let Some(why) = app.community_busy() {
        set_outcome(&app.drop_install.outcome, vec![(why.into(), false)]);
        return;
    }
    let Some(community) = app.drop_install.community.clone() else {
        return;
    };
    let Some(source) = app.drop_install.source.clone() else {
        return;
    };

    let chosen: Vec<Row> = rows.into_iter().filter(|r| r.include).collect();
    if chosen.is_empty() {
        return;
    }

    // Re-run the gate rather than trusting the one on screen: the sheet may
    // have been sitting there while the sim was started.
    let report = gate_now(app, &chosen, hangar_runtime::sim::is_running());
    if !report.is_clear() {
        set_outcome(
            &app.drop_install.outcome,
            report
                .refusals()
                .iter()
                .map(|r| (r.to_string(), false))
                .collect(),
        );
        return;
    }

    let replacing = chosen
        .iter()
        .filter(|r| r.verdict.replaces().is_some())
        .count();
    let approval = Approval::given(format!(
        "the Install tab's confirm was pressed for {}: {} to place, {replacing} to replace",
        source.display(),
        chosen.len()
    ));

    app.journal.note(
        "install",
        format!(
            "drop-install: {} package(s) into {}",
            chosen.len(),
            community.display()
        ),
    );

    // Same rule as the installer half: the field has done its job.
    app.install.typed.clear();

    *app.drop_install
        .placing
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = true;
    let placing = Arc::clone(&app.drop_install.placing);
    let outcome = Arc::clone(&app.drop_install.outcome);
    let wake = Arc::clone(&app.drop_install.wake);
    let sheet = Arc::clone(&app.drop_install.sheet);
    // The history list is cached; mark it stale so this record shows.
    let history_stale = Arc::clone(&app.install.history_stale);

    tokio::task::spawn_blocking(move || {
        let stamp = hangar_core::time::Utc::now().stamp();
        let mut lines = Vec::new();
        let mut placed = Vec::new();

        for row in &chosen {
            match drop::place(
                &row.found,
                &community,
                &stamp,
                &approval,
                row.verdict.replaces(),
            ) {
                Ok(one) => {
                    let replaced = match one.replaced.as_slice() {
                        [] => String::new(),
                        [old] => format!(
                            ", replacing {} (moved aside, not deleted)",
                            old.version.clone().unwrap_or_else(|| old.name.clone())
                        ),
                        many => format!(
                            ", replacing {} older copies (moved aside, not deleted)",
                            many.len()
                        ),
                    };
                    lines.push((
                        format!("{} — {}{replaced}", one.name, one.verification.summary()),
                        matches!(one.verification, Verification::Checked(_)),
                    ));
                    placed.push(one);
                }
                Err(e) => lines.push((format!("{}: {e}", row.found.name), false)),
            }
        }

        if !placed.is_empty() {
            let bytes = std::fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
            let filed = record::InstallRecord::of_drop(&source, bytes, &community, &placed);
            match record::file_default(&filed) {
                Ok(_) => *history_stale.lock().unwrap_or_else(|e| e.into_inner()) = true,
                Err(e) => lines.push((
                    format!("placed, but the record could not be filed: {e}"),
                    false,
                )),
            }
        }

        // Everything landed, so the proposal is spent. A partial failure keeps
        // its sheet so the rest can be retried.
        if lines.iter().all(|(_, ok)| *ok) {
            *sheet.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(lines);
        *placing.lock().unwrap_or_else(|e| e.into_inner()) = false;
        wake();
    });
}

fn set_outcome(slot: &Outcomes, lines: Vec<OutcomeLine>) {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(lines);
}

// ── drawing ──────────────────────────────────────────────────────────────────

/// The review sheet. Draws nothing when nothing has been dropped.
pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    if app.drop_install.source.is_none() {
        return;
    }

    app.drop_install.installed_count = *app
        .drop_install
        .counted
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // Cloned out so the lock guard is not held across a `&mut app` call.
    let outcome = app
        .drop_install
        .outcome
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(lines) = outcome {
        outcome_card(app, ui, &lines);
        ui.add_space(ui::ROW_GAP);
    }

    if app.drop_install.is_placing() {
        ui::group(ui, None, |ui| {
            ui::activity(ui, "Installing packages…");
        });
        ui.add_space(ui::ROW_GAP);
        return;
    }

    if app.drop_install.community.is_none() {
        generation_card(app, ui);
        ui.add_space(ui::ROW_GAP);
        return;
    }

    destination_card(app, ui);
    ui.add_space(ui::ROW_GAP);

    let sheet = app
        .drop_install
        .sheet
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let rows = match sheet.as_ref() {
        None => {
            drop(sheet);
            if app.drop_install.is_reading() {
                reading_card(ui);
                ui.add_space(ui::ROW_GAP);
            }
            return;
        }
        Some(Sheet::Refused(why)) => {
            let why = why.clone();
            drop(sheet);
            refusal_card(ui, &why);
            ui.add_space(ui::ROW_GAP);
            return;
        }
        Some(Sheet::Installer(exe)) => {
            let exe = exe.clone();
            drop(sheet);
            installer_card(ui, &exe);
            ui.add_space(ui::ROW_GAP);
            return;
        }
        Some(Sheet::Rows(rows)) => rows.clone(),
    };
    drop(sheet);

    let mut rows = rows;
    let report = gate_now(app, &rows, app.drop_install.sim_running);

    ui::group(ui, None, |ui| {
        packages(app, ui, &mut rows);
        blockers(ui, &report);
        actions(app, ui, &rows, &report);
    });
    ui.add_space(ui::ROW_GAP);

    // Write the checkbox state back, so a tick survives the next frame.
    if let Some(Sheet::Rows(held)) = app
        .drop_install
        .sheet
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        for (held, row) in held.iter_mut().zip(rows.iter()) {
            held.include = row.include;
        }
    }
}

/// **The generation is never guessed.** `minimum_game_version` looks like a
/// hint and is not one (real values overlap across both folders), so the
/// question is asked.
fn generation_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    let all = communities(app);
    let mut picked = None;
    ui::group(ui, Some("Where should it go?"), |ui| {
        if all.is_empty() {
            ui.label(
                egui::RichText::new("No Community folder was found.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::ERR),
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for community in &all {
                let name = community
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| community.display().to_string());
                if ui::segment(ui, &name, false)
                    .on_hover_text(community.display().to_string())
                    .clicked()
                {
                    picked = Some(community.clone());
                }
            }
        });
    });
    if let Some(community) = picked {
        app.drop_install.community = Some(community);
        app.drop_install.confirm = false;
        start_read(app);
    }
}

fn destination_card(app: &mut HangarApp, ui: &mut egui::Ui) {
    let Some(community) = app.drop_install.community.as_ref() else {
        return;
    };
    let name = community
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| community.display().to_string());
    let path = community.display().to_string();
    let mut change = false;
    ui::group(ui, Some("Destination"), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(format!("{name} · {path}")).truncate());
            if !app.drop_install.is_reading()
                && !app.drop_install.is_placing()
                && ui::tool_button(
                    ui,
                    egui_phosphor::regular::PENCIL_SIMPLE,
                    "Change",
                    ui::ButtonKind::Ghost,
                )
                .clicked()
            {
                change = true;
            }
        });
    });
    if change {
        app.drop_install.community = None;
        app.drop_install.confirm = false;
        *app.drop_install
            .sheet
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        *app.drop_install
            .outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn reading_card(ui: &mut egui::Ui) {
    ui::group(ui, None, |ui| {
        ui::activity(ui, "Reading packages…");
    });
}

/// Why a drop could not even be read. Never a reason it was *refused* on
/// policy — that is [`blockers`] — so every case here is the file itself.
fn refusal_card(ui: &mut egui::Ui, why: &str) {
    ui::group(ui, None, |ui| {
        ui.horizontal(|ui| {
            ui::mark(ui, ui::Tone::Err);
            ui.add_space(4.0);
            ui.add(egui::Label::new(
                egui::RichText::new(plainly_dropped(why))
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::ERR),
            ))
            .on_hover_text(why);
        });
    });
}

/// The read refusals, shortened to the part a person can act on.
///
/// Matched on the rendered text, because the sheet keeps the read's answer as
/// a string; the full text is on the hover.
fn plainly_dropped(why: &str) -> String {
    if why.contains("Compression method not supported") {
        "That .zip uses a compression this build cannot read. Unzip it and drop the folder.".into()
    } else if why.contains("holds no package") {
        "There is no add-on in there \u{2014} a package is a folder with a manifest.json in it."
            .into()
    } else if why.contains("which is not installed") {
        // `NoTool` already names the tool, and that is the actionable half.
        why.rsplit(" needs ")
            .next()
            .map(|t| format!("That archive needs {t}."))
            .unwrap_or_else(|| why.to_owned())
    } else if why.contains("is not something that can be installed") {
        "That is not an add-on. Drop a folder, a .zip or a Windows installer.".into()
    } else if why.starts_with("nothing at ") {
        "There is nothing at that path.".into()
    } else {
        why.to_owned()
    }
}

fn installer_card(ui: &mut egui::Ui, exe: &Path) {
    ui::group(ui, None, |ui| {
        ui.horizontal(|ui| {
            ui::mark(ui, ui::Tone::Accent);
            ui.add_space(4.0);
            ui.add(egui::Label::new(
                egui::RichText::new(
                    "That is a Windows installer \u{2014} it is being inspected above.",
                )
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
            ))
            .on_hover_text(exe.display().to_string());
        });
    });
}

fn packages(app: &mut HangarApp, ui: &mut egui::Ui, rows: &mut [Row]) {
    let many = rows.len() > 1;
    if many {
        ui.label(
            egui::RichText::new(format!("{} packages", rows.len()))
                .font(egui::FontId::new(ui::theme::TEXT_LG, ui::theme::semibold()))
                .color(ui::theme::TEXT_HI),
        );
        ui.add_space(8.0);
    }

    let source = app.drop_install.source.clone().unwrap_or_default();
    for row in rows.iter_mut() {
        // A row is tickable only when there is a choice to make: several
        // packages, or one the proposal did not include by default.
        if many || !row.include {
            if ui::toggle_chip(
                ui,
                if row.include { "install" } else { "skip" },
                row.include,
                ui::theme::ORANGE,
            )
            .clicked()
            {
                row.include = !row.include;
            }
            ui.add_space(4.0);
        }

        ui.add(
            egui::Label::new(
                egui::RichText::new(row.found.title())
                    .font(egui::FontId::new(
                        if many {
                            ui::theme::TEXT_MD
                        } else {
                            ui::theme::TEXT_LG
                        },
                        ui::theme::semibold(),
                    ))
                    .color(ui::theme::TEXT_HI),
            )
            .truncate(),
        )
        .on_hover_text(format!("{}\n{}", row.found.name, source.display()));

        ui.add_space(2.0);
        // Truncated rather than wrapped: one long creator must not make every
        // other row ragged.
        let mut facts = vec![
            row.found.manifest.creator.trim().to_owned(),
            row.found.manifest.content_type().label().to_owned(),
            format!("v{}", row.found.version()),
            format_size(row.found.bytes),
        ];
        facts.retain(|f| !f.is_empty());
        ui.add(
            egui::Label::new(
                egui::RichText::new(facts.join("  \u{00b7}  "))
                    .size(ui::theme::TEXT_SM)
                    .color(ui::theme::DIM),
            )
            .truncate(),
        );

        ui.add_space(4.0);
        // Outside any horizontal, so the verdict sentence wraps.
        match &row.verdict {
            Verdict::New => {
                ui.label(
                    egui::RichText::new(what_it_adds(row))
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::DIM),
                );
            }
            Verdict::Update {
                of,
                from,
                to,
                confidence,
                ..
            } => {
                let hedge = match confidence {
                    Confidence::NearCertain => "Replaces",
                    // A content-and-creator match is a proposal, not a fact:
                    // nothing in a package identifies it.
                    Confidence::Likely => "Looks like a replacement for",
                };
                let have = if from == "?" {
                    "the version".to_owned()
                } else {
                    format!("v{from}")
                };
                ui.label(
                    egui::RichText::new(format!("{hedge} {have} you have installed, with v{to}."))
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::TEXT),
                )
                .on_hover_text(of.as_str());
            }
            Verdict::Overlaps { with, .. } => {
                ui.label(
                    egui::RichText::new(format!(
                        "Covers the same place as {with}, by someone else \u{2014} installing \
                         it replaces nothing."
                    ))
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::WARN),
                );
            }
        }
        ui.add_space(if many { 12.0 } else { 6.0 });
    }
}

/// What a new package puts in the sim, when it says.
///
/// `declares` is the manifest's `ContentInfo`: the airports and objects the
/// package claims. Plenty of packages carry none.
fn what_it_adds(row: &Row) -> String {
    if row.found.declares.is_empty() {
        return "Not installed yet.".into();
    }
    let mut what: Vec<String> = row.found.declares.iter().cloned().collect();
    what.sort();
    format!("Adds {}.", what.join(", ").to_lowercase())
}

/// **Report by exception.** A pass is the expected case and is not drawn.
fn blockers(ui: &mut egui::Ui, report: &Report) {
    for checked in &report.outcomes {
        let (tone, colour, line) = match &checked.outcome {
            Outcome::Pass(_) => continue,
            Outcome::Unchecked(why) => (
                ui::Tone::Warn,
                ui::theme::WARN,
                format!("Could not check {}: {why}", checked.label),
            ),
            Outcome::Refuse(refusal) => (ui::Tone::Err, ui::theme::ERR, plainly(refusal)),
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui::mark(ui, tone);
            ui.add_space(4.0);
            // The typed refusal's exact words are on the hover, for diagnosis.
            ui.add(
                egui::Label::new(
                    egui::RichText::new(line)
                        .size(ui::theme::TEXT_BODY)
                        .color(colour),
                )
                .wrap(),
            )
            .on_hover_text(match &checked.outcome {
                Outcome::Refuse(r) => r.to_string(),
                Outcome::Unchecked(why) => why.clone(),
                Outcome::Pass(_) => String::new(),
            });
        });
    }
}

/// A gate refusal in words that say what to do about it.
///
/// The typed `Display` says what happened and why, which is right in a log and
/// wrong on a page. It is kept, on the hover.
fn plainly(refusal: &Refusal) -> String {
    match refusal {
        Refusal::WritesNotAllowed => {
            "HangarRS is not allowed to change your Community folder. Turn that on in Settings."
                .into()
        }
        Refusal::SimRunning => "Close the flight simulator first.".into(),
        Refusal::NotEnoughSpace { have, need, .. } => format!(
            "Not enough room \u{2014} {} needed, {} free.",
            hangar_core::disk::human(*need),
            hangar_core::disk::human(*have)
        ),
        Refusal::SpaceUnknown { .. } => "Could not tell how much room is free.".into(),
        Refusal::NoCommunity => "No Community folder was found to install into.".into(),
        Refusal::GenerationUnchosen { .. } => "Choose a Community folder first.".into(),
        Refusal::CommunityNotDiscovered { .. } => {
            "That Community folder is no longer available. Choose a destination again.".into()
        }
        // The prefix and runtime refusals belong to the installer pipeline and
        // cannot reach a drop.
        other => other.to_string(),
    }
}

/// Install, or don't. One row, and a sentence only where there is a
/// consequence worth naming.
fn actions(app: &mut HangarApp, ui: &mut egui::Ui, rows: &[Row], report: &Report) {
    let included: Vec<&Row> = rows.iter().filter(|r| r.include).collect();
    let replacing = included
        .iter()
        .filter(|r| r.verdict.replaces().is_some())
        .count();
    let clear = report.is_clear();

    let mut go = false;
    let mut cancel = false;
    let mut arm = false;

    ui.add_space(10.0);

    if app.drop_install.is_placing() {
        ui::activity(ui, "Installing packages…");
        return;
    }

    let armed = app.drop_install.confirm;
    ui.horizontal(|ui| {
        if armed {
            // The guard is on the action, not on the button: this is clickable
            // in an inert state, and `place` refuses.
            if ui::tool_button(
                ui,
                egui_phosphor::regular::CHECK,
                "Yes, install",
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                go = true;
            }
        } else if ui::tool_button_enabled(
            ui,
            clear && !included.is_empty(),
            egui_phosphor::regular::DOWNLOAD_SIMPLE,
            if included.len() > 1 {
                "Install them"
            } else {
                "Install"
            },
            ui::ButtonKind::Primary,
        )
        .clicked()
        {
            arm = true;
        }

        if ui::tool_button(
            ui,
            egui_phosphor::regular::X,
            "Cancel",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            cancel = true;
        }
    });

    // Nothing is ever deleted; the consequence worth naming is what a
    // replacement does to the copy already there.
    let note = if replacing > 0 {
        Some("The version you have now is moved aside, not deleted \u{2014} you can put it back.")
    } else if !armed && clear && !included.is_empty() {
        Some("Nothing is copied until you click Install.")
    } else {
        None
    };
    if let Some(note) = note {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(note)
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::DIM),
        );
    }

    if arm {
        app.drop_install.confirm = true;
    }
    if cancel {
        app.drop_install.confirm = false;
    }
    if go {
        app.drop_install.confirm = false;
        place(app, rows.to_vec());
    }
}

fn outcome_card(app: &mut HangarApp, ui: &mut egui::Ui, lines: &[OutcomeLine]) {
    let mut done = false;
    let mut view_hangar = false;
    ui::group(ui, None, |ui| {
        for (line, ok) in lines {
            ui.horizontal(|ui| {
                ui::state(
                    ui,
                    if *ok { "installed" } else { "failed" },
                    if *ok { ui::Tone::Ok } else { ui::Tone::Err },
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(line)
                            .size(ui::theme::TEXT_BODY)
                            .color(if *ok { ui::theme::TEXT } else { ui::theme::ERR }),
                    )
                    .truncate(),
                );
            });
            ui.add_space(4.0);
        }
        // With the sheet spent there is no Cancel; this is the way out.
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if lines.iter().any(|(_, ok)| *ok)
                && ui::tool_button(
                    ui,
                    egui_phosphor::regular::ARROW_RIGHT,
                    "View in Hangar",
                    ui::ButtonKind::Normal,
                )
                .clicked()
            {
                view_hangar = true;
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::PLUS,
                "Install another",
                ui::ButtonKind::Primary,
            )
            .clicked()
            {
                done = true;
            }
        });
    });
    if lines.iter().any(|(_, ok)| *ok)
        && ui::tool_button(
            ui,
            egui_phosphor::regular::ARCHIVE,
            "Preserved copies",
            ui::ButtonKind::Ghost,
        )
        .clicked()
    {
        app.settings_view = crate::settings::View::Storage;
        app.active_tab = crate::tab::Tab::Settings;
    }
    if view_hangar {
        app.hangar_selected = lines
            .iter()
            .find(|(_, ok)| *ok)
            .and_then(|(line, _)| line.split_once(" — ").map(|(name, _)| name.to_owned()));
        app.active_tab = crate::tab::Tab::Hangar;
        app.hangar_section = None;
        app.rescan_requested = true;
    }
    if done {
        app.drop_install.clear();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hangar_install::drop::{Source, VersionChange};
    use hangar_msfs::manifest::ManifestJson;
    use std::collections::BTreeSet;

    fn found(name: &str, creator: &str, version: &str, declares: &[&str]) -> Found {
        let manifest = ManifestJson::parse(&format!(
            r#"{{"title":"Placerville","creator":"{creator}","content_type":"SCENERY",
                 "package_version":"{version}","minimum_game_version":"1.39.12"}}"#
        ))
        .expect("manifest");
        Found {
            source: Source::Directory(PathBuf::from("/nonexistent").join(name)),
            name: name.into(),
            manifest,
            declares: declares
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>(),
            bytes: 4 * 1024 * 1024,
        }
    }

    /// A sheet in every shape a real drop produces: something new, an update
    /// of a differently-named folder, and an overlap by another author.
    pub(crate) fn populated_rows() -> Vec<Row> {
        vec![
            Row {
                found: found("koak", "vbcorp", "1.0.0", &["Airport KOAK"]),
                verdict: Verdict::New,
                include: true,
            },
            Row {
                found: found(
                    "vbcorp-airport-kpvf-placerville",
                    "vbcorp",
                    "3.0.0",
                    &["Airport KPVF"],
                ),
                verdict: Verdict::Update {
                    of: "zzz-vbcorp-airport-kpvf-placerville".into(),
                    from: "1.0.0".into(),
                    to: "3.0.0".into(),
                    change: VersionChange::Newer,
                    confidence: Confidence::Likely,
                },
                include: true,
            },
            Row {
                found: found(
                    "someone-elses-kpvf",
                    "otherperson",
                    "2.0.0",
                    &["Airport KPVF"],
                ),
                verdict: Verdict::Overlaps {
                    with: "vbcorp-airport-kpvf-placerville".into(),
                    shared: vec!["Airport KPVF".into()],
                },
                include: false,
            },
        ]
    }

    /// Passes are not drawn; only the refusal is.
    #[test]
    fn a_clear_gate_says_nothing_at_all() {
        use hangar_install::gate::{Checked, Outcome};

        let clear = Report {
            outcomes: vec![
                Checked {
                    label: "Community writes are allowed".into(),
                    outcome: Outcome::Pass("the toggle is on".into()),
                },
                Checked {
                    label: "the sim is not running".into(),
                    outcome: Outcome::Pass("no sim process".into()),
                },
            ],
        };
        assert_eq!(drawn_blockers(&clear), Vec::<String>::new());

        let refused = Report {
            outcomes: vec![
                Checked {
                    label: "Community writes are allowed".into(),
                    outcome: Outcome::Refuse(Refusal::WritesNotAllowed),
                },
                Checked {
                    label: "the sim is not running".into(),
                    outcome: Outcome::Pass("no sim process".into()),
                },
            ],
        };
        assert_eq!(
            drawn_blockers(&refused),
            vec![
                "HangarRS is not allowed to change your Community folder. Turn that on in \
                 Settings."
                    .to_string()
            ],
            "only the refusal, and in words that say what to do"
        );
    }

    /// What `blockers` would put on screen, without a `Ui`.
    fn drawn_blockers(report: &Report) -> Vec<String> {
        report
            .outcomes
            .iter()
            .filter_map(|c| match &c.outcome {
                Outcome::Pass(_) => None,
                Outcome::Unchecked(why) => Some(format!("Could not check {}: {why}", c.label)),
                Outcome::Refuse(r) => Some(plainly(r)),
            })
            .collect()
    }

    /// A refusal's typed `Display` says what happened; the page has to say what
    /// to do. Both survive — the second on screen, the first on the hover.
    #[test]
    fn a_refusal_is_rewritten_into_something_actionable() {
        let sim = plainly(&Refusal::SimRunning);
        assert_eq!(sim, "Close the flight simulator first.");
        assert!(
            !sim.contains("half-written"),
            "the reason is the log's job, not the page's: {sim}"
        );

        let room = plainly(&Refusal::NotEnoughSpace {
            path: PathBuf::from("/mnt/steam"),
            have: 1_000_000,
            need: 4_000_000_000,
        });
        assert!(room.contains("needed"), "{room}");
        assert!(
            !room.contains("/mnt/steam"),
            "the path is not the fix: {room}"
        );
    }

    /// A read that failed on the file itself, said so a person can act.
    #[test]
    fn an_unreadable_archive_says_what_to_do_about_it() {
        let plain = plainly_dropped(
            "/x/a.zip is not a readable archive: unsupported Zip archive: \
             Compression method not supported",
        );
        assert!(plain.contains("Unzip it"), "{plain}");

        // Anything unrecognised is passed through rather than swallowed: a
        // refusal nobody translated is still a refusal somebody must read.
        assert_eq!(
            plainly_dropped("something new went wrong"),
            "something new went wrong"
        );
    }

    /// "declares" is the manifest's word, not a person's.
    #[test]
    fn a_new_package_says_what_it_adds() {
        let mut row = Row {
            found: found("kpvf", "vbcorp", "3.0.0", &["Airport KPVF"]),
            verdict: Verdict::New,
            include: true,
        };
        assert_eq!(what_it_adds(&row), "Adds airport kpvf.");

        row.found.declares = BTreeSet::new();
        assert_eq!(what_it_adds(&row), "Not installed yet.");
    }

    /// After a clean install the sheet must not still offer to install what
    /// has just been installed, over an outcome line saying it already was.
    #[test]
    fn a_finished_drop_stops_offering_to_install_itself() {
        let state = DropState::inert();
        *state.sheet.lock().expect("lock") = Some(Sheet::Rows(tests::populated_rows()));

        // What the placing task does when every line landed.
        let lines: Vec<OutcomeLine> = vec![("kpvf — 412/412 verified".into(), true)];
        if lines.iter().all(|(_, ok)| *ok) {
            *state.sheet.lock().expect("lock") = None;
        }
        assert!(
            state.sheet.lock().expect("lock").is_none(),
            "a spent proposal comes off the page"
        );

        // A partial failure keeps its rows: what did not land is still a thing
        // to retry.
        *state.sheet.lock().expect("lock") = Some(Sheet::Rows(tests::populated_rows()));
        let mixed: Vec<OutcomeLine> = vec![
            ("kpvf — verified".into(), true),
            ("koak: no room".into(), false),
        ];
        if mixed.iter().all(|(_, ok)| *ok) {
            *state.sheet.lock().expect("lock") = None;
        }
        assert!(state.sheet.lock().expect("lock").is_some());
    }

    #[test]
    fn an_overlap_does_not_arrive_ticked() {
        let rows = populated_rows();
        assert!(rows[0].include, "a new package is ticked");
        assert!(rows[1].include, "so is an update");
        assert!(!rows[2].include, "an overlap is not");
    }

    /// The guard is on the action, not on the button.
    #[test]
    fn an_inert_state_cannot_place_anything() {
        let state = DropState::inert();
        assert!(!state.is_live());
        assert!(!state.is_placing());
    }

    #[test]
    fn a_live_state_can() {
        assert!(DropState::live(|| {}).is_live());
    }

    #[test]
    fn forgetting_a_drop_clears_the_sheet_and_the_outcome() {
        let mut state = DropState::inert();
        state.source = Some(PathBuf::from("/tmp/kpvf.zip"));
        state.confirm = true;
        *state.sheet.lock().expect("lock") = Some(Sheet::Rows(populated_rows()));
        *state.outcome.lock().expect("lock") = Some(vec![("placed".into(), true)]);

        state.clear();

        assert!(state.source.is_none());
        assert!(!state.confirm);
        assert!(state.sheet.lock().expect("lock").is_none());
        assert!(state.outcome.lock().expect("lock").is_none());
    }

    /// A new source must receive its own explicit Community choice.
    #[test]
    fn clearing_a_drop_forgets_the_chosen_destination() {
        let mut state = DropState::inert();
        state.community = Some(PathBuf::from("/x/Packages/Community2024"));
        state.source = Some(PathBuf::from("/tmp/a.zip"));
        state.clear();
        assert!(state.community.is_none());
    }
}
