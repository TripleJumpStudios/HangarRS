//! The Prefix tab — direct facts about the two Wine prefixes.
//!
//! # The separate prefix does not isolate
//!
//! There is no OverlayFS companion prefix (`AppKind` has no `overlay` variant,
//! so no config can ask for one). And the companion prefix's sim AppData is a
//! symlink into the sim's:
//!
//! ```text
//! companion-prefix/pfx/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024
//!   -> …/compatdata/2537590/pfx/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024
//! ```
//!
//! One file, two names: an installer "confined" to the companion prefix can
//! write the sim's Community folder and `EXE.xml` (FSUIPC7's does). This tab's
//! headline is that measurement, taken from the filesystem.
//!
//! # What writes
//!
//! `SimConnect.cfg`, into a live prefix, behind its own confirm that names both
//! files. It never overwrites an existing one — somebody may have tuned it.
//! The sim detection link, behind its own confirm.
//!
//! And **Set up companion prefix**, which a new user needs before any companion
//! app can be installed: it creates the directory, initialises it with the
//! sim's Proton, adds the link and writes `SimConnect.cfg`
//! (`hangar_runtime::companion_setup`, which refuses while the sim runs or when
//! the target is the sim's own prefix). [`SetupState`] has an inert form that
//! refuses (`HangarApp::new`) and a live one (`main.rs`); the guard is on the
//! action, so the inert tab still draws and accepts the click. Everything else
//! on this tab is a read.
//!
//! # The sim's own SimConnect.xml
//!
//! One row in the companion card says whether the sim prefix's server config
//! exposes IPv4 SimConnect on the port `simconnect-rs` dials
//! ([`hangar_runtime::sim_simconnect`]). It is read once per sim prefix and
//! again after a set-up run finishes, never per frame, and only by the live
//! [`SetupState`]: the inert form reads nothing, so a render test never
//! reaches a real prefix. No write and no fix button.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hangar_runtime::companion_setup::{self, Initialise, PrefixState};
use hangar_runtime::sim_simconnect;

use hangar_install::registry::{self, Hive};
use hangar_runtime::env::{self, ProtonEnv};
use hangar_runtime::simlink;
use hangar_ui as ui;

use crate::app::HangarApp;

/// A set-up running on its own thread; the slot fills when it finishes.
pub(crate) type Job = Arc<Mutex<Option<Result<String, String>>>>;

/// The companion-prefix setup action: inert or live, and its one job.
pub struct SetupState {
    /// Starts Proton. `None` is the inert form, and the action refuses.
    runner: Option<Arc<Initialise>>,
    sim_running: fn() -> bool,
    /// The second stage of the confirm.
    pub confirm: bool,
    pub(crate) job: Option<Job>,
    /// What the last run did, and whether it worked.
    pub result: Option<(String, bool)>,
    /// Whether the sim's `SimConnect.xml` is read at all. Off in the inert
    /// form, so a headless render never opens a real prefix.
    reads_sim: bool,
    /// The last reading of the sim's `SimConnect.xml`, and the prefix it was
    /// read from.
    pub(crate) sim_simconnect: Option<(PathBuf, sim_simconnect::Check)>,
}

impl SetupState {
    /// Draws, accepts the click, and refuses it.
    pub fn inert() -> Self {
        Self {
            runner: None,
            sim_running: || false,
            confirm: false,
            job: None,
            result: None,
            reads_sim: false,
            sim_simconnect: None,
        }
    }

    /// Runs `proton run wineboot` and reads `/proc` for the sim.
    pub fn live() -> Self {
        Self {
            runner: Some(Arc::new(companion_setup::proton_wineboot)),
            sim_running: hangar_runtime::sim::is_running,
            reads_sim: true,
            ..Self::inert()
        }
    }

