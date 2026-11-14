//! Hangar → Remove: the confirm sheet for deleting a package or a vendor app.
//!
//! The library half is `hangar_install::remove`. Everything that reads the disk happens on a
//! thread after a click — a package tree is walked once, at review time, never
//! while drawing — and every write is refused unless this state is live
//! (`main.rs`), whatever the buttons say. The guard is on the action.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui_phosphor::regular as icons;
use hangar_catalog::{Item, ItemKind};
use hangar_install::drop::Approval;
use hangar_install::remove::{self, Shape, Target, Tie, Uninstaller};
use hangar_ui as ui;

use crate::app::HangarApp;

/// Shown when a write is refused for want of the Settings toggle.
pub const NOT_ALLOWED: &str =
    "HangarRS is not allowed to change your Community folder. Turn that on in Settings.";
/// Shown when the simulator is up.
pub const SIM_UP: &str = "Close the flight simulator first.";

/// One package, read and measured.
#[derive(Debug, Clone)]
pub struct PackageSheet {
    pub title: String,
    pub target: Arc<Target>,
}

/// A package a vendor put in place, as the vendor sheet lists it.
#[derive(Debug, Clone)]
pub struct Tied {
    pub tie: Tie,
    pub title: String,
    /// `Err` when the package could not be read; it is then never deleted.
    pub target: Result<Arc<Target>, String>,
    pub ticked: bool,
}

#[derive(Debug, Clone)]
pub struct VendorSheet {
    pub app: String,
    pub exe: PathBuf,
    pub uninstaller: Uninstaller,
    pub packages: Vec<Tied>,
}

impl VendorSheet {
    fn chosen(&self) -> impl Iterator<Item = &Arc<Target>> {
        self.packages
            .iter()
            .filter(|p| p.ticked)
            .filter_map(|p| p.target.as_ref().ok())
    }
    fn bytes(&self) -> u64 {
        self.chosen().map(|t| t.bytes).sum()
    }
}

#[derive(Debug, Clone)]
pub enum Sheet {
    Package(PackageSheet),
    Vendor(VendorSheet),
}

#[derive(Default)]
struct Shared {
    busy: Option<String>,
    reviewed: Option<(String, Result<Sheet, String>)>,
    result: Option<(String, String, bool)>,
    /// Package names deleted by the last action, for the loadouts to forget.
    removed: Vec<String>,
}

#[derive(Default)]
pub struct State {
    live: bool,
    shared: Arc<Mutex<Shared>>,
    /// The Settings write toggle as it is now, shared with the worker so each
    /// delete reads it again — after a vendor uninstaller that can sit open for
    /// minutes, not only at the confirm click. Off until told otherwise.
    writes: Arc<AtomicBool>,
    /// Which item the sheet belongs to, and the sheet.
    pub sheet: Option<(String, Sheet)>,
    /// Why the last review or removal ended the way it did, by item.
    pub message: Option<(String, String, bool)>,
    /// A sheet just opened: scroll it into view once, then leave the page alone.
    reveal: bool,
}

impl State {
    pub fn live() -> Self {
        State {
            live: true,
            ..State::default()
        }
    }

    fn busy(&self) -> Option<String> {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .busy
            .clone()
    }

    /// Whether a removal is reading or deleting right now.
    pub fn is_working(&self) -> bool {
        self.busy().is_some()
    }

    /// Follow the Settings write toggle. `HangarApp::draw` calls it every
    /// frame and the toggle calls it on the click, so a running removal sees a
    /// switch-off before its next delete.
    pub fn follow_writes(&self, allowed: bool) {
        self.writes.store(allowed, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn show(&mut self, item: &str, sheet: Sheet) {
        self.sheet = Some((item.to_string(), sheet));
        self.reveal = true;
    }

    /// Mark a removal as running, the way a click does, without one.
    #[cfg(test)]
    pub fn set_busy(&self, item: Option<&str>) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).busy = item.map(str::to_owned);
    }
}

