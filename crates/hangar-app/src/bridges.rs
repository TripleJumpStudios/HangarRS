//! The Bridges tab — telemetry out to LittleNavMap and an EFB.
//!
//! The on/off switches for `hangar-bridges`' two output formats.
//!
//! # Nothing here opens its own link to the sim
//!
//! Both bridges are fed by [`crate::env::TelemetryLink::source`], a clone of
//! the one watch channel the status bar and the map read, so the sim sees one
//! SimConnect client however many bridges run. The standalone `efbconnect`
//! opens its own link; don't run it alongside this application.
//!
//! # A constructor must not open a socket
//!
//! [`BridgeState::inert`] is what `HangarApp::new` gets, and it refuses to
//! start anything. Only [`BridgeState::live`], built by `main.rs`, can bind a
//! port; otherwise every render test would bind TCP 51968 and broadcast UDP to
//! the subnet.
//!
//! # What can honestly be reported
//!
//! **LittleNavMap connects to us**, and `lnm_server` only asks for a frame
//! while a client is attached, so a recent poll is evidence of a client.
//!
//! **An EFB never connects.** Garmin Pilot and ForeFlight listen on UDP 49002
//! and say nothing back, so the EFB card reports *broadcasting*, never
//! *connected*.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hangar_bridges::serve::{self, Source};
use hangar_core::config::{Config, EfbBridge, LnmBridge};
use hangar_ui as ui;

use crate::app::HangarApp;

/// Which of the two bridges something refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Lnm,
    Efb,
}

/// How long a poll counts as recent when deciding whether LittleNavMap is
/// attached.
///
/// Generous next to the 5 Hz default so a bridge does not blink "disconnected"
/// between two frames, and short enough that closing LittleNavMap shows within
/// a couple of seconds.
const ATTACHED_WITHIN: Duration = Duration::from_secs(3);

/// What a bridge has actually been asked for, as opposed to what it was
/// configured to do.
///
/// Counted by wrapping the [`Source`] closure, the one place both bridges pass
/// through; neither protocol reports anything back.
#[derive(Clone, Default)]
pub struct Meter(Arc<Mutex<Counts>>);

#[derive(Clone, Copy, Default, Debug)]
pub struct Counts {
    /// Times the bridge asked for a frame.
    pub polls: u64,
    /// How many of those were recent enough that a packet went out.
    pub frames: u64,
    pub last_poll: Option<Instant>,
    pub last_frame: Option<Instant>,
}

impl Meter {
    /// Wrap a source so every request through it is counted.
    pub fn wrap(&self, inner: Source) -> Source {
        let meter = self.clone();
        Arc::new(move || {
            let frame = inner();
            meter.note(frame.as_ref());
            frame
        })
    }

    fn note(&self, frame: Option<&simconnect_tcp::Telemetry>) {
        let mut counts = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        counts.polls += 1;
        counts.last_poll = Some(now);
        // The same staleness rule `serve::fresh` applies, so "frames" means
        // packets that went out rather than ticks that happened.
        if frame.is_some_and(|t| !t.is_stale(serve::STALE_AFTER)) {
            counts.frames += 1;
            counts.last_frame = Some(now);
        }
    }

