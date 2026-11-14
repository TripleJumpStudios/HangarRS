//! Startup discovery, and the telemetry link.
//!
//! Discovery runs on a background thread so the splash can animate rather than
//! the window hanging on a cold filesystem. Both halves are **read-only**:
//! nothing here writes to Steam, the prefix, or the Community folder.

use hangar_catalog::CatalogSnapshot;
use hangar_msfs::MsfsEnv;
use hangar_steam::SteamEnv;
use simconnect_tcp::{Client, Config, Status, Telemetry};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

/// What startup found.
///
/// Steam failing is fatal to everything useful; MSFS failing is not — the app
/// still opens and says so, which is the difference between a diagnosable
/// install and a window that will not start.
pub struct Discovery {
    pub steam: Result<SteamEnv, String>,
    pub msfs: Result<MsfsEnv, String>,
    /// The library as of startup.
    ///
    /// Behind an `Arc` because the GUI reads it every frame and a rescan
    /// replaces it wholesale — the pointer swaps, the contents are never cloned
    /// per frame.
    pub catalog: Arc<CatalogSnapshot>,
    /// Installed packages that declare the same content as each other.
    ///
    /// Computed at scan time, never in a draw: it reads every package's
    /// `manifest.json` and `ContentHistory.json`. Empty on a failed discovery.
    pub overlaps: Vec<(PathBuf, hangar_install::drop::Overlap)>,
    /// The input profiles, parsed once.
    ///
    /// Loaded here rather than in `HangarApp::new` so a UI test, which builds
    /// a `Discovery` by hand, never depends on what is installed.
    pub bindings: Arc<hangar_bindings::Library>,
}