    /// A test's form: an injected initialiser and sim check.
    #[cfg(test)]
    pub fn with_runner(runner: Arc<Initialise>, sim_running: fn() -> bool) -> Self {
        Self {
            runner: Some(runner),
            sim_running,
            ..Self::inert()
        }
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// The sim's `SimConnect.xml`, read when there is no reading for this
    /// prefix yet. The inert form never reads.
    pub(crate) fn sim_simconnect(&mut self, sim_prefix: &Path) -> Option<&sim_simconnect::Check> {
        let stale = self
            .sim_simconnect
            .as_ref()
            .is_none_or(|(read_from, _)| read_from != sim_prefix);
        if stale && self.reads_sim {
            let found = sim_simconnect::check(sim_prefix, simconnect_tcp::DEFAULT_PORT);
            self.sim_simconnect = Some((sim_prefix.to_path_buf(), found));
        }
        self.sim_simconnect
            .as_ref()
            .filter(|(read_from, _)| read_from == sim_prefix)
            .map(|(_, found)| found)
    }

    /// Collect a finished job. Returns its result once.
    fn take_finished(&mut self) -> Option<Result<String, String>> {
        let done = self
            .job
            .as_ref()?
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        self.job = None;
        Some(done)
    }
}

impl Default for SetupState {
    fn default() -> Self {
        Self::inert()
    }
}

/// Start setting up the companion prefix, on its own thread: a first
/// initialisation boots Wine and takes tens of seconds.
pub fn start_setup(
    app: &mut HangarApp,
    msfs: Option<ProtonEnv>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    app.companion_setup.confirm = false;
    let refuse = |app: &mut HangarApp, why: &str| {
        app.journal.fail("Prefix", why.to_string());
        app.companion_setup.result = Some((why.to_string(), false));
    };
    let Some(runner) = app.companion_setup.runner.clone() else {
        return refuse(app, "Advanced changes are unavailable in this session.");
    };
    if app.companion_setup.is_running() {
        return;
    }
    let (Some(msfs), Some(dir)) = (msfs, app.proton.as_ref().map(|p| p.compat_data.clone())) else {
        return refuse(app, "No Proton or companion prefix location was found.");
    };
    let sim_running = (app.companion_setup.sim_running)();
    let slot = Arc::new(Mutex::new(None));
    app.companion_setup.job = Some(slot.clone());
    app.companion_setup.result = None;
    std::thread::spawn(move || {
        let done = companion_setup::set_up(&dir, &msfs, sim_running, &*runner)
            .map(|outcome| outcome.summary())
            .map_err(|e| e.to_string());
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
        wake();
    });
}

/// Move a finished setup into the tab and the journal.
pub fn poll_setup(app: &mut HangarApp) {
    let Some(done) = app.companion_setup.take_finished() else {
        return;
    };
    // The tab's status refresh: the sim's server config is read again on the
    // next draw.
    if app.companion_setup.reads_sim {
        app.companion_setup.sim_simconnect = None;
    }
    match &done {
        Ok(line) => app.journal.done("Prefix", line.clone()),
        Err(why) => app.journal.fail("Prefix", why.clone()),
    }
    app.companion_setup.result = Some(match done {
        Ok(line) => (line, true),
        Err(why) => (why, false),
    });
}

/// The .NET runtimes worth reporting, and where each one records itself.
///
/// **The two arches land in different registry views**, as on Windows: x64 in
/// `HKLM\Software\dotnet\…\x64`, x86 reflected into
/// `HKLM\Software\Wow6432Node\dotnet\…\x86`. Reading only `Software\dotnet`
/// reports an installed x86 runtime missing, so each arch tries its own view
/// first and falls back to the other.
const DOTNET_ARCHES: [&str; 2] = ["x64", "x86"];

fn dotnet_keys(arch: &str) -> Vec<String> {
    let base = |view: &str| format!(r"{view}\dotnet\Setup\InstalledVersions\{arch}");
    if arch == "x86" {
        vec![base(r"Software\Wow6432Node"), base(r"Software")]
    } else {
        vec![base(r"Software"), base(r"Software\Wow6432Node")]
    }
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    let Some(msfs) = app.discovery.msfs() else {
        ui::group(ui, None, |ui| {
            ui.label(
                egui::RichText::new("MSFS was not found, so there is no prefix to look at.")
                    .color(ui::theme::DIM),
            );
        });
        return;
    };

    let msfs_prefix = msfs.install.prefix.clone();
    let companion = app.proton.as_ref().map(|p| p.wineprefix.clone());
    let proton = app
        .discovery
        .steam()
        .and_then(|steam| ProtonEnv::for_msfs(msfs, steam));

    companion_card(app, ui, &msfs_prefix, proton.clone());
    ui.add_space(ui::ROW_GAP);
    sim_link_card(app, ui, &msfs_prefix, companion.as_deref());
    ui.add_space(ui::ROW_GAP);
    simconnect_card(app, ui, &msfs_prefix, companion.as_deref());
    ui.add_space(ui::ROW_GAP);
    ui::controls::disclosure(
        ui,
        "prefix-locations",
        "Prefix locations and shared folders",
        |ui| {
            reach_card(ui, &msfs_prefix, companion.as_deref());
            prefixes_card(ui, &msfs_prefix, companion.as_deref());
        },
    );
    ui::controls::disclosure(ui, "prefix-launch", "Launch configuration", |ui| {
        invocation_card(ui, proton.as_ref())
    });
    ui::controls::disclosure(ui, "prefix-runtimes", "Installed runtimes", |ui| {
        runtimes_card(ui, &msfs_prefix, companion.as_deref())
    });
}

/// Where the companion prefix is, what state it is in, and the action that
/// sets it up.
fn companion_card(
    app: &mut HangarApp,
    ui: &mut egui::Ui,
    msfs_prefix: &Path,
    msfs: Option<ProtonEnv>,
) {
    poll_setup(app);
    ui::group(ui, Some("Companion prefix"), |ui| {
        let Some(dir) = app.proton.as_ref().map(|p| p.compat_data.clone()) else {
            ui.label(
                egui::RichText::new("No companion prefix location could be resolved.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            return;
        };
        let status = companion_setup::status(&dir, msfs_prefix);

        ui::kv_row(ui, "State", |ui| {
            let tone = match status.state {
                PrefixState::Ready => ui::Tone::Ok,
                PrefixState::Missing | PrefixState::NotInitialised => ui::Tone::Warn,
            };
            ui::state(ui, status.state.label(), tone);
        });
        path_row(ui, "Location", &dir);
        if let Some(source) = app.companion_source {
            ui::kv_row(ui, "Chosen by", |ui| {
                ui::state(ui, source.label(), ui::Tone::Idle);
            });
        }
        if let Some(version) = &status.proton_version {
            ui::kv_row(ui, "Proton", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(version.as_str())
                            .monospace()
                            .size(ui::theme::TEXT_SM),
                    )
                    .truncate(),
                );
            });
        }
        ui::kv_row(ui, "Sim link", |ui| {
            let (word, tone) = link_word(&status.link);
            ui::state(ui, word, tone);
        });
        ui::kv_row(ui, "SimConnect.cfg", |ui| {
            let (n, of) = (status.cfg_present(), status.cfg.len());
            let tone = if n == of {
                ui::Tone::Ok
            } else {
                ui::Tone::Warn
            };
            ui::state(ui, &format!("{n} of {of}"), tone);
        });
        if let Some(found) = app.companion_setup.sim_simconnect(msfs_prefix) {
            sim_simconnect_row(ui, found);
        }

        ui.add_space(8.0);
        if app.companion_setup.is_running() {
            ui::state(ui, "setting up", ui::Tone::Accent);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        } else if !status.needs_work() {
            app.companion_setup.confirm = false;
            ui.label(
                egui::RichText::new("Nothing to set up.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
        } else if !app.companion_setup.confirm {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::WRENCH,
                "Set up companion prefix\u{2026}",
                ui::ButtonKind::Normal,
            )
            .clicked()
            {
                app.companion_setup.confirm = true;
            }
        } else {
            ui::heading(ui, "Confirm");
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Runs the sim's Proton once to create this prefix; existing files are kept.",
                )
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::TEXT_HI),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::CHECK,
                    "Set it up",
                    ui::ButtonKind::Danger,
                )
                .clicked()
                {
                    let ctx = ui.ctx().clone();
                    start_setup(app, msfs.clone(), Arc::new(move || ctx.request_repaint()));
                }
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::X,
                    "Cancel",
                    ui::ButtonKind::Ghost,
                )
                .clicked()
                {
                    app.companion_setup.confirm = false;
                }
            });
        }

        if let Some((message, ok)) = &app.companion_setup.result {
            ui.add_space(8.0);
            ui::state(
                ui,
                if *ok { "done" } else { "failed" },
                if *ok { ui::Tone::Ok } else { ui::Tone::Err },
            );
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message.as_str())
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::TEXT),
                )
                .truncate(),
            );
        }
    });
}