/// Take in whatever a thread finished since the last frame. Called from every
/// frame (`HangarApp::draw`), not only while the Hangar detail is open.
pub(crate) fn collect(app: &mut HangarApp) {
    let (reviewed, result, removed) = {
        let mut shared = app.remove.shared.lock().unwrap_or_else(|e| e.into_inner());
        (
            shared.reviewed.take(),
            shared.result.take(),
            std::mem::take(&mut shared.removed),
        )
    };
    if let Some((item, reviewed)) = reviewed {
        match reviewed {
            Ok(sheet) => {
                app.remove.sheet = Some((item, sheet));
                app.remove.reveal = true;
            }
            Err(why) => app.remove.message = Some((item, why, false)),
        }
    }
    for name in &removed {
        forget_in_loadouts(app, name);
    }
    if let Some(result) = result {
        app.journal.note("Remove", &result.1);
        app.remove.message = Some(result);
        app.rescan_requested = true;
    }
}

/// Take a deleted package out of every saved loadout (HangarRS's own files).
fn forget_in_loadouts(app: &mut HangarApp, package: &str) {
    for name in loadouts_holding(app, package) {
        let _ = crate::hangar::toggle_membership(app, &name, package, false);
    }
}

fn loadouts_holding(app: &HangarApp, package: &str) -> Vec<String> {
    app.profiles
        .names()
        .into_iter()
        .filter(|name| app.profiles.get(name).is_some_and(|p| p.contains(package)))
        .map(str::to_owned)
        .collect()
}

/// Whether Remove is offered on this item at all.
pub fn offered(item: &Item) -> bool {
    match item.kind {
        ItemKind::Package => item.community.is_some(),
        ItemKind::CompanionApp => remove::prefix_of(&item.path).is_some(),
        _ => false,
    }
}

/// The row action and, once reviewed, the sheet. Drawn at the foot of the
/// Hangar detail panel.
pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui, item: &Item) {
    collect(app);
    if !offered(item) {
        return;
    }
    ui.add_space(14.0);
    let busy = app.remove.busy();
    let open = app
        .remove
        .sheet
        .as_ref()
        .is_some_and(|(name, _)| name == &item.name);
    if !open {
        let label = if item.kind == ItemKind::Package {
            "Remove…"
        } else {
            "Uninstall…"
        };
        if ui::tool_button_enabled(
            ui,
            busy.is_none(),
            icons::TRASH,
            label,
            ui::ButtonKind::Danger,
        )
        .clicked()
        {
            app.remove.message = None;
            review(app, item);
        }
    }
    if let Some(what) = busy.filter(|what| what == &item.name) {
        ui::activity(ui, &format!("Working on {what}"));
    }
    if let Some((name, text, ok)) = &app.remove.message {
        if name == &item.name {
            ui.label(
                egui::RichText::new(text)
                    .size(ui::theme::TEXT_BODY)
                    .color(if *ok { ui::theme::TEXT } else { ui::theme::ERR }),
            );
        }
    }
    if open {
        let mut sheet = app.remove.sheet.take().map(|(_, s)| s);
        let drawn = ui.scope(|ui| match sheet.as_mut() {
            Some(Sheet::Package(p)) => package_sheet(app, ui, p),
            Some(Sheet::Vendor(v)) => vendor_sheet(app, ui, v),
            None => None,
        });
        if std::mem::take(&mut app.remove.reveal) {
            drawn.response.scroll_to_me(Some(egui::Align::Max));
        }
        let action = drawn.inner;
        match action {
            Some(Choice::Cancel) => {}
            Some(Choice::Go) => {
                if let Some(sheet) = sheet {
                    act(app, &item.name, sheet);
                }
            }
            None => app.remove.sheet = sheet.map(|s| (item.name.clone(), s)),
        }
    }
}

enum Choice {
    Go,
    Cancel,
}

/// What stops the action right now, in the words the page uses.
fn stopped_by(app: &HangarApp) -> Option<&'static str> {
    if !app.writes_allowed {
        Some(NOT_ALLOWED)
    } else if app.sim_process_running() {
        Some(SIM_UP)
    } else {
        None
    }
}

fn label(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(
        egui::RichText::new(text)
            .size(ui::theme::TEXT_BODY)
            .color(color),
    );
}

