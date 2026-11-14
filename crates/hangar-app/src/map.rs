//! Little Navmap for MSFS, using the same app entry as Hangar and Preflight.

use crate::app::HangarApp;
use hangar_core::config::{AppEntry, AppKind, Config};
use hangar_ui as ui;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub kind: AppKind,
}

impl Target {
    fn at(exe: PathBuf) -> Self {
        Self {
            exe,
            args: Vec::new(),
            kind: AppKind::Companion,
        }
    }
}

#[derive(Default)]
enum Job {
    #[default]
    Idle,
    Busy,
    Chosen(Result<Option<PathBuf>, String>),
}

#[derive(Default)]
pub struct MapState {
    live: bool,
    pub(crate) target: Option<Target>,
    job: Arc<Mutex<Job>>,
    pub(crate) message: Option<(String, bool)>,
}

impl MapState {
    pub fn live(config: &Config) -> Self {
        Self {
            live: true,
            target: discover(config),
            ..Self::default()
        }
    }

    fn busy(&self) -> bool {
        matches!(
            *self.job.lock().unwrap_or_else(|e| e.into_inner()),
            Job::Busy
        )
    }

    fn submit(&mut self, work: impl FnOnce() -> Job + Send + 'static, ctx: &egui::Context) {
        if !self.live {
            self.message = Some((
                "Little Navmap actions are unavailable in this session.".into(),
                false,
            ));
            return;
        }
        let mut job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*job, Job::Idle) {
            return;
        }
        *job = Job::Busy;
        self.message = None;
        let result = Arc::clone(&self.job);
        let ctx = ctx.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("little-navmap".into())
            .spawn(move || {
                *result.lock().unwrap_or_else(|e| e.into_inner()) = work();
                ctx.request_repaint();
            })
        {
            *job = Job::Idle;
            self.message = Some((format!("Could not start the action: {error}"), false));
        }
    }

    fn choose(&mut self, ctx: &egui::Context) {
        let directory = self
            .target
            .as_ref()
            .and_then(|target| target.exe.parent())
            .map(Path::to_owned);
        self.submit(
            move || {
                let mut picker = rfd::FileDialog::new()
                    .set_title("Choose Little Navmap for MSFS")
                    .add_filter("Windows executable", &["exe"]);
                if let Some(directory) = directory {
                    picker = picker.set_directory(directory);
                }
                Job::Chosen(
                    picker
                        .pick_file()
                        .map(|path| validate(&path).map(|()| path))
                        .transpose(),
                )
            },
            ctx,
        );
    }

    fn take_result(&self) -> Job {
        let mut job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*job, Job::Chosen(_)) {
            std::mem::take(&mut *job)
        } else {
            Job::Idle
        }
    }
}

fn validate(path: &Path) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    if !path.is_absolute() {
        return Err("Choose Little Navmap using its full file path.".into());
    }
    if !path.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err("The Little Navmap executable was not found.".into());
    }
    let mut file = std::fs::File::open(path)
        .map_err(|_| "The Little Navmap executable was not found.".to_owned())?;
    let wrong_build = || "Choose the Windows 64-bit Little Navmap executable for MSFS.".to_owned();
    let mut dos = [0; 64];
    file.read_exact(&mut dos).map_err(|_| wrong_build())?;
    if &dos[..2] != b"MZ" {
        return Err(wrong_build());
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
    file.seek(SeekFrom::Start(u64::from(offset)))
        .map_err(|_| wrong_build())?;
    let mut pe = [0; 26];
    file.read_exact(&mut pe).map_err(|_| wrong_build())?;
    if &pe[..4] != b"PE\0\0" || pe[4..6] != [0x64, 0x86] || pe[24..26] != [0x0b, 0x02] {
        return Err(wrong_build());
    }
    Ok(())
}

fn choose_target(config: &Config, candidates: impl IntoIterator<Item = PathBuf>) -> Option<Target> {
    // A selected but missing path stays visible so it can be repaired.
    if let Some(path) = &config.navigation.littlenavmap {
        return Some(target_for(config, path));
    }
    if let Some(app) = config.apps.iter().find(|app| {
        app.enabled
            && app.kind != AppKind::Native
            && (app.name.to_lowercase().replace(' ', "") == "littlenavmap"
                || app.exe.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .eq_ignore_ascii_case("littlenavmap.exe")
                }))
    }) {
        return Some(Target {
            exe: app.exe.clone(),
            args: app.args.clone(),
            kind: app.kind,
        });
    }
    candidates
        .into_iter()
        .find(|path| validate(path).is_ok())
        .map(Target::at)
}