/// The sim's `SimConnect.xml`, as one row: a state word and what was found.
fn sim_simconnect_row(ui: &mut egui::Ui, found: &sim_simconnect::Check) {
    let tone = match found {
        sim_simconnect::Check::Exposed { .. } => ui::Tone::Ok,
        sim_simconnect::Check::NoMatch { .. } | sim_simconnect::Check::Missing => ui::Tone::Warn,
        sim_simconnect::Check::Unreadable { .. } => ui::Tone::Err,
    };
    let detail = found.detail();
    ui::kv_row(ui, "Sim SimConnect.xml", |ui| {
        ui::state(ui, found.word(), tone);
        if !detail.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(detail.as_str())
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIM),
                )
                .truncate(),
            )
            .on_hover_text(detail.as_str());
        }
    });
}

fn link_word(link: &simlink::Check) -> (&'static str, ui::Tone) {
    match &link.state {
        simlink::State::Linked if link.target_exists => ("linked", ui::Tone::Ok),
        simlink::State::Linked => ("linked, target missing", ui::Tone::Err),
        simlink::State::Missing => ("missing", ui::Tone::Warn),
        simlink::State::RealDirectory { .. } => ("a real directory", ui::Tone::Accent),
        simlink::State::Elsewhere { .. } => ("points elsewhere", ui::Tone::Accent),
        simlink::State::Dangling { .. } => ("dangling", ui::Tone::Err),
        simlink::State::Unreadable(_) => ("could not be told", ui::Tone::Idle),
    }
}

