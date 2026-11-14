//! Profile maintenance, with inert actions and an immutable restore preview.
use crate::app::HangarApp;
use hangar_bindings::{diff, guide, snapshot, Library, Profile};
use hangar_ui as ui;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// The native "Save As" chooser, injected so a test never opens a dialog.
///
/// Given a suggested file name, it blocks until the person picks a destination
/// or cancels. It runs on its own thread, never the UI thread.
pub type Chooser = Arc<dyn Fn(String) -> Option<PathBuf> + Send + Sync>;

/// Where the export chooser is in its round trip.
#[derive(Default)]
enum Pick {
    #[default]
    Idle,
    Open,
    Ready(Option<PathBuf>),
}

/// One side of a comparison: the live profile, or a copy inside a backup.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Live(String),
    Saved { dir: PathBuf, id: String },
}

/// Most changes drawn for one comparison; two different devices differ everywhere.
const MAX_CHANGES: usize = 200;

#[derive(Default)]
struct Comparison {
    from: Option<Source>,
    to: Option<Source>,
    /// The pair `result` was computed for, so the files are read once per choice.
    computed: Option<(Source, Source)>,
    result: Option<Result<diff::Diff, String>>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum View {
    #[default]
    Assignments,
    Shared,
    Backups,
}

#[derive(Default)]
pub struct State {
    pub view: View,
    pub device: Option<String>,
    store: Option<PathBuf>,
    copies: Vec<snapshot::Snapshot>,
    preview: Option<Preview>,
    message: Option<String>,
    chooser: Option<Chooser>,
    pick: Arc<Mutex<Pick>>,
    /// The guide as it was when Export was pressed, waiting for a destination.
    pending: Option<String>,
    comparison: Comparison,
    /// The Assignments view's filter lists and search answer, across frames.
    pub(crate) lookup: crate::bindings::Lookup,
}
struct Preview {
    destination: PathBuf,
    original: String,
    replacement: Profile,
    source: PathBuf,
    changes: diff::Diff,
    confirm: bool,
}
impl State {
    pub fn live() -> Self {
        Self {
            store: hangar_core::paths::bindings_dir(),
            chooser: Some(native_chooser()),
            ..Self::default()
        }
    }
    #[cfg(test)]
    fn with_chooser(chooser: Chooser) -> Self {
        Self {
            chooser: Some(chooser),
            ..Self::default()
        }
    }
    fn choosing(&self) -> bool {
        matches!(
            *self.pick.lock().unwrap_or_else(|e| e.into_inner()),
            Pick::Open
        )
    }
    fn backup(&mut self, library: &Library) -> Result<(), String> {
        let store = self
            .store
            .as_ref()
            .ok_or("Backups are unavailable in this session.")?;
        let remote = library
            .remote_dir
            .as_ref()
            .ok_or("No control profile folder was found.")?;
        let copy = snapshot::take(remote, store, Some("Manual backup".into()))
            .map_err(|e| e.to_string())?;
        self.message = Some(format!(
            "Saved {} profiles · {}",
            copy.manifest.profiles.len(),
            copy.dir.display()
        ));
        self.copies = snapshot::list(store);
        Ok(())
    }
    fn guide(&self, library: &Library, selected: Option<&str>) -> Result<String, String> {
        let profiles: Vec<_> = library
            .profiles
            .iter()
            .filter(|p| {
                selected.is_none_or(|id| id == p.id())
                    && self
                        .device
                        .as_deref()
                        .is_none_or(|device| device == p.device())
            })
            .collect();
        if profiles.is_empty() {
            return Err("No profiles selected.".into());
        }
        Ok(guide::to_html(
            profiles.iter().map(|p| (p.id(), &p.profile)),
            &guide::Options::default(),
        ))
    }
    /// Build the guide now and ask where to save it. Inert: refuses, opens nothing.
    fn export(
        &mut self,
        library: &Library,
        selected: Option<&str>,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(), String> {
        let chooser = self
            .chooser
            .clone()
            .ok_or("Export is unavailable in this session.")?;
        let mut pick = self.pick.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*pick, Pick::Idle) {
            return Ok(());
        }
        let html = self.guide(library, selected)?;
        *pick = Pick::Open;
        drop(pick);
        self.pending = Some(html);
        self.message = None;
        let result = Arc::clone(&self.pick);
        let name = format!("controls-{}.html", hangar_core::Utc::now().stamp());
        if let Err(error) = std::thread::Builder::new()
            .name("guide-save-as".into())
            .spawn(move || {
                let path = chooser(name);
                *result.lock().unwrap_or_else(|e| e.into_inner()) = Pick::Ready(path);
                wake();
            })
        {
            *self.pick.lock().unwrap_or_else(|e| e.into_inner()) = Pick::Idle;
            self.pending = None;
            return Err(format!("Could not open the save dialog: {error}"));
        }
        Ok(())
    }
    /// Collect the chosen destination, if the chooser has returned, and write there.
    fn finish_export(&mut self) {
        let path = {
            let mut pick = self.pick.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(*pick, Pick::Ready(_)) {
                return;
            }
            match std::mem::take(&mut *pick) {
                Pick::Ready(path) => path,
                _ => None,
            }
        };
        let html = self.pending.take();
        self.message = Some(match (path, html) {
            (Some(path), Some(html)) => match write_guide(&html, &path) {
                Ok(saved) => format!("Guide saved · {}", saved.display()),
                Err(error) => error,
            },
            _ => "Export cancelled.".into(),
        });
    }
    fn inspect(
        &mut self,
        copy: &snapshot::Snapshot,
        loaded: &hangar_bindings::library::Loaded,
    ) -> Result<(), String> {
        snapshot::verify(copy).map_err(|e| e.to_string())?;
        if copy.profile(loaded.id()).is_none() {
            return Err("This backup does not contain the selected profile.".into());
        }
        let source = snapshot::profile_path(copy, loaded.id());
        let replacement = hangar_bindings::read(&source).map_err(|e| e.to_string())?;
        let original = std::fs::read_to_string(&loaded.summary.path).map_err(|e| e.to_string())?;
        let current = hangar_bindings::parse(&original).map_err(|e| e.to_string())?;
        let changes = diff::compare(&current, &replacement);
        self.preview = Some(Preview {
            destination: loaded.summary.path.clone(),
            original,
            replacement,
            source,
            changes,
            confirm: false,
        });
        Ok(())
    }
    fn restore(&mut self) -> Result<PathBuf, String> {
        let store = self
            .store
            .as_ref()
            .ok_or("Restore is unavailable in this session.")?;
        let preview = self
            .preview
            .as_ref()
            .filter(|p| p.confirm)
            .ok_or("Review and confirm a profile first.")?;
        if std::fs::read_to_string(&preview.destination).map_err(|e| e.to_string())?
            != preview.original
        {
            return Err("The profile changed. Compare it again before restoring.".into());
        }
        let approval = hangar_bindings::save::Approval::given(format!(
            "Restore {} from {}",
            preview.destination.display(),
            preview.source.display()
        ));
        hangar_bindings::save::save(&preview.replacement, &preview.destination, store, &approval)
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let remote = preview
            .destination
            .parent()
            .ok_or("No profile folder.")?
            .to_owned();
        self.preview = None;
        self.message = Some("Profile restored. A backup of the previous version was saved.".into());
        self.copies = snapshot::list(store);
        Ok(remote)
    }
}

/// The name actually written: the chosen one, with `.html` added when it has
/// no extension at all. `controls` becomes `controls.html`; `guide.htm` and
/// `notes.txt` are left as chosen.
fn guide_path(chosen: &Path) -> PathBuf {
    if chosen.extension().is_some() {
        chosen.to_owned()
    } else {
        let mut name = chosen.as_os_str().to_owned();
        name.push(".html");
        PathBuf::from(name)
    }
}

/// Save the guide where the person chose, replacing a file they agreed to
/// replace.
///
/// The native dialog asks before replacing, so an existing file at the
/// *chosen* name is replaced. One at a name HangarRS made up by adding `.html`
/// was never shown to anyone, so it is refused. The HTML goes to a hidden
/// temporary file in the same directory and is renamed over the target, so
/// the old file is either intact or wholly replaced, never half-written.
fn write_guide(html: &str, chosen: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    let path = guide_path(chosen);
    if path != chosen && std::fs::symlink_metadata(&path).is_ok() {
        return Err(format!(
            "{} already exists. Export again and choose it to replace it.",
            path.display()
        ));
    }
    replace_atomically(&path, |file| file.write_all(html.as_bytes()))
        .map_err(|e| format!("Could not save the guide: {e}"))?;
    Ok(path)
}

/// Write through a temporary sibling, then rename it over `path`.
///
/// Same directory because a rename is only atomic within one filesystem. The
/// temporary file is removed on every failure path.
fn replace_atomically(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name chosen"))?;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let temp = dir.join(format!(
        ".{}.hangar-tmp-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let written = write(&mut file)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temp, path)
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

fn native_chooser() -> Chooser {
    Arc::new(|name| {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save controls guide")
            .add_filter("Web page", &["html"])
            .set_file_name(name);
        if let Some(dir) = dirs::document_dir().or_else(dirs::home_dir) {
            dialog = dialog.set_directory(dir);
        }
        dialog.save_file()
    })
}

impl State {
    /// Every profile that can be compared: the live ones, then each backup's copies.
    fn sources(&self, library: &Library) -> Vec<(Option<Source>, String)> {
        let mut options = vec![(None, "Choose a profile".to_owned())];
        options.extend(
            crate::bindings::profile_options(library)
                .into_iter()
                .filter_map(|(id, label)| {
                    Some((Some(Source::Live(id?)), format!("Current · {label}")))
                }),
        );
        for copy in &self.copies {
            let taken = copy
                .dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            for entry in &copy.manifest.profiles {
                options.push((
                    Some(Source::Saved {
                        dir: copy.dir.clone(),
                        id: entry.id.clone(),
                    }),
                    format!("{taken} · {}", entry.friendly_name.trim()),
                ));
            }
        }
        options
    }
    fn load(&self, library: &Library, source: &Source) -> Result<Profile, String> {
        match source {
            Source::Live(id) => library
                .get(id)
                .map(|loaded| loaded.profile.clone())
                .ok_or_else(|| "That profile is no longer present.".into()),
            Source::Saved { dir, id } => {
                let copy = self
                    .copies
                    .iter()
                    .find(|copy| &copy.dir == dir)
                    .ok_or("That backup is no longer listed.")?;
                snapshot::verify(copy).map_err(|e| e.to_string())?;
                if copy.profile(id).is_none() {
                    return Err("This backup does not contain that profile.".into());
                }
                hangar_bindings::read(snapshot::profile_path(copy, id)).map_err(|e| e.to_string())
            }
        }
    }
    /// Recompute only when the chosen pair changes; files are read once per choice.
    fn compare(&mut self, library: &Library) {
        let pair = match (&self.comparison.from, &self.comparison.to) {
            (Some(from), Some(to)) => (from.clone(), to.clone()),
            _ => {
                self.comparison.computed = None;
                self.comparison.result = None;
                return;
            }
        };
        if self.comparison.computed.as_ref() == Some(&pair) {
            return;
        }
        let result = self
            .load(library, &pair.0)
            .and_then(|from| Ok(diff::compare(&from, &self.load(library, &pair.1)?)));
        self.comparison.result = Some(result);
        self.comparison.computed = Some(pair);
    }
}

/// Any two profiles, live or backed up, side by side.
fn comparison(state: &mut State, library: &Library, ui: &mut egui::Ui) {
    ui.add_space(18.0);
    ui::heading(ui, "Compare profiles");
    let options = state.sources(library);
    let label = |value: &Option<Source>| {
        options
            .iter()
            .find(|(option, _)| option == value)
            .map_or_else(|| options[0].1.clone(), |(_, label)| label.clone())
    };
    ui.horizontal_wrapped(|ui| {
        let text = label(&state.comparison.from);
        ui::select(
            ui,
            "compare-from",
            &mut state.comparison.from,
            &text,
            300.0,
            &options,
        );
        ui.label(egui_phosphor::regular::ARROW_RIGHT);
        let text = label(&state.comparison.to);
        ui::select(
            ui,
            "compare-to",
            &mut state.comparison.to,
            &text,
            300.0,
            &options,
        );
    });
    state.compare(library);
    match &state.comparison.result {
        None => {
            ui.label(egui::RichText::new("Choose two profiles to compare.").color(ui::theme::DIM));
        }
        Some(Err(error)) => {
            ui.label(error);
        }
        Some(Ok(changes)) if changes.is_empty() => {
            ui.label("No differences.");
        }
        Some(Ok(changes)) => {
            ui.label(
                egui::RichText::new(changes.summary())
                    .size(ui::theme::TEXT_MD)
                    .color(ui::theme::TEXT_HI),
            );
            draw_grouped(ui, &Grouped::of(changes));
        }
    }
}

/// A comparison, sorted the way it is read: bindings first, by context, then
/// the file's shape. Actions only one side has are counted, not listed: a sim
/// update adds dozens nobody has bound, and they would bury the bindings
/// under the cap.
#[derive(Debug, Default, PartialEq)]
struct Grouped<'a> {
    bindings: Vec<(&'a str, Vec<&'a diff::Change>)>,
    other: Vec<&'a diff::Change>,
    actions_added: usize,
    actions_removed: usize,
}

impl<'a> Grouped<'a> {
    fn of(changes: &'a diff::Diff) -> Self {
        use diff::Change;
        let mut grouped = Grouped::default();
        for change in &changes.changes {
            match change {
                Change::Bound { context, .. }
                | Change::Unbound { context, .. }
                | Change::Rebound { context, .. } => {
                    match grouped.bindings.iter_mut().find(|(c, _)| *c == context) {
                        Some((_, list)) => list.push(change),
                        None => grouped.bindings.push((context, vec![change])),
                    }
                }
                Change::ActionAdded { .. } => grouped.actions_added += 1,
                Change::ActionRemoved { .. } => grouped.actions_removed += 1,
                _ => grouped.other.push(change),
            }
        }
        grouped
    }
}

/// Width kept for the before → after column, as on the Assignments rows.
const CHANGE_TAIL_W: f32 = 260.0;

fn draw_grouped(ui: &mut egui::Ui, grouped: &Grouped) {
    let mut drawn = 0;
    for (context, list) in &grouped.bindings {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(*context)
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );
        for change in list {
            if drawn == MAX_CHANGES {
                break;
            }
            binding_change_row(ui, change);
            drawn += 1;
        }
    }
    if !grouped.other.is_empty() && drawn < MAX_CHANGES {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("Profile")
                .size(ui::theme::TEXT_SM)
                .color(ui::theme::DIM),
        );
        for change in grouped.other.iter().take(MAX_CHANGES - drawn) {
            ui.add(egui::Label::new(change.to_string()).truncate());
            drawn += 1;
        }
    }
    let total: usize =
        grouped.bindings.iter().map(|(_, l)| l.len()).sum::<usize>() + grouped.other.len();
    if total > drawn {
        ui.label(egui::RichText::new(format!("and {} more", total - drawn)).color(ui::theme::DIM));
    }
    if grouped.actions_added + grouped.actions_removed > 0 {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(format!(
                "Actions only one side has: {} added, {} removed. A sim update, usually.",
                grouped.actions_added, grouped.actions_removed
            ))
            .size(ui::theme::TEXT_SM)
            .color(ui::theme::DIM),
        );
    }
}

