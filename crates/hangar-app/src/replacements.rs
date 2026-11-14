//! Settings view for preserved package copies. All disk work follows an action.
use crate::app::HangarApp;
use hangar_install::replacements::{self, Copy};
use hangar_ui as ui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type Listing = Result<Arc<Vec<Copy>>, String>;
#[derive(Default)]
struct Snapshot {
    busy: bool,
    listing: Option<Listing>,
    message: Option<String>,
}
#[derive(Default)]
pub struct State {
    live: bool,
    shared: Arc<Mutex<Snapshot>>,
    confirm: Option<Arc<Copy>>,
}
impl State {
    pub fn live() -> Self {
        Self {
            live: true,
            ..Self::default()
        }
    }
    fn refresh(&mut self, communities: Vec<PathBuf>, ctx: egui::Context) {
        self.confirm = None;
        let mut state = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if state.busy {
            return;
        }
        state.busy = true;
        state.message = None;
        drop(state);
        let shared = Arc::clone(&self.shared);
        std::thread::spawn(move || {
            let result = read(&communities);
            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
            state.listing = Some(result);
            state.busy = false;
            ctx.request_repaint();
        });
    }
    fn purge(
        &mut self,
        writes_allowed: bool,
        other_install_running: bool,
        communities: Vec<PathBuf>,
        ctx: egui::Context,
    ) {
        let mut state = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if !self.live || !writes_allowed || other_install_running || state.busy {
            state.message = Some(
                "Deletion is unavailable while writes are disabled or an install or removal is running."
                    .into(),
            );
            return;
        }
        let Some(copy) = self.confirm.take() else {
            return;
        };
        state.busy = true;
        drop(state);
        let shared = Arc::clone(&self.shared);
        std::thread::spawn(move || {
            let approval = hangar_install::drop::Approval::given(format!(
                "Delete {} ({} bytes)",
                copy.path().display(),
                copy.bytes
            ));
            let result = replacements::purge(
                &copy,
                writes_allowed,
                hangar_runtime::sim::is_running(),
                &approval,
            );
            let listing = read(&communities);
            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
            state.message = Some(match result {
                Ok(()) => "Preserved copy deleted.".into(),
                Err(e) => e.to_string(),
            });
            state.listing = Some(listing);
            state.busy = false;
            ctx.request_repaint();
        });
    }
}
fn read(communities: &[PathBuf]) -> Listing {
    let mut roots = std::collections::BTreeSet::new();
    let mut copies = Vec::new();
    for community in communities {
        let parent = community
            .parent()
            .ok_or("Community has no parent folder")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if roots.insert(parent) {
            copies.extend(replacements::scan(community).map_err(|e| e.to_string())?);
        }
    }
    Ok(Arc::new(copies))
}

pub fn draw(app: &mut HangarApp, ui: &mut egui::Ui) {
    let communities = crate::drop_install::communities(app);
    // The shared busy state: an install, a placement or a Remove.
    let installing = app.community_busy().is_some();
    draw_state(
        &mut app.replacements,
        app.writes_allowed,
        installing,
        communities,
        ui,
    );
}

fn draw_state(
    state: &mut State,
    writes_allowed: bool,
    installing: bool,
    communities: Vec<PathBuf>,
    ui: &mut egui::Ui,
) {
    let (busy, listing, message) = {
        let state = state.shared.lock().unwrap_or_else(|e| e.into_inner());
        (state.busy, state.listing.clone(), state.message.clone())
    };
    ui::group(ui, Some("Preserved copies"), |ui| {
        if ui::tool_button(
            ui,
            egui_phosphor::regular::ARROW_CLOCKWISE,
            "Refresh",
            ui::ButtonKind::Ghost,
        )
        .clicked()
        {
            state.refresh(communities.clone(), ui.ctx().clone());
        }
        if busy {
            ui::state(ui, "Working", ui::Tone::Idle);
        }
        if let Some(message) = message {
            ui.label(message);
        }
        match listing {
            None => {
                ui.label("Refresh to inspect previous package versions.");
            }
            Some(Err(error)) => {
                ui.label(egui::RichText::new(error).color(ui::theme::ORANGE));
            }
            Some(Ok(copies)) => {
                let total = copies
                    .iter()
                    .fold(0u64, |total, copy| total.saturating_add(copy.bytes));
                ui::kv_row(ui, "Preserved copies", |ui| {
                    ui.label(format!(
                        "{} · {}",
                        copies.len(),
                        hangar_core::disk::human(total)
                    ));
                });
                for copy in copies.iter() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "{} · {} packages · {}",
                            copy.label(),
                            copy.packages,
                            hangar_core::disk::human(copy.bytes)
                        ))
                        .on_hover_text(copy.path().display().to_string());
                        if ui::tool_button_enabled(
                            ui,
                            writes_allowed && !busy && !installing,
                            egui_phosphor::regular::TRASH,
                            "Delete copy",
                            ui::ButtonKind::Ghost,
                        )
                        .clicked()
                        {
                            state.confirm = Some(Arc::new(copy.clone()));
                        }
                    });
                }
            }
        }
        if !writes_allowed {
            state.confirm = None;
        }
        if let Some(copy) = state.confirm.clone() {
            ui.label(format!(
                "Permanently delete {} of preserved packages?",
                hangar_core::disk::human(copy.bytes)
            ));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(copy.path().display().to_string()).monospace(),
                )
                .truncate(),
            );
            ui.horizontal_wrapped(|ui| {
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::TRASH,
                    "Delete permanently",
                    ui::ButtonKind::Danger,
                )
                .clicked()
                {
                    state.purge(
                        writes_allowed,
                        installing,
                        communities.clone(),
                        ui.ctx().clone(),
                    );
                }
                if ui::tool_button(
                    ui,
                    egui_phosphor::regular::X,
                    "Cancel",
                    ui::ButtonKind::Ghost,
                )
                .clicked()
                {
                    state.confirm = None;
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn populated_and_confirmed_copies_fit_without_deleting() {
        let root = std::env::temp_dir().join(format!("hangar-copy-ui-{}", std::process::id()));
        let copy_path = root.join(".hangar-replaced/2026-09-11T00-00-00Z/package");
        std::fs::create_dir_all(&copy_path).unwrap();
        std::fs::create_dir_all(root.join("Community")).unwrap();
        std::fs::write(copy_path.join("payload"), b"old").unwrap();
        let copies = Arc::new(replacements::scan(&root.join("Community")).unwrap());
        for width in [360.0, 600.0, 900.0] {
            for confirmed in [false, true] {
                let mut state = State::default();
                state.shared.lock().unwrap().listing = Some(Ok(Arc::clone(&copies)));
                if confirmed {
                    state.confirm = Some(Arc::new(copies[0].clone()));
                }
                let ctx = egui::Context::default();
                ui::theme::install(&ctx);
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 800.0),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run_ui(input, |ui| {
                    let right = ui.max_rect().right();
                    draw_state(&mut state, true, false, Vec::new(), ui);
                    assert!(ui.min_rect().right() <= right + 0.1);
                });
                assert!(copy_path.exists());
                assert!(!state.shared.lock().unwrap().busy);
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_inert_action_cannot_delete_even_with_writes_allowed() {
        let mut state = State::default();
        state.purge(true, false, Vec::new(), egui::Context::default());
        let snapshot = state.shared.lock().unwrap();
        assert!(!snapshot.busy);
        assert!(snapshot.message.is_some());
    }
}