    pub fn read(&self) -> Counts {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Counts {
    /// Whether something has asked this bridge for a frame lately.
    ///
    /// For LittleNavMap that means a client is attached. For the EFB it means
    /// only that the loop is turning — see the module note.
    pub fn polled_recently(&self) -> bool {
        self.last_poll
            .is_some_and(|at| at.elapsed() < ATTACHED_WITHIN)
    }

    pub fn sending(&self) -> bool {
        self.last_frame
            .is_some_and(|at| at.elapsed() < ATTACHED_WITHIN)
    }
}

/// One bridge that is up.
struct Running {
    handle: tokio::task::JoinHandle<()>,
    since: Instant,
    meter: Meter,
    /// Set if the server future returned. A bind failure lands here
    /// immediately; otherwise these loops run until aborted.
    stopped: Arc<Mutex<Option<String>>>,
    /// What it was started with, so a config change can be noticed.
    port: u16,
    rate_hz: u32,
    /// EFB only. `None` for LittleNavMap, which embeds no name.
    app_name: Option<String>,
}

impl Running {
    fn stopped_with(&self) -> Option<String> {
        self.stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for Running {
    /// Dropping a `JoinHandle` **detaches** the task rather than cancelling it,
    /// so a bridge switched off without this would go on holding its port and
    /// broadcasting to the network for the life of the process.
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// The two bridges, and whether this state is allowed to start them.
pub struct BridgeState {
    /// False for [`BridgeState::inert`]. See the module note.
    live: bool,
    /// Called when a bridge stops on its own, so the UI repaints and says so.
    wake: Arc<dyn Fn() + Send + Sync>,
    lnm: Option<Running>,
    efb: Option<Running>,
}

impl BridgeState {
    /// A state that can never open a socket. What `HangarApp::new` gets.
    pub fn inert() -> Self {
        Self {
            live: false,
            wake: Arc::new(|| {}),
            lnm: None,
            efb: None,
        }
    }

    /// A state that can. Built by `main.rs`, once there is a window.
    pub fn live(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            live: true,
            wake: Arc::new(wake),
            lnm: None,
            efb: None,
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    fn running(&self, which: Which) -> Option<&Running> {
        match which {
            Which::Lnm => self.lnm.as_ref(),
            Which::Efb => self.efb.as_ref(),
        }
    }

    /// Whether a bridge is up, whatever the config says it should be.
    pub fn is_running(&self, which: Which) -> bool {
        self.running(which)
            .is_some_and(|r| r.stopped_with().is_none())
    }

    /// The reason a bridge stopped by itself, usually a port already in use.
    pub fn failure(&self, which: Which) -> Option<String> {
        self.running(which).and_then(Running::stopped_with)
    }

    pub fn counts(&self, which: Which) -> Option<Counts> {
        self.running(which).map(|r| r.meter.read())
    }

    pub fn uptime(&self, which: Which) -> Option<Duration> {
        self.running(which).map(|r| r.since.elapsed())
    }

    /// Bring both bridges into line with what the config asks for.
    ///
    /// Idempotent, so it is safe to call every frame: a bridge already running
    /// with the settings it was started with is left entirely alone. A settings
    /// change restarts it, because neither `lnm_server` nor `efb_broadcast` can
    /// be re-pointed once it has bound.
    pub fn sync(&mut self, config: &Config, source: &Source) {
        self.sync_lnm(&config.bridges.lnm, source);
        self.sync_efb(&config.bridges.efb, source);
    }

    fn sync_lnm(&mut self, want: &LnmBridge, source: &Source) {
        let matches = self.lnm.as_ref().is_some_and(|r| {
            r.stopped_with().is_none() && r.port == want.port && r.rate_hz == want.rate_hz
        });
        if !want.enabled {
            // A failed bridge is kept so its reason stays on screen; only an
            // explicit switch-off clears it.
            if self
                .lnm
                .as_ref()
                .is_some_and(|r| r.stopped_with().is_none())
            {
                self.lnm = None;
            }
            return;
        }
        if matches || !self.live {
            return;
        }
        // A dead one is replaced rather than left holding the card.
        self.lnm = None;

        let meter = Meter::default();
        let stopped = Arc::new(Mutex::new(None));
        let metered = meter.wrap(source.clone());
        let (port, rate_hz) = (want.port, want.rate_hz);
        let handle = tokio::spawn({
            let stopped = Arc::clone(&stopped);
            let wake = Arc::clone(&self.wake);
            async move {
                let reason = match serve::lnm_server(port, metered, rate_hz).await {
                    Ok(()) => "the listener closed".to_string(),
                    Err(e) => e.to_string(),
                };
                *stopped.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
                wake();
            }
        });
        self.lnm = Some(Running {
            handle,
            since: Instant::now(),
            meter,
            stopped,
            port,
            rate_hz,
            app_name: None,
        });
    }

    fn sync_efb(&mut self, want: &EfbBridge, source: &Source) {
        let matches = self.efb.as_ref().is_some_and(|r| {
            r.stopped_with().is_none()
                && r.port == want.port
                && r.rate_hz == want.rate_hz
                && r.app_name.as_deref() == Some(want.app_name.as_str())
        });
        if !want.enabled {
            if self
                .efb
                .as_ref()
                .is_some_and(|r| r.stopped_with().is_none())
            {
                self.efb = None;
            }
            return;
        }
        if matches || !self.live {
            return;
        }
        self.efb = None;

        let meter = Meter::default();
        let stopped = Arc::new(Mutex::new(None));
        let metered = meter.wrap(source.clone());
        let (port, rate_hz, app_name) = (want.port, want.rate_hz, want.app_name.clone());
        let handle = tokio::spawn({
            let stopped = Arc::clone(&stopped);
            let wake = Arc::clone(&self.wake);
            let app_name = app_name.clone();
            async move {
                let reason = match serve::efb_broadcast(port, app_name, metered, rate_hz).await {
                    Ok(()) => "the socket closed".to_string(),
                    Err(e) => e.to_string(),
                };
                *stopped.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
                wake();
            }
        });
        self.efb = Some(Running {
            handle,
            since: Instant::now(),
            meter,
            stopped,
            port,
            rate_hz,
            app_name: Some(app_name),
        });
    }
}

// ── the tab ──────────────────────────────────────────────────────────────────

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    if let Some(reason) = app.config_problem.clone() {
        malformed_config(ui, &reason);
        ui.add_space(ui::ROW_GAP);
    }

    let mut changed = false;
    changed |= efb_card(app, ui);
    ui.add_space(ui::ROW_GAP);
    changed |= lnm_card(app, ui);
    ui.add_space(ui::ROW_GAP);
    ui::controls::disclosure(ui, "connection-feed", "Flight data", |ui| {
        feed_card(app, ui)
    });

    if changed {
        app.save_config();
        app.sync_bridges();
    }
}

/// A config that will not parse is not a config that says "bridges off".
///
/// Same distinction the catalog draws between missing and malformed: editing a
/// switch here would write a file that replaces one the person is halfway
/// through fixing by hand.
fn malformed_config(ui: &mut egui::Ui, reason: &str) {
    ui::group(ui, Some("Config"), |ui| {
        ui.horizontal(|ui| {
            ui::state(ui, "unreadable", ui::Tone::Err);
            ui.add(
                egui::Label::new(
                    egui::RichText::new("config.toml could not be read")
                        .size(ui::theme::TEXT_MD)
                        .color(ui::theme::TEXT_HI),
                )
                .truncate(),
            );
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(reason)
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Saving would overwrite the file that will not parse. Fix it by hand first.",
            )
            .size(ui::theme::TEXT_BODY)
            .color(ui::theme::WARN),
        );
    });
}

/// LittleNavMap: TCP, it connects to us, and a client can be observed.
fn lnm_card(app: &mut HangarApp, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    let running = app.bridges.is_running(Which::Lnm);
    let counts = app.bridges.counts(Which::Lnm);
    let failure = app.bridges.failure(Which::Lnm);
    let uptime = app.bridges.uptime(Which::Lnm);
    let live = app.bridges.is_live();

    // No group heading: `header` draws the title.
    ui::group(ui, None, |ui| {
        header(
            ui,
            "Remote Little Navmap",
            &mut app.config.bridges.lnm.enabled,
            &mut changed,
        );

        state_line(
            ui,
            live,
            running,
            &failure,
            uptime,
            if counts.is_some_and(|c| c.polled_recently()) {
                "Connected"
            } else {
                "Waiting for client"
            },
        );
        ui.add_space(8.0);
        ui::controls::disclosure(ui, "lnm-setup", "Setup", |ui| {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                port_stepper(ui, "Port", &mut app.config.bridges.lnm.port, &mut changed);
                ui.add_space(18.0);
                rate_stepper(ui, &mut app.config.bridges.lnm.rate_hz, &mut changed);
            });
        });

        if running {
            let counts = counts.unwrap_or_default();
            ui.add_space(6.0);
            // A poll only happens while a client is attached.
            packets_row(ui, counts);
        }
    });
    changed
}

/// The EFB: UDP broadcast, and nothing ever answers.
fn efb_card(app: &mut HangarApp, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    let running = app.bridges.is_running(Which::Efb);
    let counts = app.bridges.counts(Which::Efb);
    let failure = app.bridges.failure(Which::Efb);
    let uptime = app.bridges.uptime(Which::Efb);
    let live = app.bridges.is_live();

    ui::group(ui, None, |ui| {
        header(
            ui,
            "ForeFlight / Garmin Pilot",
            &mut app.config.bridges.efb.enabled,
            &mut changed,
        );

        state_line(
            ui,
            live,
            running,
            &failure,
            uptime,
            if counts.is_some_and(|c| c.sending()) {
                "Sending"
            } else {
                "Waiting for flight data"
            },
        );
        ui.add_space(8.0);
        ui::controls::disclosure(ui, "efb-setup", "Setup", |ui| {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                port_stepper(ui, "Port", &mut app.config.bridges.efb.port, &mut changed);
                ui.add_space(18.0);
                rate_stepper(ui, &mut app.config.bridges.efb.rate_hz, &mut changed);
            });
            if app.config.bridges.efb.port != 49002 {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Your tablet expects port 49002 and will not receive this feed.",
                    )
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::WARN),
                );
            }

            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui::heading(ui, "Device name");
                if ui::text_field(
                    ui,
                    &mut app.config.bridges.efb.app_name,
                    "HangarRS",
                    200.0_f32.min(ui.available_width()),
                )
                .changed()
                {
                    changed = true;
                }
            });