fn discover(config: &Config) -> Option<Target> {
    let mut candidates = Vec::new();
    if let Some(data) = dirs::data_local_dir() {
        candidates.push(data.join("hangar-rs/littlenavmap-msfs/littlenavmap.exe"));
    }
    if let Some((dir, _)) = config.companion_prefix() {
        candidates.push(dir.join("pfx/drive_c/Program Files/Little Navmap/littlenavmap.exe"));
    }
    choose_target(config, candidates)
}

fn target_for(config: &Config, path: &Path) -> Target {
    config
        .apps
        .iter()
        .find(|entry| entry.exe == path)
        .map_or_else(
            || Target::at(path.to_owned()),
            |entry| Target {
                exe: entry.exe.clone(),
                args: entry.args.clone(),
                kind: entry.kind,
            },
        )
}

/// Register the selected executable once; retain its name, arguments and launch mode.
fn register(config: &mut Config, path: &Path) {
    if let Some(entry) = config.apps.iter_mut().find(|entry| entry.exe == path) {
        entry.enabled = true;
        // A Windows PE selected here cannot be a native Linux process.
        if entry.kind == AppKind::Native {
            entry.kind = AppKind::Companion;
        }
    } else {
        let base = if config
            .apps
            .iter()
            .any(|entry| entry.name == "Little Navmap")
        {
            "Little Navmap MSFS"
        } else {
            "Little Navmap"
        };
        let mut name = base.to_owned();
        let mut suffix = 2;
        while config.apps.iter().any(|entry| entry.name == name) {
            name = format!("{base} ({suffix})");
            suffix += 1;
        }
        config.apps.push(AppEntry {
            name,
            exe: path.to_owned(),
            args: vec![],
            kind: AppKind::Companion,
            uninstaller: None,
            enabled: true,
        });
    }
}

pub(crate) fn open(app: &mut HangarApp, ctx: &egui::Context) -> Result<(), String> {
    if !app.map.live {
        return Err("Little Navmap actions are unavailable in this session.".into());
    }
    if app.map.busy() {
        return Err("Wait for the current Little Navmap action.".into());
    }
    let target = app
        .map
        .target
        .clone()
        .ok_or("Choose Little Navmap first.")?;
    validate(&target.exe)?;
    // Auto-detected installations join the same catalog used by Preflight.
    if !app
        .config
        .apps
        .iter()
        .any(|entry| entry.enabled && entry.exe == target.exe)
    {
        select(app, Some(target.exe.clone()))?;
    }
    let entry = app
        .config
        .apps
        .iter()
        .find(|entry| entry.enabled && entry.exe == target.exe)
        .ok_or("Little Navmap is missing from Apps.")?;
    if app.actions.is_busy(&entry.name) {
        return Err("Little Navmap is already starting.".into());
    }
    if entry.kind == AppKind::Native {
        return Err("Choose the Windows 64-bit Little Navmap executable for MSFS.".into());
    }
    let env = if entry.kind == AppKind::SimSession {
        hangar_runtime::ProtonEnv::for_msfs(
            app.discovery.msfs().ok_or("MSFS was not found.")?,
            app.discovery.steam().ok_or("Steam was not found.")?,
        )
    } else {
        app.proton.clone()
    }
    .ok_or("The MSFS Proton setup is unavailable.")?;
    let item = hangar_catalog::apps::item(entry);
    let ctx = ctx.clone();
    app.actions.clear_result();
    crate::apps::spawn_launch(&app.actions, env, &item, &app.journal, move || {
        ctx.request_repaint()
    });
    Ok(())
}

