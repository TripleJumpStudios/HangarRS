//! A draggable sequence row with an insertion line above/below the target.

use crate::theme;

#[derive(Clone)]
struct Drag<T> {
    list: egui::Id,
    item: T,
}

pub enum SequenceEdit<T> {
    Move {
        item: T,
        gap: usize,
    },
    Remove,
    /// The details toggle was pressed.
    Details,
}

/// One launch item. Dragging is confined to the title, leaving buttons usable.
pub struct SequenceRow<'a, T> {
    pub list: egui::Id,
    pub item: T,
    pub index: usize,
    pub total: usize,
    pub title: &'a str,
    pub detail: &'a str,
    pub status: &'a str,
    pub tone: crate::Tone,
    pub editable: bool,
    pub removable: bool,
    /// `Some(open)` draws a details toggle while the row is not being edited.
    pub details: Option<bool>,
}

impl<T: Clone + std::fmt::Debug + std::hash::Hash + Send + Sync + 'static> SequenceRow<'_, T> {
    pub fn show(self, ui: &mut egui::Ui) -> Option<SequenceEdit<T>> {
        self.show_with(ui, |_| {})
    }

    /// Keep results and session controls with their item, above its separator.
    pub fn show_with(
        self,
        ui: &mut egui::Ui,
        actions: impl FnOnce(&mut egui::Ui),
    ) -> Option<SequenceEdit<T>> {
        let mut edit = None;
        let row = ui
            .push_id(self.list.with(&self.item), |ui| {
                ui.horizontal(|ui| {
                    let width =
                        (ui.available_width() - 164.0).clamp(100.0, theme::LABEL_COLUMN_MAX_WIDTH);
                    let title = |ui: &mut egui::Ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, 44.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                if self.editable {
                                    ui.label(
                                        crate::icon(egui_phosphor::regular::DOTS_SIX_VERTICAL)
                                            .color(theme::DIMMER),
                                    );
                                }
                                ui.label(
                                    egui::RichText::new((self.index + 1).to_string())
                                        .small()
                                        .color(theme::DIMMER),
                                );
                                ui.vertical(|ui| {
                                    ui.set_width((width - 54.0).max(40.0));
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(self.title)
                                                .size(theme::TEXT_MD)
                                                .color(theme::TEXT_HI),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(self.title);
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(self.detail)
                                                .size(theme::TEXT_SM)
                                                .color(theme::DIM),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(self.detail);
                                });
                            },
                        );
                    };
                    if self.editable {
                        ui.dnd_drag_source(
                            self.list.with(&self.item),
                            Drag {
                                list: self.list,
                                item: self.item.clone(),
                            },
                            title,
                        )
                        .response
                        .on_hover_text("Drag to change launch order");
                    } else {
                        title(ui);
                    }
                    ui.horizontal(|ui| {
                        let glyph = match self.status {
                            "Included" => egui_phosphor::regular::CHECK,
                            "Already running" | "Running" => egui_phosphor::regular::PLAY_CIRCLE,
                            "Waiting" | "Starting" | "Preparing scenery" => {
                                egui_phosphor::regular::HOURGLASS
                            }
                            "Failed" => egui_phosphor::regular::WARNING_CIRCLE,
                            "Skipped" => egui_phosphor::regular::PROHIBIT,
                            _ => egui_phosphor::regular::CIRCLE,
                        };
                        if self.status != "Included" {
                            ui.label(
                                crate::icon(glyph)
                                    .size(theme::TEXT_LG)
                                    .color(self.tone.color()),
                            )
                            .on_hover_text(self.status);
                        }
                        if self.editable {
                            for (glyph, label, enabled, gap) in [
                                (
                                    egui_phosphor::regular::ARROW_UP,
                                    "Move up",
                                    self.index > 0,
                                    self.index.saturating_sub(1),
                                ),
                                (
                                    egui_phosphor::regular::ARROW_DOWN,
                                    "Move down",
                                    self.index + 1 < self.total,
                                    self.index + 2,
                                ),
                            ] {
                                if ui
                                    .add_enabled_ui(enabled, |ui| {
                                        crate::controls::action_icon(ui, glyph, label)
                                    })
                                    .inner
                                    .clicked()
                                {
                                    edit = Some(SequenceEdit::Move {
                                        item: self.item.clone(),
                                        gap,
                                    });
                                }
                            }
                            if self.removable
                                && crate::controls::action_icon(
                                    ui,
                                    egui_phosphor::regular::X,
                                    "Remove from flight",
                                )
                                .clicked()
                            {
                                edit = Some(SequenceEdit::Remove);
                            }
                        }
                        if let Some(open) = self.details.filter(|_| !self.editable) {
                            let (glyph, label) = if open {
                                (egui_phosphor::regular::CARET_UP, "Hide details")
                            } else {
                                (egui_phosphor::regular::INFO, "Details")
                            };
                            if crate::controls::action_icon(ui, glyph, label).clicked() {
                                edit = Some(SequenceEdit::Details);
                            }
                        }
                        if !self.editable && self.status != "Included" {
                            ui.label(
                                egui::RichText::new(self.status)
                                    .small()
                                    .color(self.tone.color()),
                            );
                        }
                    });
                });
            })
            .response;
        if self.editable
            && row
                .dnd_hover_payload::<Drag<T>>()
                .is_some_and(|d| d.list == self.list)
        {
            let below = ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| p.y > row.rect.center().y);
            ui.painter().hline(
                row.rect.x_range(),
                if below {
                    row.rect.bottom()
                } else {
                    row.rect.top()
                },
                egui::Stroke::new(2.0, theme::ORANGE),
            );
            if let Some(drag) = row.dnd_release_payload::<Drag<T>>() {
                edit = Some(SequenceEdit::Move {
                    item: drag.item.clone(),
                    gap: self.index + usize::from(below),
                });
            }
        }
        actions(ui);
        let (rule, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(
            rule.x_range(),
            rule.center().y,
            egui::Stroke::new(1.0, theme::STROKE_SOFT),
        );
        edit
    }
}