fn package_sheet(app: &HangarApp, ui: &mut egui::Ui, sheet: &PackageSheet) -> Option<Choice> {
    let target = &sheet.target;
    let size = hangar_msfs::manifest::format_size(target.bytes);
    let loadouts = loadouts_holding(app, &target.name);
    let mut choice = None;
    ui::group(ui, Some("Remove"), |ui| {
        ui::kv_row(ui, "Deletes", |ui| {
            ui.add(egui::Label::new(&sheet.title).truncate())
                .on_hover_text(target.path.display().to_string());
        });
        ui::kv_row(ui, "Size", |ui| {
            ui.label(match target.shape {
                Shape::Directory => format!("1 package · {size}"),
                Shape::Link { .. } => "1 package · a link".to_string(),
            });
        });
        ui::kv_row(ui, "From", |ui| {
            ui.add(egui::Label::new(folder_name(&target.community)).truncate())
                .on_hover_text(target.community.display().to_string());
        });
        if !loadouts.is_empty() {
            ui::kv_row(ui, "Loadouts", |ui| {
                ui.add(egui::Label::new(loadouts.join(", ")).truncate());
            });
        }
        ui.add_space(6.0);
        if let Shape::Link { target: to } = &target.shape {
            ui.label(
                egui::RichText::new("Its copy in the vendor's library stays; remove that there.")
                    .size(ui::theme::TEXT_BODY)
                    .color(ui::theme::DIM),
            )
            .on_hover_text(to.display().to_string());
        }
        label(ui, "This cannot be undone.", ui::theme::TEXT);
        let stop = stopped_by(app);
        if let Some(why) = stop {
            label(ui, why, ui::theme::WARN);
        }
        ui.horizontal(|ui| {
            let go = match target.shape {
                Shape::Directory => format!("Delete · {size}"),
                Shape::Link { .. } => "Delete".to_string(),
            };
            if ui::tool_button_enabled(
                ui,
                stop.is_none(),
                icons::TRASH,
                &go,
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                choice = Some(Choice::Go);
            }
            if ui::tool_button(ui, icons::X, "Cancel", ui::ButtonKind::Ghost).clicked() {
                choice = Some(Choice::Cancel);
            }
        });
    });
    choice
}

