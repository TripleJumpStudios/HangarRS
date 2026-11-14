//! HangarRS — the one-stop toolbox for flight sim on Linux.
//!
//! This binary and `hangar-ui` are the only crates allowed to depend on egui.
//! Everything else is headless and speaks over channels, which is what makes
//! the subsystems testable and reusable on their own.

mod app;
mod app_ids;
mod apps;
mod binding_tools;
mod bindings;
mod bridges;
mod console;
mod drop_install;
mod env;
mod flight;
mod flight_details;
mod hangar;
mod install;
mod journal;
mod map;
mod prefix;
mod preflight;
mod profiles;
mod remove;
mod replacements;
mod settings;
mod tab;

use app::HangarApp;
use env::{Discovery, TelemetryLink};
use hangar_ui as ui;
use std::sync::mpsc;

/// Run discovery off the UI thread so the splash animates instead of the
/// window hanging on a cold filesystem.
///
/// The result is always a [`Discovery`], never an error: a machine without
/// Steam or without MSFS still opens the app, which then says what is missing.
/// Failing to start would leave nowhere to report the reason.
fn spawn_boot() -> mpsc::Receiver<Discovery> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(Discovery::run());
    });
    rx
}

/// Startup narration for the splash, keyed on how long boot has been running.
///
/// Deliberately time-driven rather than tied to a real progress signal: the
/// alternative is a progress bar that sits at one value for two seconds, which
/// reads as a hang.
fn boot_status(elapsed: f32) -> &'static str {
    if elapsed < 0.8 {
        "STARTING UP"
    } else if elapsed < 2.0 {
        "FINDING STEAM"
    } else {
        "READING LIBRARY"
    }
}

/// Application lifecycle.
///
/// Three states rather than a bool: the fade exists so the main window does not
/// snap in at full brightness the instant the splash clears, and it needs to
/// draw the real app underneath while it runs.
enum State {
    Splash {
        rx: mpsc::Receiver<Discovery>,
        /// Held here once discovery returns, until `MIN_SPLASH_SECS` elapses.
        ///
        /// Boxed because `Discovery` carries two whole environment structs and
        /// would otherwise set the size of every variant of this enum.
        boot: Option<Box<Discovery>>,
        splash: ui::Splash,
    },
    FadingIn {
        app: Box<HangarApp>,
        started: std::time::Instant,
    },
    Running {
        app: Box<HangarApp>,
    },
}

struct Wrapper {
    /// `Option` so the state can be moved out and replaced during a frame.
    state: Option<State>,
}

impl eframe::App for Wrapper {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let Some(state) = self.state.take() else {
            debug_assert!(false, "wrapper state missing");
            return;
        };

        self.state = Some(match state {
            State::Splash {
                rx,
                mut boot,
                mut splash,
            } => {
                if boot.is_none() {
                    if let Ok(b) = rx.try_recv() {
                        boot = Some(Box::new(b));
                    }
                }
                let elapsed = splash.elapsed();
                let ready = boot.is_some();
                splash.show(ui, ready, boot_status(elapsed));

                if ready && elapsed >= ui::MIN_SPLASH_SECS {
                    // `HangarApp::new` builds every outside-world piece inert;
                    // this is the one place each is switched on. Headless
                    // crates get a wake callback, never an egui type.
                    let ctx = ui.ctx().clone();
                    let sim = TelemetryLink::spawn({
                        let ctx = ctx.clone();
                        move || ctx.request_repaint()
                    });
                    let monitor = hangar_runtime::AppMonitor::spawn(move || ctx.request_repaint());
                    let mut app =
                        HangarApp::new(*boot.expect("ready implies present"), sim, monitor);
                    // Launching or picking in Little Navmap still needs a click.
                    app.map = crate::map::MapState::live(&app.config);
                    // Binds a port and broadcasts to the subnet once enabled.
                    let ctx = ui.ctx().clone();
                    app.bridges = crate::bridges::BridgeState::live(move || ctx.request_repaint());
                    // Whatever the config already asked for comes up now.
                    app.sync_bridges();
                    let ctx = ui.ctx().clone();
                    app.actions = crate::apps::AppActions::live();
                    app.observer = crate::flight_details::Observer::live();
                    app.preflight.enable_saving();
                    app.binding_tools = crate::binding_tools::State::live();
                    app.replacements = crate::replacements::State::live();
                    app.remove = crate::remove::State::live();
                    // Starting Proton to initialise a companion prefix.
                    app.companion_setup = crate::prefix::SetupState::live();
                    // Can start a real installer writing into a live prefix.
                    app.install = crate::install::InstallState::live(move || ctx.request_repaint());
                    // Can copy a dropped folder or archive into Community.
                    let ctx = ui.ctx().clone();
                    app.drop_install =
                        crate::drop_install::DropState::live(move || ctx.request_repaint());
                    // A line written by a background reader must reach the
                    // screen while nothing else is driving a frame.
                    let ctx = ui.ctx().clone();
                    app.journal = crate::journal::Journal::with_wake(move || ctx.request_repaint());
                    State::FadingIn {
                        app: Box::new(app),
                        started: std::time::Instant::now(),
                    }
                } else {
                    State::Splash { rx, boot, splash }
                }
            }

            State::FadingIn { mut app, started } => {
                // Captured before the panels claim it; the root `Ui` is the
                // viewport (egui 0.35 has no `Context::screen_rect`).
                let viewport = ui.max_rect();
                app.draw(ui);

                const FADE_SECS: f32 = 0.30;
                let alpha = 1.0 - (started.elapsed().as_secs_f32() / FADE_SECS).clamp(0.0, 1.0);

                if alpha > 0.004 {
                    let a = (alpha * 255.0) as u8;
                    ui.ctx()
                        .layer_painter(egui::LayerId::new(
                            egui::Order::Foreground,
                            egui::Id::new("fade_overlay"),
                        ))
                        .rect_filled(viewport, 0.0, egui::Color32::from_black_alpha(a));
                    ui.ctx().request_repaint();
                    State::FadingIn { app, started }
                } else {
                    State::Running { app }
                }
            }

            State::Running { mut app } => {
                app.draw(ui);
                State::Running { app }
            }
        });
    }
}

fn main() -> eframe::Result {
    // Package checks must work without opening a window or starting discovery.
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--version")
    {
        println!("HangarRS {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // Held for the life of the process: dropping the guard would abort the
    // SimConnect client's reconnect loop.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _guard = runtime.enter();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "warn,hangar_app=info,hangar_steam=info,hangar_msfs=info".into()
            }),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HangarRS")
            // Wayland matches a .desktop file by app_id, not by title.
            .with_app_id("hangar-rs")
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "HangarRS",
        options,
        Box::new(|cc| {
            ui::theme::install(&cc.egui_ctx);
            Ok(Box::new(Wrapper {
                state: Some(State::Splash {
                    rx: spawn_boot(),
                    boot: None,
                    splash: ui::Splash::new(),
                }),
            }))
        }),
    )
}

#[cfg(test)]
mod ui_capture;