/// The measurement that matters, taken from the filesystem.
fn reach_card(ui: &mut egui::Ui, msfs_prefix: &Path, companion: Option<&Path>) {
    ui::group(ui, Some("Companion prefix reach"), |ui| {
        let answer =
            companion.and_then(|p| hangar_install::gate::reaches_sim_content(p, msfs_prefix));

        ui::kv_row(ui, "Answer", |ui| {
            // Warn, not error: it is how the prefix is set up, but anything
            // reasoning from "separate prefix" is wrong.
            let (word, tone) = match answer {
                Some(true) => ("yes", ui::Tone::Warn),
                Some(false) => ("no", ui::Tone::Ok),
                // `None` is never "safe": a wrong `false` would let an
                // installer past the gate's sim check.
                None => ("could not be told", ui::Tone::Idle),
            };
            ui::state(ui, word, tone);
        });

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(match answer {
                Some(true) => {
                    "One directory, two names: an installer running in the companion prefix \
                     can write the sim's Community folder and EXE.xml."
                }
                Some(false) => "The two prefixes' sim AppData directories are separate here.",
                None => "Could not be resolved.",
            })
            .size(ui::theme::TEXT_BODY)
            .color(match answer {
                Some(true) => ui::theme::WARN,
                _ => ui::theme::DIM,
            }),
        );
    });
}

/// The link every companion app's sim detection runs through.
///
/// Read every frame; the write is behind its own confirm. Prose stays out of
/// `ui.horizontal`, where a label extends the row instead of wrapping.
fn sim_link_card(
    app: &mut HangarApp,
    ui: &mut egui::Ui,
    msfs_prefix: &Path,
    companion: Option<&Path>,
) {
    ui::group(ui, Some("Sim detection link"), |ui| {
        let Some(companion) = companion else {
            ui.label(
                egui::RichText::new("There is no companion prefix, so there is no link to check.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            return;
        };

        let found = simlink::check(companion, msfs_prefix);

        ui::kv_row(ui, "State", |ui| {
            let (text, tone) = link_word(&found);
            ui::state(ui, text, tone);
        });

        ui.add_space(4.0);
        if !found.is_good() {
            ui.label(
                egui::RichText::new(found.why())
                    .size(ui::theme::TEXT_SM)
                    .color(if found.is_good() {
                        ui::theme::DIM
                    } else {
                        ui::theme::WARN
                    }),
            );
        }
        ui.add_space(8.0);

        path_row(ui, "Link", &found.link);
        path_row(ui, "Target", &found.want);

        if !found.can_repair() {
            app.prefix_confirm_link = false;
            return;
        }

        ui.add_space(10.0);
        if !app.prefix_confirm_link {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::LINK,
                "Create the link\u{2026}",
                ui::ButtonKind::Normal,
            )
            .clicked()
            {
                app.prefix_confirm_link = true;
            }
            return;
        }

        ui.add_space(4.0);
        ui::heading(ui, "Confirm");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Creates a symlink inside a live prefix. A real directory, or a link aimed \
                 elsewhere, is refused rather than replaced.",
            )
            .size(ui::theme::TEXT_BODY)
            .color(ui::theme::TEXT_HI),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::CHECK,
                "Create it",
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                make_sim_link(app, &found);
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::X,
                "Cancel",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.prefix_confirm_link = false;
            }
        });
    });
}