impl Discovery {
    pub fn run() -> Self {
        let steam = SteamEnv::discover().map_err(|e| e.to_string());
        let msfs = match &steam {
            Ok(steam) => MsfsEnv::discover(steam).map_err(|e| e.to_string()),
            Err(_) => Err("Steam was not found, so MSFS could not be located".into()),
        };
        // No package libraries are configured yet; the Community folders are
        // the whole library on this machine.
        let catalog = match &msfs {
            Ok(env) => Arc::new(CatalogSnapshot::from_scan(hangar_catalog::scan(env, &[]))),
            Err(_) => Arc::new(CatalogSnapshot::default()),
        };
        let overlaps = match &msfs {
            Ok(env) => env
                .communities
                .iter()
                .flat_map(|c| {
                    let installed = hangar_install::drop::installed_in(&c.path);
                    hangar_install::drop::overlaps(&installed)
                        .into_iter()
                        .map(move |o| (c.path.clone(), o))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let bindings = Arc::new(match &steam {
            Ok(steam) => hangar_bindings::Library::discover(steam),
            Err(_) => hangar_bindings::Library::default(),
        });
        Self {
            steam,
            msfs,
            catalog,
            overlaps,
            bindings,
        }
    }

    /// The Steam environment, if discovery succeeded.
    pub fn steam(&self) -> Option<&SteamEnv> {
        self.steam.as_ref().ok()
    }

    /// The MSFS environment, if discovery succeeded.
    pub fn msfs(&self) -> Option<&MsfsEnv> {
        self.msfs.as_ref().ok()
    }
}

/// The live link to a running sim.
///
/// Owns the SimConnect client and its two channels. The client reconnects for
/// as long as this lives; MSFS closes the port when you leave a flight while
/// the process keeps running.
pub struct TelemetryLink {
    /// `None` for an [`TelemetryLink::idle`] link, which never connects.
    _client: Option<Client>,
    /// Held so an idle link's receivers keep a live sender behind them.
    _idle: Option<(watch::Sender<Telemetry>, watch::Sender<Status>)>,
    telemetry: watch::Receiver<Telemetry>,
    status: watch::Receiver<Status>,
}

impl TelemetryLink {
    /// Start connecting. Returns immediately; the sim need not be running.
    ///
    /// `repaint` is the whole GUI integration, so `simconnect-tcp` needs no
    /// toolkit dependency.
    pub fn spawn(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let config = Config {
            app_name: "HangarRS".into(),
            ..Default::default()
        };
        let client = Client::spawn(config, Some(Arc::new(repaint)));
        Self {
            telemetry: client.telemetry(),
            status: client.status(),
            _client: Some(client),
            _idle: None,
        }
    }

    /// A link that never connects, for tests and headless rendering: no task,
    /// no socket, so a render test never attaches to a sim someone is flying.
    #[cfg(test)]
    pub fn idle() -> Self {
        let (telemetry_tx, telemetry) = watch::channel(Telemetry::default());
        let (status_tx, status) = watch::channel(Status::Disconnected);
        Self {
            _client: None,
            _idle: Some((telemetry_tx, status_tx)),
            telemetry,
            status,
        }
    }

    /// Current connection state.
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// A telemetry source for the output bridges.
    ///
    /// A clone of the same watch channel the status bar and the map read: a
    /// bridge never opens a second SimConnect client. (`efbconnect`, which
    /// does, is a separate binary; don't run both at once.)
    ///
    /// Staleness is not filtered here; `serve::fresh` applies
    /// [`hangar_bridges::serve::STALE_AFTER`], the one definition of "live".
    pub fn source(&self) -> hangar_bridges::serve::Source {
        let telemetry = self.telemetry.clone();
        Arc::new(move || Some(telemetry.borrow().clone()))
    }

    /// Latest snapshot, however old.
    pub fn telemetry(&self) -> Telemetry {
        self.telemetry.borrow().clone()
    }

    /// Whether the sim is connected *and* still sending.
    ///
    /// Distinct from [`Self::status`]: the socket can stay open while the sim
    /// is paused, in a menu or loading, and that is not live data.
    pub fn is_streaming(&self) -> bool {
        self.status.borrow().is_connected()
            && !self
                .telemetry
                .borrow()
                .is_stale(simconnect_tcp::DEFAULT_STALE_AFTER)
    }
}

/// Whether the window system will ever deliver a dropped file.
///
/// # winit 0.30 has no Wayland file-drop
///
/// winit 0.30.13 emits `WindowEvent::DroppedFile` only from its Windows, X11
/// and macOS backends, so on Wayland `RawInput::dropped_files` stays empty and
/// [`crate::install::take_dropped_file`] never runs. The Install tab uses this
/// to avoid promising a drag-and-drop the platform will not deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDrops {
    /// The backend emits `DroppedFile`.
    Available,
    /// A Wayland session. `x11_too` is whether `DISPLAY` is also set, which
    /// decides whether relaunching on Xwayland is advice or a dead end.
    NotOnWayland { x11_too: bool },
}

impl FileDrops {
    pub fn available(self) -> bool {
        self == FileDrops::Available
    }
}

/// What this process's environment says. Reads the same variables winit does.
pub fn file_drops() -> FileDrops {
    file_drops_from(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("WAYLAND_SOCKET").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    )
}

/// The rule, separated from the environment so it can be tested.
///
/// The same rule as winit's in `platform_impl/linux/mod.rs`: Wayland is
/// checked first (Xwayland makes X11 reachable too), and an empty variable
/// counts as not set, which is why `WAYLAND_DISPLAY= hangar-rs` works.
pub fn file_drops_from(
    wayland_display: Option<&str>,
    wayland_socket: Option<&str>,
    display: Option<&str>,
) -> FileDrops {
    let set = |v: Option<&str>| v.is_some_and(|v| !v.is_empty());
    if set(wayland_display) || set(wayland_socket) {
        FileDrops::NotOnWayland {
            x11_too: set(display),
        }
    } else {
        FileDrops::Available
    }
}

#[cfg(test)]
mod file_drop_tests {
    use super::*;

    #[test]
    fn a_wayland_session_cannot_receive_a_dropped_file() {
        assert_eq!(
            file_drops_from(Some("wayland-0"), None, Some(":0")),
            FileDrops::NotOnWayland { x11_too: true }
        );
        assert_eq!(
            file_drops_from(None, Some("4"), None),
            FileDrops::NotOnWayland { x11_too: false }
        );
    }

    #[test]
    fn an_x11_session_can() {
        assert!(file_drops_from(None, None, Some(":0")).available());
    }

    /// winit treats an empty variable as not set, so clearing it falls
    /// through to X11.
    #[test]
    fn an_empty_variable_is_not_a_wayland_session() {
        assert!(file_drops_from(Some(""), Some(""), Some(":0")).available());
    }

    /// No display at all is a headless render test, which must take the X11
    /// branch so the populated card is rendered.
    #[test]
    fn no_display_at_all_reads_as_available() {
        assert!(file_drops_from(None, None, None).available());
    }
}