/// Returns the dragged item and whether to insert it below this row.
pub fn reorder_row<T: Clone + std::fmt::Debug + std::hash::Hash + Send + Sync + 'static>(
    ui: &mut egui::Ui,
    list: egui::Id,
    item: T,
    number: usize,
    title: &str,
    detail: &str,
) -> Option<(T, bool)> {
    let id = list.with(&item);
    let response = ui
        .dnd_drag_source(id, Drag { list, item }, |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::icon(egui_phosphor::regular::DOTS_SIX_VERTICAL).color(theme::DIM));
                ui.label(egui::RichText::new(number.to_string()).color(theme::ORANGE));
                let detail_width = ui
                    .fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                detail.to_owned(),
                                egui::FontId::proportional(theme::TEXT_SM),
                                theme::DIM,
                            )
                            .size()
                            .x
                    })
                    .min(ui.available_width() * 0.55);
                let title_width = (ui.available_width() - detail_width - 12.0).max(0.0);
                ui.add_sized(
                    [title_width, 24.0],
                    egui::Label::new(
                        egui::RichText::new(title)
                            .size(theme::TEXT_MD)
                            .color(theme::TEXT_HI),
                    )
                    .truncate(),
                );
                ui.add_sized(
                    [detail_width, 24.0],
                    egui::Label::new(
                        egui::RichText::new(detail)
                            .size(theme::TEXT_SM)
                            .color(theme::DIM),
                    )
                    .truncate(),
                );
            });
        })
        .response
        .on_hover_text("Drag to change launch order");
    let below = ui
        .input(|input| input.pointer.interact_pos())
        .is_some_and(|pos| pos.y > response.rect.center().y);
    if response
        .dnd_hover_payload::<Drag<T>>()
        .is_some_and(|drag| drag.list == list)
    {
        let y = if below {
            response.rect.bottom()
        } else {
            response.rect.top()
        };
        ui.painter().hline(
            response.rect.x_range(),
            y,
            egui::Stroke::new(2.0, theme::ORANGE),
        );
        if let Some(drag) = response.dnd_release_payload::<Drag<T>>() {
            return Some((drag.item.clone(), below));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_row_supports_pointer_drag_and_keyboard_moves() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let frame = |events| {
            let mut edits = Vec::new();
            let mut rectangles = Vec::new();
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    for index in 0..3 {
                        let response = ui.scope(|ui| {
                            if let Some(edit) = (SequenceRow {
                                list: egui::Id::new("sequence-test"),
                                item: index,
                                index,
                                total: 3,
                                title: ["MSFS", "Alpha", "Zulu"][index],
                                detail: "Sample app",
                                status: "Included",
                                tone: crate::Tone::Idle,
                                editable: true,
                                removable: index != 0,
                                details: None,
                            })
                            .show(ui)
                            {
                                edits.push(edit);
                            }
                        });
                        rectangles.push(response.response.rect);
                    }
                },
            );
            (rectangles, edits)
        };
        let (rects, _) = frame(vec![]);
        let from = rects[2].left_center() + egui::vec2(80.0, 0.0);
        let to = rects[0].left_center() + egui::vec2(80.0, -10.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(from), button(from, true)]);
        frame(vec![egui::Event::PointerMoved(
            from - egui::vec2(0.0, 12.0),
        )]);
        frame(vec![egui::Event::PointerMoved(to)]);
        let (_, edits) = frame(vec![button(to, false)]);
        assert!(edits
            .iter()
            .any(|edit| matches!(edit, SequenceEdit::Move { item: 2, gap: 0 })));
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        ctx.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        let mut moved = false;
        for _ in 0..8 {
            frame(vec![key(egui::Key::Tab, true), key(egui::Key::Tab, false)]);
            let (_, edits) = frame(vec![
                key(egui::Key::Enter, true),
                key(egui::Key::Enter, false),
            ]);
            moved |= edits
                .iter()
                .any(|edit| matches!(edit, SequenceEdit::Move { .. }));
            if moved {
                break;
            }
        }
        assert!(moved, "launch order must be editable without dragging");
    }

    #[test]
    fn pointer_drag_returns_the_requested_insertion_gap() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let list = egui::Id::new("test-order");
        let frames = |events| {
            let mut rectangles = Vec::new();
            let mut dropped = None;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    for (index, name) in ["Alpha", "MSFS", "Zulu"].iter().enumerate() {
                        let row = ui.scope(|ui| {
                            if let Some((item, below)) =
                                reorder_row(ui, list, index, index + 1, name, "ready")
                            {
                                dropped = Some((item, index + usize::from(below)));
                            }
                        });
                        rectangles.push(row.response.rect);
                    }
                },
            );
            (rectangles, dropped)
        };
        let (rectangles, _) = frames(vec![]);
        let from = rectangles[2].center();
        let to = rectangles[0].center() - egui::vec2(0.0, 6.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frames(vec![egui::Event::PointerMoved(from), button(from, true)]);
        frames(vec![egui::Event::PointerMoved(
            from - egui::vec2(0.0, 10.0),
        )]);
        frames(vec![egui::Event::PointerMoved(to)]);
        let (_, dropped) = frames(vec![button(to, false)]);
        assert_eq!(dropped, Some((2, 0)));
    }
}