/// A write on this tab. Refusals are reported in the same shape as a
/// failure, because from a person's side both mean "it did not happen".
fn make_sim_link(app: &mut HangarApp, found: &simlink::Check) {
    if !app.actions.sessions.is_live() {
        app.prefix_result = Some((
            "Advanced changes are unavailable in this session.".into(),
            false,
        ));
        return;
    }
    app.prefix_confirm_link = false;
    match simlink::repair(found) {
        Ok(path) => {
            app.journal
                .done("Prefix", format!("linked {}", path.display()));
            app.prefix_result = Some((format!("linked {}", path.display()), true));
        }
        Err(e) => {
            app.journal.fail("Prefix", e.to_string());
            app.prefix_result = Some((e.to_string(), false));
        }
    }
}

fn prefixes_card(ui: &mut egui::Ui, msfs_prefix: &Path, companion: Option<&Path>) {
    ui::group(ui, Some("Prefixes"), |ui| {
        path_row(ui, "MSFS", msfs_prefix);
        match companion {
            Some(path) => path_row(ui, "Companion", path),
            None => ui::kv_row(ui, "Companion", |ui| {
                ui::state(ui, "none", ui::Tone::Idle);
            }),
        }
    });
}

/// Exactly what a companion app is started with.
fn invocation_card(ui: &mut egui::Ui, proton: Option<&ProtonEnv>) {
    ui::group(ui, Some("How a Windows app is started"), |ui| {
        let Some(proton) = proton else {
            ui.label(
                egui::RichText::new(
                    "No compatibility tool is configured for MSFS in Steam. Read-only features \
                     work; nothing can be launched.",
                )
                .size(ui::theme::TEXT_BODY)
                .color(ui::theme::WARN),
            );
            return;
        };

        let (program, args) = proton.invocation();
        let line = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
            .join(" ");
        // Wrapping rather than truncated: the command is the thing worth
        // reading in full.
        ui.label(
            egui::RichText::new(format!("{line} <the app> <its arguments>"))
                .monospace()
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::TEXT),
        );
        ui.add_space(8.0);

        ui::heading(ui, "Environment");
        ui.add_space(3.0);
        for (key, value) in proton.vars() {
            ui::kv_row(ui, &key.to_string_lossy(), |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(value.to_string_lossy().into_owned())
                            .monospace()
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIM),
                    )
                    .truncate(),
                );
            });
        }
    });
}

/// The two `SimConnect.cfg` placements, and the write that fills them.
fn simconnect_card(
    app: &mut HangarApp,
    ui: &mut egui::Ui,
    msfs_prefix: &Path,
    companion: Option<&Path>,
) {
    ui::group(ui, Some("SimConnect.cfg"), |ui| {
        ui::controls::disclosure(ui, "simconnect-config", "Configuration", |ui| {
            ui.label(
                egui::RichText::new(env::SIMCONNECT_CFG)
                    .monospace()
                    .size(ui::theme::TEXT_SM),
            );
        });
        ui.add_space(8.0);

        for (label, prefix) in [
            Some(("MSFS", msfs_prefix)),
            companion.map(|p| ("Companion", p)),
        ]
        .into_iter()
        .flatten()
        {
            ui::heading(ui, label);
            ui.add_space(3.0);
            for path in env::simconnect_cfg_paths(prefix) {
                cfg_row(ui, &path);
            }
            ui.add_space(6.0);
        }

        let targets: Vec<PathBuf> = [Some(msfs_prefix), companion]
            .into_iter()
            .flatten()
            .flat_map(env::simconnect_cfg_paths)
            .filter(|p| !p.exists())
            .collect();

        if targets.is_empty() {
            // Outside any `ui.horizontal`, where a label would not wrap.
            ui::state(ui, "in place", ui::Tone::Ok);
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Every location already has a file. Nothing to write.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            );
            app.prefix_confirm_cfg = false;
            return;
        }

        if !app.prefix_confirm_cfg {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::FLOPPY_DISK,
                &format!(
                    "Write {} missing file{}\u{2026}",
                    targets.len(),
                    plural(targets.len())
                ),
                ui::ButtonKind::Normal,
            )
            .clicked()
            {
                app.prefix_confirm_cfg = true;
            }
            return;
        }

        // The confirm names every file, because this writes into a live prefix.
        ui.add_space(4.0);
        ui::heading(ui, "Confirm");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Write these files? Nothing that already exists is touched.")
                .size(ui::theme::TEXT_MD)
                .color(ui::theme::TEXT_HI),
        );
        ui.add_space(3.0);
        for path in &targets {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(path.display().to_string())
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::WARN),
                )
                .truncate(),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui::tool_button(
                ui,
                egui_phosphor::regular::CHECK,
                "Write them",
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                write_cfg(app, msfs_prefix, companion);
            }
            if ui::tool_button(
                ui,
                egui_phosphor::regular::X,
                "Cancel",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                app.prefix_confirm_cfg = false;
            }
        });
    });

    if let Some((message, ok)) = &app.prefix_result {
        ui.add_space(8.0);
        ui::group(ui, None, |ui| {
            ui::state(
                ui,
                if *ok { "written" } else { "failed" },
                if *ok { ui::Tone::Ok } else { ui::Tone::Err },
            );
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message.as_str())
                        .size(ui::theme::TEXT_BODY)
                        .color(ui::theme::TEXT),
                )
                .truncate(),
            );
        });
    }
}