/// Selection and persistence are one guarded action; a failed save keeps the
/// previous choice in memory and on screen. None restores auto-discovery.
pub(crate) fn select(app: &mut HangarApp, path: Option<PathBuf>) -> Result<(), String> {
    if !app.map.live {
        return Err("Little Navmap actions are unavailable in this session.".into());
    }
    if app.config_problem.is_some() {
        return Err("Repair the configuration in Settings before changing this choice.".into());
    }
    if let Some(path) = &path {
        validate(path)?;
    }
    let mut config = app.config.clone();
    if let Some(path) = &path {
        register(&mut config, path);
    }
    config.navigation.littlenavmap = path;
    let destination = app
        .config_path
        .as_ref()
        .ok_or_else(|| "The configuration location is unavailable.".to_owned())?;
    config
        .write_to(destination)
        .map_err(|e| format!("Could not save Little Navmap: {e}"))?;
    app.map.target = discover(&config);
    app.config = config;
    app.rescan_requested = true;
    Ok(())
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    match app.map.take_result() {
        Job::Chosen(Ok(Some(path))) => {
            if let Err(error) = select(app, Some(path)) {
                app.map.message = Some((error, false));
            }
        }
        Job::Chosen(Err(error)) => app.map.message = Some((error, false)),
        _ => {}
    }
    let busy = app.map.busy();
    ui::page_header(ui, "Map", |_ui| {});
    ui::heading(ui, "Little Navmap");
    ui.add_space(12.0);
    ui::group(ui, None, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui::tool_button_enabled(
                ui,
                !busy && app.map.target.is_some(),
                egui_phosphor::regular::ARROW_SQUARE_OUT,
                "Open Little Navmap",
                ui::ButtonKind::Primary,
            )
            .clicked()
            {
                app.map.message = open(app, ui.ctx()).err().map(|error| (error, false));
            }
        });
        ui.add_space(12.0);
        ui::controls::disclosure_open(ui, "lnm-setup", "Setup", app.map.target.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui::tool_button_enabled(
                    ui,
                    !busy,
                    egui_phosphor::regular::FOLDER_OPEN,
                    "Choose executable",
                    ui::ButtonKind::Normal,
                )
                .clicked()
                {
                    app.map.choose(ui.ctx());
                }
                if app.config.navigation.littlenavmap.is_some()
                    && ui::tool_button_enabled(
                        ui,
                        !busy,
                        egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                        "Use detected",
                        ui::ButtonKind::Ghost,
                    )
                    .clicked()
                {
                    app.map.message = select(app, None).err().map(|error| (error, false));
                }
            });
            if let Some(target) = &app.map.target {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(target.exe.display().to_string())
                            .monospace()
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIM),
                    )
                    .truncate(),
                )
                .on_hover_text(target.exe.display().to_string());
            } else {
                ui.label("Choose Little Navmap for Windows 64-bit.");
            }
        });
        if let Some((message, ok)) = &app.map.message {
            ui.label(egui::RichText::new(message).color(if *ok {
                ui::theme::DIM
            } else {
                ui::theme::WARN
            }));
        }
    });
    ui.add_space(ui::ROW_GAP);
    ui::group(ui, None, |ui| {
        ui::state(
            ui,
            if app.sim_process_running() {
                "MSFS running"
            } else {
                "MSFS stopped"
            },
            ui::Tone::Idle,
        );
        if let Some(target) = &app.map.target {
            if let Some(entry) = app.config.apps.iter().find(|entry| entry.exe == target.exe) {
                if app.actions.is_busy(&entry.name) {
                    ui::state(ui, "Starting Little Navmap", ui::Tone::Accent);
                }
                if let Some((message, ok)) = app
                    .actions
                    .take_result()
                    .filter(|(message, _)| message.starts_with(&entry.name))
                {
                    ui.label(egui::RichText::new(message).color(if ok {
                        ui::theme::DIM
                    } else {
                        ui::theme::WARN
                    }));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hangarrs-lnm-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn executable(&self) -> PathBuf {
            let path = self.0.join("littlenavmap.exe");
            let mut bytes = [0; 128];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..70].copy_from_slice(b"PE\0\0\x64\x86");
            bytes[88..90].copy_from_slice(b"\x0b\x02");
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovery_prefers_user_choice_and_keeps_windows_arguments_and_mode() {
        let scratch = Scratch::new();
        let candidate = scratch.executable();
        let mut config = Config::default();
        assert_eq!(
            choose_target(&config, [candidate.clone()]),
            Some(Target::at(candidate.clone()))
        );
        config.apps.push(hangar_core::config::AppEntry {
            name: "Little Navmap".into(),
            exe: "/registered/littlenavmap.exe".into(),
            args: vec!["--settings-path".into(), "/my settings".into()],
            kind: AppKind::SimSession,
            uninstaller: None,
            enabled: true,
        });
        let target = choose_target(&config, [candidate.clone()]).unwrap();
        assert_eq!(target.exe, config.apps[0].exe);
        assert_eq!(target.args, config.apps[0].args);
        assert_eq!(target.kind, AppKind::SimSession);
        config.navigation.littlenavmap = Some(config.apps[0].exe.clone());
        assert_eq!(choose_target(&config, []).unwrap(), target);
        config.navigation.littlenavmap = Some("/missing/chosen/littlenavmap".into());
        assert_eq!(
            choose_target(&config, [candidate]).unwrap().exe,
            config.navigation.littlenavmap.unwrap()
        );
    }

    #[test]
    fn missing_and_non_executable_candidates_are_not_detected() {
        let scratch = Scratch::new();
        let path = scratch.0.join("readme");
        std::fs::write(&path, "not executable").unwrap();
        assert!(choose_target(
            &Config::default(),
            [path, scratch.0.clone(), scratch.0.join("missing")]
        )
        .is_none());
        assert!(validate(Path::new("relative/littlenavmap")).is_err());
    }

    #[test]
    fn native_and_32_bit_builds_are_refused_without_executing_them() {
        let scratch = Scratch::new();
        let exe = scratch.executable();
        assert!(validate(&exe).is_ok());
        let mut bytes = std::fs::read(&exe).unwrap();
        bytes[68..70].copy_from_slice(&[0x4c, 0x01]);
        std::fs::write(&exe, &bytes).unwrap();
        assert!(validate(&exe).unwrap_err().contains("Windows 64-bit"));
        std::fs::write(&exe, [0u8; 128]).unwrap();
        assert!(validate(&exe).is_err());
        let mut config = Config::default();
        config.apps.push(AppEntry {
            name: "Little Navmap".into(),
            exe,
            args: vec![],
            kind: AppKind::Native,
            enabled: true,
            uninstaller: None,
        });
        assert!(choose_target(&config, []).is_none());
    }

    #[test]
    fn map_selection_registers_once_for_hangar_and_preflight() {
        let scratch = Scratch::new();
        let exe = scratch.executable();
        let mut config = Config::default();
        register(&mut config, &exe);
        config.apps[0].args = vec!["--settings-path".into(), "MSFS settings".into()];
        config.apps[0].kind = AppKind::SimSession;
        config.apps[0].enabled = false;
        register(&mut config, &exe);
        assert_eq!(config.apps.len(), 1);
        assert!(config.apps[0].enabled);
        let item = hangar_catalog::apps::item(&config.apps[0]);
        let request = crate::apps::request_for(&item);
        assert_eq!(request.mode, hangar_runtime::RunMode::SimSession);
        assert_eq!(request.args, ["--settings-path", "MSFS settings"]);
        assert_eq!(request.exe, exe);
    }

    #[test]
    fn inert_open_picker_and_worker_actions_do_nothing() {
        let mut state = MapState {
            target: Some(Target::at("/must/not/run".into())),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        state.choose(&ctx);
        state.submit(|| panic!("inert worker must not run"), &ctx);
        assert!(!state.busy());
        assert!(matches!(state.take_result(), Job::Idle));
        assert!(state.message.as_ref().is_some_and(|(_, success)| !success));
    }

    #[test]
    fn chooser_results_are_consumed_once_and_busy_actions_cannot_overlap() {
        let mut state = MapState {
            live: true,
            ..Default::default()
        };
        *state.job.lock().unwrap() = Job::Busy;
        state.submit(
            || panic!("busy worker must not run"),
            &egui::Context::default(),
        );
        assert!(state.busy());
        *state.job.lock().unwrap() = Job::Chosen(Ok(None));
        assert!(matches!(state.take_result(), Job::Chosen(Ok(None))));
        assert!(matches!(state.take_result(), Job::Idle));
        assert!(!state.busy());
    }
}
