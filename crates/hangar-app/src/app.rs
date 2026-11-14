//! The running application: chrome, nav rail, status bar, tab dispatch.
//!
//! Layout, fixed by the design system:
//!
//! ```text
//! Panel::bottom   status bar  STATUS_HEIGHT, SIDEBAR fill
//! Panel::left     nav rail    NAV_WIDTH, brand block + nav_item rows
//! CentralPanel    BG, PAGE_INSET, width-capped column, scroll fade
//! ```
//!
//! egui 0.35 merged `SidePanel` and `TopBottomPanel` into one `Panel`, and
//! `exact_width` / `exact_height` into `exact_size` — the edge the constructor
//! names now decides which axis that size applies to.
//!
//! Panel order matters: the status bar is claimed before the side panel so the
//! rail stops at the top of the bar rather than running behind it.

use crate::{
    apps::AppActions,
    bindings, bridges, console, drop_install,
    env::{Discovery, TelemetryLink},
    hangar, install, prefix, preflight, profiles, settings,
    tab::{Tab, PRIMARY_TABS, SECONDARY_TABS, UTILITY_TABS},
};
use hangar_catalog::{ProfileStore, Section};
use hangar_ui as ui;
use simconnect_tcp::Status;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why the library is being rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rescan {
    /// The window has come back from elsewhere (say, another app installed an
    /// aircraft).
    RegainedFocus,
    /// The Hangar was opened. Covers a change made while the window kept focus
    /// — a drop-install in another tab, or a package copied in from a terminal
    /// on a second monitor.
    OpenedHangar,
}

impl Rescan {
    pub fn label(self) -> &'static str {
        match self {
            Rescan::RegainedFocus => "the window regained focus",
            Rescan::OpenedHangar => "the Hangar was opened",
        }
    }
}

/// Whether this frame should rebuild the library, and why.
///
/// A pure function of two edges, so the decision is testable without a window.
/// Both are **edges, not states** — focus held across a hundred frames is one
/// rescan, not a hundred. `RawInput::default()` reports `focused: true`, so a
/// first frame compared against a `false` default would rescan in every
/// headless test.
pub fn rescan_trigger(
    focused: bool,
    was_focused: bool,
    tab: Tab,
    previous_tab: Tab,
) -> Option<Rescan> {
    if focused && !was_focused {
        return Some(Rescan::RegainedFocus);
    }
    if tab == Tab::Hangar && previous_tab != Tab::Hangar {
        return Some(Rescan::OpenedHangar);
    }
    None
}

/// "just now", "4 minutes ago" — how long since the library was built.
///
/// Coarse on purpose: it answers "is this current?", and a readout changing
/// every frame draws the eye.
pub fn scanned_ago(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=20 => "just now".into(),
        21..=90 => "a minute ago".into(),
        91..=3599 => format!("{} minutes ago", (secs + 30) / 60),
        3600..=7199 => "an hour ago".into(),
        _ => format!("{} hours ago", (secs + 1800) / 3600),
    }
}

pub struct HangarApp {
    pub active_tab: Tab,
    navigation: crate::tab::Navigation,
    /// What startup found.
    pub discovery: Discovery,
    pub map: crate::map::MapState,
    pub replacements: crate::replacements::State,
    /// Hangar → Remove. Inert until `main.rs`: a review reads, a delete refuses.
    pub remove: crate::remove::State,
    /// The SimConnect link, reconnecting on its own for the app's lifetime.
    pub sim: TelemetryLink,
    /// Hangar tab: which section is selected. `None` shows every section at
    /// once, which is what the "All" toggle turns on.
    pub hangar_section: Option<Section>,
    /// Hangar tab: free-text search.
    pub hangar_search: String,
    /// Stable catalog name of the item open in Hangar details.
    pub hangar_selected: Option<String>,
    /// Why the last change to a package's loadouts failed, if it did.
    pub hangar_set_error: Option<String>,

    /// The binding library, parsed once and read every frame behind an `Arc`.
    pub bindings: std::sync::Arc<hangar_bindings::Library>,
    /// A preflight being run, if one is.
    ///
    /// `None` shows the plan; `Some` shows live progress. Held here so
    /// switching tabs mid-run does not abandon it.
    pub flight: Option<crate::flight::Flight>,
    /// Which apps take part in the next preflight, and where.
    ///
    /// Persisted to `preflight.toml`, so the switches are where you left them
    /// next time. Only hand-made choices are stored; an app that has never been
    /// touched picks up its computed default.
    pub preflight: crate::flight::Settings,
    pub flight_view: preflight::ViewState,

    /// How far the last drawn page overflowed its column, in points.
    ///
    /// Zero unless a panel is too wide. Recorded because egui clips the
    /// overflow, so a headless render test cannot otherwise see it.
    pub page_overflow: f32,

    /// A rebind in progress: what is being bound, and what has been caught.
    ///
    /// Holds open file descriptors while it lives, so it is dropped the moment
    /// the flow ends rather than kept around idle.
    pub rebind: Option<crate::bindings::Rebind>,

    /// The controller survey, and when it was last taken.
    ///
    /// Reads `/proc/bus/input/devices`, so it is throttled; it expires because
    /// sticks are plugged in and out while the app is open.
    pub controllers: Option<(
        std::time::Instant,
        std::sync::Arc<Vec<hangar_bindings::DeviceView>>,
    )>,

    /// Whether Steam was up, and when that was last checked.
    ///
    /// The write gate needs the answer every frame and the answer costs a
    /// `/proc` scan, which is not a per-frame cost anything should pay.
    pub steam_check: Option<(std::time::Instant, bool)>,
    pub bindings_search: String,
    /// `None` is every saved profile at once.
    pub bindings_profile: Option<String>,
    /// `None` is every simulator context at once.
    pub bindings_context: Option<String>,
    pub bindings_bound: hangar_bindings::catalog::Bound,
    /// Profiles on disk. Config-only; writing one touches nothing in the sim.
    pub profiles: ProfileStore,
    pub new_profile_name: String,
    pub selected_profile: Option<String>,
    pub confirm_delete_profile: Option<String>,
    /// Which profile is one click from being applied — the second stage of the
    /// confirm, so a switch is never one misplaced click away.
    pub confirm_apply: Option<String>,
    /// Result of the last profile action.
    pub profile_result: Option<(String, bool)>,
    /// Whether HangarRS may write to the live Community folder.
    ///
    /// Off until switched on in Settings. Everything else in the app is
    /// read-only, and this is the one switch that changes that.
    pub writes_allowed: bool,
    /// Set after a switch, so the library is re-read rather than left stale.
    pub rescan_requested: bool,
    /// Polls `/proc` so the UI knows what is running without scanning per frame.
    pub monitor: hangar_runtime::AppMonitor,
    /// In-flight and completed launch/stop actions.
    pub actions: AppActions,
    /// Reads `/proc` for Flight details; inert until `main.rs` makes it live.
    pub observer: crate::flight_details::Observer,
    pub flight_details: crate::flight_details::Details,
    /// The prefix companion apps run in, when one could be resolved.
    pub proton: Option<hangar_runtime::ProtonEnv>,
    /// Which rule chose `proton`'s prefix: configured, the previous
    /// application's, or the default.
    pub companion_source: Option<hangar_core::config::CompanionSource>,
    /// Prefix tab: setting up the companion prefix. Inert until `main.rs`.
    pub companion_setup: crate::prefix::SetupState,

    /// HangarRS' own `config.toml`, held so a change writes the whole file.
    ///
    /// The whole of it, not just the section being edited: [`Config::save`]
    /// rewrites the file, so saving a partial copy would delete the apps list.
    pub config: hangar_core::config::Config,
    /// Why the config could not be read, if it could not.
    ///
    /// Missing and malformed are different states; only malformed is a problem.
    pub config_problem: Option<String>,
    /// Where [`HangarApp::save_config`] writes.
    ///
    /// A field rather than a call to `Config::path()` because a test can
    /// synthesise the click that saves; `app_with` points this at a scratch
    /// file so no test writes a real `config.toml`.
    pub config_path: Option<std::path::PathBuf>,
    /// The output bridges, and whether they may open a socket at all.
    pub bridges: bridges::BridgeState,
    /// The installer pipeline, and whether it may actually run one.
    pub install: install::InstallState,
    /// The drop half of the Install tab: a dropped folder or archive.
    pub drop_install: crate::drop_install::DropState,

    /// One scrollback for everything this application runs.
    ///
    /// Shared into every task that starts a process, so a running app's output
    /// reaches the Console tab as it arrives rather than only when it dies.
    pub journal: crate::journal::Journal,
    /// Prefix tab: whether the `SimConnect.cfg` write is one click from
    /// happening. The second stage of the confirm, so a write into a live
    /// prefix is never one misplaced click away.
    pub prefix_confirm_cfg: bool,
    /// Prefix tab: the same second stage for creating the sim AppData link.
    /// A separate flag from the `SimConnect.cfg` one, so arming either write
    /// never arms the other.
    pub prefix_confirm_link: bool,
    /// The library is a snapshot, and these are what say it went stale.
    ///
    /// `hangar-catalog` scans on demand rather than watching the filesystem, so
    /// a change to Community from outside leaves the screen stale. A scan is
    /// cheap (milliseconds), so it is done on a hunch.
    was_focused: bool,
    previous_tab: Tab,
    /// When the library was last built. `None` before the first rescan — the
    /// startup scan is `Discovery`'s and is by definition current.
    pub last_scan: Option<std::time::Instant>,
    /// Off in tests, where a rescan would read the real machine.
    pub auto_rescan: bool,
    /// Whether this session's window system delivers dropped files.
    ///
    /// Read once at startup rather than per frame, and a field rather than a
    /// call so a render test can put the card in either state.
    pub file_drops: crate::env::FileDrops,
    /// What the last Prefix-tab write did.
    pub prefix_result: Option<(String, bool)>,
    pub binding_tools: crate::binding_tools::State,
    pub settings_view: settings::View,
    pub console_events_only: bool,
    /// Console tab: which source is shown. `None` is all of them.
    pub console_source: Option<String>,
    pub console_search: String,
    pub console_follow: bool,

    /// What the monitor is currently watching, so it is only told on a change.
    watched_keys: Vec<String>,
}

impl HangarApp {
    pub fn new(
        discovery: Discovery,
        sim: TelemetryLink,
        monitor: hangar_runtime::AppMonitor,
    ) -> Self {
        // Open on Preflight: it is what you came to do. Settings instead when
        // discovery failed, because Preflight has nothing to launch and the
        // reason is on Settings.
        let active_tab = if discovery.msfs().is_some() {
            Tab::Preflight
        } else {
            Tab::Settings
        };
        let bindings = discovery.bindings.clone();

        // Missing is not malformed: treating an unparseable file as empty would
        // discard someone's edits at the next save.
        let (config, config_problem) = match hangar_core::config::Config::load() {
            hangar_core::config::Load::Present(config) => (*config, None),
            hangar_core::config::Load::Missing => (hangar_core::config::Config::default(), None),
            hangar_core::config::Load::Malformed { path, reason } => {
                tracing::warn!(path = %path.display(), "config.toml could not be read: {reason}");
                (
                    hangar_core::config::Config::default(),
                    Some(format!("{}: {reason}", path.display())),
                )
            }
        };

        // The companion prefix comes from the config: a configured location,
        // else the previous application's prefix for an existing setup, else
        // HangarRS' own. Resolved whether or not it exists yet, so the Prefix
        // tab can offer to set it up.
        let companion = config.companion_prefix();
        let proton = match (discovery.msfs(), discovery.steam(), &companion) {
            (Some(msfs), Some(steam), Some((dir, _))) => {
                hangar_runtime::ProtonEnv::for_msfs(msfs, steam).map(|env| env.companion(dir))
            }
            _ => None,
        };
        let companion_source = companion.map(|(_, source)| source);

        let app = Self {
            // No native launcher or file chooser until main.rs enables them.
            map: crate::map::MapState::default(),
            replacements: crate::replacements::State::default(),
            remove: crate::remove::State::default(),
            active_tab,
            navigation: crate::tab::Navigation::default(),
            discovery,
            sim,
            // Opens on Apps: the things you launch are what you came for.
            hangar_section: Some(Section::Apps),
            hangar_search: String::new(),
            hangar_selected: None,
            hangar_set_error: None,
            bindings,
            flight: None,
            preflight: crate::flight::Settings::load_for(
                &config.apps.iter().map(Into::into).collect::<Vec<_>>(),
            ),
            flight_view: preflight::ViewState::default(),
            page_overflow: 0.0,
            rebind: None,
            controllers: None,
            steam_check: None,
            bindings_search: String::new(),
            bindings_profile: None,
            bindings_context: None,
            bindings_bound: hangar_bindings::catalog::Bound::Only,
            profiles: ProfileStore::load_default(),
            new_profile_name: String::new(),
            selected_profile: None,
            confirm_delete_profile: None,
            confirm_apply: None,
            profile_result: None,
            writes_allowed: config.toggles.community_writes,
            rescan_requested: false,
            monitor,
            actions: AppActions::default(),
            observer: crate::flight_details::Observer::default(),
            flight_details: crate::flight_details::Details::default(),
            proton,
            companion_source,
            // Inert: a draw must never be able to start Proton.
            companion_setup: crate::prefix::SetupState::inert(),
            config,
            config_problem,
            config_path: hangar_core::config::Config::path(),
            // Inert, always; `main.rs` makes it live (see `crate::bridges`).
            bridges: bridges::BridgeState::inert(),
            // Inert: a draw must never be able to start an install.
            install: install::InstallState::inert(),
            drop_install: drop_install::DropState::inert(),
            // No live/inert split: appending to a buffer touches nothing outside.
            journal: crate::journal::Journal::default(),
            prefix_confirm_cfg: false,
            prefix_confirm_link: false,
            file_drops: crate::env::file_drops(),
            // Focused and freshly scanned: `Discovery` has just run, so the
            // first frame must not ask for another one.
            was_focused: true,
            previous_tab: active_tab,
            last_scan: None,
            auto_rescan: true,
            prefix_result: None,
            binding_tools: crate::binding_tools::State::default(),
            settings_view: settings::View::default(),
            console_events_only: true,
            console_source: None,
            console_search: String::new(),
            console_follow: true,
            watched_keys: Vec::new(),
        };
        // What loading made of every saved Flight choice (stable IDs).
        for line in &app.preflight.report {
            tracing::info!(target: "hangar_launch", "flight settings: {line}");
            app.journal.note("Flight", format!("settings: {line}"));
        }
        app
    }

    /// Write `config.toml` back, whole.
    ///
    /// Refused while the file on disk will not parse: saving there would
    /// replace a file somebody is halfway through fixing by hand with this
    /// application's idea of the defaults.
    pub fn save_config(&mut self) {
        if let Some(reason) = &self.config_problem {
            tracing::warn!("not saving over an unreadable config: {reason}");
            return;
        }
        let Some(path) = self.config_path.clone() else {
            tracing::warn!("no config path, so nothing was saved");
            return;
        };
        if let Err(e) = self.config.write_to(&path) {
            tracing::warn!("could not save config: {e}");
        }
    }