/// One `SimConnect.cfg` location: there, absent, or there and different.
///
/// The key is `short(path)`, enough to tell the placements apart; the full
/// path is on the hover.
fn cfg_row(ui: &mut egui::Ui, path: &Path) {
    let (word, tone) = match std::fs::read_to_string(path) {
        Ok(text) if text == env::SIMCONNECT_CFG => ("Configured", ui::Tone::Ok),
        // Left alone: somebody may have tuned it.
        Ok(_) => ("present, edited", ui::Tone::Accent),
        Err(_) => ("absent", ui::Tone::Warn),
    };
    ui::kv_row(ui, &short(path), |ui| {
        ui::state(ui, word, tone).on_hover_text(path.display().to_string());
    });
}

fn write_cfg(app: &mut HangarApp, msfs_prefix: &Path, companion: Option<&Path>) {
    if !app.actions.sessions.is_live() {
        app.prefix_result = Some((
            "Advanced changes are unavailable in this session.".into(),
            false,
        ));
        return;
    }
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for prefix in [Some(msfs_prefix), companion].into_iter().flatten() {
        match env::ensure_simconnect_cfg(prefix) {
            Ok(paths) => written.extend(paths),
            Err(e) => failed.push(format!("{}: {e}", prefix.display())),
        }
    }
    app.prefix_confirm_cfg = false;
    let ok = failed.is_empty();
    let message = if ok {
        for path in &written {
            app.journal
                .done("Prefix", format!("wrote {}", path.display()));
        }
        format!("{} file{} written", written.len(), plural(written.len()))
    } else {
        for line in &failed {
            app.journal.fail("Prefix", line.clone());
        }
        failed.join("; ")
    };
    app.prefix_result = Some((message, ok));
}

/// What the prefix's own registry says is installed in it.
fn runtimes_card(ui: &mut egui::Ui, msfs_prefix: &Path, companion: Option<&Path>) {
    ui::group(ui, Some("Runtimes"), |ui| {
        for (label, prefix) in [
            Some(("MSFS", msfs_prefix)),
            companion.map(|p| ("Companion", p)),
        ]
        .into_iter()
        .flatten()
        {
            ui::heading(ui, label);
            ui.add_space(3.0);
            for arch in DOTNET_ARCHES {
                let found = dotnet_keys(arch).into_iter().find_map(|key| {
                    registry::value(prefix, Hive::LocalMachine, &key, "InstallLocation")
                        .filter(|v| !v.trim().is_empty())
                        .map(|location| (key, location))
                });
                ui::kv_row(ui, &format!(".NET {arch}"), |ui| match &found {
                    // The answering key is on the hover; it is wider than the
                    // location.
                    Some((key, location)) => {
                        ui::state(ui, "installed", ui::Tone::Ok)
                            .on_hover_text(format!("HKLM\\{key}"));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(location.as_str())
                                    .monospace()
                                    .size(ui::theme::TEXT_SM)
                                    .color(ui::theme::DIM),
                            )
                            .truncate(),
                        );
                    }
                    None => {
                        ui::state(ui, "not recorded", ui::Tone::Idle);
                    }
                });
            }
            ui.add_space(6.0);
        }
    });
}

// ── small helpers ────────────────────────────────────────────────────────────

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The last two components, which is what distinguishes these paths.
fn short(path: &Path) -> String {
    let mut parts: Vec<String> = path
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.reverse();
    parts.join("/")
}