/// One binding change: what the control is, then what it was and is now.
fn binding_change_row(ui: &mut egui::Ui, change: &diff::Change) {
    use diff::Change;
    let (action, slot, from, to) = match change {
        Change::Bound {
            action, slot, to, ..
        } => (action, slot, None, Some(to)),
        Change::Unbound {
            action, slot, from, ..
        } => (action, slot, Some(from), None),
        Change::Rebound {
            action,
            slot,
            from,
            to,
            ..
        } => (action, slot, Some(from), Some(to)),
        _ => return,
    };
    let described = hangar_bindings::describe::describe(action);
    ui.horizontal(|ui| {
        let tail = CHANGE_TAIL_W.min(ui.available_width() * 0.5);
        let middle = (ui.available_width() - tail).max(40.0);
        ui.allocate_ui_with_layout(
            egui::vec2(middle, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width(middle);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&described.title)
                            .font(egui::FontId::new(ui::theme::TEXT_MD, ui::theme::semibold()))
                            .color(ui::theme::TEXT_HI),
                    )
                    .truncate(),
                )
                .on_hover_text(action.as_str());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(slot.as_str())
                            .size(ui::theme::TEXT_SM)
                            .color(ui::theme::DIMMER),
                    )
                    .truncate(),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(tail, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width(tail);
                // Before on top, after under it with an arrow: read down.
                let side = |text: Option<&String>, color, arrow: &str| {
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{arrow}{}",
                            text.map_or("not bound", |t| t.trim())
                        ))
                        .monospace()
                        .size(ui::theme::TEXT_SM)
                        .color(if text.is_some() {
                            color
                        } else {
                            ui::theme::DIMMER
                        }),
                    )
                    .truncate()
                };
                ui.add(side(from, ui::theme::DIM, ""));
                ui.add(side(to, ui::theme::ORANGE, "→ "));
            },
        );
    });
}