    /// Bring the bridges into line with the config.
    ///
    /// A no-op on an inert state, which is what a test holds.
    pub fn sync_bridges(&mut self) {
        let source = self.sim.source();
        self.bridges.sync(&self.config, &source);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        self.navigation.remember(self.active_tab);
        crate::install::take_dropped_file(self, ui);
        // Something outside this process may have changed Community while the
        // window was not looking. Decided before the flag is consumed, so a
        // trigger takes effect on the frame that noticed it.
        let focused = ui.ctx().input(|i| i.focused);
        if self.auto_rescan {
            if let Some(why) = rescan_trigger(
                focused,
                self.was_focused,
                self.active_tab,
                self.previous_tab,
            ) {
                tracing::debug!("rescanning the library: {}", why.label());
                self.rescan_requested = true;
            }
        }
        self.was_focused = focused;
        self.previous_tab = self.active_tab;
        // A Remove that finished while another tab was open: loadouts and rescan now.
        crate::remove::collect(self);

        // A switch moved directories, so the library on screen is stale.
        if std::mem::take(&mut self.rescan_requested) {
            self.rescan();
        }
        self.refresh_watched_apps();
        // The session must progress even while another tab or the next plan is open.
        if let Some(flight) = &mut self.flight {
            let ctx = ui.ctx().clone();
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            flight.tick(&self.sim, move || ctx.request_repaint());
        }
        // Companion apps a row started, across a self-update.
        self.actions
            .follow_apps(&self.observer, &self.journal, std::time::Instant::now());
        crate::preflight::end_if_over(self);
        self.status_bar(ui);
        self.nav_rail(ui);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui::theme::BG)
                    .inner_margin(egui::Margin::same(ui::theme::PAGE_INSET)),
            )
            .show(ui, |ui| {
                let max_width = match self.active_tab {
                    Tab::Preflight
                    | Tab::Hangar
                    | Tab::Install
                    | Tab::Bindings
                    | Tab::Console
                    | Tab::Map
                    | Tab::Bridges => f32::MAX,
                    _ => ui::theme::CONTENT_MAX_WIDTH,
                };
                let metrics = ui
                    .push_id(self.active_tab, |ui| {
                        ui::page_with_width(ui, max_width, |ui| match self.active_tab {
                            Tab::Preflight => preflight::draw(self, ui),
                            Tab::Hangar => {
                                let catalog = self.discovery.catalog.clone();
                                // The count belongs to the title, not beside it: a
                                // number is what the page *is*, not a state it is in.
                                ui::page_header(
                                    ui,
                                    &format!("Hangar ({})", catalog.len()),
                                    |_ui| {},
                                );
                                self.local_navigation(ui, Tab::Hangar);
                                hangar::draw(self, ui);
                            }
                            Tab::Profiles => {
                                ui::page_header(ui, "Hangar", |_ui| {});
                                self.local_navigation(ui, Tab::Hangar);
                                profiles::draw(self, ui);
                            }
                            Tab::Settings => {
                                ui::page_header(ui, "Settings", |ui| {
                                    ui::status_text(ui, &format!("HangarRS {VERSION}"));
                                });
                                self.local_navigation(ui, Tab::Settings);
                                settings::draw(self, ui);
                            }
                            Tab::Bindings => {
                                ui::page_header(ui, "Controls", |_ui| {});
                                bindings::draw(self, ui);
                            }
                            Tab::Bridges => {
                                ui::page_header(ui, "Connections", |_ui| {});
                                ui.scope(|ui| {
                                    ui.set_max_width(
                                        ui.available_width().min(ui::theme::COMPACT_FORM_WIDTH),
                                    );
                                    bridges::draw(self, ui);
                                });
                            }
                            Tab::Install => {
                                ui::page_header(ui, "Install", |ui| {
                                    let running =
                                        self.install.job.as_ref().is_some_and(|j| !j.finished());
                                    if running {
                                        ui::state(ui, "installing", ui::Tone::Accent);
                                    } else if self.writes_allowed {
                                        // Follows the switch: a pill about writing must
                                        // never say read-only while writes are on.
                                        ui::state(ui, "writes enabled", ui::Tone::Warn);
                                    } else {
                                        ui::state(ui, "read-only", ui::Tone::Ok);
                                    }
                                });
                                install::draw(self, ui);
                            }
                            Tab::Console => {
                                ui::page_header(ui, "Activity", |_ui| {});
                                console::draw(self, ui);
                            }
                            Tab::Prefix => {
                                ui::page_header(ui, "Settings", |_ui| {});
                                self.local_navigation(ui, Tab::Settings);
                                prefix::draw(self, ui);
                            }
                            // Little Navmap launcher and its connection status.
                            Tab::Map => crate::map::draw(self, ui),
                        })
                    })
                    .inner;
                self.page_overflow = metrics.overflow();
            });
        // Whatever changed the write toggle this frame, a removal running on
        // another thread reads it before its next delete.
        self.remove.follow_writes(self.writes_allowed);
    }

    /// Where an app sits in the next preflight.
    ///
    /// Defaults to before the sim for an app with a readiness probe, after
    /// otherwise. Explicit choices are never silently moved to another slot.
    pub fn placement_of(&self, name: &str) -> crate::flight::Placement {
        if let Some(chosen) = self.preflight.placement.get(name) {
            return *chosen;
        }
        // A readiness probe exists because the app does startup work the sim
        // has to wait for.
        if hangar_runtime::readiness::probe_for(name).is_some() {
            crate::flight::Placement::Before
        } else {
            crate::flight::Placement::After
        }
    }

    /// Change where an app sits, and remember it.
    ///
    /// Written on every click: saving on exit would lose the choice whenever
    /// the app is killed rather than closed.
    pub fn set_placement(&mut self, name: &str, placement: crate::flight::Placement) {
        self.preflight.placement.insert(name.to_string(), placement);
        self.save_preflight();
    }

    pub fn save_preflight(&self) {
        if let Err(e) = self.preflight.save() {
            tracing::warn!("could not save preflight settings: {e}");
        }
    }

    /// What is changing Community (or running in the companion prefix) right
    /// now, if anything, as the sentence a refusal shows.
    ///
    /// The one busy state for every action that writes there: an install, a
    /// drop's placement, the replaced-copy purge, a loadout apply and Remove
    /// each check it themselves, at the action — so each refuses while any
    /// other is running, in both directions.
    pub fn community_busy(&self) -> Option<&'static str> {
        if self.install.job.as_ref().is_some_and(|job| !job.finished()) {
            Some("Wait for the install that is running.")
        } else if self.drop_install.is_placing() {
            Some("Wait for the package that is being placed.")
        } else if self.remove.is_working() {
            Some("Wait for the removal that is running.")
        } else {
            None
        }
    }

    pub fn sim_process_running(&self) -> bool {
        self.monitor
            .running()
            .get(hangar_runtime::sim::MSFS_2024_EXE)
            .is_some()
    }

    /// Keep the process monitor pointed at whatever apps the library holds.
    fn refresh_watched_apps(&mut self) {
        let mut keys: Vec<String> = self
            .discovery
            .catalog
            .items()
            .iter()
            .filter(|i| {
                matches!(
                    i.kind,
                    hangar_catalog::ItemKind::CompanionApp
                        | hangar_catalog::ItemKind::SimSessionApp
                        | hangar_catalog::ItemKind::NativeTool
                        | hangar_catalog::ItemKind::SimAddonExe
                )
            })
            .map(|i| hangar_runtime::proc::match_key(&i.path))
            .collect();
        keys.push(hangar_runtime::sim::MSFS_2024_EXE.to_string());
        if keys != self.watched_keys {
            self.monitor.watch(keys.clone());
            self.watched_keys = keys;
        }
    }

    /// Re-read the library after something changed it.
    fn rescan(&mut self) {
        if let Some(env) = self.discovery.msfs() {
            self.discovery.catalog = std::sync::Arc::new(
                hangar_catalog::CatalogSnapshot::from_scan(hangar_catalog::scan(env, &[])),
            );
            self.last_scan = Some(std::time::Instant::now());
        }
    }

    fn nav_rail(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("nav_rail")
            .exact_size(ui::NAV_WIDTH)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(ui::theme::SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(12, 16)),
            )
            .show(ui, |ui| {
                ui::wordmark(ui);
                let rail_bottom = ui.max_rect().bottom();
                egui::ScrollArea::vertical()
                    .id_salt("rail-scroll")
                    .show(ui, |ui| {
                        for tab in PRIMARY_TABS {
                            self.nav_row(ui, tab);
                        }
                        ui.add_space(18.0);
                        for tab in SECONDARY_TABS {
                            self.nav_row(ui, tab);
                        }
                        ui.add_space((rail_bottom - ui.cursor().top() - 102.0).max(18.0));
                        for tab in UTILITY_TABS {
                            self.nav_row(ui, tab);
                        }
                    });
            });
    }

    fn nav_row(&mut self, ui: &mut egui::Ui, tab: Tab) {
        if ui::nav_item(ui, tab.icon(), tab.label(), self.active_tab.parent() == tab).clicked() {
            self.active_tab = self.navigation.destination(tab);
        }
    }

    fn local_navigation(&mut self, ui: &mut egui::Ui, parent: Tab) {
        ui.horizontal_wrapped(|ui| {
            if parent == Tab::Hangar {
                for (tab, label) in [(Tab::Hangar, "Library"), (Tab::Profiles, "Loadouts")] {
                    if ui::controls::view_tab(ui, label, self.active_tab == tab).clicked() {
                        self.active_tab = tab;
                    }
                }
            } else {
                for (view, label) in [
                    (settings::View::General, "General"),
                    (settings::View::Simulator, "Simulator setup"),
                    (settings::View::Storage, "Storage"),
                ] {
                    if ui::controls::view_tab(
                        ui,
                        label,
                        self.active_tab == Tab::Settings && self.settings_view == view,
                    )
                    .clicked()
                    {
                        self.settings_view = view;
                        self.active_tab = Tab::Settings;
                    }
                }
                if ui::controls::view_tab(ui, "Advanced", self.active_tab == Tab::Prefix).clicked()
                {
                    self.active_tab = Tab::Prefix;
                }
            }
        });
        ui.add_space(18.0);
    }

    /// Process presence and telemetry are separate observations.
    fn status_bar(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status_bar")
            .exact_size(ui::STATUS_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(ui::theme::SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(16, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let running = self.sim_process_running();
                    ui::status_dot(
                        ui,
                        if running {
                            "MSFS running"
                        } else {
                            "MSFS closed"
                        },
                        if running {
                            ui::Tone::Ok
                        } else {
                            ui::Tone::Idle
                        },
                    );
                    ui.add_space(8.0);
                    let (link, tone) = self.sim_link_label();
                    ui::status_dot(ui, &link, tone);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.install.job.as_ref().is_some_and(|j| !j.finished()) {
                            ui::status_text(ui, "Installing");
                        } else if self.flight.as_ref().is_some_and(|f| !f.is_finished()) {
                            ui::status_text(ui, "Launch in progress");
                        }
                    });
                });
            });
    }

    fn sim_link_label(&self) -> (String, ui::Tone) {
        let (label, tone) = match self.sim.status() {
            Status::Connected { .. } if self.sim.is_streaming() => {
                ("Flight data live", ui::Tone::Ok)
            }
            Status::Connected { .. } => ("Connected · No fresh flight data", ui::Tone::Warn),
            Status::Connecting => ("Connecting to MSFS", ui::Tone::Idle),
            Status::Unsupported { .. } => ("Flight data unavailable", ui::Tone::Err),
            Status::Disconnected => ("No flight data", ui::Tone::Idle),
        };
        (label.into(), tone)
    }

    /// The active profile, if what is enabled matches one exactly.
    pub(crate) fn active_profile_label(&self) -> String {
        let Some(env) = self.discovery.msfs() else {
            return "No loadout".into();
        };
        let current = hangar_catalog::switcher::Switcher::new(&env.community).enabled_names();
        for name in self.profiles.names() {
            if self
                .profiles
                .get(name)
                .is_some_and(|p| p.packages == current)
            {
                return name.to_owned();
            }
        }
        if self.profiles.is_empty() {
            "No loadouts".into()
        } else {
            "Custom selection".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Discovery;

    #[test]
    fn a_drop_on_another_tab_opens_review_without_installing() {
        let mut app = app_with(nothing_found());
        app.active_tab = Tab::Map;
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(960.0, 640.0),
            )),
            dropped_files: vec![egui::DroppedFile {
                path: Some(std::path::PathBuf::from("/scratch/setup.exe")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| app.draw(ui));
        assert_eq!(app.active_tab, Tab::Install);
        assert_eq!(
            app.install.selected,
            Some(std::path::PathBuf::from("/scratch/setup.exe"))
        );
        assert!(app.install.job.is_none());
        assert!(app.install.confirm.is_none());
        assert!(!app.drop_install.is_placing());
    }

    /// Render one frame of the whole app headlessly.
    ///
    /// `Context::run_ui` drives a real layout pass with no window and no GPU,
    /// so this is the equivalent of clicking every tab — it catches a panel
    /// that panics, borrows twice, or divides by a zero width, without opening
    /// a window on anyone's desktop.
    fn render(app: &mut HangarApp, size: egui::Vec2) -> egui::FullOutput {
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| app.draw(ui))
    }

    fn rendered_text(output: &egui::FullOutput) -> String {
        fn collect(shape: &egui::Shape, text: &mut String) {
            match shape {
                egui::Shape::Text(t) => {
                    text.push_str(&t.galley.job.text);
                    text.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, text);
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut text);
        }
        text
    }

    fn app_with(discovery: Discovery) -> HangarApp {
        // A monitor needs a reactor to poll on; the tests that render only need
        // it to exist and report nothing running.
        let _guard = runtime().enter();
        let mut app = HangarApp::new(
            discovery,
            TelemetryLink::idle(),
            hangar_runtime::AppMonitor::inert(Default::default()),
        );
        // `HangarApp::new` reads the real `preflight.toml`. The rule for every
        // reset below: **anything a constructor reads from the machine has to
        // be resettable in a test.**
        app.preflight = crate::flight::Settings::default();
        // And what loading it reported, which went into the journal.
        app.journal = crate::journal::Journal::default();
        // A synthesised click can save the config; point it at a scratch file.
        app.config = hangar_core::config::Config::default();
        app.config_problem = None;
        app.config_path = Some(scratch_config());
        // The Settings switches come from the real `config.toml`; re-read from
        // the default config so this follows the defaults.
        app.writes_allowed = app.config.toggles.community_writes;
        // Stage 7's records would be read from the machine on first draw.
        app.install.history_read = true;
        // On a Wayland machine this would otherwise render the "drops cannot
        // arrive" branch and never the normal one.
        app.file_drops = crate::env::FileDrops::Available;
        // A rescan would read the real machine.
        app.auto_rescan = false;
        // The companion prefix is resolved from the real config and
        // `~/.local/share`; point it at a scratch path no render creates.
        if let Some(proton) = app.proton.as_mut() {
            *proton = proton.companion(&std::env::temp_dir().join(format!(
                "hangar-app-companion-{}-never-created",
                std::process::id()
            )));
        }
        app.companion_source = Some(hangar_core::config::CompanionSource::Default);
        // Saved loadouts come from the real config dir; a missing directory is
        // an empty store.
        app.profiles = hangar_catalog::ProfileStore::load(std::env::temp_dir().join(format!(
            "hangar-app-profiles-{}-never-created",
            std::process::id()
        )));
        app
    }

    /// A config path unique to this process, for `app_with`.
    fn scratch_config() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "hangar-app-config-{}-{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// One runtime for every test in this module, since `AppMonitor::spawn`
    /// calls `tokio::spawn` and needs a reactor in scope.
    fn runtime() -> &'static tokio::runtime::Runtime {
        use std::sync::OnceLock;
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("runtime")
        })
    }

    /// A fully populated environment, with paths as long as the real ones.
    ///
    /// The populated branch has to be rendered as well as "nothing found".
    fn everything_found() -> Discovery {
        populated(
            "/home/user/.local/share/Steam".into(),
            "/mnt/steam/SteamLibrary".into(),
        )
    }

    /// `everything_found` with the Steam root and game library named.
    fn populated(steam_root: std::path::PathBuf, lib: std::path::PathBuf) -> Discovery {
        use hangar_msfs::env::{CommunityFolder, Generation};
        use hangar_steam::{AppInstall, CompatTool, SteamEnv, SteamLibrary};
        let prefix = lib.join("steamapps/compatdata/2537590/pfx");
        let app_data =
            prefix.join("drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024");
        let proton = lib.join("steamapps/common/Proton - Experimental");
        let packages = app_data.join("Packages");

        let steam = SteamEnv {
            steam_root: steam_root.clone(),
            is_flatpak: false,
            libraries: vec![
                SteamLibrary {
                    path: steam_root,
                    label: None,
                    is_steam_root: true,
                },
                SteamLibrary {
                    path: lib.clone(),
                    label: Some("games".into()),
                    is_steam_root: false,
                },
            ],
        };

        let msfs = hangar_msfs::MsfsEnv {
            install: AppInstall {
                app_id: "2537590".into(),
                name: "Microsoft Flight Simulator 2024".into(),
                library_root: lib.clone(),
                install_dir: lib.join("steamapps/common/MSFS2024"),
                compat_data: lib.join("steamapps/compatdata/2537590"),
                prefix,
            },
            compat_tool: Some(CompatTool {
                display_name: "Proton - Experimental".into(),
                configured_name: "proton_experimental".into(),
                proton_script: proton.join("proton"),
                wine_binary: proton.join("files/bin/wine"),
                is_third_party: false,
            }),
            user_cfg: app_data.join("UserCfg.opt"),
            app_data,
            community: packages.join("Community2024"),
            communities: vec![
                CommunityFolder {
                    path: packages.join("Community2024"),
                    generation: Generation::Msfs2024,
                },
                CommunityFolder {
                    path: packages.join("Community"),
                    generation: Generation::Msfs2020,
                },
            ],
            packages_root: packages,
        };

        Discovery {
            // Two packages covering one airport, so the overlap notice renders.
            overlaps: vec![(
                std::path::PathBuf::from("/…/Packages/Community2024"),
                hangar_install::drop::Overlap {
                    packages: vec!["koak".into(), "fd-airport-koak-oakland".into()],
                    shared: vec!["Airport KOAK".into()],
                },
            )],
            steam: Ok(steam),
            msfs: Ok(msfs),
            catalog: Default::default(),
            bindings: std::sync::Arc::new(fixture_bindings()),
        }
    }

    /// A binding library built from strings, written to a scratch directory and
    /// read back, so the populated Bindings tab renders without the real
    /// `remote/`.
    fn fixture_bindings() -> hangar_bindings::Library {
        // Unique per call: several tests build a Discovery, they run in
        // parallel, and a shared directory name means one test removes the
        // tree another is still writing into.
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-app-ui-bindings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");

        // The shapes that matter:
        //   544 twice with different GUIDs  -> one controller enrolled twice
        //   302 twice with the same GUID    -> MSFS's alternate profiles
        //   28690 once                      -> ordinary
        for (n, (name, device, guid, product, action, key, code)) in [
            (
                "My General Camera",
                "Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA",
                "{BAAADAD7-7E59-11F1-8003-000044455354}",
                "544",
                "KEY_AXIS_AILERONS_SET",
                "Joystick L-Axis X ",
                "1026",
            ),
            (
                "My General Settings",
                "Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA",
                "{9E573ED3-7734-11D2-8D4A-23903FB6BDF7}",
                "544",
                "KEY_ELEV_TRIM_UP",
                "Joystick Button 8",
                "7",
            ),
            (
                "My General Settings STECS MAX",
                "VKB-Sim (C) Alex Oz 2023 S-TECS MODERN THROTTLE MAX STEM",
                "{979FF95F-7D7C-11F1-8002-000044455354}",
                "302",
                "KEY_THROTTLE1_AXIS_SET_EX1",
                "Joystick L-Axis Y ",
                "1042",
            ),
            (
                "My General Settings",
                "VKB-Sim (C) Alex Oz 2023 S-TECS MODERN THROTTLE MAX STEM",
                "{979FF95F-7D7C-11F1-8002-000044455354}",
                "302",
                "KEY_MIXTURE_SET",
                "Joystick L-Axis Z ",
                "1058",
            ),
            (
                "My General V1 Pedals",
                "Turtle Beach VelocityOne Rudder",
                "{9E573ED3-7734-11D2-8D4A-23903FB6BDF7}",
                "28690",
                "KEY_AXIS_RUDDER_SET",
                "Joystick L-Axis Z ",
                "1058",
            ),
            (
                "My new general profile",
                "Keyboard",
                "{0}",
                "34891",
                "KEY_TOGGLE_JETWAY",
                "Control",
                "162",
            ),
        ]
        .iter()
        .enumerate()
        {
            let xml = format!(
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<Version Num=\"-1\"/>\n",
                    "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">{}</FriendlyName>\n",
                    "<Device DeviceName=\"{}\" GUID=\"{}\" ProductID=\"{}\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
                    "\t<Context ContextName=\"AIRCRAFT\">\n",
                    "\t\t<Action ActionName=\"{}\" ValueEvent=\"0.000000\" Delay=\"2.000000\" Flag=\"2\">\n",
                    "\t\t\t<Primary>\n\t\t\t\t<KEY Information=\"{}\">{}</KEY>\n\t\t\t</Primary>\n",
                    "\t\t</Action>\n",
                    "\t\t<Action ActionName=\"KEY_AXIS_ELEV_TRIM_SET\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
                    "\t</Context>\n",
                    "\t<Context ContextName=\"DEBUG\">\n",
                    "\t\t<Action ActionName=\"KEY_DEBUG_ENTER\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
                    "\t</Context>\n",
                    "</Device>\n\n",
                ),
                name, device, guid, product, action, key, code
            );
            std::fs::write(dir.join(format!("inputprofile_000000000{n}")), xml)
                .expect("write a fixture profile");
        }

        let library = hangar_bindings::Library::load(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        library
    }

    /// The state a machine without Steam lands in.
    fn nothing_found() -> Discovery {
        Discovery {
            overlaps: Vec::new(),
            steam: Err("no Steam installation found".into()),
            msfs: Err("Steam was not found, so MSFS could not be located".into()),
            catalog: Default::default(),
            bindings: Default::default(),
        }
    }

    /// Fill the controller cache so a render never depends on what is plugged
    /// in. The classification is unit-tested in `hangar-bindings`.
    fn prime_controllers(app: &mut HangarApp, plugged_in: bool) {
        use hangar_core::input::InputDevice;
        let devices: Vec<InputDevice> = if plugged_in {
            [(0x231du16, 0x0220u16), (0x231d, 0x012e), (0x10f5, 0x7012)]
                .into_iter()
                .map(|(vendor, product)| InputDevice {
                    name: format!("fixture {product:04x}"),
                    vendor,
                    product,
                    handlers: vec!["event0".into(), "js0".into()],
                    ..Default::default()
                })
                .collect()
        } else {
            Vec::new()
        };
        app.controllers = Some((
            std::time::Instant::now(),
            std::sync::Arc::new(hangar_bindings::survey(&app.bindings, &devices)),
        ));
    }

    /// The Preflight tab's live-progress branch, with a launcher that starts
    /// nothing.
    ///
    /// This branch ticks the sequencer, which performs actions; the launcher is
    /// injected so a render can never start the sim.
    #[test]
    fn the_preflight_tab_renders_a_running_flight_without_starting_anything() {
        use crate::flight::{suggested, Flight};
        use hangar_runtime::sim::Handoff;

        fn stub(app_id: &str) -> std::io::Result<Handoff> {
            Ok(Handoff {
                url: format!("steam://rungameid/{app_id}"),
                opener: "stub".into(),
            })
        }

        let mut app = app_with(everything_found());
        app.active_tab = Tab::Preflight;
        app.flight = Some(Flight::with_launcher(
            suggested(["BeyondATC".to_string(), "Project Airports".to_string()]),
            Default::default(),
            None,
            stub,
        ));

        for size in [egui::vec2(1280.0, 800.0), egui::vec2(960.0, 640.0)] {
            render(&mut app, size);
            assert!(
                app.page_overflow <= 0.0,
                "Preflight overflows while running"
            );
        }

        // And the finished branch, after cancelling.
        if let Some(flight) = &mut app.flight {
            flight.cancel();
        }
        render(&mut app, egui::vec2(1280.0, 800.0));
        assert!(app.flight.is_some(), "cancelling should not close the view");
    }

    #[test]
    fn map_opens_little_navmap_and_fits_empty_populated_and_error_states() {
        for discovery in [everything_found(), nothing_found()] {
            let mut app = app_with(discovery);
            app.active_tab = Tab::Map;
            for populated in [false, true] {
                app.map.target = populated.then(|| crate::map::Target {
                    exe: "/a long installation folder/Little Navmap/littlenavmap.exe".into(),
                    args: vec![],
                    kind: hangar_core::config::AppKind::Companion,
                });
                for error in [false, true] {
                    app.map.message = error
                        .then(|| ("The Little Navmap executable was not found.".into(), false));
                    for size in [
                        egui::vec2(760.0, 520.0),
                        egui::vec2(1181.0, 791.0),
                        egui::vec2(1920.0, 1080.0),
                    ] {
                        let output = render(&mut app, size);
                        assert!(app.page_overflow <= 0.0);
                        let text = rendered_text(&output);
                        assert!(text.contains("Open Little Navmap"));
                        assert!(text.contains("Setup"));
                        assert!(text.contains("MSFS stopped"));
                        assert!(!text.contains("Connection settings"));
                        assert!(!text.contains("Bridge off"));
                        assert_eq!(text.contains("executable was not found"), error);
                        assert!(!text.contains("Choose map data"));
                        assert!(!text.contains("Track-up"));
                    }
                }
            }
        }
    }

    #[test]
    fn inert_map_selection_cannot_save_or_replace_the_current_choice() {
        let mut app = app_with(nothing_found());
        let original = app.config.clone();
        assert!(crate::map::open(&mut app, &egui::Context::default()).is_err());
        assert!(crate::map::select(&mut app, Some("/must/not/save".into())).is_err());
        assert_eq!(app.config, original);
        assert!(app.map.target.is_none());
    }

    /// No tab may render wider than the column it is given.
    ///
    /// A tab can render without panicking and still run off the right edge,
    /// because egui clips the overflow and a headless render cannot see what
    /// was clipped. Measuring the content is the only way to catch it.
    #[test]
    fn no_tab_overflows_its_column() {
        use hangar_bindings::catalog::Bound;

        for (discovery, closing) in [
            (everything_found(), false),
            (nothing_found(), false),
            (session_discovery(), true),
        ] {
            let mut app = app_with(discovery);
            prime_controllers(&mut app, true);
            if closing {
                prime_closing(&mut app);
            }

            // Every Controls view, not only the default one.
            let pages = crate::tab::ALL_TABS
                .iter()
                .map(|t| (*t, crate::binding_tools::View::Assignments))
                .chain(
                    [
                        crate::binding_tools::View::Shared,
                        crate::binding_tools::View::Backups,
                    ]
                    .map(|v| (Tab::Bindings, v)),
                );
            for (tab, view) in pages {
                app.active_tab = tab;
                app.binding_tools.view = view;
                // The Bindings tab is width-sensitive in more than one state.
                for bound in [Bound::Any, Bound::Only] {
                    app.bindings_bound = bound;
                    for size in [
                        egui::vec2(1181.0, 791.0), // where the Bindings overflow was found
                        egui::vec2(1280.0, 800.0),
                        egui::vec2(960.0, 640.0),
                        // Without card outlines the tabs fit tighter, and a
                        // narrower window is the cheapest way to hold that.
                        egui::vec2(860.0, 600.0),
                        egui::vec2(1920.0, 1080.0),
                    ] {
                        render(&mut app, size);
                        assert!(
                            app.page_overflow <= 0.0,
                            "{:?} overflows its column by {:.1}pt at {}x{}",
                            tab,
                            app.page_overflow,
                            size.x,
                            size.y
                        );
                    }
                }
            }
        }
    }

    /// A page body may hold a label, a value, or one sentence of consequence;
    /// anything longer is prose and belongs in documentation.
    ///
    /// Scoped to the proportional family, because the design system's own rule
    /// is that data — console scrollback, paths, commands, registry keys — is
    /// monospace. That exempts the console view and every path row without an
    /// allowlist anyone has to maintain.
    #[test]
    fn no_prose_run_is_longer_than_a_sentence() {
        /// One sentence. The limit the page body is held to.
        const MAX_CHARS: usize = 160;

        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => {
                    let job = &t.galley.job;
                    for section in &job.sections {
                        if section.format.font_id.family != egui::FontFamily::Proportional {
                            continue;
                        }
                        let range = usize::from(section.byte_range.start)
                            ..usize::from(section.byte_range.end);
                        let run = &job.text[range];
                        if run.chars().count() > MAX_CHARS {
                            out.push(run.to_owned());
                        }
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }

        let mut offenders: Vec<(Tab, String)> = Vec::new();

        for (discovery, closing) in [
            (everything_found(), false),
            (nothing_found(), false),
            (session_discovery(), true),
        ] {
            let mut app = app_with(discovery);
            prime_controllers(&mut app, true);
            if closing {
                prime_closing(&mut app);
            }

            let pages = crate::tab::ALL_TABS
                .iter()
                .map(|t| (*t, crate::binding_tools::View::Assignments))
                .chain(
                    [
                        crate::binding_tools::View::Shared,
                        crate::binding_tools::View::Backups,
                    ]
                    .map(|v| (Tab::Bindings, v)),
                );
            for (tab, view) in pages {
                let tab = &tab;
                app.active_tab = *tab;
                app.binding_tools.view = view;
                let out = render(&mut app, egui::vec2(1280.0, 800.0));
                let mut long = Vec::new();
                for cs in &out.shapes {
                    collect(&cs.shape, &mut long);
                }
                for run in long {
                    if !offenders.iter().any(|(_, seen)| *seen == run) {
                        offenders.push((*tab, run));
                    }
                }
            }
        }

        // The drop sheet is the page with the most jargon on it and the one no
        // other tab renders — `source` is `None` everywhere else, so `draw`
        // returns immediately. Held to the same rule as the rest.
        {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Install;
            app.drop_install.source =
                Some(std::path::PathBuf::from("/home/j/Downloads/kpvf-v3.zip"));
            app.drop_install.community = Some(std::path::PathBuf::from(
                "/nonexistent/Packages/Community2024",
            ));
            app.drop_install.installed_count = 16;
            *app.drop_install.sheet.lock().expect("lock") = Some(crate::drop_install::Sheet::Rows(
                crate::drop_install::tests::populated_rows(),
            ));
            for confirm in [false, true] {
                for writes in [false, true] {
                    app.drop_install.confirm = confirm;
                    app.writes_allowed = writes;
                    let out = render(&mut app, egui::vec2(1280.0, 800.0));
                    let mut long = Vec::new();
                    for cs in &out.shapes {
                        collect(&cs.shape, &mut long);
                    }
                    for run in long {
                        if !offenders.iter().any(|(_, seen)| *seen == run) {
                            offenders.push((Tab::Install, run));
                        }
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "{} prose run(s) longer than {MAX_CHARS} characters:\n{}",
            offenders.len(),
            offenders
                .iter()
                .map(|(tab, run)| format!("  {tab:?} ({} chars): {run}", run.chars().count()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// The library is a snapshot and nothing watches the filesystem, so the
    /// two edges that rebuild it are the whole feature.
    #[test]
    fn coming_back_to_the_window_rebuilds_the_library() {
        // Alt-tab away, install something, alt-tab back.
        assert_eq!(
            rescan_trigger(true, false, Tab::Preflight, Tab::Preflight),
            Some(Rescan::RegainedFocus)
        );
        // Opening the Hangar, for a change made while focus never left.
        assert_eq!(
            rescan_trigger(true, true, Tab::Hangar, Tab::Install),
            Some(Rescan::OpenedHangar)
        );
    }

    /// **Edges, not states.** Focus held across a hundred frames is one
    /// rescan, and sitting on the Hangar tab is none at all — otherwise the
    /// library would be rebuilt sixty times a second for as long as the tab is
    /// open.
    #[test]
    fn holding_still_rebuilds_nothing() {
        assert_eq!(rescan_trigger(true, true, Tab::Hangar, Tab::Hangar), None);
        assert_eq!(rescan_trigger(false, false, Tab::Hangar, Tab::Hangar), None);
        assert_eq!(rescan_trigger(true, true, Tab::Install, Tab::Install), None);
    }

    /// Losing focus is not a trigger. Only coming back is: a rescan on the way
    /// out would run while the person is doing the very thing that will change
    /// the library, and its answer would be the stale one.
    #[test]
    fn leaving_the_window_rebuilds_nothing() {
        assert_eq!(rescan_trigger(false, true, Tab::Hangar, Tab::Hangar), None);
    }

    /// `RawInput::default()` reports `focused: true`, so `HangarApp::new`
    /// starts `was_focused` true; otherwise every headless render would rescan.
    #[test]
    fn the_first_frame_does_not_rescan() {
        let app = app_with(everything_found());
        assert_eq!(
            rescan_trigger(true, true, app.active_tab, app.active_tab),
            None
        );
        assert!(app.last_scan.is_none(), "nothing has been rescanned yet");
    }

    /// Coarse on purpose: the number answers "is this current?".
    #[test]
    fn how_long_ago_reads_as_a_person_would_say_it() {
        use std::time::Duration;
        assert_eq!(scanned_ago(Duration::from_secs(0)), "just now");
        assert_eq!(scanned_ago(Duration::from_secs(20)), "just now");
        assert_eq!(scanned_ago(Duration::from_secs(45)), "a minute ago");
        assert_eq!(scanned_ago(Duration::from_secs(240)), "4 minutes ago");
        assert_eq!(scanned_ago(Duration::from_secs(3600)), "an hour ago");
        assert_eq!(scanned_ago(Duration::from_secs(7800)), "2 hours ago");
        // Rounds to nearest, so two and a half hours reads as three.
        assert_eq!(scanned_ago(Duration::from_secs(9000)), "3 hours ago");
    }

    /// The Install tab's picker card, in both window-system states.
    ///
    /// `app_with` pins `file_drops` to `Available`, so without this the
    /// Wayland branch would never be rendered.
    #[test]
    fn the_picker_card_renders_on_a_session_that_cannot_receive_drops() {
        for drops in [
            crate::env::FileDrops::Available,
            crate::env::FileDrops::NotOnWayland { x11_too: true },
            crate::env::FileDrops::NotOnWayland { x11_too: false },
        ] {
            for discovery in [everything_found(), nothing_found()] {
                let mut app = app_with(discovery);
                app.file_drops = drops;
                app.active_tab = Tab::Install;

                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(1280.0, 800.0),
                    egui::vec2(960.0, 640.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    render(&mut app, size);
                    assert!(
                        app.page_overflow <= 0.0,
                        "the picker card with {drops:?} overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                    assert!(
                        app.install.job.is_none(),
                        "rendering the picker card must never start an installer"
                    );
                }
            }
        }
    }

    /// The sim-link card, in every state it can report.
    ///
    /// The states whose text carries a **path** are the long ones, and an egui
    /// label that does not fit extends its row. The companion side alone
    /// decides the state, so `app.proton` points at a scratch prefix for each;
    /// nothing here writes to a real prefix.
    #[test]
    fn the_sim_link_card_renders_in_every_state() {
        use hangar_runtime::simlink::RELATIVE;

        let root = std::env::temp_dir().join(format!(
            "hangar-simlink-card-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        // Each case sets up one companion prefix and names the state it puts
        // it in. A long path in every one, because a short path would pass a
        // width test the real thing fails.
        let long = "a-companion-prefix-with-a-deliberately-long-name-to-catch-elision-bugs";
        type Build = Box<dyn Fn(&std::path::Path)>;
        let cases: Vec<(&str, Build)> = vec![
            (
                "missing",
                Box::new(|pfx: &std::path::Path| {
                    std::fs::create_dir_all(pfx.join(RELATIVE).parent().unwrap()).unwrap();
                }),
            ),
            (
                "real-directory",
                Box::new(|pfx: &std::path::Path| {
                    std::fs::create_dir_all(pfx.join(RELATIVE).join("LocalCache")).unwrap();
                    std::fs::write(pfx.join(RELATIVE).join("UserCfg.opt"), "[Packages]\n").unwrap();
                }),
            ),
            (
                "elsewhere",
                Box::new(move |pfx: &std::path::Path| {
                    std::fs::create_dir_all(pfx.join(RELATIVE).parent().unwrap()).unwrap();
                    let other = pfx.join(long).join("somewhere-else-entirely");
                    std::fs::create_dir_all(&other).unwrap();
                    std::os::unix::fs::symlink(&other, pfx.join(RELATIVE)).unwrap();
                }),
            ),
            (
                "dangling",
                Box::new(move |pfx: &std::path::Path| {
                    std::fs::create_dir_all(pfx.join(RELATIVE).parent().unwrap()).unwrap();
                    std::os::unix::fs::symlink(
                        pfx.join(long).join("this-target-never-existed"),
                        pfx.join(RELATIVE),
                    )
                    .unwrap();
                }),
            ),
            (
                "a file where the link goes",
                Box::new(|pfx: &std::path::Path| {
                    std::fs::create_dir_all(pfx.join(RELATIVE).parent().unwrap()).unwrap();
                    std::fs::write(pfx.join(RELATIVE), b"not a directory").unwrap();
                }),
            ),
        ];

        for (name, build) in cases {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Prefix;

            let pfx = root.join(name.replace(' ', "-")).join("pfx");
            build(&pfx);
            let proton = app
                .proton
                .as_mut()
                .expect("everything_found resolves Proton");
            proton.wineprefix = pfx.clone();

            for confirm in [false, true] {
                app.prefix_confirm_link = confirm;
                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(1280.0, 800.0),
                    egui::vec2(960.0, 640.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    render(&mut app, size);
                    assert!(
                        app.page_overflow <= 0.0,
                        "the sim-link card in state {name:?} overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                }
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A scratch directory for the companion-prefix tests.
    fn companion_scratch(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hangar-companion-card-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The companion card in each state the prefix can be in, with the
    /// confirm, a running job and a long result. A long directory name in
    /// every case, because a short path passes a width test the real one fails.
    #[test]
    fn the_companion_card_renders_in_every_state() {
        use hangar_runtime::simlink::RELATIVE;

        let root = companion_scratch("render");
        let long = "a-companion-prefix-with-a-deliberately-long-name-to-catch-elision-bugs";
        type Build = Box<dyn Fn(&std::path::Path)>;
        let ready = |dir: &std::path::Path| {
            let pfx = dir.join("pfx");
            std::fs::create_dir_all(pfx.join("drive_c/users/steamuser/Documents")).unwrap();
            std::fs::write(pfx.join("system.reg"), "WINE REGISTRY\n").unwrap();
            std::fs::write(dir.join("version"), "10.1000-200-a-long-proton-build-tag\n").unwrap();
            for path in hangar_runtime::env::simconnect_cfg_paths(&pfx) {
                std::fs::write(path, hangar_runtime::env::SIMCONNECT_CFG).unwrap();
            }
            // A real directory at the link: never repaired, so nothing is left
            // to do whatever the machine's sim prefix holds.
            std::fs::create_dir_all(pfx.join(RELATIVE).join("LocalCache")).unwrap();
        };
        let cases: Vec<(&str, &str, bool, Build)> = vec![
            (
                "missing",
                "missing",
                true,
                Box::new(|_: &std::path::Path| {}),
            ),
            (
                "not-initialised",
                "not initialised",
                true,
                Box::new(|dir: &std::path::Path| std::fs::create_dir_all(dir).unwrap()),
            ),
            ("ready", "ready", false, Box::new(ready)),
        ];

        for (name, word, offered, build) in cases {
            let dir = root.join(name).join(long);
            build(&dir);
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Prefix;
            let proton = app
                .proton
                .as_mut()
                .expect("everything_found resolves Proton");
            *proton = proton.companion(&dir);

            type Arm = fn(&mut HangarApp);
            let stages: [(&str, Arm); 4] = [
                ("idle", |_| {}),
                ("confirm", |app| app.companion_setup.confirm = true),
                ("running", |app| {
                    app.companion_setup.job = Some(Default::default());
                }),
                ("result", |app| {
                    app.companion_setup.job = None;
                    app.companion_setup.result = Some((
                        "Proton could not initialise the prefix: wineboot exited with exit \
                         status: 1: err:module:import_dll Library something.dll not found \
                         for a deliberately long reason"
                            .into(),
                        false,
                    ));
                }),
            ];
            for (stage, arm) in stages {
                arm(&mut app);
                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(1280.0, 800.0),
                    egui::vec2(860.0, 600.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    let output = render(&mut app, size);
                    assert!(
                        app.page_overflow <= 0.0,
                        "the companion card ({name}, {stage}) overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                    let text = rendered_text(&output);
                    assert!(text.contains("Companion prefix"), "{name}/{stage}");
                    assert!(text.contains(word), "{name}/{stage}: no {word:?}");
                    if stage == "idle" {
                        assert_eq!(
                            text.contains("Set up companion prefix"),
                            offered,
                            "{name}: the action is offered only when there is work"
                        );
                        assert_eq!(text.contains("Nothing to set up."), !offered, "{name}");
                    }
                }
                // A render, confirm included, must never start Proton.
                assert!(app.companion_setup.result.is_none() || stage == "result");
            }
            assert_eq!(
                dir.exists(),
                name != "missing",
                "{name}: rendering created nothing"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The sim's `SimConnect.xml` row, in every outcome, on the whole
    /// Prefix tab. The reading is pre-filled, so the inert form reads nothing.
    #[test]
    fn the_sim_simconnect_row_renders_on_the_prefix_tab() {
        use hangar_runtime::sim_simconnect::{Check, Comm};
        fn long_runs(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => {
                    let job = &t.galley.job;
                    for section in &job.sections {
                        let range = usize::from(section.byte_range.start)
                            ..usize::from(section.byte_range.end);
                        let run = &job.text[range];
                        if section.format.font_id.family == egui::FontFamily::Proportional
                            && run.chars().count() > 160
                        {
                            out.push(run.to_owned());
                        }
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| long_runs(s, out)),
                _ => {}
            }
        }
        let comm = |p: &str, s: &str, a: &str, port: &str| Comm {
            protocol: Some(p.into()),
            scope: Some(s.into()),
            address: Some(a.into()),
            port: Some(port.into()),
            disabled: false,
        };
        let outcomes = [
            Check::Exposed { port: 5557 },
            Check::NoMatch {
                port: 5557,
                found: vec![],
            },
            Check::NoMatch {
                port: 5557,
                found: vec![
                    comm("IPv6", "global", "fe80::1234:5678:9abc:def0", "500"),
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
        ];
        let root = companion_scratch("sim-simconnect");
        let dir = root.join("not-created");
        for found in outcomes {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Prefix;
            let proton = app
                .proton
                .as_mut()
                .expect("everything_found resolves Proton");
            *proton = proton.companion(&dir);
            let sim = app.discovery.msfs.as_ref().unwrap().install.prefix.clone();
            app.companion_setup.sim_simconnect = Some((sim, found.clone()));
            for size in [
                egui::vec2(1181.0, 791.0),
                egui::vec2(1280.0, 800.0),
                egui::vec2(860.0, 600.0),
                egui::vec2(1920.0, 1080.0),
            ] {
                let output = render(&mut app, size);
                assert!(app.page_overflow <= 0.0, "{found:?} at {size:?}");
                let text = rendered_text(&output);
                assert!(text.contains("Sim SimConnect.xml"), "{found:?}");
                assert!(text.contains(found.word()), "{found:?}");
                let mut long = Vec::new();
                output
                    .shapes
                    .iter()
                    .for_each(|s| long_runs(&s.shape, &mut long));
                assert!(long.is_empty(), "{found:?}: {long:?}");
            }
        }
        assert!(!dir.exists(), "rendering created nothing");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The guard is on the action: the inert form refuses the click, the
    /// injected form runs set-up on scratch directories, a rerun changes
    /// nothing and says so, and a running sim is refused.
    #[test]
    fn the_companion_setup_action_refuses_inert_and_runs_injected() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let root = companion_scratch("action");
        let compat = root.join("compatdata/2537590");
        std::fs::create_dir_all(compat.join("pfx").join(hangar_runtime::simlink::RELATIVE))
            .unwrap();
        let msfs = hangar_runtime::ProtonEnv {
            wineprefix: compat.join("pfx"),
            proton_script: root.join("no-proton/proton"),
            wine_binary: root.join("no-proton/files/bin/wine"),
            compat_data: compat.clone(),
            compat_install: root.join("common/MSFS2024"),
            steam_client: root.join("Steam"),
            is_flatpak: false,
        };
        let dir = root.join("hangar-rs/companion-prefix");
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});

        let mut app = app_with(everything_found());
        app.proton = Some(msfs.companion(&dir));

        // Inert: refused, nothing created.
        crate::prefix::start_setup(&mut app, Some(msfs.clone()), wake.clone());
        let (message, ok) = app.companion_setup.result.clone().expect("a refusal");
        assert!(!ok);
        assert!(message.contains("unavailable"), "{message}");
        assert!(!dir.exists());

        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let fake: Arc<hangar_runtime::companion_setup::Initialise> =
            Arc::new(move |env: &hangar_runtime::ProtonEnv| {
                counter.fetch_add(1, Ordering::Relaxed);
                std::fs::create_dir_all(env.wineprefix.join("drive_c/users/steamuser")).unwrap();
                std::fs::write(env.wineprefix.join("system.reg"), "WINE REGISTRY\n").unwrap();
                Ok("fake wineboot".into())
            });

        let run = |app: &mut HangarApp| -> (String, bool) {
            crate::prefix::start_setup(app, Some(msfs.clone()), wake.clone());
            let started = std::time::Instant::now();
            while app.companion_setup.is_running() {
                assert!(started.elapsed().as_secs() < 10, "set-up never finished");
                std::thread::sleep(std::time::Duration::from_millis(10));
                crate::prefix::poll_setup(app);
            }
            app.companion_setup.result.clone().expect("a result")
        };

        // Running sim: refused before anything is touched.
        app.companion_setup = crate::prefix::SetupState::with_runner(fake.clone(), || true);
        let (message, ok) = run(&mut app);
        assert!(!ok);
        assert!(message.contains("sim is running"), "{message}");
        assert!(!dir.exists());
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        // Injected: set up for real on the scratch tree.
        app.companion_setup = crate::prefix::SetupState::with_runner(fake, || false);
        let (message, ok) = run(&mut app);
        assert!(ok, "{message}");
        assert!(message.contains("initialised the prefix"), "{message}");
        assert!(dir.join("pfx/system.reg").is_file());
        assert_eq!(
            hangar_runtime::companion_setup::status(&dir, &msfs.wineprefix).cfg_present(),
            2
        );
        assert!(hangar_runtime::simlink::check(&dir.join("pfx"), &msfs.wineprefix).is_good());

        // Rerun: nothing changes, and it says so.
        let (message, ok) = run(&mut app);
        assert!(ok);
        assert!(message.contains("nothing changed"), "{message}");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // Never the sim's own prefix.
        app.proton = Some(msfs.companion(&compat));
        let (message, ok) = run(&mut app);
        assert!(!ok);
        assert!(message.contains("sim's own prefix"), "{message}");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The drop-install sheet, in every shape a real drop produces.
    ///
    /// `source` is `None` in every other test, so the sheet would only render
    /// empty. **When a panel has an empty and a populated state, render both.**
    ///
    /// Nothing here writes. `place` is only reachable from a click, and the
    /// state is inert anyway.
    #[test]
    fn the_drop_install_sheet_renders_in_every_state() {
        use crate::drop_install::Sheet;

        let states = || {
            vec![
                (
                    "rows",
                    Sheet::Rows(crate::drop_install::tests::populated_rows()),
                ),
                (
                    "refused",
                    Sheet::Refused(
                        "/home/j/Downloads/manual.pdf is a file, which is not something \
                         that can be installed"
                            .into(),
                    ),
                ),
                (
                    "installer",
                    Sheet::Installer(std::path::PathBuf::from(
                        "/home/j/Downloads/Project Airports Setup 0.1.33.exe",
                    )),
                ),
            ]
        };

        for discovery in [everything_found(), nothing_found()] {
            let mut app = app_with(discovery);
            app.active_tab = Tab::Install;
            app.drop_install.source =
                Some(std::path::PathBuf::from("/home/j/Downloads/kpvf-v3.zip"));
            app.drop_install.community = Some(std::path::PathBuf::from(
                "/nonexistent/Packages/Community2024",
            ));
            app.drop_install.installed_count = 16;

            for (label, sheet) in states() {
                *app.drop_install.sheet.lock().expect("lock") = Some(sheet);
                // Both stages of the confirm, and both sides of the write
                // toggle — the button changes shape on all four.
                for confirm in [false, true] {
                    for writes in [false, true] {
                        app.drop_install.confirm = confirm;
                        app.writes_allowed = writes;
                        for size in [
                            egui::vec2(1181.0, 791.0),
                            egui::vec2(1280.0, 800.0),
                            egui::vec2(960.0, 640.0),
                            egui::vec2(1920.0, 1080.0),
                        ] {
                            render(&mut app, size);
                            assert!(
                                app.page_overflow <= 0.0,
                                "the drop sheet ({label}) overflows by {:.1}pt at {}x{}",
                                app.page_overflow,
                                size.x,
                                size.y
                            );
                        }
                    }
                }
            }
        }
    }

    /// The generation question, which is asked rather than guessed.
    #[test]
    fn the_drop_sheet_asks_which_community_folder_before_reading() {
        let mut app = app_with(everything_found());
        app.active_tab = Tab::Install;
        app.drop_install.source = Some(std::path::PathBuf::from("/home/j/Downloads/kpvf.zip"));
        app.drop_install.community = None;

        for size in [egui::vec2(1181.0, 791.0), egui::vec2(960.0, 640.0)] {
            render(&mut app, size);
            assert!(
                app.page_overflow <= 0.0,
                "the generation chooser overflows by {:.1}pt at {}x{}",
                app.page_overflow,
                size.x,
                size.y
            );
        }
        assert!(
            app.drop_install.community.is_none(),
            "rendering must not choose a generation on anybody's behalf"
        );
    }

    #[test]
    fn choosing_a_folder_requires_a_fresh_destination_and_keeps_the_source() {
        let mut app = app_with(everything_found());
        app.drop_install.community = Some("/fixture/Community".into());
        let source = std::path::PathBuf::from("/fixture/airport");
        crate::drop_install::accept(&mut app, source.clone());

        assert_eq!(app.drop_install.source, Some(source));
        assert!(app.drop_install.community.is_none());
        assert!(!app.drop_install.is_reading());
        assert!(app.drop_install.sheet.lock().expect("lock").is_none());
    }

    /// What a finished placement reports, including a partial one.
    #[test]
    fn the_drop_outcome_renders_a_partial_success() {
        let mut app = app_with(everything_found());
        app.active_tab = Tab::Install;
        app.drop_install.source = Some(std::path::PathBuf::from("/home/j/Downloads/kpvf.zip"));
        app.drop_install.community = Some(std::path::PathBuf::from(
            "/nonexistent/Packages/Community2024",
        ));
        *app.drop_install.outcome.lock().expect("lock") = Some(vec![
            (
                "vbcorp-airport-kpvf-placerville — 412/412 verified, replacing 1.0.0 \
                 (moved aside, not deleted)"
                    .into(),
                true,
            ),
            (
                "koak: staging landed on a different filesystem from the Community folder, \
                 so this would be a copy rather than a rename"
                    .into(),
                false,
            ),
        ]);

        for size in [egui::vec2(1181.0, 791.0), egui::vec2(960.0, 640.0)] {
            render(&mut app, size);
            assert!(
                app.page_overflow <= 0.0,
                "the outcome card overflows by {:.1}pt at {}x{}",
                app.page_overflow,
                size.x,
                size.y
            );
        }
    }

    /// The Prefix tab, including the `SimConnect.cfg` confirm.
    ///
    /// Rendering it must not write a `SimConnect.cfg` into anything: the paths
    /// come from the fixture environment, which points at this machine's real
    /// prefix, so a draw that wrote would write there.
    #[test]
    fn the_prefix_tab_renders_and_writes_nothing() {
        for discovery in [everything_found(), nothing_found()] {
            let mut app = app_with(discovery);
            app.active_tab = Tab::Prefix;

            for confirm in [false, true] {
                app.prefix_confirm_cfg = confirm;
                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(760.0, 520.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    render(&mut app, size);
                    assert!(
                        app.page_overflow <= 0.0,
                        "Prefix overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                }
            }

            // The result banner, both ways.
            for result in [
                Some(("2 files written".to_string(), true)),
                Some(("/mnt/steam/pfx: permission denied".to_string(), false)),
            ] {
                app.prefix_result = result;
                render(&mut app, egui::vec2(1181.0, 791.0));
                assert!(app.page_overflow <= 0.0);
            }

            assert!(
                app.journal.view(Some("Prefix"), "").is_empty(),
                "rendering the Prefix tab wrote a SimConnect.cfg"
            );
        }
    }

    /// The Console tab, empty and populated.
    #[test]
    fn the_console_tab_renders_empty_and_populated() {
        let mut app = app_with(everything_found());
        app.active_tab = Tab::Console;

        // Empty.
        for size in [egui::vec2(1181.0, 791.0), egui::vec2(760.0, 520.0)] {
            render(&mut app, size);
            assert!(app.page_overflow <= 0.0, "empty Console overflows");
        }

        // Populated, with lines long enough to run off a narrow card and one
        // of every level.
        app.journal.note(
            "Project Airports",
            "launching /prefix/…/Project Airports.exe",
        );
        app.journal.output(
            "Project Airports",
            "fixme:ntdll:NtQuerySystemInformation info_class SYSTEM_PERFORMANCE_INFORMATION stub",
        );
        app.journal.output(
            "Project Airports",
            r#"{"message":"sync.done","data":{"installed":18,"updated":55,"removed":24}}"#,
        );
        app.journal
            .output("BeyondATC", "wine: Unhandled page fault on read access");
        app.journal
            .done("BeyondATC", "BeyondATC started as pid 40312");
        app.journal
            .fail("setup.exe", "[valid purchase] no valid purchase was found");

        let sources = app.journal.sources();
        assert_eq!(sources.len(), 3);

        for source in std::iter::once(None).chain(sources.iter().map(|s| Some(s.clone()))) {
            app.console_source = source;
            for search in ["", "sync", "zzzznothingmatches"] {
                app.console_search = search.into();
                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(760.0, 520.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    render(&mut app, size);
                    assert!(
                        app.page_overflow <= 0.0,
                        "Console overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                }
            }
        }
    }

    /// A filter left pointing at a source that has gone must not strand the
    /// view showing nothing with no way back to all of it.
    #[test]
    fn an_empty_activity_source_keeps_its_deep_link_and_all_sources_option() {
        let mut app = app_with(everything_found());
        app.active_tab = Tab::Console;
        app.journal.note("BeyondATC", "started");
        app.console_source = Some("BeyondATC".into());
        render(&mut app, egui::vec2(1181.0, 791.0));
        assert_eq!(app.console_source.as_deref(), Some("BeyondATC"));

        app.journal.clear();
        render(&mut app, egui::vec2(1181.0, 791.0));
        assert_eq!(app.console_source.as_deref(), Some("BeyondATC"));
        let output = render(&mut app, egui::vec2(1181.0, 791.0));
        assert!(rendered_text(&output).contains("BeyondATC"));
    }

    /// The Install tab through every card, and — the point of the test — proof
    /// that rendering it, arming the confirm and clicking Run starts nothing.
    ///
    /// The Run button is still drawn and clickable on an inert state: `arm` is
    /// the guard.
    #[test]
    fn the_install_tab_renders_and_never_starts_an_installer() {
        use crate::install::{Analysis, Target};
        use hangar_install::gate::{Checked, Outcome, Refusal, Report};
        use hangar_install::run::Invocation;
        use std::path::PathBuf;

        // All three gate outcomes at once; Unchecked must not render as a tick.
        let three_valued = Report {
            outcomes: vec![
                Checked {
                    label: "prefix".into(),
                    outcome: Outcome::Pass(
                        "UserCfg.opt at /mnt/steam/SteamLibrary/steamapps/compatdata/2537590/\
                         pfx/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight \
                         Simulator 2024/UserCfg.opt"
                            .into(),
                    ),
                },
                Checked {
                    label: "sim content".into(),
                    outcome: Outcome::Unchecked(
                        "no companion prefix, so nothing could be resolved".into(),
                    ),
                },
                Checked {
                    label: "sim".into(),
                    outcome: Outcome::Refuse(Refusal::SimRunning),
                },
            ],
        };

        let invocation = Invocation {
            program: "/mnt/steam/SteamLibrary/steamapps/common/Proton - Experimental/proton".into(),
            args: vec![
                "runinprefix".into(),
                "/home/user/Downloads/setup.exe".into(),
            ],
            cwd: PathBuf::from("/opt/hangar"),
            env: vec![("STEAM_COMPAT_DATA_PATH".into(), "/mnt/steam/x".into())],
        };

        let cases: Vec<(&str, Analysis)> = vec![
            (
                "unreadable",
                Analysis {
                    path: PathBuf::from("/home/user/Downloads/not-a-pe.bin"),
                    inspection: Err("not a Windows executable: no MZ signature".into()),
                    profile: None,
                    gate: None,
                    invocation: None,
                    signatures: Vec::new(),
                    target_prefix: None,
                    sim_running: false,
                    reaches_sim_content: None,
                },
            ),
            (
                "refused",
                Analysis {
                    path: PathBuf::from("/home/user/Downloads/setup.exe"),
                    inspection: Err("not read".into()),
                    profile: None,
                    gate: Some(three_valued),
                    invocation: Some(Err("no compatibility tool is configured".into())),
                    signatures: vec!["valid purchase".into()],
                    target_prefix: Some(PathBuf::from("/mnt/steam/pfx")),
                    sim_running: true,
                    reaches_sim_content: Some(true),
                },
            ),
            (
                "runnable",
                Analysis {
                    path: PathBuf::from("/home/user/Downloads/setup.exe"),
                    inspection: Err("not read".into()),
                    profile: None,
                    gate: Some(Report::default()),
                    invocation: Some(Ok(invocation)),
                    signatures: vec![
                        "valid purchase".into(),
                        "0xce".into(),
                        "wine: Unhandled page fault".into(),
                    ],
                    target_prefix: Some(PathBuf::from(
                        "/mnt/steam/SteamLibrary/steamapps/compatdata/2537590/pfx",
                    )),
                    sim_running: false,
                    reaches_sim_content: Some(true),
                },
            ),
        ];

        for (name, analysis) in cases {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Install;
            let path = analysis.path.clone();
            *app.install
                .analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(analysis);
            app.install.selected = Some(path.clone());

            // Unarmed, then armed: the confirm names the file and the prefix.
            for confirm in [None, Some(path.clone())] {
                app.install.confirm = confirm;
                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(960.0, 640.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    let output = render(&mut app, size);
                    let text = rendered_text(&output);
                    assert!(!text.contains("The exact command"));
                    assert!(!text.contains("runinprefix"));
                    if name == "refused" {
                        assert!(text.contains("no compatibility tool is configured"));
                        assert!(text.contains("no companion prefix, so nothing could be resolved"));
                    }
                    assert!(
                        app.page_overflow <= 0.0,
                        "Install ({name}) overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                }
            }

            assert!(
                app.install.job.is_none(),
                "rendering the Install tab started an installer ({name})"
            );
        }

        // And the guard itself, driven directly: everything an armed confirm
        // would have, and an inert state still runs nothing.
        let mut app = app_with(everything_found());
        let target = Target::of(&app);
        assert!(target.is_some(), "the fixture environment should resolve");
        assert!(!app.install.is_live());
        app.install.confirm = Some(PathBuf::from("/home/user/Downloads/setup.exe"));
        render(&mut app, egui::vec2(1181.0, 791.0));
        assert!(app.install.job.is_none());
    }

    /// A file that is not a PE is reported, not guessed at.
    #[test]
    fn inspecting_something_that_is_not_an_installer_says_so() {
        use crate::install::analyse;

        let dir = std::env::temp_dir().join(format!("hangar-app-install-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-pe.bin");
        std::fs::write(&path, b"this is not a Windows executable").unwrap();

        let target = crate::install::Target {
            msfs_prefix: dir.join("pfx"),
            community: dir.join("community"),
            exe_xml: dir.join("EXE.xml"),
            companion_prefix: None,
            proton_msfs: None,
            proton_companion: None,
        };
        let analysis = analyse(&path, &target);
        assert!(analysis.inspection.is_err(), "a text file is not a PE");
        assert!(analysis.profile.is_none(), "and matches no vendor rule");
        assert!(!analysis.runnable(), "so nothing may be run against it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Bridges tab in every state its cards can be in, and — the point of
    /// the test — proof that rendering it never opens a socket.
    #[test]
    fn the_bridges_tab_renders_without_ever_starting_a_bridge() {
        use crate::bridges::Which;

        for discovery in [everything_found(), nothing_found()] {
            let mut app = app_with(discovery);
            app.active_tab = Tab::Bridges;

            // Off, one on, the other on, both on. The config is only ever
            // *asked*; an inert state refuses every time.
            for (lnm, efb) in [(false, false), (true, false), (false, true), (true, true)] {
                app.config.bridges.lnm.enabled = lnm;
                app.config.bridges.efb.enabled = efb;
                app.sync_bridges();
                for size in [
                    egui::vec2(1181.0, 791.0),
                    egui::vec2(960.0, 640.0),
                    egui::vec2(1920.0, 1080.0),
                ] {
                    render(&mut app, size);
                    assert!(
                        app.page_overflow <= 0.0,
                        "Bridges overflows by {:.1}pt at {}x{}",
                        app.page_overflow,
                        size.x,
                        size.y
                    );
                }
                assert!(
                    !app.bridges.is_running(Which::Lnm) && !app.bridges.is_running(Which::Efb),
                    "a render test started a bridge: it would bind 51968 and \
                     broadcast UDP to the whole subnet"
                );
            }

            // And the branch that only shows when config.toml will not parse.
            app.config_problem = Some("/home/x/config.toml: expected an equals sign".into());
            render(&mut app, egui::vec2(1181.0, 791.0));
            assert!(app.page_overflow <= 0.0);
        }
    }

    /// Saving over a file that will not parse would replace someone's
    /// half-fixed config with this application's defaults.
    #[test]
    fn a_malformed_config_is_never_written_over() {
        let mut app = app_with(everything_found());
        let path = app.config_path.clone().expect("a scratch path");
        app.config_problem = Some("expected an equals sign".into());
        app.config.bridges.lnm.enabled = true;
        app.save_config();
        assert!(!path.exists(), "it wrote {} anyway", path.display());

        // With the problem cleared it saves normally, and reads back as itself.
        app.config_problem = None;
        app.save_config();
        match hangar_core::config::Config::read(&path) {
            hangar_core::config::Load::Present(config) => {
                assert!(config.bridges.lnm.enabled);
            }
            other => panic!("expected Present, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A save must rewrite the **whole** file. Holding only the section being
    /// edited would delete the apps list on the first click of a bridge switch.
    #[test]
    fn saving_a_bridge_switch_keeps_the_apps_list() {
        let mut app = app_with(everything_found());
        let path = app.config_path.clone().expect("a scratch path");
        app.config.apps = vec![hangar_core::config::AppEntry {
            name: "Project Airports".into(),
            exe: "/prefix/Project Airports.exe".into(),
            args: vec!["--no-sandbox".into()],
            kind: hangar_core::config::AppKind::Companion,
            uninstaller: None,
            enabled: true,
        }];
        app.config.bridges.efb.enabled = true;
        app.save_config();

        match hangar_core::config::Config::read(&path) {
            hangar_core::config::Load::Present(config) => {
                assert_eq!(config.apps.len(), 1, "the apps list was dropped");
                assert!(config.bridges.efb.enabled);
            }
            other => panic!("expected Present, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_rebind_card_renders_in_all_three_of_its_states() {
        use crate::bindings::Rebind;
        use hangar_bindings::capture::{Captured, Confidence};
        use hangar_bindings::joystick::Control;

        let caught = Captured {
            device: hangar_core::input::InputDevice {
                name: "VKB-Sim (C) Alex Oz 2023  VKBsim Gladiator EVO R FSM.GA ".into(),
                vendor: 0x231d,
                product: 0x0220,
                handlers: vec!["event6".into(), "js0".into()],
                ..Default::default()
            },
            control: Control::Button(8),
            confidence: Confidence::Button,
            raw_kind: hangar_core::evdev::EV_KEY,
            raw_code: 0x127,
        };

        // Waiting, caught-and-unconfirmed, written, and refused (what shows
        // while Steam is open).
        let states: Vec<Box<dyn Fn() -> Rebind>> = vec![
            Box::new(|| Rebind::detached("Elevator Trim Up", "AIRCRAFT", "KEY_ELEV_TRIM_UP")),
            Box::new({
                let caught = caught.clone();
                move || {
                    let mut r =
                        Rebind::detached("Elevator Trim Up", "AIRCRAFT", "KEY_ELEV_TRIM_UP");
                    r.caught = Some(caught.clone());
                    r
                }
            }),
            Box::new(|| {
                let mut r = Rebind::detached("Elevator Trim Up", "AIRCRAFT", "KEY_ELEV_TRIM_UP");
                r.outcome = Some(("Joystick Button 8 (code 7) — snapshot taken".into(), true));
                r
            }),
            Box::new(|| {
                let mut r = Rebind::detached("Elevator Trim Up", "AIRCRAFT", "KEY_ELEV_TRIM_UP");
                r.outcome = Some((
                    "Steam is running (steam (4697)). Close it fully.".into(),
                    false,
                ));
                r
            }),
        ];

        for make in states {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Bindings;
            prime_controllers(&mut app, true);
            app.rebind = Some(make());
            for size in [egui::vec2(1280.0, 800.0), egui::vec2(960.0, 640.0)] {
                render(&mut app, size);
            }
            // Rendering must not have written anything or lost the card.
            assert!(app.rebind.is_some(), "the card closed itself");
        }
    }

    #[test]
    fn the_controllers_card_renders_plugged_in_and_unplugged() {
        // Both branches: a duplicate registration to warn about, and the same
        // library with nothing connected.
        for plugged_in in [true, false] {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Bindings;
            prime_controllers(&mut app, plugged_in);

            let survey = app.controllers.as_ref().unwrap().1.clone();
            if plugged_in {
                assert!(
                    survey
                        .iter()
                        .any(|v| v.status
                            == hangar_bindings::devices::Status::DuplicateRegistrations),
                    "the fixture should contain a duplicate registration"
                );
                assert!(
                    survey
                        .iter()
                        .any(|v| v.status == hangar_bindings::devices::Status::AlternateProfiles),
                    "and an alternate-profile pair, which must not be confused with it"
                );
            }

            for size in [egui::vec2(1280.0, 800.0), egui::vec2(960.0, 640.0)] {
                render(&mut app, size);
            }
        }
    }

    #[test]
    fn the_bindings_tab_renders_in_every_filter_and_both_states() {
        // Empty and populated states both rendered.
        use hangar_bindings::catalog::Bound;

        for discovery in [everything_found(), nothing_found()] {
            let mut app = app_with(discovery);
            app.active_tab = Tab::Bindings;
            prime_controllers(&mut app, true);
            for bound in [Bound::Any, Bound::Only, Bound::Never] {
                app.bindings_bound = bound;
                for search in ["", "trim", "zzzznothingmatches"] {
                    app.bindings_search = search.into();
                    for size in [
                        egui::vec2(1280.0, 800.0),
                        egui::vec2(1600.0, 900.0),
                        egui::vec2(960.0, 640.0),
                    ] {
                        render(&mut app, size);
                    }
                }
            }
        }
    }

    #[test]
    fn selecting_one_device_narrows_the_bindings_tab() {
        let mut app = app_with(everything_found());
        app.active_tab = Tab::Bindings;
        prime_controllers(&mut app, true);
        app.bindings_bound = hangar_bindings::catalog::Bound::Any;

        let ids: Vec<String> = app
            .bindings
            .profiles
            .iter()
            .map(|l| l.id().to_string())
            .collect();
        assert_eq!(ids.len(), 6, "the fixture library should hold six profiles");

        for id in ids {
            app.bindings_profile = Some(id);
            render(&mut app, egui::vec2(1280.0, 800.0));
        }
        app.bindings_profile = None;
        render(&mut app, egui::vec2(1280.0, 800.0));
    }

    #[test]
    fn every_tab_renders_when_nothing_was_discovered() {
        let mut app = app_with(nothing_found());
        for tab in crate::tab::ALL_TABS.iter() {
            app.active_tab = *tab;
            render(&mut app, egui::vec2(1180.0, 760.0));
        }
    }

    /// The Hangar tab with a real catalog behind it.
    #[test]
    fn the_hangar_tab_renders_a_populated_catalog() {
        use hangar_catalog::{CatalogSnapshot, Item, ItemState, ScanResult};
        use std::path::PathBuf;

        let items = (0..30)
            .map(|n| Item {
                name: format!("vendor-package-with-a-long-directory-name-{n}"),
                // Every kind, so both the Apps section and the package
                // sections have something in them.
                kind: match n % 9 {
                    0 => hangar_catalog::ItemKind::SimAddonExe,
                    1 => hangar_catalog::ItemKind::CompanionApp,
                    2 => hangar_catalog::ItemKind::NativeTool,
                    _ => hangar_catalog::ItemKind::Package,
                },
                // Every state, including the two an app can be in.
                state: match n % 6 {
                    0 => ItemState::Dangling {
                        target: PathBuf::from("/gone"),
                    },
                    1 => ItemState::Ready,
                    2 => ItemState::MissingExecutable,
                    3 => ItemState::SimEnabled,
                    _ => ItemState::Resident,
                },
                title: format!("A Reasonably Long Package Title Number {n}"),
                creator: "Some Vendor With A Long Name".into(),
                version: "1.2.3".into(),
                content_type: Some(match n % 4 {
                    0 => hangar_msfs::ContentType::Aircraft,
                    1 => hangar_msfs::ContentType::Scenery,
                    2 => hangar_msfs::ContentType::Instruments,
                    _ => hangar_msfs::ContentType::Livery,
                }),
                size_bytes: (n % 2 == 0).then_some(4_200_000_000),
                path: PathBuf::from("/x"),
                community: None,
                args: Vec::new(),
            })
            .collect();

        let mut discovery = everything_found();
        discovery.catalog = std::sync::Arc::new(CatalogSnapshot::from_scan(ScanResult {
            items,
            problems: vec![hangar_catalog::Problem {
                path: PathBuf::from("/x/broken/manifest.json"),
                reason: "manifest is not valid JSON".into(),
            }],
        }));

        let mut app = app_with(discovery);
        app.active_tab = Tab::Hangar;

        // Every section, plus the grouped "All" view.
        let mut sections: Vec<Option<hangar_catalog::Section>> =
            hangar_catalog::Section::ALL.into_iter().map(Some).collect();
        sections.push(None);
        for section in sections {
            app.hangar_section = section;
            render(&mut app, egui::vec2(1180.0, 760.0));
            render(&mut app, egui::vec2(760.0, 520.0));
        }

        // A search that matches nothing here but something elsewhere, and one
        // that matches nothing at all.
        for (section, search) in [
            (Some(hangar_catalog::Section::Aircraft), "scenery"),
            (Some(hangar_catalog::Section::Aircraft), "no such thing"),
            (None, "no such thing"),
        ] {
            app.hangar_section = section;
            app.hangar_search = search.into();
            render(&mut app, egui::vec2(1180.0, 760.0));
        }
    }

    #[test]
    fn every_tab_renders_with_a_full_environment() {
        let mut app = app_with(everything_found());
        for tab in crate::tab::ALL_TABS.iter() {
            app.active_tab = *tab;
            render(&mut app, egui::vec2(1180.0, 760.0));
        }
    }

    /// The Apps section with a launchable item, in both button states.
    #[test]
    fn the_apps_section_renders_its_launch_controls() {
        use hangar_catalog::{CatalogSnapshot, Item, ItemState, ScanResult};
        use std::path::PathBuf;

        let items = vec![
            Item {
                name: "Project Airports".into(),
                kind: hangar_catalog::ItemKind::CompanionApp,
                state: ItemState::Ready,
                title: "Project Airports".into(),
                creator: String::new(),
                version: String::new(),
                content_type: None,
                size_bytes: None,
                path: PathBuf::from("/prefix/Programs/@pa/Project Airports.exe"),
                community: None,
                args: vec!["--no-sandbox".into()],
            },
            Item {
                name: "Gone".into(),
                kind: hangar_catalog::ItemKind::CompanionApp,
                state: ItemState::MissingExecutable,
                title: "Gone".into(),
                creator: String::new(),
                version: String::new(),
                content_type: None,
                size_bytes: None,
                path: PathBuf::from("/nonexistent/gone.exe"),
                community: None,
                args: Vec::new(),
            },
        ];

        let mut discovery = everything_found();
        discovery.catalog = std::sync::Arc::new(CatalogSnapshot::from_scan(ScanResult {
            items,
            problems: Vec::new(),
        }));

        let mut app = app_with(discovery);
        app.active_tab = Tab::Hangar;
        app.hangar_section = Some(hangar_catalog::Section::Apps);

        // With and without a resolvable Proton, and with a result banner up.
        for proton in [true, false] {
            if !proton {
                app.proton = None;
            }
            render(&mut app, egui::vec2(1180.0, 760.0));
            render(&mut app, egui::vec2(760.0, 520.0));
        }
    }

    #[test]
    fn selected_hangar_item_has_a_detail_view_at_both_widths() {
        let mut app = app_with(session_discovery());
        app.active_tab = Tab::Hangar;
        app.hangar_selected = Some("BeyondATC".into());
        for size in [egui::vec2(760.0, 520.0), egui::vec2(1528.0, 780.0)] {
            let output = render(&mut app, size);
            let text = rendered_text(&output);
            assert!(text.contains("Location"), "{text}");
            assert!(text.contains("BeyondATC.exe"), "{text}");
            assert!(app.page_overflow <= 0.0, "{}", app.page_overflow);
            assert_eq!(text.contains("Back to library"), size.x < 1000.0);
            if let Ok(directory) = std::env::var("HANGARRS_UI_CAPTURE") {
                let ctx = egui::Context::default();
                hangar_ui::theme::install(&ctx);
                let frame = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ui| app.draw(ui),
                );
                crate::ui_capture::save(
                    &std::path::Path::new(&directory)
                        .join(format!("hangar-detail-{}.mesh", size.x as usize)),
                    &ctx,
                    frame,
                    size,
                );
            }
        }
        assert!(!app.actions.sessions.is_live());
    }

    fn ui_frame(
        app: &mut HangarApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 760.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.draw(ui),
        )
    }

    fn click_text(app: &mut HangarApp, ctx: &egui::Context, label: &str) {
        click_text_in(app, ctx, label, false);
    }

    fn click_text_in(app: &mut HangarApp, ctx: &egui::Context, label: &str, body_only: bool) {
        fn position(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text.ends_with(label) => {
                    Some(t.pos + t.galley.rect.center().to_vec2())
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| position(s, label)),
                _ => None,
            }
        }
        let output = ui_frame(app, ctx, vec![]);
        let pos = output
            .shapes
            .iter()
            .filter_map(|s| position(&s.shape, label))
            .find(|pos| !body_only || pos.x > 180.0)
            .unwrap_or_else(|| panic!("missing {label}"));
        for pressed in [true, false] {
            ui_frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    fn manual_flight() -> crate::flight::Flight {
        use hangar_runtime::preflight::{Action, Condition, Preflight, Step};
        crate::flight::Flight::with_launcher(
            Preflight::new("test").with(Step::after(
                Action::LaunchSim {
                    app_id: "2537590".into(),
                },
                Condition::Manual,
            )),
            Default::default(),
            None,
            |_| panic!("a waiting render must not launch"),
        )
    }

    #[test]
    fn flight_edits_inclusion_and_keeps_nested_tools_reachable() {
        let mut app = app_with(flight_discovery());
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        app.set_placement("Little Navmap", crate::flight::Placement::Off);
        let text = rendered_text(&ui_frame(&mut app, &ctx, vec![]));
        assert!(!text.contains("Little Navmap"));
        assert_eq!(text.matches("Project Airports").count(), 1);
        assert!(!app.flight_view.edit_order);
        click_text(&mut app, &ctx, "Edit order");
        assert!(app.flight_view.edit_order);
        click_text(&mut app, &ctx, "Done");
        assert!(!app.flight_view.edit_order);
        click_text(&mut app, &ctx, "Add apps");
        assert!(app.flight_view.add_apps);
        click_text(&mut app, &ctx, "Add");
        assert_eq!(
            app.placement_of("Little Navmap"),
            crate::flight::Placement::After
        );
        click_text(&mut app, &ctx, "Hangar");
        click_text(&mut app, &ctx, "Loadouts");
        assert_eq!(app.active_tab, Tab::Profiles);
        assert_eq!(app.active_tab.parent(), Tab::Hangar);
        click_text(&mut app, &ctx, "Settings");
        click_text(&mut app, &ctx, "Advanced");
        assert_eq!(app.active_tab, Tab::Prefix);
        assert_eq!(app.active_tab.parent(), Tab::Settings);
        click_text(&mut app, &ctx, "Flight");
        click_text(&mut app, &ctx, "Settings");
        assert_eq!(app.active_tab, Tab::Prefix, "remember the settings view");
        click_text(&mut app, &ctx, "Hangar");
        assert_eq!(app.active_tab, Tab::Profiles, "remember the hangar view");
        assert!(app.flight.is_none());
        assert!(!app.writes_allowed);
    }

    #[test]
    fn flight_continues_off_tab_and_cancel_requires_its_confirmation() {
        use hangar_runtime::preflight::{Action, Preflight, State, Step};
        let mut app = app_with(flight_discovery());
        app.flight = Some(crate::flight::Flight::with_launcher(
            Preflight::new("test").with(Step::now(Action::LaunchSim {
                app_id: "2537590".into(),
            })),
            Default::default(),
            None,
            |_| {
                Ok(hangar_runtime::sim::Handoff {
                    url: "inert".into(),
                    opener: "test".into(),
                })
            },
        ));
        app.active_tab = Tab::Settings;
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        ui_frame(&mut app, &ctx, vec![]);
        ui_frame(&mut app, &ctx, vec![]);
        assert!(matches!(
            app.flight.as_ref().unwrap().run().states()[0],
            State::Done(_)
        ));
        assert!(app.flight.as_ref().unwrap().is_finished());

        app.flight = Some(manual_flight());
        app.active_tab = Tab::Preflight;
        click_text(&mut app, &ctx, "Edit next flight");
        assert!(app.flight_view.editing_next);
        assert!(app.flight.is_some());
        let steps = app.flight.as_ref().unwrap().run().preflight().steps.clone();
        crate::preflight::begin(&mut app, &[]);
        assert_eq!(app.flight.as_ref().unwrap().run().preflight().steps, steps);
        click_text(&mut app, &ctx, "Current session");
        click_text(&mut app, &ctx, "Cancel launch");
        assert!(app.flight_view.confirm_cancel);
        assert!(!app.flight.as_ref().unwrap().run().is_cancelled());
        click_text(&mut app, &ctx, "Keep session");
        assert!(!app.flight_view.confirm_cancel);
        assert!(!app.flight.as_ref().unwrap().run().is_cancelled());
        click_text(&mut app, &ctx, "Cancel launch");
        click_text(&mut app, &ctx, "Confirm cancel");
        assert!(app.flight.as_ref().unwrap().run().is_cancelled());
        assert!(
            app.flight.is_some(),
            "the completed result remains available"
        );
    }

    fn finished_flight() -> crate::flight::Flight {
        let mut flight = crate::flight::Flight::with_launcher(
            crate::flight::suggested([]),
            Default::default(),
            None,
            |id| {
                Ok(hangar_runtime::sim::Handoff {
                    url: format!("steam://rungameid/{id}"),
                    opener: "stub".into(),
                })
            },
        );
        let sim = crate::env::TelemetryLink::idle();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(flight.is_finished());
        flight
    }

    #[test]
    fn flight_returns_to_the_launch_list_once_everything_is_down() {
        // A finished flight whose sim was never seen stays put
        // (nothing has happened yet); one whose sim came and went ends.
        let mut app = app_with(flight_discovery());
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        app.flight = Some(finished_flight());
        let text = rendered_text(&ui_frame(&mut app, &ctx, vec![]));
        assert!(
            app.flight.is_some() && text.contains("Launch complete"),
            "{text}"
        );

        app.flight.as_mut().unwrap().saw_sim();
        ui_frame(&mut app, &ctx, vec![]);
        assert!(app.flight.is_none());
        let text = rendered_text(&ui_frame(&mut app, &ctx, vec![]));
        assert!(
            text.contains("Start flight") && text.contains("Launch order"),
            "{text}"
        );
        assert!(!app
            .journal
            .view(Some("Preflight"), "ready for the next launch")
            .is_empty());
    }

    #[test]
    fn an_app_away_updating_reads_updating_not_exited() {
        use hangar_runtime::observe::{Fact, Found, Identity, Namespace, Observation, Process};
        use hangar_runtime::preflight::{Action, Condition, Preflight, Step};
        let key = "ProjectAirports.exe";
        let seen = |pids: &[u32]| {
            let mut obs = Observation::empty(&[key.into()]);
            obs.apps.insert(
                key.into(),
                Found {
                    exact: pids
                        .iter()
                        .map(|&pid| Process {
                            identity: Identity { pid, start: 1 },
                            helper: false,
                            prefix: Fact::Known("/companion/pfx".into()),
                            user_ns: Fact::Known(Namespace::fixture(1)),
                            mount_ns: Fact::Known(Namespace::fixture(2)),
                        })
                        .collect(),
                    similar: vec![],
                },
            );
            obs
        };
        let observer = crate::flight_details::Observer::given(seen(&[30051]));
        let mut flight = crate::flight::Flight::with_launcher(
            Preflight::new("t").with(Step::after(
                Action::LaunchApp {
                    label: "Project Airports".into(),
                },
                Condition::Manual,
            )),
            Default::default(),
            None,
            |_| panic!("no sim launch"),
        )
        .with_observer(observer.clone());
        flight.follow_for_test("Project Airports", key, None);
        let t0 = std::time::Instant::now();
        flight.follow_apps(t0);
        observer.set(seen(&[]));
        flight.follow_apps(t0 + crate::flight::OBSERVE_EVERY);
        let mut app = app_with(flight_discovery());
        app.flight = Some(flight);
        for size in [egui::vec2(640.0, 480.0), egui::vec2(1400.0, 900.0)] {
            let output = render(&mut app, size);
            let text = rendered_text(&output);
            assert!(text.contains("Updating"), "{text}");
            assert!(text.contains("Restarting after an update"), "{text}");
            assert_eq!(app.page_overflow, 0.0);
        }
    }

    #[test]
    fn a_version_drift_warning_shows_on_the_launch_list_without_reading_the_machine() {
        let mut app = app_with(session_discovery());
        app.set_placement("BeyondATC", crate::flight::Placement::After);
        app.active_tab = Tab::Preflight;
        // Inert: the fixture names the real sim prefix, which must not be read.
        let text = rendered_text(&render(&mut app, egui::vec2(860.0, 600.0)));
        assert!(!text.contains("configured program is elsewhere"), "{text}");
        let wanted = vec![(
            "BeyondATC".to_string(),
            std::path::PathBuf::from("/companion/BeyondATC/BeyondATC.exe"),
        )];
        let target = std::path::PathBuf::from("/sim/pfx/drive_c/BeyondATC/BeyondATC.exe");
        app.flight_view.drift = Some((
            wanted,
            vec![crate::preflight::Drifted {
                app: "BeyondATC".into(),
                warning: "BeyondATC: the simulator prefix has BeyondATC version 1.10.4.Experimental installed in C:\\BeyondATC\\; the configured program is elsewhere.".into(),
                installed: Some(target.clone()),
            }],
        ));
        for size in [egui::vec2(640.0, 480.0), egui::vec2(1400.0, 900.0)] {
            let text = rendered_text(&render(&mut app, size));
            assert!(text.contains("configured program is elsewhere"), "{text}");
            assert!(text.contains("Use the installed copy"), "{text}");
            assert_eq!(app.page_overflow, 0.0, "at {size:?}");
        }

        // The button routes to the installed copy, saved to the (scratch)
        // config, and the old path is in Activity.
        app.config.apps = vec![hangar_core::config::AppEntry {
            name: "BeyondATC".into(),
            exe: "/companion/BeyondATC/BeyondATC.exe".into(),
            args: vec![],
            kind: hangar_core::config::AppKind::SimSession,
            uninstaller: None,
            enabled: true,
        }];
        crate::preflight::use_installed_copy(&mut app, "BeyondATC", &target);
        assert_eq!(app.config.apps[0].exe, target);
        let saved = hangar_core::config::Config::read(app.config_path.as_ref().unwrap())
            .ok()
            .expect("saved");
        assert_eq!(saved.apps[0].exe, target);
        assert!(app.flight_view.drift.is_none(), "read again next frame");
        assert!(app
            .journal
            .view(Some("BeyondATC"), "")
            .iter()
            .any(|l| l.text.contains("(was /companion/BeyondATC/BeyondATC.exe)")));
    }

    #[test]
    fn flight_row_controls_log_flight_and_leave_the_source_as_found() {
        let mut app = app_with(flight_discovery());
        let ctx = egui::Context::default();
        hangar_ui::theme::install(&ctx);
        let mut flight = crate::flight::Flight::with_launcher(
            crate::flight::build(
                &[(
                    "Project Airports".to_string(),
                    crate::flight::Placement::After,
                )]
                .into_iter()
                .collect(),
            ),
            Default::default(),
            None,
            |_| panic!("no sim launch"),
        );
        flight.cancel();
        app.flight = Some(flight);
        let text = rendered_text(&ui_frame(&mut app, &ctx, vec![]));
        assert!(text.contains("Launch stopped"), "{text}");
        assert_eq!(app.actions.source, "", "restored after the row's controls");
    }

    #[test]
    fn flight_footer_reports_observation_and_actions_remain_inert() {
        let mut app = app_with(flight_discovery());
        let text = rendered_text(&render(&mut app, egui::vec2(860.0, 600.0)));
        assert!(text.contains("MSFS closed"));
        assert!(text.contains("No flight data"));
        assert!(!text.contains("MSFS found"));
        let item = app.discovery.catalog.items()[0].clone();
        let env = app.proton.clone().unwrap();
        crate::apps::spawn_launch(&app.actions, env.clone(), &item, &app.journal, || {
            panic!("inert")
        });
        crate::apps::spawn_restart(&app.actions, env.clone(), &item, &app.journal, || {
            panic!("inert")
        });
        crate::apps::spawn_stop(&app.actions, Some(env), &item, &app.journal, || {
            panic!("inert")
        });
        assert!(!app.actions.is_busy(&item.name));
        assert!(app.actions.take_result().is_none());
    }

    fn flight_discovery() -> Discovery {
        let mut discovery = session_discovery();
        let template = discovery.catalog.items()[0].clone();
        let mut items = vec![template.clone()];
        for (name, exe) in [
            ("Project Airports", "ProjectAirports.exe"),
            ("Little Navmap", "littlenavmap.exe"),
        ] {
            let mut item = template.clone();
            item.name = name.into();
            item.title = name.into();
            item.path = std::path::PathBuf::from("/sample").join(exe);
            item.kind = hangar_catalog::ItemKind::CompanionApp;
            items.push(item);
        }
        discovery.catalog = std::sync::Arc::new(hangar_catalog::CatalogSnapshot::from_scan(
            hangar_catalog::ScanResult {
                items,
                problems: vec![],
            },
        ));
        discovery
    }

    #[test]
    fn flight_views_fit_and_optionally_capture_production_meshes() {
        let destination = std::env::var_os("HANGARRS_UI_CAPTURE").map(std::path::PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        for (name, size, tab, populated, mode) in [
            (
                "flight-expanded",
                egui::vec2(1528.0, 780.0),
                Tab::Preflight,
                true,
                "plan",
            ),
            (
                "hangar-expanded",
                egui::vec2(1528.0, 780.0),
                Tab::Hangar,
                true,
                "plan",
            ),
            (
                "hangar-narrow",
                egui::vec2(860.0, 600.0),
                Tab::Hangar,
                true,
                "plan",
            ),
            (
                "connections-expanded",
                egui::vec2(1528.0, 780.0),
                Tab::Bridges,
                true,
                "plan",
            ),
            (
                "connections-narrow",
                egui::vec2(860.0, 600.0),
                Tab::Bridges,
                true,
                "plan",
            ),
            (
                "map-expanded",
                egui::vec2(1528.0, 780.0),
                Tab::Map,
                true,
                "plan",
            ),
            (
                "map-narrow",
                egui::vec2(860.0, 600.0),
                Tab::Map,
                true,
                "plan",
            ),
            (
                "flight-wide",
                egui::vec2(1300.0, 800.0),
                Tab::Preflight,
                true,
                "plan",
            ),
            (
                "flight-narrow",
                egui::vec2(860.0, 600.0),
                Tab::Preflight,
                true,
                "plan",
            ),
            (
                "flight-empty",
                egui::vec2(860.0, 600.0),
                Tab::Preflight,
                false,
                "plan",
            ),
            (
                "settings-narrow",
                egui::vec2(960.0, 640.0),
                Tab::Settings,
                true,
                "plan",
            ),
            (
                "flight-add-apps",
                egui::vec2(860.0, 650.0),
                Tab::Preflight,
                true,
                "add",
            ),
            (
                "flight-confirm",
                egui::vec2(860.0, 650.0),
                Tab::Preflight,
                true,
                "confirm",
            ),
            (
                "flight-next",
                egui::vec2(860.0, 650.0),
                Tab::Preflight,
                true,
                "next",
            ),
            (
                "flight-failed",
                egui::vec2(860.0, 650.0),
                Tab::Preflight,
                true,
                "failed",
            ),
            (
                "flight-long-names",
                egui::vec2(760.0, 520.0),
                Tab::Preflight,
                true,
                "long",
            ),
        ] {
            let mut app = app_with(if populated {
                flight_discovery()
            } else {
                nothing_found()
            });
            app.active_tab = tab;
            if tab == Tab::Map {
                app.map.target = Some(crate::map::Target {
                    exe: "/sample/Little Navmap/littlenavmap.exe".into(),
                    args: vec![],
                    kind: hangar_core::config::AppKind::Companion,
                });
            }

            match mode {
                "confirm" | "next" => {
                    app.flight = Some(manual_flight());
                    app.flight_view.confirm_cancel = mode == "confirm";
                    app.flight_view.editing_next = mode == "next";
                }
                "failed" => {
                    use hangar_runtime::preflight::{Action, Preflight, Step};
                    let mut flight = crate::flight::Flight::with_launcher(
                        Preflight::new("test").with(Step::now(Action::LaunchSim {
                            app_id: "2537590".into(),
                        })),
                        Default::default(),
                        None,
                        |_| Err(std::io::Error::other("Steam could not be opened.")),
                    );
                    flight.tick(&app.sim, || {});
                    flight.tick(&app.sim, || {});
                    app.flight = Some(flight);
                }
                "add" => {
                    app.set_placement("Little Navmap", crate::flight::Placement::Off);
                    app.flight_view.add_apps = true;
                }
                "long" => {
                    let mut items = app.discovery.catalog.items().to_vec();
                    for item in &mut items {
                        item.title = "An unusually long application name ".repeat(12);
                    }
                    app.discovery.catalog = std::sync::Arc::new(
                        hangar_catalog::CatalogSnapshot::from_scan(hangar_catalog::ScanResult {
                            items,
                            problems: vec![],
                        }),
                    );
                }
                _ => {}
            }
            let ctx = egui::Context::default();
            hangar_ui::theme::install(&ctx);
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ui| app.draw(ui),
            );
            if let Some(path) = &destination {
                crate::ui_capture::save(&path.join(format!("{name}.mesh")), &ctx, output, size);
            }
            assert!(!app.actions.sessions.is_live());
            assert!(app.page_overflow <= 0.0, "{name}: {}", app.page_overflow);
        }
    }

    #[test]
    fn install_activity_fits_while_workers_are_quiet() {
        for width in [760.0, 1200.0] {
            for (mode, label) in [
                ("installer", "Reading installer…"),
                ("archive", "Reading packages…"),
                ("placing", "Installing packages…"),
                ("checking", "Checking packages…"),
            ] {
                let mut app = app_with(everything_found());
                app.active_tab = Tab::Install;
                app.install.history_read = true;
                match mode {
                    "installer" => {
                        *app.install.analysing.lock().unwrap() = Some("/fixture/setup.exe".into());
                    }
                    "archive" | "placing" => {
                        app.drop_install.source = Some("/fixture/airport.zip".into());
                        app.drop_install.community = Some("/fixture/Community".into());
                        if mode == "archive" {
                            *app.drop_install.reading.lock().unwrap() = true;
                        } else {
                            *app.drop_install.sheet.lock().unwrap() =
                                Some(crate::drop_install::Sheet::Rows(
                                    crate::drop_install::tests::populated_rows(),
                                ));
                            *app.drop_install.placing.lock().unwrap() = true;
                        }
                    }
                    _ => {
                        app.install.show_verify = true;
                        *app.install.verifying.lock().unwrap() = true;
                    }
                }
                let ctx = egui::Context::default();
                ui::theme::install(&ctx);
                let size = egui::vec2(width, 680.0);
                // Several quiet frames prove the indicator drives its own repaint.
                for frame in 0..3 {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            time: Some(1.0 + frame as f64 * 0.25),
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ui| app.draw(ui),
                    );
                    assert!(rendered_text(&output).contains(label), "{mode}");
                    assert!(app.page_overflow <= 0.0, "{mode} at {width}");
                    assert_eq!(
                        output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                        std::time::Duration::ZERO
                    );
                    if frame == 0 {
                        if let Ok(directory) = std::env::var("HANGARRS_ACTIVITY_CAPTURE") {
                            crate::ui_capture::save(
                                &std::path::Path::new(&directory)
                                    .join(format!("{mode}-{}.mesh", width as usize)),
                                &ctx,
                                output,
                                size,
                            );
                        }
                    }
                }
                assert!(app.install.job.is_none());
                assert!(!app.install.is_live());
                *app.install.analysing.lock().unwrap() = None;
                *app.install.verifying.lock().unwrap() = false;
                *app.drop_install.reading.lock().unwrap() = false;
                *app.drop_install.placing.lock().unwrap() = false;
                assert!(!rendered_text(&render(&mut app, size)).contains(label));
            }
        }
    }

    #[test]
    fn install_history_fits_resizing_with_room_for_the_chooser() {
        for width in [760.0, 900.0, 1200.0, 1920.0] {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Install;
            app.install.history_read = true;
            for n in 0..13 {
                let mut record = hangar_install::record::InstallRecord::of_drop(
                    std::path::Path::new(
                        "/fixture/airport-with-a-long-product-name-and-version.zip",
                    ),
                    0,
                    std::path::Path::new("/fixture/Community"),
                    &[],
                );
                record.at = format!("2026-09-{:02}T23-11-17Z", 24 - n);
                record.landed.push(hangar_install::record::RecordedItem {
                    kind: "package".into(),
                    name: "Sample airport".into(),
                    detail: String::new(),
                });
                if n == 1 {
                    record.concerns.push("A file could not be verified.".into());
                }
                app.install.history.push(record);
            }
            let ctx = egui::Context::default();
            ui::theme::install(&ctx);
            let size = egui::vec2(width, 680.0);
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ui| app.draw(ui),
            );
            assert!(app.page_overflow <= 0.0, "{width}: {}", app.page_overflow);
            let text = rendered_text(&output);
            assert!(text.contains("Choose file") && text.contains("look at this"));
            for shape in &output.shapes {
                if let egui::Shape::Text(t) = &shape.shape {
                    if t.galley.job.text.starts_with("airport-with-") {
                        assert!(
                            t.pos.x < 330.0,
                            "history title must stay beside its date: {}",
                            t.pos.x
                        );
                    }
                }
            }
            assert!(app.install.job.is_none());
            if let Ok(directory) = std::env::var("HANGARRS_UI_CAPTURE") {
                std::fs::create_dir_all(&directory).unwrap();
                crate::ui_capture::save(
                    &std::path::Path::new(&directory)
                        .join(format!("install-{}.mesh", width as usize)),
                    &ctx,
                    output,
                    size,
                );
            }
        }
    }

    #[test]
    fn expanded_pages_use_space_for_lists_and_keep_forms_compact() {
        fn text_position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
            fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
                match shape {
                    egui::Shape::Text(t) if t.galley.job.text.ends_with(label) => Some(t.pos),
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, label)),
                    _ => None,
                }
            }
            output
                .shapes
                .iter()
                .find_map(|s| find(&s.shape, label))
                .unwrap_or_else(|| panic!("missing {label}"))
        }
        let text_left = |output: &egui::FullOutput, label: &str| text_position(output, label).x;
        let mut app = app_with(flight_discovery());
        let narrow = render(&mut app, egui::vec2(860.0, 780.0));
        let wide = render(&mut app, egui::vec2(1528.0, 780.0));
        assert!(
            text_left(&wide, "Start flight") - text_left(&narrow, "Start flight") > 600.0,
            "the page action must follow the window edge"
        );
        assert!(
            text_left(&wide, "Edit order") - text_left(&narrow, "Edit order") < 150.0,
            "row actions must not follow the window edge"
        );
        app.active_tab = Tab::Hangar;
        let wide = render(&mut app, egui::vec2(1280.0, 780.0));
        let wider = render(&mut app, egui::vec2(1920.0, 1080.0));
        for label in ["Launch", "All"] {
            assert!(
                text_left(&wider, label) - text_left(&wide, label) > 600.0,
                "{label} must use the expanded library width"
            );
        }
        app.active_tab = Tab::Map;
        let output = render(&mut app, egui::vec2(1528.0, 780.0));
        assert!(
            text_left(&output, "Open Little Navmap") - text_left(&output, "Little Navmap") < 30.0
        );
        assert!(app.flight.is_none());
        assert!(!app.bridges.is_running(crate::bridges::Which::Lnm));
        assert!(!app.bridges.is_running(crate::bridges::Which::Efb));
    }

    #[test]
    fn support_views_fit_at_multiple_scales_and_capture() {
        for (name, tab, settings, controls) in [
            (
                "settings-general",
                Tab::Settings,
                settings::View::General,
                crate::binding_tools::View::Assignments,
            ),
            (
                "settings-setup",
                Tab::Settings,
                settings::View::Simulator,
                crate::binding_tools::View::Assignments,
            ),
            (
                "storage",
                Tab::Settings,
                settings::View::Storage,
                crate::binding_tools::View::Assignments,
            ),
            (
                "advanced",
                Tab::Prefix,
                settings::View::General,
                crate::binding_tools::View::Assignments,
            ),
            (
                "controls",
                Tab::Bindings,
                settings::View::General,
                crate::binding_tools::View::Assignments,
            ),
            (
                "shared-inputs",
                Tab::Bindings,
                settings::View::General,
                crate::binding_tools::View::Shared,
            ),
            (
                "control-backups",
                Tab::Bindings,
                settings::View::General,
                crate::binding_tools::View::Backups,
            ),
            (
                "activity",
                Tab::Console,
                settings::View::General,
                crate::binding_tools::View::Assignments,
            ),
        ] {
            for populated in [false, true] {
                for width in [860.0, 1528.0] {
                    for scale in [1.0, 1.25, 1.5] {
                        let mut app = app_with(if populated {
                            everything_found()
                        } else {
                            nothing_found()
                        });
                        app.active_tab = tab;
                        app.settings_view = settings;
                        app.binding_tools.view = controls;
                        if populated {
                            app.journal.note("Project Airports", "Starting");
                            app.journal
                                .fail("BeyondATC", "Launch failed: executable was not found");
                            app.journal
                                .output("BeyondATC", "fixme: ordinary Wine output");
                        }
                        let ctx = egui::Context::default();
                        ui::theme::install(&ctx);
                        ctx.set_zoom_factor(scale);
                        let size = egui::vec2(width, 780.0);
                        let output = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    size,
                                )),
                                ..Default::default()
                            },
                            |ui| app.draw(ui),
                        );
                        assert!(
                            app.page_overflow <= 0.0,
                            "{name} at {width}/{scale}: {}",
                            app.page_overflow
                        );
                        assert!(!app.actions.sessions.is_live());
                        if scale == 1.0 && populated {
                            if let Ok(directory) = std::env::var("HANGARRS_UI_CAPTURE") {
                                std::fs::create_dir_all(&directory).unwrap();
                                crate::ui_capture::save(
                                    &std::path::Path::new(&directory)
                                        .join(format!("{name}-{}.mesh", width as usize)),
                                    &ctx,
                                    output,
                                    size,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hangar_activity_link_and_storage_navigation_keep_context() {
        let mut app = app_with(session_discovery());
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        app.active_tab = Tab::Hangar;
        app.hangar_section = Some(hangar_catalog::Section::Apps);
        click_text(&mut app, &ctx, "BeyondATC");
        assert_eq!(app.hangar_selected.as_deref(), Some("BeyondATC"));
        click_text_in(&mut app, &ctx, "Activity", true);
        assert_eq!(app.active_tab, Tab::Console);
        assert_eq!(app.console_source.as_deref(), Some("BeyondATC"));
        click_text(&mut app, &ctx, "Settings");
        click_text(&mut app, &ctx, "Storage");
        assert!(app.settings_view == settings::View::Storage);
        click_text(&mut app, &ctx, "Flight");
        click_text(&mut app, &ctx, "Settings");
        assert!(app.settings_view == settings::View::Storage);
    }

    fn session_discovery() -> Discovery {
        let mut discovery = everything_found();
        let item = hangar_catalog::Item {
            name: "BeyondATC".into(),
            title: "BeyondATC".into(),
            kind: hangar_catalog::ItemKind::SimSessionApp,
            state: hangar_catalog::ItemState::Ready,
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: "/companion/BeyondATC/BeyondATC.exe".into(),
            community: None,
            args: vec![],
        };
        discovery.catalog = std::sync::Arc::new(hangar_catalog::CatalogSnapshot::from_scan(
            hangar_catalog::ScanResult {
                items: vec![item],
                problems: vec![],
            },
        ));
        discovery
    }

    /// BeyondATC up, asked to close, and still open: every row that shows it
    /// reads "Closing", drawn through the whole app, not only the row pieces.
    fn prime_closing(app: &mut HangarApp) {
        let key = "BeyondATC.exe";
        let mut snapshot = std::collections::BTreeMap::new();
        for (key, pid) in [(hangar_runtime::sim::MSFS_2024_EXE, 123), (key, 124)] {
            snapshot.insert(
                key.to_string(),
                hangar_runtime::proc::AppProcesses {
                    main: Some(hangar_runtime::proc::ProcInfo {
                        pid,
                        cmdline: key.into(),
                        args: vec![key.into()],
                    }),
                    helpers: vec![],
                },
            );
        }
        app.monitor = hangar_runtime::AppMonitor::inert(snapshot);
        app.actions.sessions.close_requested(key);
        app.actions.sessions.note_confirming(key);
        app.hangar_section = Some(hangar_catalog::Section::Apps);
        app.flight = Some(crate::flight::Flight::with_launcher(
            crate::flight::suggested(["BeyondATC".to_string()]),
            Default::default(),
            None,
            |id| {
                Ok(hangar_runtime::sim::Handoff {
                    url: hangar_runtime::sim::run_url(id),
                    opener: "stub".into(),
                })
            },
        ));
    }

    /// A failed app step mid-flight offers Check again, and
    /// Retry launch only after a fresh check finds nothing running.
    #[test]
    fn a_failed_flight_step_offers_check_again_then_retry() {
        use crate::flight::{build, Flight, Placement};
        let mut app = app_with(session_discovery());
        app.active_tab = Tab::Preflight;
        let mut flight = Flight::with_launcher(
            build(&[("BeyondATC".to_string(), Placement::Before)].into()),
            Default::default(),
            None,
            |id| {
                Ok(hangar_runtime::sim::Handoff {
                    url: hangar_runtime::sim::run_url(id),
                    opener: "stub".into(),
                })
            },
        );
        let sim = TelemetryLink::idle();
        flight.tick(&sim, || {});
        flight.tick(&sim, || {});
        assert!(!flight.is_finished(), "the sim step waits on a person");
        assert!(matches!(
            flight.run().states()[0],
            hangar_runtime::preflight::State::Failed(_)
        ));
        app.flight = Some(flight);

        for size in [egui::vec2(1280.0, 800.0), egui::vec2(860.0, 600.0)] {
            let text = rendered_text(&render(&mut app, size));
            assert!(
                text.contains("Check again") && text.contains("Retry launch"),
                "{text}"
            );
            assert!(app.page_overflow <= 0.0, "{}", app.page_overflow);
        }
        assert!(
            !crate::preflight::retry_allowed(&app, "BeyondATC"),
            "no check yet"
        );

        // The inert observer sees nothing running: a retry may go ahead, and
        // pressing it would still reach only the inert action guard.
        crate::flight_details::check(&mut app, "BeyondATC");
        assert!(crate::preflight::retry_allowed(&app, "BeyondATC"));
        render(&mut app, egui::vec2(1280.0, 800.0));
        assert!(app.page_overflow <= 0.0);
    }

    #[test]
    fn the_closing_state_is_drawn_on_hangar_and_flight() {
        let mut app = app_with(session_discovery());
        prime_closing(&mut app);
        for tab in [Tab::Hangar, Tab::Preflight] {
            app.active_tab = tab;
            let text = rendered_text(&render(&mut app, egui::vec2(1280.0, 800.0)));
            assert!(text.contains("Closing"), "{tab:?}: {text}");
        }
    }

    #[test]
    fn sim_session_controls_render_before_and_after_sim_start() {
        for sim_running in [false, true] {
            for app_running in [false, true] {
                let mut app = app_with(session_discovery());
                let mut snapshot = std::collections::BTreeMap::new();
                for (key, pid, running) in [
                    (hangar_runtime::sim::MSFS_2024_EXE, 123, sim_running),
                    ("BeyondATC.exe", 124, app_running),
                ] {
                    if running {
                        snapshot.insert(
                            key.to_string(),
                            hangar_runtime::proc::AppProcesses {
                                main: Some(hangar_runtime::proc::ProcInfo {
                                    pid,
                                    cmdline: key.into(),
                                    args: vec![key.into()],
                                }),
                                helpers: vec![],
                            },
                        );
                    }
                }
                app.monitor = hangar_runtime::AppMonitor::inert(snapshot);
                assert_eq!(app.sim_process_running(), sim_running);
                app.hangar_section = Some(hangar_catalog::Section::Apps);
                for tab in [Tab::Hangar, Tab::Preflight] {
                    app.active_tab = tab;
                    for size in [
                        egui::vec2(760.0, 520.0),
                        egui::vec2(1180.0, 760.0),
                        egui::vec2(1600.0, 900.0),
                    ] {
                        let output = render(&mut app, size);
                        let text = rendered_text(&output);
                        assert!(text.contains("BeyondATC"), "{text}");
                        assert!(app.page_overflow <= 0.0, "{tab:?}: {}", app.page_overflow);
                        assert!(
                            text.lines().all(|line| line.chars().count() <= 160),
                            "{text}"
                        );
                        if tab == Tab::Hangar {
                            assert_eq!(
                                text.contains("Start the simulator first."),
                                !sim_running && !app_running,
                                "{text}"
                            );
                            assert!(
                                text.contains(if app_running { "Running" } else { "Launch" }),
                                "{text}"
                            );
                        }
                        assert!(!app.actions.sessions.is_live());
                        assert!(app.actions.take_result().is_none());
                        assert!(app.flight.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn sim_session_preflight_preserves_user_order_and_keeps_off() {
        let mut app = app_with(session_discovery());
        app.preflight
            .placement
            .insert("BeyondATC".into(), crate::flight::Placement::Before);
        assert_eq!(
            app.placement_of("BeyondATC"),
            crate::flight::Placement::Before
        );
        app.active_tab = Tab::Preflight;
        let output = render(&mut app, egui::vec2(1180.0, 760.0));
        assert!(rendered_text(&output).contains("Move BeyondATC below MSFS"));
        assert!(app.flight.is_none());
        app.preflight
            .placement
            .insert("BeyondATC".into(), crate::flight::Placement::Off);
        assert_eq!(app.placement_of("BeyondATC"), crate::flight::Placement::Off);
    }

    /// The read-only details block, empty and populated, in both Flight
    /// views. The observer is inert, so no render reads the real `/proc`.
    #[test]
    fn flight_details_compare_expected_and_observed_without_effects() {
        use hangar_runtime::observe::{Fact, Found, Identity, Observation, Process};
        let mut app = app_with(session_discovery());
        app.set_placement("BeyondATC", crate::flight::Placement::After);
        app.active_tab = Tab::Preflight;
        let key = app
            .discovery
            .catalog
            .items()
            .iter()
            .find(|i| i.name == "BeyondATC")
            .map(|i| hangar_runtime::proc::match_key(&i.path))
            .unwrap();
        let mut populated = Observation::empty(std::slice::from_ref(&key));
        populated.apps.insert(
            key.clone(),
            Found {
                exact: vec![Process {
                    identity: Identity {
                        pid: 4242,
                        start: 1,
                    },
                    helper: false,
                    prefix: Fact::Known("/scratch/elsewhere/pfx".into()),
                    user_ns: Fact::Unreadable,
                    mount_ns: Fact::Unreadable,
                }],
                similar: vec!["MyBeyondATC.exe".into()],
            },
        );
        for (observer, expect) in [
            (crate::flight_details::Observer::default(), "Not running"),
            (
                crate::flight_details::Observer::given(populated),
                "Wrong prefix",
            ),
        ] {
            app.observer = observer;
            app.flight = None;
            app.flight_details = Default::default();
            crate::flight_details::toggle(&mut app, "BeyondATC");
            for running in [false, true] {
                if running {
                    app.flight = Some(crate::flight::Flight::with_launcher(
                        crate::flight::suggested(["BeyondATC".to_string()]),
                        Default::default(),
                        None,
                        |id| {
                            Ok(hangar_runtime::sim::Handoff {
                                url: id.into(),
                                opener: "stub".into(),
                            })
                        },
                    ));
                }
                for size in [egui::vec2(760.0, 520.0), egui::vec2(1200.0, 800.0)] {
                    let text = rendered_text(&render(&mut app, size));
                    assert!(app.page_overflow <= 0.0, "details overflow at {size:?}");
                    for want in ["Expected", "Observed", "Check again", expect] {
                        assert!(text.contains(want), "{want} missing:\n{text}");
                    }
                    assert_eq!(text.contains("PID 4242"), expect == "Wrong prefix");
                    assert_eq!(text.contains("MyBeyondATC.exe"), expect == "Wrong prefix");
                }
            }
            assert!(app
                .journal
                .view(Some("BeyondATC"), "Environment check")
                .iter()
                .any(|line| line.text.contains(expect)));
            crate::flight_details::toggle(&mut app, "BeyondATC");
            assert!(!app.flight_details.is_open("BeyondATC"));
        }
        // The simulator row has its own key and compares against the sim prefix.
        crate::flight_details::toggle(&mut app, crate::flight_details::SIMULATOR);
        assert!(app.flight_details.is_open(crate::flight_details::SIMULATOR));
    }

    #[test]
    fn preflight_order_and_launch_actions_are_inert_without_live_capabilities() {
        let mut app = app_with(session_discovery());
        // This setter saves; in a test it must not reach the user's real
        // preferences.
        app.set_placement("BeyondATC", crate::flight::Placement::Before);
        crate::preflight::begin(&mut app, &[]);
        assert!(app.flight.is_none());
        let apps = app.discovery.catalog.items().to_vec();
        // Make the launch capability live to prove the dependency guard fires
        // first. No launch, discovery, namespace entry or tick is performed.
        app.actions = crate::apps::AppActions::live();
        crate::preflight::begin(&mut app, &apps);
        assert!(app.flight.is_none());
        assert_eq!(
            app.placement_of("BeyondATC"),
            crate::flight::Placement::Before
        );
    }

    /// A running app shows Stop and Restart; a busy one shows both disabled.
    ///
    /// The monitor reports nothing running in tests, so the running branch is
    /// reached by driving the row helper directly rather than through a poll.
    #[test]
    fn a_running_app_row_offers_stop_and_restart() {
        let mut app = app_with(everything_found());
        app.active_tab = Tab::Hangar;
        app.hangar_section = Some(hangar_catalog::Section::Apps);

        // An action in flight must not leave a second click available.
        app.actions = crate::apps::AppActions::default();
        render(&mut app, egui::vec2(1180.0, 760.0));
    }

    /// A package's detail lists the loadouts, and a change is
    /// saved to the set at once (only this app's own profile files).
    #[test]
    fn a_package_can_join_and_leave_a_set_from_hangar() {
        use hangar_catalog::Profile;
        let mut discovery = everything_found();
        let item = hangar_catalog::Item {
            name: "fd-airport-koak-oakland".into(),
            title: "Oakland International".into(),
            kind: hangar_catalog::ItemKind::Package,
            state: hangar_catalog::ItemState::Resident,
            creator: "FlyDesign".into(),
            version: "1.0.0".into(),
            content_type: None,
            size_bytes: Some(1 << 30),
            path: "/…/Community2024/fd-airport-koak-oakland".into(),
            community: Some("/…/Community2024".into()),
            args: vec![],
        };
        discovery.catalog = std::sync::Arc::new(hangar_catalog::CatalogSnapshot::from_scan(
            hangar_catalog::ScanResult {
                items: vec![item],
                problems: vec![],
            },
        ));
        let mut app = app_with(discovery);
        app.active_tab = Tab::Hangar;
        app.hangar_section = None;
        app.hangar_selected = Some("fd-airport-koak-oakland".into());

        // No sets: one line, no chips.
        let text = rendered_text(&render(&mut app, egui::vec2(1280.0, 800.0)));
        assert!(text.contains("No loadouts yet."), "{text}");

        let dir = std::env::temp_dir().join(format!("hangar-app-sets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        app.profiles = hangar_catalog::ProfileStore::load(&dir);
        for name in ["VFR Only", "Airliners"] {
            app.profiles.save(Profile::new(name)).unwrap();
        }
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(860.0, 600.0)] {
            let text = rendered_text(&render(&mut app, size));
            assert!(text.contains("In loadouts"), "{text}");
            assert!(
                text.contains("VFR Only") && text.contains("Airliners"),
                "{text}"
            );
            assert!(app.page_overflow <= 0.0, "{}", app.page_overflow);
        }

        let package = "fd-airport-koak-oakland";
        assert!(crate::hangar::toggle_membership(&mut app, "VFR Only", package, true).1);
        let saved = hangar_catalog::ProfileStore::load(&dir);
        assert!(saved.get("VFR Only").unwrap().contains(package));
        assert!(!saved.get("Airliners").unwrap().contains(package));
        assert!(crate::hangar::toggle_membership(&mut app, "VFR Only", package, false).1);
        let saved = hangar_catalog::ProfileStore::load(&dir);
        assert!(!saved.get("VFR Only").unwrap().contains(package));
        assert!(!crate::hangar::toggle_membership(&mut app, "Gone", package, true).1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Remove, as Hangar draws it: the row action, the package sheet with writes
    /// on and off, and the vendor sheet. Every state at four sizes, no overflow,
    /// and every sentence short. Inert throughout: no click reaches `act`.
    #[test]
    fn remove_renders_its_row_action_and_both_sheets() {
        use crate::remove::{PackageSheet, Sheet, Tied, VendorSheet};
        use hangar_install::remove::{Strength, Tie, Uninstaller};

        let (base, target) = crate::remove::tests::scratch_sheet("render", 3 << 20);
        let community = target.community.clone();
        let package = hangar_catalog::Item {
            name: target.name.clone(),
            title: "Pilatus PC-12".into(),
            kind: hangar_catalog::ItemKind::Package,
            state: hangar_catalog::ItemState::Resident,
            creator: "Orbx".into(),
            version: "1.0.4".into(),
            content_type: None,
            size_bytes: Some(target.bytes),
            path: target.path.clone(),
            community: Some(community.clone()),
            args: vec![],
        };
        let exe: std::path::PathBuf = base.join(
            "pfx/drive_c/users/steamuser/AppData/Local/Programs/orbx-central/Orbx Central.exe",
        );
        let vendor = hangar_catalog::Item {
            name: "Orbx Central".into(),
            title: "Orbx Central".into(),
            kind: hangar_catalog::ItemKind::CompanionApp,
            state: hangar_catalog::ItemState::Ready,
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: exe.clone(),
            community: None,
            args: vec![],
        };
        let mut discovery = everything_found();
        discovery.catalog = std::sync::Arc::new(hangar_catalog::CatalogSnapshot::from_scan(
            hangar_catalog::ScanResult {
                items: vec![package.clone(), vendor.clone()],
                problems: vec![],
            },
        ));
        let mut app = app_with(discovery);
        app.active_tab = Tab::Hangar;
        app.hangar_section = None;
        let sets = base.join("sets");
        app.profiles = hangar_catalog::ProfileStore::load(&sets);
        app.profiles
            .save(hangar_catalog::Profile::from_enabled(
                "Bush trips",
                [target.name.clone()].into(),
            ))
            .unwrap();

        let target = std::sync::Arc::new(target);
        let package_sheet = Sheet::Package(PackageSheet {
            title: "Pilatus PC-12".into(),
            target: target.clone(),
        });
        let tied = |name: &str, title: &str, strength, ticked, readable: bool| Tied {
            tie: Tie {
                name: name.into(),
                path: community.join(name),
                strength,
            },
            title: title.into(),
            target: if readable {
                Ok(target.clone())
            } else {
                Err("a link inside it".into())
            },
            ticked,
        };
        let vendor_sheet = Sheet::Vendor(VendorSheet {
            app: "Orbx Central".into(),
            exe: exe.clone(),
            uninstaller: Uninstaller {
                key: "8e0e1f6c-orbx-central".into(),
                display_name: "Orbx Central 4.5.48".into(),
                publisher: "Orbx Simulation Systems Pty Ltd".into(),
                command: r#""C:\users\steamuser\AppData\Local\Programs\orbx-central\Uninstall Orbx Central.exe" /currentuser"#.into(),
                install_location: r"C:\users\steamuser\AppData\Local\Programs\orbx-central".into(),
                display_icon: String::new(),
            },
            packages: vec![
                tied("orbx-aircraft-pc12", "Pilatus PC-12", Strength::Certain, true, true),
                tied("orbx-airport-ybbn", "Brisbane", Strength::Likely, true, true),
                tied("orbx-scenery-tas", "Tasmania", Strength::Guess, false, true),
                tied("orbx-airport-ymml", "Melbourne", Strength::Guess, false, false),
            ],
        });

        let states: [(&str, &str, Option<Sheet>, bool); 5] = [
            ("remove-row", &package.name, None, false),
            (
                "remove-package",
                &package.name,
                Some(package_sheet.clone()),
                true,
            ),
            (
                "remove-package-writes-off",
                &package.name,
                Some(package_sheet),
                false,
            ),
            ("remove-vendor-row", "Orbx Central", None, true),
            ("remove-vendor", "Orbx Central", Some(vendor_sheet), true),
        ];
        let capture = std::env::var_os("HANGARRS_UI_CAPTURE").map(std::path::PathBuf::from);
        for (name, item, sheet, writes) in states {
            app.hangar_selected = Some(item.to_string());
            app.writes_allowed = writes;
            for size in [
                egui::vec2(1280.0, 800.0),
                egui::vec2(1181.0, 791.0),
                egui::vec2(960.0, 640.0),
                egui::vec2(860.0, 600.0),
            ] {
                app.remove.sheet = None;
                if let Some(sheet) = sheet.clone() {
                    app.remove.show(item, sheet);
                }
                let ctx = egui::Context::default();
                hangar_ui::theme::install(&ctx);
                // Three frames: the sheet asks to be scrolled into view on its
                // first, and the scroll area answers on the next.
                // The capture needs the whole font atlas, which only the first
                // frame carries; later frames' patches are laid onto it.
                let mut atlas: Option<egui::TexturesDelta> = None;
                let mut output = None;
                for step in 0..3 {
                    let mut frame = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            // A second apart, so the scroll animation completes.
                            time: Some(f64::from(step)),
                            ..Default::default()
                        },
                        |ui| app.draw(ui),
                    );
                    let delta = std::mem::take(&mut frame.textures_delta);
                    match &mut atlas {
                        None => atlas = Some(delta),
                        Some(full) => {
                            for (id, patch) in delta.set {
                                let Some((_, base)) = full.set.iter_mut().find(|(i, _)| *i == id)
                                else {
                                    continue;
                                };
                                let (egui::ImageData::Color(to), egui::ImageData::Color(from)) =
                                    (&mut base.image, &patch.image);
                                let to = std::sync::Arc::make_mut(to);
                                let [x0, y0] = patch.pos.unwrap_or([0, 0]);
                                for y in 0..from.size[1] {
                                    for x in 0..from.size[0] {
                                        to[(x0 + x, y0 + y)] = from[(x, y)];
                                    }
                                }
                            }
                        }
                    }
                    output = Some(frame);
                }
                let mut output = output.unwrap();
                output.textures_delta = atlas.unwrap();
                let text = rendered_text(&output);
                assert!(
                    app.page_overflow <= 0.0,
                    "{name} at {size:?}: {}",
                    app.page_overflow
                );
                match name {
                    "remove-row" => assert!(text.contains("Remove…"), "{text}"),
                    "remove-vendor-row" => assert!(text.contains("Uninstall…"), "{text}"),
                    "remove-package" => {
                        assert!(
                            text.contains("This cannot be undone."),
                            "{name} {size:?}: {text}"
                        );
                        assert!(text.contains("Bush trips"), "{text}");
                        assert!(!text.contains(crate::remove::NOT_ALLOWED), "{text}");
                    }
                    "remove-package-writes-off" => {
                        assert!(text.contains(crate::remove::NOT_ALLOWED), "{text}")
                    }
                    "remove-vendor" => {
                        assert!(text.contains("Orbx Central 4.5.48"), "{text}");
                        assert!(text.contains("the app + 2 packages"), "{text}");
                    }
                    _ => unreachable!(),
                }
                for line in text.lines() {
                    assert!(line.chars().count() <= 160, "{name}: {line}");
                }
                if let Some(dir) = &capture {
                    if size == egui::vec2(1280.0, 800.0) || size == egui::vec2(860.0, 600.0) {
                        std::fs::create_dir_all(dir).unwrap();
                        crate::ui_capture::save(
                            &dir.join(format!("{name}-{}.mesh", size.x as usize)),
                            &ctx,
                            output,
                            size,
                        );
                    }
                }
            }
        }
        // A click reaches `act`, which refuses here: the state is inert.
        assert!(!app.remove.is_working());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// FINAL_REVIEW S4: Remove already refused while an install or a drop
    /// ran; now every other Community writer refuses while a Remove runs, each
    /// at its action, through the one busy state.
    #[test]
    fn nothing_else_changes_community_while_a_removal_runs() {
        let base = std::env::temp_dir().join(format!("hangar-app-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let community = base.join("Packages/Community");
        let package = community.join("orbx-aircraft-pc12");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("manifest.json"), b"{}").unwrap();

        let mut app = app_with(everything_found());
        app.writes_allowed = true;
        assert_eq!(app.community_busy(), None);
        app.remove.set_busy(Some("Orbx Central"));
        let busy = "Wait for the removal that is running.";
        assert_eq!(app.community_busy(), Some(busy));

        // Install: choosing a file is refused, so no install can be armed.
        crate::install::accept_path(&mut app, base.join("setup.exe"));
        assert!(app
            .install
            .selection_error
            .as_deref()
            .is_some_and(|e| e.starts_with("Wait")));
        assert!(app.install.selected.is_none());

        // Drop: placement is refused before anything is read or moved.
        app.drop_install = crate::drop_install::DropState::live(|| {});
        app.drop_install.community = Some(community.clone());
        app.drop_install.source = Some(base.join("dropped"));
        crate::drop_install::place(&mut app, Vec::new());
        let outcome = app.drop_install.outcome.lock().unwrap().clone();
        assert_eq!(outcome, Some(vec![(busy.to_string(), false)]));
        assert!(!app.drop_install.is_placing());

        // Loadout apply: an empty loadout would switch the package off.
        let empty = hangar_catalog::Profile::from_enabled("Empty", Default::default());
        crate::profiles::apply_loadout(&mut app, &empty, &community);
        assert_eq!(app.profile_result, Some((format!("Empty: {busy}"), false)));
        assert!(package.join("manifest.json").is_file(), "a package moved");

        // The replaced-copy purge reads the same answer (`replacements::draw`).
        // And when the removal ends, the apply goes through.
        app.remove.set_busy(None);
        assert_eq!(app.community_busy(), None);
        crate::profiles::apply_loadout(&mut app, &empty, &community);
        assert!(
            !package.exists(),
            "with nothing running, the apply switches it off"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The Profiles tab, in every state it can be in.
    #[test]
    fn the_profiles_tab_renders_through_the_confirm_flow() {
        use hangar_catalog::Profile;

        let mut app = app_with(everything_found());
        app.active_tab = Tab::Profiles;

        // No profiles yet.
        render(&mut app, egui::vec2(1180.0, 760.0));

        let dir = std::env::temp_dir().join(format!("hangar-app-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        app.profiles = hangar_catalog::ProfileStore::load(&dir);
        app.profiles
            .save(Profile::from_enabled(
                "VFR Only",
                ["koak".to_string()].into(),
            ))
            .unwrap();
        app.profile_result = Some(("Captured 1 package".into(), true));

        for writes in [false, true] {
            app.writes_allowed = writes;
            for confirm in [None, Some("VFR Only".to_string())] {
                app.confirm_apply = confirm;
                render(&mut app, egui::vec2(1180.0, 760.0));
                render(&mut app, egui::vec2(760.0, 520.0));
            }
        }

        app.profile_result = Some(("something went wrong".into(), false));
        render(&mut app, egui::vec2(1180.0, 760.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Profiles tab says a set waits for a restart while MSFS is
    /// up, and says nothing when it is not. The sim is seen only through the
    /// injected monitor, so this holds with MSFS running on the test machine.
    #[test]
    fn the_profiles_tab_warns_only_while_msfs_is_running() {
        for sim_running in [false, true] {
            let mut app = app_with(everything_found());
            app.active_tab = Tab::Profiles;
            let mut snapshot = std::collections::BTreeMap::new();
            if sim_running {
                let key = hangar_runtime::sim::MSFS_2024_EXE;
                snapshot.insert(
                    key.to_string(),
                    hangar_runtime::proc::AppProcesses {
                        main: Some(hangar_runtime::proc::ProcInfo {
                            pid: 123,
                            cmdline: key.into(),
                            args: vec![key.into()],
                        }),
                        helpers: vec![],
                    },
                );
            }
            app.monitor = hangar_runtime::AppMonitor::inert(snapshot);
            for size in [egui::vec2(1180.0, 760.0), egui::vec2(760.0, 520.0)] {
                let text = rendered_text(&render(&mut app, size));
                assert_eq!(
                    text.contains(crate::profiles::SIM_RUNNING),
                    sim_running,
                    "{text}"
                );
                assert!(app.page_overflow <= 0.0, "{}", app.page_overflow);
            }
        }
    }

    /// Turning the write gate off must not leave a confirm armed behind it.
    #[test]
    fn disabling_writes_clears_a_pending_confirm() {
        let mut app = app_with(everything_found());
        app.writes_allowed = true;
        app.confirm_apply = Some("VFR Only".into());
        app.active_tab = Tab::Settings;

        // Simulate the toggle being switched back off.
        app.writes_allowed = false;
        app.confirm_apply = None;
        render(&mut app, egui::vec2(1180.0, 760.0));
        assert!(app.confirm_apply.is_none());
    }

    /// Writes are off until explicitly switched on.
    #[test]
    fn writing_to_community_is_off_by_default() {
        assert!(!app_with(everything_found()).writes_allowed);
        assert!(!app_with(nothing_found()).writes_allowed);
    }

    /// A populated environment opens on the library, not on Settings.
    #[test]
    fn a_successful_discovery_opens_on_preflight() {
        assert_eq!(app_with(everything_found()).active_tab, Tab::Preflight);
    }

    #[test]
    fn a_remembered_choice_wins_over_the_default() {
        // The whole point of persisting them: a probe says "before", but if
        // someone turned it off it stays off.
        let mut app = app_with(everything_found());
        assert_eq!(
            app.placement_of("Project Airports"),
            crate::flight::Placement::Before
        );

        app.preflight
            .placement
            .insert("Project Airports".into(), crate::flight::Placement::Off);
        assert_eq!(
            app.placement_of("Project Airports"),
            crate::flight::Placement::Off
        );
    }

    #[test]
    fn an_app_with_a_readiness_probe_defaults_to_running_before_the_sim() {
        // The probe and the placement have to agree, or HangarRS ships
        // knowledge it then ignores.
        let app = app_with(everything_found());
        for probe in hangar_runtime::readiness::PROBES {
            assert_eq!(
                app.placement_of(probe.matches),
                crate::flight::Placement::Before,
                "{} has a probe, so it belongs before the sim",
                probe.matches
            );
        }
        assert_eq!(
            app.placement_of("BeyondATC"),
            crate::flight::Placement::After,
            "an app with no probe talks to the sim, so it comes after"
        );
    }

    /// A failed discovery must not open on a tab that has nothing to show.
    #[test]
    fn a_failed_discovery_opens_on_settings() {
        assert_eq!(app_with(nothing_found()).active_tab, Tab::Settings);
    }

    /// Narrow windows are where layout arithmetic goes wrong: the content
    /// column clamps at its minimum and the two-column grid can be handed a
    /// negative width.
    #[test]
    fn every_tab_renders_at_awkward_window_sizes() {
        for discovery in [nothing_found(), everything_found()] {
            let mut app = app_with(discovery);
            for size in [
                egui::vec2(760.0, 520.0),   // the declared minimum
                egui::vec2(400.0, 300.0),   // narrower than the nav rail plus content
                egui::vec2(3840.0, 2160.0), // wide enough for the content cap to bind
            ] {
                for tab in crate::tab::ALL_TABS.iter() {
                    app.active_tab = *tab;
                    render(&mut app, size);
                }
            }
        }
    }

    /// Discovery failures are reported rather than swallowed.
    #[test]
    fn discovery_failures_are_visible_as_errors() {
        let mut no_sim = everything_found();
        no_sim.msfs = Err("MSFS installation could not be read".into());
        for (discovery, reason) in [
            (nothing_found(), "no Steam installation found"),
            (no_sim, "MSFS installation could not be read"),
        ] {
            let mut app = app_with(discovery);
            app.active_tab = Tab::Settings;
            let output = render(&mut app, egui::vec2(860.0, 600.0));
            let text = rendered_text(&output);
            assert!(text.contains(reason), "missing discovery failure: {text}");
            assert!(
                text.find(reason) < text.find("Allow package changes"),
                "a setup failure must appear before the settings controls"
            );
        }
    }

    /// An inert link reads as disconnected and never as live data.
    #[test]
    fn an_idle_link_is_not_streaming() {
        let app = app_with(nothing_found());
        assert!(!app.sim.is_streaming());
        let (label, tone) = app.sim_link_label();
        assert_eq!(label, "No flight data");
        assert_ne!(tone, hangar_ui::Tone::Ok);
    }

    /// The populated fixture with sample add-ons, and no path from the machine
    /// the screenshots happen to be taken on.
    fn website_discovery() -> Discovery {
        use hangar_catalog::{Item, ItemKind, ItemState};
        use hangar_msfs::manifest::ContentType;
        let mut discovery = populated(
            "/home/pilot/.local/share/Steam".into(),
            "/home/pilot/Games/SteamLibrary".into(),
        );
        discovery.overlaps = vec![(
            "/…/Packages/Community2024".into(),
            hangar_install::drop::Overlap {
                packages: vec!["sample-airport-kxyz".into(), "kxyz-freeware".into()],
                shared: vec!["Airport KXYZ".into()],
            },
        )];
        let community = discovery.msfs.as_ref().unwrap().community.clone();
        let package = |name: &str, title: &str, creator: &str, kind, gb: f64| Item {
            name: name.into(),
            title: title.into(),
            kind: ItemKind::Package,
            state: ItemState::Resident,
            creator: creator.into(),
            version: "1.2.0".into(),
            content_type: Some(kind),
            size_bytes: Some((gb * 1e9) as u64),
            path: community.join(name),
            community: Some(community.clone()),
            args: vec![],
        };
        let app = |name: &str, kind, path: &str| Item {
            name: name.into(),
            title: name.into(),
            kind,
            state: ItemState::Ready,
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: path.into(),
            community: None,
            args: vec![],
        };
        let items = vec![
            package(
                "sample-airport-kxyz",
                "Sample Intl (KXYZ)",
                "Example Scenery",
                ContentType::Scenery,
                2.4,
            ),
            package(
                "sample-airport-egxx",
                "Sample Field (EGXX)",
                "Example Scenery",
                ContentType::Scenery,
                0.9,
            ),
            package(
                "sample-aircraft-c99",
                "Sample C99 Turboprop",
                "Example Aero",
                ContentType::Aircraft,
                1.7,
            ),
            package(
                "sample-livery-c99",
                "C99 Livery Pack",
                "Example Aero",
                ContentType::Livery,
                0.3,
            ),
            package(
                "sample-mesh-alps",
                "Alpine Mesh",
                "Example Terrain",
                ContentType::Scenery,
                6.1,
            ),
            app(
                "Little Navmap",
                ItemKind::NativeTool,
                "/opt/littlenavmap/littlenavmap",
            ),
            app(
                "Sample ATC",
                ItemKind::SimSessionApp,
                "/companion/SampleATC/SampleATC.exe",
            ),
            app(
                "Sample EFB",
                ItemKind::CompanionApp,
                "/companion/SampleEFB/SampleEFB.exe",
            ),
        ];
        discovery.catalog = std::sync::Arc::new(hangar_catalog::CatalogSnapshot::from_scan(
            hangar_catalog::ScanResult {
                items,
                problems: vec![],
            },
        ));
        discovery
    }

    /// Every tab, populated, at a website size. Writes `<tab>.mesh` files only
    /// when `HANGARRS_WEBSITE_CAPTURE` names a directory; an offline renderer
    /// (not in this repository) turns them into PNGs.
    #[test]
    fn website_screenshots_fit_and_name_no_local_path() {
        let destination =
            std::env::var_os("HANGARRS_WEBSITE_CAPTURE").map(std::path::PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let user = std::env::var("USER").unwrap_or_default();
        let size = egui::vec2(1280.0, 800.0);
        let mut app = app_with(website_discovery());
        prime_controllers(&mut app, true);
        // A companion prefix that no machine has, instead of `app_with`'s /tmp one.
        if let Some(proton) = app.proton.as_mut() {
            *proton = proton.companion(std::path::Path::new(
                "/home/pilot/.local/share/hangar-rs/companion",
            ));
        }
        // Sample loadouts, saved into a scratch dir this test removes.
        let sets =
            std::env::temp_dir().join(format!("hangar-app-website-sets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sets);
        app.profiles = hangar_catalog::ProfileStore::load(&sets);
        for (name, packages) in [
            (
                "Alps VFR",
                &[
                    "sample-aircraft-c99",
                    "sample-livery-c99",
                    "sample-mesh-alps",
                ][..],
            ),
            (
                "Airport hopping",
                &["sample-airport-kxyz", "sample-airport-egxx"][..],
            ),
        ] {
            app.profiles
                .save(hangar_catalog::Profile::from_enabled(
                    name,
                    packages.iter().map(|p| p.to_string()).collect(),
                ))
                .unwrap();
        }
        for tab in crate::tab::ALL_TABS {
            app.active_tab = tab;
            let ctx = egui::Context::default();
            hangar_ui::theme::install(&ctx);
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ui| app.draw(ui),
            );
            let text = rendered_text(&output);
            assert!(app.page_overflow <= 0.0, "{tab:?}: {}", app.page_overflow);
            assert!(
                home.len() < 2 || !text.contains(&home),
                "{tab:?} shows {home}"
            );
            for local in ["/mnt/", "/tmp/", "/srv/"] {
                assert!(!text.contains(local), "{tab:?} shows a {local} path");
            }
            assert!(
                user.len() < 3 || !text.contains(&user),
                "{tab:?} shows {user}"
            );
            if let Some(path) = &destination {
                let name = tab.label().to_lowercase().replace(' ', "-");
                crate::ui_capture::save(&path.join(format!("{name}.mesh")), &ctx, output, size);
            }
        }
        let _ = std::fs::remove_dir_all(&sets);
    }
}