/// A path row that says so when the path is not there.
fn path_row(ui: &mut egui::Ui, key: &str, path: &Path) {
    let exists = path.exists();
    ui::kv_row(ui, key, |ui| {
        if !exists {
            ui::state(ui, "missing", ui::Tone::Err);
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(path.display().to_string())
                    .monospace()
                    .size(ui::theme::TEXT_SM)
                    .color(if exists {
                        ui::theme::TEXT
                    } else {
                        ui::theme::ERR
                    }),
            )
            .truncate(),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The x86 runtime is reflected into Wow6432Node.
    #[test]
    fn each_arch_looks_in_its_own_registry_view_first() {
        let x64 = dotnet_keys("x64");
        assert!(x64[0].starts_with(r"Software\dotnet"), "{:?}", x64);
        assert!(x64[1].starts_with(r"Software\Wow6432Node"), "{:?}", x64);

        let x86 = dotnet_keys("x86");
        assert!(
            x86[0].starts_with(r"Software\Wow6432Node"),
            "the x86 runtime lands under Wow6432Node, and looking in Software first \
             reports it missing while it sits there installed: {x86:?}"
        );
        assert!(x86[1].starts_with(r"Software\dotnet"), "{:?}", x86);
    }

    /// Both views are tried, so a prefix that happens to carry only one shape
    /// is still answered.
    #[test]
    fn both_views_are_tried_for_each_arch() {
        for arch in DOTNET_ARCHES {
            assert_eq!(dotnet_keys(arch).len(), 2, "{arch}");
        }
    }

    #[test]
    fn a_row_label_keeps_what_distinguishes_the_path() {
        assert_eq!(
            short(Path::new(
                "/pfx/drive_c/users/steamuser/Documents/SimConnect.cfg"
            )),
            "Documents/SimConnect.cfg"
        );
        assert_eq!(
            short(Path::new("/pfx/drive_c/users/steamuser/SimConnect.cfg")),
            "steamuser/SimConnect.cfg"
        );
    }

    /// The config this tab offers to write has to be the one the runtime
    /// defines, or an app would be pointed at a port nothing is listening on.
    #[test]
    fn the_offered_config_is_the_runtimes_own() {
        assert!(env::SIMCONNECT_CFG.contains("Port=5557"));
        assert!(env::SIMCONNECT_CFG.contains("Protocol=IPv4"));
    }

    /// Two placements, and the pair is the point — one alone leaves either the
    /// official DLL or node-simconnect unable to find it.
    #[test]
    fn both_placements_are_offered() {
        let paths = env::simconnect_cfg_paths(Path::new("/pfx"));
        assert_eq!(paths.len(), 2);
        assert!(paths
            .iter()
            .any(|p| p.ends_with("Documents/SimConnect.cfg")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with("steamuser/SimConnect.cfg")));
    }

    // ── the sim's SimConnect.xml row ─────────────────────────────────────────

    /// A fake sim prefix under the temp dir. Never the live one.
    fn sim_scratch(tag: &str) -> PathBuf {
        let pfx = std::env::temp_dir().join(format!(
            "hangar-prefix-sim-xml-{tag}-{}/pfx",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(pfx.parent().unwrap());
        std::fs::create_dir_all(&pfx).unwrap();
        pfx
    }

    fn write_sim_xml(pfx: &Path, text: &str) {
        let file = sim_simconnect::path(pfx);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    const REAL_SHAPE: &str = "<SimBase.Document Type=\"SimConnect\" version=\"1,0\">\
        <SimConnect.Comm><Protocol>IPv4</Protocol><Scope>local</Scope>\
        <Address>127.0.0.1</Address><MaxClients>64</MaxClients><Port>5557</Port>\
        </SimConnect.Comm></SimBase.Document>";

    /// One of each outcome, the long ones as long as they get.
    fn every_outcome() -> Vec<sim_simconnect::Check> {
        use sim_simconnect::{Check, Comm};
        let comm = |p: &str, s: &str, a: &str, port: &str| Comm {
            protocol: Some(p.into()),
            scope: Some(s.into()),
            address: Some(a.into()),
            port: Some(port.into()),
            disabled: false,
        };
        vec![
            Check::Exposed { port: 5557 },
            Check::NoMatch {
                port: 5557,
                found: vec![],
            },
            Check::NoMatch {
                port: 5557,
                found: vec![
                    comm("IPv6", "global", "fe80::1234:5678:9abc:def0", "500"),
                    comm("Pipe", "local", ".", "Custom/SimConnect"),
                    comm("IPv4", "global", "192.168.100.200", "5558"),
                    comm("IPv4", "link", "0.0.0.0", "65535"),
                ],
            },
            Check::Missing,
            Check::Unreadable {
                why: "is not valid XML: unexpected end of stream at 1:37 in a \
                      deliberately long reason that would overflow a narrow column"
                    .into(),
            },
        ]
    }

    /// Each outcome renders inside a card on a page at four sizes without
    /// overflow, and no proportional run is longer than a sentence.
    #[test]
    fn the_sim_simconnect_row_renders_every_outcome_within_its_column() {
        fn text_runs(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => {
                    let job = &t.galley.job;
                    // Every run, whatever its family, for the content checks;
                    // proportional ones again for the prose limit.
                    out.push(format!("\u{1}{}", job.text));
                    for section in &job.sections {
                        if section.format.font_id.family == egui::FontFamily::Proportional {
                            let range = usize::from(section.byte_range.start)
                                ..usize::from(section.byte_range.end);
                            out.push(job.text[range].to_owned());
                        }
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| text_runs(s, out)),
                _ => {}
            }
        }

        for found in every_outcome() {
            for size in [
                egui::vec2(1181.0, 791.0),
                egui::vec2(1280.0, 800.0),
                egui::vec2(860.0, 600.0),
                egui::vec2(1920.0, 1080.0),
            ] {
                let ctx = egui::Context::default();
                hangar_ui::theme::install(&ctx);
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                };
                let mut overflow = 0.0;
                let out = ctx.run_ui(input, |ui| {
                    overflow = ui::page(ui, |ui| {
                        ui::group(ui, Some("Companion prefix"), |ui| {
                            sim_simconnect_row(ui, &found);
                        });
                    })
                    .overflow();
                });
                assert!(
                    overflow <= 0.0,
                    "{found:?} overflows by {overflow:.1}pt at {}x{}",
                    size.x,
                    size.y
                );
                let mut runs = Vec::new();
                for cs in &out.shapes {
                    text_runs(&cs.shape, &mut runs);
                }
                assert!(runs.iter().any(|r| r.contains("Sim SimConnect.xml")));
                assert!(runs.iter().any(|r| r.contains(found.word())), "{found:?}");
                for run in runs.iter().filter(|r| !r.starts_with('\u{1}')) {
                    assert!(run.chars().count() <= 160, "prose: {run}");
                }
            }
        }
    }

    /// The live form reads the fixture once, keeps the reading, and reads
    /// again after a set-up run finishes; nothing is created.
    #[test]
    fn the_live_form_reads_once_and_again_after_a_set_up() {
        let pfx = sim_scratch("live");
        write_sim_xml(&pfx, REAL_SHAPE);
        let mut state = SetupState::inert();
        state.reads_sim = true;

        assert_eq!(
            state.sim_simconnect(&pfx),
            Some(&sim_simconnect::Check::Exposed { port: 5557 })
        );
        // Not per frame: a changed file is not seen until the refresh.
        std::fs::remove_file(sim_simconnect::path(&pfx)).unwrap();
        assert!(state.sim_simconnect(&pfx).unwrap().is_exposed());

        // A finished set-up is the refresh: `poll_setup` clears the reading
        // (mirrored here, since that needs a whole `HangarApp`).
        let mut app_state = state;
        app_state.sim_simconnect = None;
        assert_eq!(
            app_state.sim_simconnect(&pfx),
            Some(&sim_simconnect::Check::Missing)
        );
        assert!(
            !sim_simconnect::path(&pfx).exists(),
            "a read creates nothing"
        );

        // Another prefix is another reading.
        let other = sim_scratch("live-other");
        assert_eq!(
            app_state.sim_simconnect(&other),
            Some(&sim_simconnect::Check::Missing)
        );
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(pfx.parent().unwrap());
        let _ = std::fs::remove_dir_all(other.parent().unwrap());
    }

    /// The inert form, which every render test uses, reads nothing — even
    /// with a valid file in place — so no test reaches a real sim prefix.
    #[test]
    fn the_inert_form_reads_nothing() {
        let pfx = sim_scratch("inert");
        write_sim_xml(&pfx, REAL_SHAPE);
        for mut state in [SetupState::inert(), SetupState::default()] {
            assert!(state.sim_simconnect(&pfx).is_none());
            assert!(state.sim_simconnect.is_none());
        }
        let _ = std::fs::remove_dir_all(pfx.parent().unwrap());
    }

    /// The live constructor is the one that reads.
    #[test]
    fn only_the_live_form_reads() {
        assert!(SetupState::live().reads_sim);
        assert!(!SetupState::inert().reads_sim);
    }
}