            standalone_card(ui);
        });

        if running {
            let counts = counts.unwrap_or_default();
            ui.add_space(6.0);
            // Not a "client" row: a broadcast send succeeds whether or not
            // any device is listening.
            ui::inline_value(ui, "Reception", |ui| {
                ui::state(ui, "cannot be known", ui::Tone::Idle);
            });
            packets_row(ui, counts);
        }
    });
    changed
}

/// Where the frames come from, and the one rule about it.
fn feed_card(app: &HangarApp, ui: &mut egui::Ui) {
    ui::group(ui, Some("Feed"), |ui| {
        let (label, tone) = match (app.sim.status().is_connected(), app.sim.is_streaming()) {
            (_, true) => ("streaming", ui::Tone::Ok),
            (true, false) => ("connected, nothing arriving", ui::Tone::Warn),
            (false, false) => ("not connected", ui::Tone::Idle),
        };
        ui::inline_value(ui, "SimConnect", |ui| {
            ui::state(ui, label, tone);
        });

        if app.sim.is_streaming() {
            let t = app.sim.telemetry();
            ui::inline_value(ui, "Position", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{:.4}, {:.4}   {:.0} ft   {:03.0}\u{b0} track",
                            t.latitude_deg, t.longitude_deg, t.altitude_ft, t.ground_track_deg,
                        ))
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::TEXT),
                    )
                    .truncate(),
                );
            });
        }
    });
}