fn vendor_sheet(app: &HangarApp, ui: &mut egui::Ui, sheet: &mut VendorSheet) -> Option<Choice> {
    let mut choice = None;
    let count = sheet.chosen().count();
    let size = hangar_msfs::manifest::format_size(sheet.bytes());
    ui::group(ui, Some("Uninstall"), |ui| {
        ui::kv_row(ui, "Runs", |ui| {
            ui.add(egui::Label::new(&sheet.uninstaller.display_name).truncate())
                .on_hover_text(&sheet.uninstaller.command);
        });
        ui::kv_row(ui, "Deletes", |ui| {
            ui.label(match count {
                0 => "the app only".to_string(),
                1 => format!("the app + 1 package · {size}"),
                n => format!("the app + {n} packages · {size}"),
            });
        });
        if sheet.packages.is_empty() {
            label(
                ui,
                "No packages in Community look like this vendor's.",
                ui::theme::DIM,
            );
        }
        for tied in &mut sheet.packages {
            ui.horizontal(|ui| {
                let usable = tied.target.is_ok();
                if ui::toggle_chip(ui, "include", tied.ticked && usable, ui::theme::ORANGE)
                    .on_hover_text(tied.tie.strength.label())
                    .clicked()
                    && usable
                {
                    tied.ticked = !tied.ticked;
                }
                let line = match &tied.target {
                    Ok(t) => format!(
                        "{} · {} · {}",
                        tied.title,
                        hangar_msfs::manifest::format_size(t.bytes),
                        tied.tie.strength.label()
                    ),
                    Err(why) => format!("{} · cannot be read: {why}", tied.title),
                };
                ui.add(egui::Label::new(line).truncate())
                    .on_hover_text(tied.tie.path.display().to_string());
            });
        }
        ui.add_space(6.0);
        label(
            ui,
            "Its uninstaller runs first; packages are deleted only once the app is gone.",
            ui::theme::TEXT,
        );
        let stop = stopped_by(app);
        if let Some(why) = stop {
            label(ui, why, ui::theme::WARN);
        }
        ui.horizontal(|ui| {
            let go = if count == 0 {
                "Uninstall".to_string()
            } else {
                format!("Uninstall and delete · {size}")
            };
            if ui::tool_button_enabled(
                ui,
                stop.is_none(),
                icons::TRASH,
                &go,
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                choice = Some(Choice::Go);
            }
            if ui::tool_button(ui, icons::X, "Cancel", ui::ButtonKind::Ghost).clicked() {
                choice = Some(Choice::Cancel);
            }
        });
    });
    choice
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Read and measure, on a thread. Reading writes nothing, so it runs inert too.
fn review(app: &mut HangarApp, item: &Item) {
    let shared = Arc::clone(&app.remove.shared);
    shared.lock().unwrap_or_else(|e| e.into_inner()).busy = Some(item.name.clone());
    let item = item.clone();
    let packages: Vec<Item> = app
        .discovery
        .catalog
        .items()
        .iter()
        .filter(|i| i.kind == ItemKind::Package && i.community.is_some())
        .cloned()
        .collect();
    crate::install::refresh_history(&mut app.install);
    let records = app.install.history.clone();
    std::thread::spawn(move || {
        let reviewed = match item.kind {
            ItemKind::Package => review_package(&item),
            _ => review_vendor(&item, &packages, &records),
        };
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.reviewed = Some((item.name.clone(), reviewed));
        s.busy = None;
    });
}

fn review_package(item: &Item) -> Result<Sheet, String> {
    let community = item.community.as_ref().ok_or("not in a Community folder")?;
    let target = Target::inspect(&item.path, community).map_err(|e| e.to_string())?;
    Ok(Sheet::Package(PackageSheet {
        title: item.display_title().to_string(),
        target: Arc::new(target),
    }))
}

pub(crate) fn review_vendor(
    item: &Item,
    packages: &[Item],
    records: &[hangar_install::record::InstallRecord],
) -> Result<Sheet, String> {
    // Whether the uninstaller worked is read from the exe being gone
    // afterwards, so an exe already gone could never tell.
    if !item.path.is_file() {
        return Err(exe_missing(item.display_title()));
    }
    let uninstaller = remove::uninstaller_for(&item.path).ok_or_else(|| {
        format!(
            "{} has no Windows uninstaller in its prefix.",
            item.display_title()
        )
    })?;
    let refs: Vec<_> = packages
        .iter()
        .map(|p| remove::PackageRef {
            name: p.name.clone(),
            path: p.path.clone(),
            creator: p.creator.clone(),
        })
        .collect();
    let tied = remove::ties(&uninstaller, &item.path, &refs, records)
        .into_iter()
        .map(|tie| {
            let package = packages.iter().find(|p| p.path == tie.path);
            let target = package
                .and_then(|p| p.community.as_ref())
                .ok_or_else(|| "not in a Community folder".to_string())
                .and_then(|c| Target::inspect(&tie.path, c).map_err(|e| e.to_string()))
                .map(Arc::new);
            Tied {
                title: package
                    .map(|p| p.display_title().to_string())
                    .unwrap_or_else(|| tie.name.clone()),
                ticked: tie.strength.preticked() && target.is_ok(),
                target,
                tie,
            }
        })
        .collect();
    Ok(Sheet::Vendor(VendorSheet {
        app: item.display_title().to_string(),
        exe: item.path.clone(),
        uninstaller,
        packages: tied,
    }))
}

/// The confirmed action. Refused here, not at the button, when inert.
fn act(app: &mut HangarApp, item: &str, sheet: Sheet) {
    let refuse = |app: &mut HangarApp, why: &str| {
        app.remove.message = Some((item.to_string(), why.to_string(), false));
    };
    if !app.remove.live {
        return refuse(app, "Removing is unavailable in this session.");
    }
    if let Some(why) = stopped_by(app) {
        return refuse(app, why);
    }
    // The one busy state every Community writer checks, this one included.
    if let Some(why) = app.community_busy() {
        return refuse(app, why);
    }
    // Checked before anything is marked busy, so a refusal leaves nothing to undo.
    let proton = match &sheet {
        Sheet::Package(_) => None,
        Sheet::Vendor(v) => {
            if !v.exe.is_file() {
                return refuse(app, &exe_missing(&v.app));
            }
            let Some(proton) = vendor_proton(app, &v.exe) else {
                return refuse(
                    app,
                    "Its prefix is not the companion prefix HangarRS runs apps in.",
                );
            };
            if app
                .monitor
                .running()
                .get(&hangar_core::proc::match_key(&v.exe))
                .is_some()
            {
                return refuse(app, &format!("Close {} first.", v.app));
            }
            Some(proton)
        }
    };
    app.remove.follow_writes(app.writes_allowed);
    let writes = Arc::clone(&app.remove.writes);
    let shared = Arc::clone(&app.remove.shared);
    shared.lock().unwrap_or_else(|e| e.into_inner()).busy = Some(item.to_string());
    let item = item.to_string();
    match (sheet, proton) {
        (Sheet::Vendor(v), Some(proton)) => {
            let journal = app.journal.clone();
            tokio::spawn(async move {
                let outcome = uninstall_then_delete(&v, &proton, &writes, journal).await;
                finish(&shared, &item, outcome);
            });
        }
        (Sheet::Package(p), _) => {
            std::thread::spawn(move || {
                let outcome = delete_one(&p.target, &p.title, &writes);
                finish(&shared, &item, outcome);
            });
        }
        (Sheet::Vendor(_), None) => finish(
            &shared,
            &item,
            ("Nothing was run.".to_string(), false, Vec::new()),
        ),
    }
}

/// Why a vendor whose exe is already gone is never uninstalled from here.
fn exe_missing(app: &str) -> String {
    format!(
        "{app} is not on disk, so HangarRS cannot tell whether its uninstaller worked. \
         Nothing was run; remove its packages one at a time."
    )
}

/// Whether the vendor's uninstaller finished: it exited 0 **and** the app's
/// exe is gone. Cancelled, failed, killed or still installed deletes nothing.
fn uninstalled(app: &str, code: Option<i32>, exe_still_there: bool) -> Result<(), String> {
    match code {
        Some(0) => {}
        Some(code) => {
            return Err(format!(
            "{app}'s uninstaller was cancelled or failed (exit {code}); no packages were deleted."
        ))
        }
        None => {
            return Err(format!(
                "{app}'s uninstaller was stopped before it finished; no packages were deleted."
            ))
        }
    }
    if exe_still_there {
        return Err(format!(
            "{app} is still installed; no packages were deleted."
        ));
    }
    Ok(())
}

fn finish(shared: &Arc<Mutex<Shared>>, item: &str, outcome: (String, bool, Vec<String>)) {
    let (text, ok, removed) = outcome;
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    s.result = Some((item.to_string(), text, ok));
    s.removed = removed;
    s.busy = None;
}

/// One package through the library's guarded delete. The write toggle and the
/// simulator are both read here, at the delete, not taken from the confirm.
fn delete_one(target: &Target, title: &str, writes: &AtomicBool) -> (String, bool, Vec<String>) {
    let approval = Approval::given(format!(
        "Delete {} ({} bytes)",
        target.path.display(),
        target.bytes
    ));
    let stamp = hangar_core::time::Utc::now().stamp();
    match remove::delete(
        target,
        writes.load(Ordering::SeqCst),
        hangar_runtime::sim::is_running(),
        &approval,
        &stamp,
    ) {
        Ok(r) => (
            format!(
                "Deleted {title} ({}).",
                if r.was_link {
                    "the link".to_string()
                } else {
                    hangar_msfs::manifest::format_size(r.bytes)
                }
            ),
            true,
            vec![r.name],
        ),
        Err(e) => (format!("{title} was not removed: {e}"), false, Vec::new()),
    }
}

/// The companion prefix, when the vendor app lives in it.
fn vendor_proton(app: &HangarApp, exe: &Path) -> Option<hangar_runtime::ProtonEnv> {
    let proton = app.proton.clone()?;
    let prefix = remove::prefix_of(exe)?;
    (proton.wineprefix == prefix).then_some(proton)
}

async fn uninstall_then_delete(
    vendor: &VendorSheet,
    proton: &hangar_runtime::ProtonEnv,
    writes: &AtomicBool,
    journal: crate::journal::Journal,
) -> (String, bool, Vec<String>) {
    // The "gone afterwards" test means nothing unless it was there before.
    if !vendor.exe.is_file() {
        return (exe_missing(&vendor.app), false, Vec::new());
    }
    let invocation = match remove::uninstall_invocation(&vendor.uninstaller, proton) {
        Ok(i) => i,
        Err(e) => {
            return (
                format!("{} was not uninstalled: {e}", vendor.app),
                false,
                Vec::new(),
            )
        }
    };
    journal.note("Remove", format!("running {}", invocation.command_line()));
    let sink = journal.clone();
    let on_line: hangar_install::run::OnLine =
        Arc::new(move |line: &str| sink.note("Remove", line));
    let ran = match hangar_install::run::run(&invocation, &[], Some(on_line)).await {
        Ok(ran) => ran,
        Err(e) => {
            return (
                format!("{} was not uninstalled: {e}", vendor.app),
                false,
                Vec::new(),
            )
        }
    };
    if let Err(why) = uninstalled(&vendor.app, ran.code, vendor.exe.exists()) {
        return (why, false, Vec::new());
    }
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for tied in vendor.packages.iter().filter(|p| p.ticked) {
        let Ok(target) = &tied.target else { continue };
        let (text, ok, names) = delete_one(target, &tied.title, writes);
        journal.note("Remove", &text);
        if ok {
            removed.extend(names);
        } else {
            failed.push(tied.title.clone());
        }
    }
    let text = if failed.is_empty() {
        format!(
            "Uninstalled {} and deleted {} packages.",
            vendor.app,
            removed.len()
        )
    } else {
        format!(
            "Uninstalled {}; {} packages deleted, not: {}.",
            vendor.app,
            removed.len(),
            failed.join(", ")
        )
    };
    (text, failed.is_empty(), removed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A real package on a scratch tree, read the way a click reads it.
    pub(crate) fn scratch_sheet(tag: &str, bytes: usize) -> (PathBuf, Target) {
        let base =
            std::env::temp_dir().join(format!("hangar-remove-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let community = base.join("Packages/Community2024");
        let package = community.join("orbx-aircraft-pc12");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("manifest.json"), b"{}").unwrap();
        std::fs::write(package.join("model.bin"), vec![0u8; bytes]).unwrap();
        let target = Target::inspect(&package, &community).unwrap();
        (base, target)
    }

    #[test]
    fn only_community_packages_and_prefix_apps_are_offered_remove() {
        let mut item = Item {
            name: "p".into(),
            kind: ItemKind::Package,
            state: hangar_catalog::ItemState::Resident,
            title: "P".into(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: "/x/Community/p".into(),
            community: Some("/x/Community".into()),
            args: vec![],
        };
        assert!(offered(&item));
        item.community = None;
        assert!(!offered(&item));
        item.kind = ItemKind::CompanionApp;
        item.path = "/pfx/drive_c/Programs/orbx-central/Orbx Central.exe".into();
        assert!(offered(&item));
        item.path = "/home/user/littlenavmap/littlenavmap".into();
        assert!(!offered(&item));
        item.kind = ItemKind::NativeTool;
        assert!(!offered(&item));
    }

    #[test]
    fn a_vendor_with_no_uninstall_entry_says_so() {
        let item = Item {
            name: "x".into(),
            kind: ItemKind::CompanionApp,
            state: hangar_catalog::ItemState::Ready,
            title: "Something".into(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: std::env::temp_dir().join(format!(
                "hangar-remove-none-{}/pfx/drive_c/App/App.exe",
                std::process::id()
            )),
            community: None,
            args: vec![],
        };
        // The exe has to be there for the uninstaller question to arise.
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        std::fs::write(&item.path, b"MZ").unwrap();
        let Err(why) = review_vendor(&item, &[], &[]) else {
            panic!("no registry, no uninstaller")
        };
        assert!(why.contains("no Windows uninstaller"));
        let _ = std::fs::remove_dir_all(item.path.ancestors().nth(4).unwrap());
    }

    /// A vendor in a scratch prefix whose "uninstaller" is a shell script run
    /// by `/bin/sh` standing in for Proton, and one ticked package in a
    /// scratch Community. Nothing outside the temporary directory is touched.
    struct ScratchVendor {
        base: PathBuf,
        package: PathBuf,
        exe: PathBuf,
        sheet: VendorSheet,
        proton: hangar_runtime::ProtonEnv,
    }

    fn scratch_vendor(tag: &str, uninstaller: &str, exe_present: bool) -> ScratchVendor {
        let (base, target) = scratch_sheet(&format!("vendor-{tag}"), 4096);
        let prefix = base.join("pfx");
        let app_dir = prefix.join("drive_c/Vendor App");
        std::fs::create_dir_all(&app_dir).unwrap();
        let exe = app_dir.join("Vendor App.exe");
        if exe_present {
            std::fs::write(&exe, b"MZ").unwrap();
        }
        std::fs::write(app_dir.join("Uninstall Vendor App.exe"), uninstaller).unwrap();
        let package = target.path.clone();
        let sheet = VendorSheet {
            app: "Vendor App".into(),
            exe: exe.clone(),
            uninstaller: Uninstaller {
                key: "vendor-app".into(),
                display_name: "Vendor App 1.0".into(),
                publisher: "Vendor".into(),
                command: r#""C:\Vendor App\Uninstall Vendor App.exe" /currentuser"#.into(),
                install_location: r"C:\Vendor App".into(),
                display_icon: String::new(),
            },
            packages: vec![Tied {
                tie: Tie {
                    name: target.name.clone(),
                    path: target.path.clone(),
                    strength: remove::Strength::Certain,
                },
                title: "Pilatus PC-12".into(),
                target: Ok(Arc::new(target)),
                ticked: true,
            }],
        };
        let proton = hangar_runtime::ProtonEnv {
            wineprefix: prefix,
            // Not a file, so the invocation falls back to `wine_binary`.
            proton_script: base.join("no-proton-here"),
            wine_binary: PathBuf::from("/bin/sh"),
            compat_data: base.clone(),
            compat_install: base.clone(),
            steam_client: base.clone(),
            is_flatpak: false,
        };
        ScratchVendor {
            base,
            package,
            exe,
            sheet,
            proton,
        }
    }

    fn run_vendor(v: &ScratchVendor, writes: bool) -> (String, bool, Vec<String>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let writes = AtomicBool::new(writes);
        rt.block_on(uninstall_then_delete(
            &v.sheet,
            &v.proton,
            &writes,
            crate::journal::Journal::default(),
        ))
    }

    /// FINAL_REVIEW S2: an exe already gone before the run proves nothing, so
    /// nothing runs and nothing is deleted — and the sheet never opens.
    #[test]
    fn a_vendor_whose_exe_is_already_gone_runs_nothing_and_deletes_nothing() {
        let v = scratch_vendor("gone", "echo ran > ran.txt\n", false);
        let (text, ok, removed) = run_vendor(&v, true);
        assert!(!ok);
        assert!(text.contains("is not on disk"), "{text}");
        assert!(removed.is_empty());
        assert!(v.package.join("manifest.json").is_file(), "package deleted");
        assert!(
            !v.exe.with_file_name("ran.txt").exists(),
            "the uninstaller ran"
        );

        let item = Item {
            name: "Vendor App".into(),
            kind: ItemKind::CompanionApp,
            state: hangar_catalog::ItemState::MissingExecutable,
            title: "Vendor App".into(),
            creator: String::new(),
            version: String::new(),
            content_type: None,
            size_bytes: None,
            path: v.exe.clone(),
            community: None,
            args: vec![],
        };
        let Err(why) = review_vendor(&item, &[], &[]) else {
            panic!("a missing exe opened the vendor sheet")
        };
        assert!(why.contains("is not on disk"), "{why}");
        std::fs::remove_dir_all(&v.base).unwrap();
    }

    /// FINAL_REVIEW S2: cancelled, failed, or "succeeded" with the app still
    /// there — each deletes nothing. The exit code is checked as well as the
    /// exe, so a failing uninstaller that got as far as the exe still counts
    /// as failed.
    #[test]
    fn a_cancelled_or_failed_uninstall_deletes_nothing() {
        for (tag, script, says) in [
            ("cancel", "exit 1\n", "cancelled or failed (exit 1)"),
            (
                "failed-late",
                "mv 'Vendor App.exe' 'Vendor App.exe.old'\nexit 2\n",
                "cancelled or failed (exit 2)",
            ),
            ("still-there", "exit 0\n", "is still installed"),
        ] {
            let v = scratch_vendor(tag, script, true);
            let (text, ok, removed) = run_vendor(&v, true);
            assert!(!ok, "{tag}");
            assert!(text.contains(says), "{tag}: {text}");
            assert!(removed.is_empty(), "{tag}");
            assert!(
                v.package.join("manifest.json").is_file(),
                "{tag}: package deleted"
            );
            std::fs::remove_dir_all(&v.base).unwrap();
        }
    }

    /// The other half of S2: exit 0 and the app gone deletes the ticked package.
    #[test]
    fn a_finished_uninstall_deletes_the_ticked_packages() {
        let v = scratch_vendor(
            "done",
            "mv 'Vendor App.exe' 'Vendor App.exe.old'\nexit 0\n",
            true,
        );
        let (text, ok, removed) = run_vendor(&v, true);
        assert!(ok, "{text}");
        assert_eq!(removed, vec!["orbx-aircraft-pc12".to_string()]);
        assert!(!v.package.exists());
        std::fs::remove_dir_all(&v.base).unwrap();
    }

    /// FINAL_REVIEW S3: the write toggle is switched off while the vendor's
    /// dialog is open. The delete afterwards reads the toggle as it is then,
    /// the way it reads the simulator, and deletes nothing.
    #[test]
    fn switching_writes_off_during_the_uninstaller_stops_the_deletes() {
        let v = scratch_vendor(
            "toggle",
            "echo > started\nn=0\nwhile [ ! -f go ] && [ $n -lt 500 ]; do sleep 0.01; n=$((n+1)); done\n\
             mv 'Vendor App.exe' 'Vendor App.exe.old'\nexit 0\n",
            true,
        );
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let writes = Arc::new(AtomicBool::new(true));
        let task = {
            let (sheet, proton, writes) = (v.sheet.clone(), v.proton.clone(), Arc::clone(&writes));
            rt.spawn(async move {
                uninstall_then_delete(&sheet, &proton, &writes, crate::journal::Journal::default())
                    .await
            })
        };
        let folder = v.exe.parent().unwrap().to_path_buf();
        let waited = std::time::Instant::now();
        while !folder.join("started").exists() {
            assert!(
                waited.elapsed() < std::time::Duration::from_secs(10),
                "the uninstaller never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // The person switches "Allow package changes" off, then finishes the dialog.
        writes.store(false, Ordering::SeqCst);
        std::fs::write(folder.join("go"), b"").unwrap();
        let (text, ok, removed) = rt.block_on(task).unwrap();
        assert!(!ok, "{text}");
        assert!(text.contains("not:"), "{text}");
        assert!(removed.is_empty());
        assert!(v.package.join("manifest.json").is_file(), "package deleted");
        assert!(!v.exe.exists(), "the uninstaller did finish");
        std::fs::remove_dir_all(&v.base).unwrap();
    }

    #[test]
    fn the_uninstall_verdict_needs_exit_zero_and_the_exe_gone() {
        assert!(uninstalled("A", Some(0), false).is_ok());
        assert!(uninstalled("A", Some(0), true).is_err());
        assert!(uninstalled("A", Some(1), false).is_err());
        assert!(uninstalled("A", Some(3010), false).is_err());
        assert!(uninstalled("A", None, false).is_err());
    }
}
