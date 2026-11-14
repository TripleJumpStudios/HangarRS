//! The application chrome: brand block, nav rail rows, status-bar items.
//!
//! These live in the design system rather than in `hangar-app` for the same
//! reason every other control does — the nav row is a button, and the house
//! rule is that no raw `egui::Button::new` exists outside this crate.

use crate::{controls::Tone, theme};
use egui::{Color32, CornerRadius, RichText, Stroke};

/// Width of the left nav rail.
pub const NAV_WIDTH: f32 = 174.0;

/// Height of the bottom status bar.
pub const STATUS_HEIGHT: f32 = 30.0;

/// Height of one nav row.
const NAV_ROW_H: f32 = 40.0;

/// Compact rail wordmark; the larger illustrated mark remains on the splash.
pub fn wordmark(ui: &mut egui::Ui) {
    let mut job = egui::text::LayoutJob::default();
    for (text, color) in [("Hangar", theme::TEXT_HI), ("RS", theme::ORANGE)] {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(23.0),
                color,
                ..Default::default()
            },
        );
    }
    ui.add_space(6.0);
    ui.label(job);
    ui.add_space(24.0);
}

/// The brand block at the top of the nav rail: painted mark, product name,
/// tracked-out tagline.
///
/// The mark is drawn rather than loaded so the crate carries no image asset and
/// no texture lifetime: a hangar arch — two uprights and a curved roof — in the
/// accent colour.
pub fn brand_block(ui: &mut egui::Ui, name: &str, tagline: &str) {
    ui.vertical_centered(|ui| {
        let size = egui::vec2(64.0, 64.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        paint_hangar_mark(ui.painter(), rect);

        ui.add_space(8.0);
        ui.label(
            RichText::new(name)
                .font(egui::FontId::new(15.0, theme::semibold()))
                .color(theme::TEXT_HI),
        );
        ui.add_space(3.0);
        ui.label(
            RichText::new(theme::track_out(tagline))
                .text_style(theme::eyebrow_style())
                .color(theme::DIMMER),
        );
    });
}

/// A hangar in outline: an arched roof over two uprights, on a ground line.
fn paint_hangar_mark(painter: &egui::Painter, rect: egui::Rect) {
    let stroke = Stroke::new(2.0_f32, theme::ORANGE);
    let inset = rect.shrink(9.0);
    let ground = inset.bottom();
    let spring = inset.top() + inset.height() * 0.45;

    // Arched roof, sampled as a polyline so it joins the uprights cleanly.
    let cx = inset.center().x;
    let rx = inset.width() * 0.5;
    let ry = spring - inset.top();
    let arch: Vec<egui::Pos2> = (0..=24)
        .map(|i| {
            let t = std::f32::consts::PI * (i as f32 / 24.0);
            egui::pos2(cx - rx * t.cos(), spring - ry * t.sin())
        })
        .collect();
    painter.add(egui::Shape::line(arch, stroke));

    // Uprights down to the ground line.
    painter.line_segment(
        [
            egui::pos2(inset.left(), spring),
            egui::pos2(inset.left(), ground),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(inset.right(), spring),
            egui::pos2(inset.right(), ground),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(inset.left(), ground),
            egui::pos2(inset.right(), ground),
        ],
        Stroke::new(2.0_f32, theme::ORANGE.gamma_multiply(0.55)),
    );

    // Door opening, dimmer so the mark reads as depth rather than a flat glyph.
    let door_w = inset.width() * 0.30;
    let door = egui::Rect::from_min_max(
        egui::pos2(cx - door_w * 0.5, ground - inset.height() * 0.30),
        egui::pos2(cx + door_w * 0.5, ground),
    );
    painter.rect_filled(
        door,
        CornerRadius::same(1),
        theme::ORANGE.gamma_multiply(0.30),
    );
}

/// One nav rail row: icon + label, accent bar and tinted fill when active.
///
/// The active marker is painted into the rail's left *margin*, outside the
/// button rect, so the label text does not shift when a row becomes active.
pub fn nav_item(ui: &mut egui::Ui, glyph: &str, label: &str, active: bool) -> egui::Response {
    let fg = if active { theme::ORANGE } else { theme::DIM };

    // Icon and label are separate runs: the icon must resolve in the icon
    // family, not the proportional chain (see `theme::ICONS`).
    let mut job = egui::text::LayoutJob::default();
    let fmt = |font| egui::TextFormat {
        font_id: font,
        color: fg,
        valign: egui::Align::Center,
        ..Default::default()
    };
    job.append(glyph, 0.0, fmt(egui::FontId::new(16.0, theme::icons())));
    job.append(label, 10.0, fmt(egui::FontId::proportional(15.0)));

    let background = ui.painter().add(egui::Shape::Noop);
    let response = ui.add(
        egui::Button::new(job)
            .fill(if active {
                theme::PANEL_HI
            } else {
                Color32::TRANSPARENT
            })
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(4))
            .min_size(egui::vec2(ui.available_width(), NAV_ROW_H)),
    );
    if response.hovered() && !active {
        ui.painter().set(
            background,
            egui::Shape::rect_filled(response.rect, CornerRadius::same(4), theme::PANEL),
        );
    }
    crate::controls::focus_outline(ui, &response);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });

    if active {
        let r = response.rect;
        let accent = egui::Rect::from_min_max(
            egui::pos2(r.left() - 12.0, r.center().y - 10.0),
            egui::pos2(r.left() - 9.0, r.center().y + 10.0),
        );
        ui.painter().rect_filled(accent, 2.0, theme::ORANGE);
    }
    ui.add_space(2.0);
    response
}

/// Hairline between nav groups, inset from the rail edges.
pub fn nav_separator(ui: &mut egui::Ui) {
    ui.add_space(9.0);
    let width = (ui.available_width() - 16.0).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(1.0_f32, theme::STROKE_SOFT),
        );
    });
    ui.add_space(9.0);
}

/// Vertical hairline between status-bar items.
pub fn status_sep(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 12.0), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0_f32, theme::STROKE),
    );
}

/// Status-bar item with a leading state dot.
pub fn status_dot(ui: &mut egui::Ui, text: &str, tone: Tone) {
    crate::controls::dot(ui, tone.color(), 7.0);
    ui.label(RichText::new(text).small().color(theme::TEXT));
}

/// Plain status-bar text, dimmed.
pub fn status_text(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(theme::DIM));
}

/// Monospaced status-bar figure — counts, versions, rates.
pub fn status_mono(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).monospace().size(12.0).color(color));
}