/// The spare-machine case, and the warning that goes with it.
fn standalone_card(ui: &mut egui::Ui) {
    ui::group(ui, None, |ui| {
        ui.label(
            egui::RichText::new(
                "If efbconnect is already running, stop it before enabling the EFB bridge.",
            )
            .size(ui::theme::TEXT_BODY)
            .color(ui::theme::WARN),
        );
    });
}

// ── pieces ───────────────────────────────────────────────────────────────────

/// The connection name and its on/off switch.
fn header(ui: &mut egui::Ui, title: &str, enabled: &mut bool, changed: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .font(egui::FontId::new(ui::theme::TEXT_LG, ui::theme::semibold()))
                .color(ui::theme::TEXT_HI),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let on = *enabled;
            if ui::toggle_chip(ui, if on { "On" } else { "Off" }, on, ui::theme::ORANGE).clicked() {
                *enabled = !on;
                *changed = true;
            }
        });
    });
}

/// `−  51968  +`, clamped to the ports a user may bind.
fn port_stepper(ui: &mut egui::Ui, label: &str, port: &mut u16, changed: &mut bool) {
    ui.vertical(|ui| {
        ui::heading(ui, label);
        ui.add_space(2.0);
        let step = ui::stepper(ui, |ui| {
            ui.add_sized(
                egui::vec2(56.0, ui::theme::CONTROL_H),
                egui::Label::new(
                    egui::RichText::new(port.to_string())
                        .monospace()
                        .size(ui::theme::TEXT_MD)
                        .color(ui::theme::TEXT_HI),
                ),
            );
        });
        // Below 1024 needs root, which this application will never have and
        // should never ask for.
        match step {
            Some(ui::Step::Dec) => {
                *port = port.saturating_sub(1).max(1024);
                *changed = true;
            }
            Some(ui::Step::Inc) => {
                *port = port.saturating_add(1);
                *changed = true;
            }
            None => {}
        }
    });
}