pub fn navigation(app: &mut HangarApp, ui: &mut egui::Ui) {
    app.binding_tools.finish_export();
    ui.horizontal_wrapped(|ui| {
        for (view, label) in [
            (View::Assignments, "Assignments"),
            (View::Shared, "Shared inputs"),
            (View::Backups, "Backups"),
        ] {
            if ui::controls::view_tab(ui, label, app.binding_tools.view == view).clicked() {
                app.binding_tools.view = view;
            }
        }
    });
    ui.add_space(12.0);
}

fn export_button(state: &mut State, library: &Library, selected: Option<&str>, ui: &mut egui::Ui) {
    let choosing = state.choosing();
    if ui::tool_button_enabled(
        ui,
        !choosing,
        egui_phosphor::regular::EXPORT,
        if choosing {
            "Choosing destination…"
        } else {
            "Export guide…"
        },
        ui::ButtonKind::Ghost,
    )
    .clicked()
    {
        let ctx = ui.ctx().clone();
        if let Err(error) = state.export(library, selected, move || ctx.request_repaint()) {
            state.message = Some(error);
        }
    }
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    let library = Arc::clone(&app.bindings);
    if app.binding_tools.view == View::Shared {
        let mut count = 0;
        for loaded in library.profiles.iter().filter(|p| {
            app.bindings_profile
                .as_deref()
                .is_none_or(|id| id == p.id())
        }) {
            if app
                .binding_tools
                .device
                .as_deref()
                .is_some_and(|device| device != loaded.device())
            {
                continue;
            }
            for shared in hangar_bindings::catalog::shared(loaded.id(), &loaded.profile) {
                if app
                    .bindings_context
                    .as_deref()
                    .is_some_and(|context| context != shared.context)
                {
                    continue;
                }
                count += 1;
                ui::heading(ui, &shared.label);
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} · {}",
                        loaded.name(),
                        shared.context,
                        shared.kind.as_str()
                    ))
                    .small()
                    .color(ui::theme::DIM),
                );
                for (action, _) in shared.uses {
                    ui.label(hangar_bindings::describe::describe(&action).title)
                        .on_hover_text(action);
                }
                ui.add_space(12.0);
            }
        }
        if count == 0 {
            ui.label("No shared inputs in this view.");
        }
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if ui::tool_button(
            ui,
            egui_phosphor::regular::COPY,
            "Back up all profiles",
            ui::ButtonKind::Primary,
        )
        .clicked()
        {
            if let Err(error) = app.binding_tools.backup(&library) {
                app.binding_tools.message = Some(error);
            }
        }
        if ui::tool_button(
            ui,
            egui_phosphor::regular::ARROW_CLOCKWISE,
            "Refresh",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            if let Some(store) = &app.binding_tools.store {
                app.binding_tools.copies = snapshot::list(store);
            }
        }
        export_button(
            &mut app.binding_tools,
            &library,
            app.bindings_profile.as_deref(),
            ui,
        );
    });
    ui.add_space(12.0);
    if let Some(message) = &app.binding_tools.message {
        ui.label(message);
    }
    if app.binding_tools.copies.is_empty() {
        ui.label("Refresh to find saved backups, or create a backup.");
    }
    for copy in app.binding_tools.copies.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{} · {} profiles · {}",
                copy.dir
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default(),
                copy.manifest.profiles.len(),
                hangar_core::disk::human(copy.bytes())
            ));
            let loaded = app
                .bindings_profile
                .as_deref()
                .and_then(|id| library.get(id));
            if ui::tool_button_enabled(
                ui,
                loaded.is_some(),
                egui_phosphor::regular::ARROWS_LEFT_RIGHT,
                "Compare",
                ui::ButtonKind::Ghost,
            )
            .clicked()
            {
                if let Some(loaded) = loaded {
                    if let Err(error) = app.binding_tools.inspect(&copy, loaded) {
                        app.binding_tools.message = Some(error);
                    }
                }
            }
        });
    }
    if app.bindings_profile.is_none() {
        ui.label("Select a control profile to compare or restore.");
    }
    let mut restore = false;
    let mut close = false;
    let locked = crate::bindings::editing_locked(app);
    if let Some(preview) = &mut app.binding_tools.preview {
        ui.add_space(18.0);
        ui::heading(ui, &preview.replacement.friendly_name.name);
        ui.label(preview.changes.summary());
        ui::controls::disclosure(ui, "restore-diff", "Changes", |ui| {
            for change in &preview.changes.changes {
                ui.label(change.to_string());
            }
        });
        if preview.confirm {
            ui.label("Replace this control profile with the inspected backup?");
            ui.label(preview.destination.display().to_string());
            ui.label(format!("From {}", preview.source.display()));
        }
        ui.horizontal_wrapped(|ui| {
            if ui::tool_button_enabled(
                ui,
                !locked && !preview.changes.is_empty(),
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                if preview.confirm {
                    "Confirm restore"
                } else {
                    "Restore profile"
                },
                ui::ButtonKind::Danger,
            )
            .clicked()
            {
                if preview.confirm {
                    restore = true;
                } else {
                    preview.confirm = true;
                }
            }
            close = ui::tool_button(ui, "", "Cancel", ui::ButtonKind::Ghost).clicked();
        });
    }
    if close {
        app.binding_tools.preview = None;
    }
    if restore {
        match app.binding_tools.restore() {
            Ok(dir) => {
                app.bindings = Arc::new(Library::load(dir));
                app.controllers = None;
            }
            Err(error) => app.binding_tools.message = Some(error),
        }
    }
    comparison(&mut app.binding_tools, &library, ui);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inert_maintenance_actions_refuse_at_the_boundary() {
        let mut state = State::default();
        assert!(state.backup(&Library::default()).is_err());
        assert!(state.export(&Library::default(), None, || {}).is_err());
        assert!(!state.choosing(), "inert export never opens a chooser");
        assert!(state.restore().is_err());
    }
    #[test]
    fn backup_compare_restore_and_stale_preview_use_only_scratch_files() {
        let root = std::env::temp_dir().join(format!("hangar-maintenance-{}", std::process::id()));
        let remote = root.join("remote");
        let store = root.join("backups");
        std::fs::create_dir_all(&remote).unwrap();
        let file = remote.join("inputprofile_0000000001");
        let original = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<Version Num=\"-1\"/>\n",
            "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">Original</FriendlyName>\n",
            "<Device DeviceName=\"Keyboard\" GUID=\"{0}\" ProductID=\"0\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
            "\t<Context ContextName=\"AIRCRAFT\">\n",
            "\t\t<Action ActionName=\"KEY_AXIS_ELEV_TRIM_SET\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
            "\t</Context>\n</Device>\n\n",
        );
        std::fs::write(&file, original).unwrap();
        hangar_bindings::roundtrip_ok(&file).unwrap();
        let mut state = State {
            store: Some(store.clone()),
            ..State::default()
        };
        state.backup(&Library::load(&remote)).unwrap();
        let copy = state.copies[0].clone();
        assert_eq!(
            std::fs::read_to_string(snapshot::profile_path(&copy, "inputprofile_0000000001"))
                .unwrap(),
            original
        );
        let changed = original.replace("Original", "Changed");
        std::fs::write(&file, &changed).unwrap();
        let library = Library::load(&remote);
        state.inspect(&copy, &library.profiles[0]).unwrap();
        assert_eq!(state.preview.as_ref().unwrap().changes.len(), 1);
        assert!(state.restore().is_err(), "confirmation required");
        state.preview.as_mut().unwrap().confirm = true;
        std::fs::write(&file, original.replace("Original", "Changed again")).unwrap();
        assert!(state.restore().unwrap_err().contains("changed"));
        std::fs::write(&file, &changed).unwrap();
        state.restore().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert!(
            state.copies.len() >= 2,
            "restore preserves the previous version"
        );
        let chosen = root.join("chosen").join("guide.html");
        std::fs::create_dir_all(chosen.parent().unwrap()).unwrap();
        let answer = chosen.clone();
        state.chooser = Some(Arc::new(move |_| Some(answer.clone())));
        state.export(&Library::load(&remote), None, || {}).unwrap();
        settle(&mut state);
        assert!(std::fs::read_to_string(&chosen).unwrap().contains("<html"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Wait for the injected chooser's thread, then collect its answer.
    fn settle(state: &mut State) {
        for _ in 0..200 {
            if !state.choosing() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        state.finish_export();
    }

    const PROFILE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Version Num=\"-1\"/>\n",
        "<FriendlyName PlatformAvailability=\"1\" Locked=\"false\">NAME</FriendlyName>\n",
        "<Device DeviceName=\"Keyboard\" GUID=\"{0}\" ProductID=\"0\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
        "\t<Context ContextName=\"AIRCRAFT\">\n",
        "\t\t<Action ActionName=\"ACTION\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
        "\t\t\t<Primary>\n\t\t\t\t<KEY Information=\"Joystick Button 8\">7</KEY>\n\t\t\t</Primary>\n",
        "\t\t</Action>\n",
        "\t</Context>\n</Device>\n\n",
    );

    /// Two scratch profiles that differ in name and in what one control does.
    fn scratch_library(tag: &str) -> (PathBuf, Library) {
        let root =
            std::env::temp_dir().join(format!("hangar-compare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (n, name, action) in [
            (1, "Alpha", "KEY_ELEV_TRIM_UP"),
            (2, "Bravo", "KEY_ELEV_TRIM_DN"),
        ] {
            std::fs::write(
                root.join(format!("inputprofile_000000000{n}")),
                PROFILE.replace("NAME", name).replace("ACTION", action),
            )
            .unwrap();
        }
        let library = Library::load(&root);
        assert_eq!(library.profiles.len(), 2);
        (root, library)
    }

    /// Render one frame headlessly; return the drawn text and how far past `width` it ran.
    fn render(width: f32, draw: impl FnMut(&mut egui::Ui)) -> (String, f32) {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.job.text.clone()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let mut draw = draw;
        let ctx = egui::Context::default();
        ui::theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 800.0),
            )),
            ..Default::default()
        };
        let mut overflow = 0.0f32;
        let output = ctx.run_ui(input, |ui| {
            draw(ui);
            overflow = ui.min_rect().right() - width;
        });
        let mut runs = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut runs);
        }
        for run in &runs {
            assert!(run.chars().count() <= 160, "prose run too long: {run}");
        }
        (runs.join("\n"), overflow)
    }

    #[test]
    fn save_as_renders_idle_choosing_and_saved_without_opening_a_dialog() {
        let (root, library) = scratch_library("save-as");
        // Inert: the button draws, pressing it refuses.
        let mut inert = State::default();
        for width in [760.0, 1200.0] {
            let (text, overflow) =
                render(width, |ui| export_button(&mut inert, &library, None, ui));
            assert!(text.contains("Export guide"));
            assert!(overflow <= 0.0, "export overflows by {overflow}");
        }
        assert!(inert.export(&library, None, || {}).is_err());

        // Choosing: a chooser that blocks until the test releases it.
        let destination = root.join("out").join("controls.html");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let answer = destination.clone();
        let mut state = State::with_chooser(Arc::new(move |name| {
            assert!(name.ends_with(".html"));
            gate.lock().unwrap().recv().ok();
            Some(answer.clone())
        }));
        state
            .export(&library, Some("inputprofile_0000000001"), || {})
            .unwrap();
        assert!(state.choosing());
        let (text, _) = render(760.0, |ui| export_button(&mut state, &library, None, ui));
        assert!(text.contains("Choosing destination"));
        release.send(()).unwrap();
        settle(&mut state);
        let saved = std::fs::read_to_string(&destination).unwrap();
        assert!(saved.contains("Alpha") && !saved.contains("Bravo"));
        assert!(state.message.as_deref().unwrap().starts_with("Guide saved"));

        // The dialog confirmed Replace, so the chosen file is replaced; a
        // cancel writes nothing.
        state.chooser = Some(Arc::new({
            let destination = destination.clone();
            move |_| Some(destination.clone())
        }));
        state.export(&library, None, || {}).unwrap();
        settle(&mut state);
        assert!(state.message.as_deref().unwrap().starts_with("Guide saved"));
        let replaced = std::fs::read_to_string(&destination).unwrap();
        assert!(replaced.contains("Alpha") && replaced.contains("Bravo"));
        assert_eq!(leftovers(destination.parent().unwrap()), 0);
        state.chooser = Some(Arc::new(|_| None));
        state.export(&library, None, || {}).unwrap();
        settle(&mut state);
        assert_eq!(state.message.as_deref(), Some("Export cancelled."));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Hidden temporary files left in a directory by a guide write.
    fn leftovers(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".hangar-tmp-")
            })
            .count()
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hangar-guide-save-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_guide_replaces_the_chosen_file_atomically() {
        let dir = scratch_dir("replace");
        let target = dir.join("guide.html");
        std::fs::write(&target, "old").unwrap();
        assert_eq!(write_guide("<html>new", &target).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "<html>new");
        assert_eq!(leftovers(&dir), 0, "temp file left after success");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_name_without_an_extension_gets_html() {
        let dir = scratch_dir("extension");
        let saved = write_guide("<html>", &dir.join("controls")).unwrap();
        assert_eq!(saved, dir.join("controls.html"));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "<html>");
        assert!(!dir.join("controls").exists());
        // Another extension is the person's choice and is kept.
        assert_eq!(guide_path(&dir.join("a.htm")), dir.join("a.htm"));
        assert_eq!(guide_path(&dir.join("a.txt")), dir.join("a.txt"));
        // The added name was never shown in the dialog, so it is not replaced.
        std::fs::write(&saved, "kept").unwrap();
        let refused = write_guide("<html>again", &dir.join("controls")).unwrap_err();
        assert!(refused.contains("already exists"), "{refused}");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "kept");
        assert_eq!(leftovers(&dir), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_guide_write_leaves_no_temp_file_and_the_old_file_intact() {
        let dir = scratch_dir("failure");
        let target = dir.join("guide.html");
        std::fs::write(&target, "old").unwrap();
        // The write itself fails part-way.
        let error = replace_atomically(&target, |file| {
            use std::io::Write;
            file.write_all(b"half")?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "disk full");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(leftovers(&dir), 0, "temp file left after a failed write");
        // The rename fails: the chosen name is a directory.
        let folder = dir.join("folder.html");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("inside"), "x").unwrap();
        let refused = write_guide("<html>", &folder).unwrap_err();
        assert!(refused.starts_with("Could not save the guide"), "{refused}");
        assert_eq!(leftovers(&dir), 0, "temp file left after a failed rename");
        // A folder that does not exist creates nothing.
        assert!(write_guide("<html>", &dir.join("missing").join("g.html")).is_err());
        assert!(!dir.join("missing").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Action churn from a sim update is counted, never listed, so it cannot
    /// push a binding change past the cap.
    #[test]
    fn a_comparison_groups_bindings_by_context_and_counts_action_churn() {
        use diff::Change;
        use hangar_bindings::model::Slot;
        let mut changes: Vec<Change> = (0..MAX_CHANGES + 5)
            .map(|n| Change::ActionAdded {
                context: "PLANE".into(),
                action: format!("KEY_NEW_{n}"),
            })
            .collect();
        changes.push(Change::ProfileRenamed {
            from: "A".into(),
            to: "B".into(),
        });
        for (context, action) in [
            ("PLANE", "KEY_GEAR_UP"),
            ("CAMERA", "KEY_ZOOM"),
            ("PLANE", "KEY_FLAPS"),
        ] {
            changes.push(Change::Bound {
                context: context.into(),
                action: action.into(),
                slot: Slot::Primary,
                to: "Joystick Button 1".into(),
            });
        }
        let changes = diff::Diff { changes };
        let grouped = Grouped::of(&changes);
        assert_eq!(grouped.actions_added, MAX_CHANGES + 5);
        assert_eq!(grouped.other.len(), 1);
        let contexts: Vec<_> = grouped
            .bindings
            .iter()
            .map(|(c, l)| (*c, l.len()))
            .collect();
        assert_eq!(contexts, [("PLANE", 2), ("CAMERA", 1)]);

        let (text, overflow) = render(760.0, |ui| draw_grouped(ui, &grouped));
        assert!(overflow <= 0.0, "{overflow}");
        assert!(text.contains("Joystick Button 1"), "{text}");
        assert!(!text.contains("KEY_NEW_0"), "churn is counted: {text}");
        assert!(
            text.contains(&format!("{} added", MAX_CHANGES + 5)),
            "{text}"
        );
        assert!(!text.contains("more"), "nothing hidden: {text}");
    }

    #[test]
    fn comparison_renders_nothing_selected_and_two_profiles_with_differences() {
        let (root, library) = scratch_library("compare");
        let store = root.join("backups");
        let mut state = State {
            store: Some(store),
            ..State::default()
        };
        for width in [760.0, 1200.0] {
            let (text, overflow) = render(width, |ui| comparison(&mut state, &library, ui));
            assert!(text.contains("Choose two profiles to compare."));
            assert!(overflow <= 0.0, "comparison overflows by {overflow}");
        }
        assert!(state.comparison.result.is_none());

        // Two live profiles.
        state.comparison.from = Some(Source::Live("inputprofile_0000000001".into()));
        state.comparison.to = Some(Source::Live("inputprofile_0000000002".into()));
        for width in [760.0, 1200.0] {
            let (text, overflow) = render(width, |ui| comparison(&mut state, &library, ui));
            assert!(overflow <= 0.0, "comparison overflows by {overflow}");
            assert!(text.contains("Alpha") && text.contains("Bravo"), "{text}");
            assert!(!text.contains("Choose two profiles"));
        }
        let changes = state.comparison.result.as_ref().unwrap().as_ref().unwrap();
        assert!(
            changes.len() >= 2,
            "rename plus a different binding: {changes:?}"
        );
        // Bindings first, under their context, by the name a person knows.
        let changes = changes.clone();
        let (text, _) = render(1200.0, |ui| comparison(&mut state, &library, ui));
        let grouped = Grouped::of(&changes);
        assert!(!grouped.bindings.is_empty(), "{changes:?}");
        let (context, list) = &grouped.bindings[0];
        assert!(text.contains(context), "{text}");
        if let diff::Change::Rebound { action, .. }
        | diff::Change::Bound { action, .. }
        | diff::Change::Unbound { action, .. } = list[0]
        {
            let title = hangar_bindings::describe::describe(action).title;
            let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
            assert!(
                at(&title) < at("renamed"),
                "bindings before the rest: {text}"
            );
        }

        // A backed-up copy against a live profile; the same profile is identical.
        state.backup(&library).unwrap();
        let dir = state.copies[0].dir.clone();
        assert!(state
            .sources(&library)
            .iter()
            .any(|(source, _)| matches!(source, Some(Source::Saved { .. }))));
        state.comparison.from = Some(Source::Saved {
            dir,
            id: "inputprofile_0000000001".into(),
        });
        state.comparison.to = Some(Source::Live("inputprofile_0000000001".into()));
        let (text, _) = render(1200.0, |ui| comparison(&mut state, &library, ui));
        assert!(text.contains("No differences."), "{text}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