/// Frames per second. Clamped at 1 for the same reason `serve::period` clamps
/// it: refusing to run a bridge over a typo is worse than running it slowly.
fn rate_stepper(ui: &mut egui::Ui, rate: &mut u32, changed: &mut bool) {
    ui.vertical(|ui| {
        ui::heading(ui, "Rate");
        ui.add_space(2.0);
        let step = ui::stepper(ui, |ui| {
            ui.add_sized(
                egui::vec2(56.0, ui::theme::CONTROL_H),
                egui::Label::new(
                    egui::RichText::new(format!("{rate} Hz"))
                        .monospace()
                        .size(ui::theme::TEXT_MD)
                        .color(ui::theme::TEXT_HI),
                ),
            );
        });
        match step {
            Some(ui::Step::Dec) => {
                *rate = rate.saturating_sub(1).max(1);
                *changed = true;
            }
            // The sim delivers about 30 Hz, so more than that would resend the
            // same frame — and every EFB datagram is a broadcast every device
            // on the network has to look at.
            Some(ui::Step::Inc) => {
                *rate = rate.saturating_add(1).min(30);
                *changed = true;
            }
            None => {}
        }
    });
}

/// Up, down, or refused — and the reason when it is the last one.
fn state_line(
    ui: &mut egui::Ui,
    live: bool,
    running: bool,
    failure: &Option<String>,
    uptime: Option<Duration>,
    active_label: &str,
) {
    ui.vertical(|ui| {
        if let Some(reason) = failure {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(reason.as_str())
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::ERR),
                )
                .truncate(),
            );
            ui::state(ui, "stopped", ui::Tone::Err);
        } else if running {
            if let Some(up) = uptime {
                ui.label(
                    egui::RichText::new(format!("up {}", duration(up)))
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(ui::theme::DIM),
                );
            }
            ui::state(ui, active_label, ui::Tone::Ok);
        } else if !live {
            // Only reachable from a headless render.
            ui::state(ui, "Stopped", ui::Tone::Idle);
        } else {
            ui::state(ui, "Stopped", ui::Tone::Idle);
        }
    });
}

fn packets_row(ui: &mut egui::Ui, counts: Counts) {
    ui::inline_value(ui, "Sent", |ui| {
        let tone = if counts.sending() {
            ui::theme::OK
        } else {
            ui::theme::DIM
        };
        ui.label(
            egui::RichText::new(format!(
                "{} packets / {} ticks",
                counts.frames, counts.polls
            ))
            .monospace()
            .size(ui::theme::TEXT_SM)
            .color(tone),
        );
    });
    if counts.polls > 0 && counts.frames == 0 {
        ui.label(
            egui::RichText::new(
                "Ticking, but every frame was too old to send \u{2014} the sim is not \
                 delivering telemetry.",
            )
            .size(ui::theme::TEXT_BODY)
            .color(ui::theme::WARN),
        );
    }
}

fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simconnect_tcp::Telemetry;

    fn frame(age: Option<Duration>) -> Option<Telemetry> {
        Some(Telemetry {
            latitude_deg: 47.5,
            updated_at: age.map(|a| Instant::now() - a),
            ..Default::default()
        })
    }

    /// Every render test depends on this: an inert state that started a
    /// bridge would bind TCP 51968 and broadcast UDP to the subnet.
    #[test]
    fn an_inert_state_starts_nothing_however_hard_the_config_asks() {
        let mut state = BridgeState::inert();
        let mut config = Config::default();
        config.bridges.lnm.enabled = true;
        config.bridges.efb.enabled = true;

        let source: Source = Arc::new(|| frame(Some(Duration::ZERO)));
        state.sync(&config, &source);

        assert!(!state.is_running(Which::Lnm));
        assert!(!state.is_running(Which::Efb));
        assert!(state.counts(Which::Lnm).is_none());
        assert!(state.counts(Which::Efb).is_none());
    }

    #[test]
    fn an_inert_state_says_it_is_inert() {
        assert!(!BridgeState::inert().is_live());
    }

    /// Ticks and packets are separate counts: polling steadily while sending
    /// nothing is the "sim stopped delivering" case.
    #[test]
    fn the_meter_separates_ticks_from_packets() {
        let meter = Meter::default();
        let fresh: Source = Arc::new(|| frame(Some(Duration::ZERO)));
        let wrapped = meter.wrap(fresh);
        for _ in 0..3 {
            let _ = wrapped();
        }
        let counts = meter.read();
        assert_eq!(counts.polls, 3);
        assert_eq!(counts.frames, 3);
        assert!(counts.sending());

        let meter = Meter::default();
        let old: Source = Arc::new(|| frame(Some(Duration::from_secs(5))));
        let wrapped = meter.wrap(old);
        for _ in 0..3 {
            let _ = wrapped();
        }
        let counts = meter.read();
        assert_eq!(counts.polls, 3, "the bridge still asked");
        assert_eq!(counts.frames, 0, "and nothing went out");
        assert!(counts.polled_recently());
        assert!(!counts.sending());
    }

    /// A frame that never arrived is not the same as one that arrived long ago,
    /// and neither is something to broadcast.
    #[test]
    fn a_frame_that_never_arrived_counts_as_a_tick_and_not_a_packet() {
        let meter = Meter::default();
        let never: Source = Arc::new(|| frame(None));
        let wrapped = meter.wrap(never);
        let _ = wrapped();
        assert_eq!(meter.read().polls, 1);
        assert_eq!(meter.read().frames, 0);

        let meter = Meter::default();
        let nothing: Source = Arc::new(|| None);
        let wrapped = meter.wrap(nothing);
        let _ = wrapped();
        assert_eq!(meter.read().polls, 1);
        assert_eq!(meter.read().frames, 0);
    }

    /// A default `Counts` must not read as a live client.
    #[test]
    fn nothing_seen_is_not_a_connection() {
        let counts = Counts::default();
        assert!(!counts.polled_recently());
        assert!(!counts.sending());
    }

    /// The wrapper must hand the frame through untouched — a meter that
    /// swallowed or altered a frame would break the formats it is measuring.
    #[test]
    fn wrapping_a_source_does_not_change_what_it_returns() {
        let meter = Meter::default();
        let wrapped = meter.wrap(Arc::new(|| frame(Some(Duration::ZERO))));
        let out = wrapped().expect("a frame");
        assert_eq!(out.latitude_deg, 47.5);
    }

    #[test]
    fn uptime_reads_in_units_a_person_uses() {
        assert_eq!(duration(Duration::from_secs(9)), "9s");
        assert_eq!(duration(Duration::from_secs(75)), "1m 15s");
        assert_eq!(duration(Duration::from_secs(3700)), "1h 1m");
    }

    /// The config default and the crate constant have to agree, or the bridge
    /// binds a port the client is not looking at.
    #[test]
    fn the_default_ports_match_the_protocols() {
        assert_eq!(LnmBridge::default().port, hangar_bridges::lnm::DEFAULT_PORT);
        assert_eq!(EfbBridge::default().port, hangar_bridges::efb::DEFAULT_PORT);
    }

    /// Both off by default: a bridge is a socket on the network, and nothing
    /// should open one because the application was started.
    #[test]
    fn both_bridges_are_off_until_switched_on() {
        let config = Config::default();
        assert!(!config.bridges.lnm.enabled);
        assert!(!config.bridges.efb.enabled);
    }
}
