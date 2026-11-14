//! Structural primitives: page headers, groups, key/value rows, dividers.

use crate::theme;
use egui::{Align, CornerRadius, Layout, Margin, RichText, Stroke};

/// Group heading — sentence case, at the weight the nav rail's labels carry.
///
/// [`group`] has no box, so these words carry the grouping on their own.
/// [`theme::TEXT_LG`], one clear step above the body it groups — not the nav
/// rail's 13 pt, which reads as large only because the rail has no body text.
pub fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(egui::FontId::new(theme::TEXT_LG, theme::semibold()))
            .color(theme::TEXT_HI),
    );
}

/// Standard page header: title on the left, arbitrary widgets right.
///
/// No kicker line above the title: the nav rail already groups the tabs.
pub fn page_header(ui: &mut egui::Ui, title: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).heading().color(theme::TEXT_HI));
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.add_space(12.0);
}

/// A one-line subtitle under a page header.
pub fn subtitle(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(theme::TEXT_BODY).color(theme::DIM));
    ui.add_space(10.0);
}

// ── The page grid ────────────────────────────────────────────────────────────
//
// Every tab lays out on one grid, so the same card is the same fraction of the
// window whichever tab it is on. A hand-rolled split per tab is how they drift.

/// The single gap between columns, and between stacked rows.
pub const GUTTER: f32 = 14.0;

/// Space between a page header and the first row, and between rows.
///
/// [`group`] has no outline, so this gap is the only thing separating two
/// stacked groups. The nav rail spends 34 px between its groups, so this is not
/// generous by the standard the page is being held to.
pub const ROW_GAP: f32 = 18.0;

/// How a two-column row divides its width.
///
/// Named choices rather than a free fraction: the ratio is a design decision
/// that should be made once and reused, not re-invented per tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Split {
    /// 50/50 — two peers of comparable weight.
    Even,
    /// 58/42 — a visual lead (a map, a console, a detail pane) beside a column
    /// of readouts.
    Lead,
}

impl Split {
    /// Fraction of the content width (gutter already removed) taken by the
    /// lead column.
    fn lead_fraction(self) -> f32 {
        match self {
            Split::Even => 0.50,
            Split::Lead => 0.58,
        }
    }
}

/// Card geometry constants, shared by [`group`] and [`Cols::card`].
const CARD_PAD: f32 = 14.0;
const CARD_RADIUS: u8 = 12;

/// Geometry of a completed [`two_columns`] row.
pub struct RowGeom {
    /// Content height of the lead column as drawn this frame.
    pub lead_height: f32,
    /// Height of the row — the taller of the two columns.
    pub row_height: f32,
}

/// Column builder handed to [`two_columns`]. Call [`Cols::card`] twice.
///
/// One builder rather than two closure arguments: panel code captures
/// `&mut self`, and two closures passed side by side would both hold that
/// borrow at once. Adding columns in sequence keeps only one alive at a time.
pub struct Cols<'u> {
    ui: &'u mut egui::Ui,
    /// Top-left of the row, in screen space. Both columns are placed from here.
    origin: egui::Pos2,
    widths: [f32; 2],
    /// Height available to the row, used as the slot height before the content
    /// decides its own.
    height: f32,
    /// Clip rect of the parent, captured before the first child borrows it.
    parent_clip: egui::Rect,
    next: usize,
    /// Reserved background shape + drawn content rect, per column.
    slots: [Option<(egui::layers::ShapeIdx, egui::Rect)>; 2],
}

impl Cols<'_> {
    /// Add the next column as a card.
    ///
    /// The card's background is *reserved* here and painted once both columns
    /// are known, so the shorter card stretches to the row's height in the same
    /// frame it is drawn.
    ///
    /// Content stays its natural height and top-aligns; only the background
    /// stretches. Content that grows to fill the row *becomes* the tallest
    /// column and keeps growing frame after frame. A card whose content must
    /// fill converges its own height against [`RowGeom`], with a clamp and an
    /// overflow guard.
    ///
    /// The slot rect is computed from the row origin, not from a cursor the
    /// previous column advanced: otherwise one over-wide child (a label in a
    /// layout that does not wrap) pushes the second column off the window. The
    /// clip rect keeps the painting inside the slot too.
    pub fn card(&mut self, label: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
        let i = self.next.min(1);
        let x = self.origin.x + if i == 0 { 0.0 } else { self.widths[0] + GUTTER };
        let slot = egui::Rect::from_min_size(
            egui::pos2(x, self.origin.y),
            egui::vec2(self.widths[i], self.height),
        );

        // Reserved before the content so the fill lands *behind* it.
        let bg = self.ui.painter().add(egui::Shape::Noop);
        let clip = slot.intersect(self.parent_clip);

        let mut child = self.ui.new_child(
            egui::UiBuilder::new()
                .max_rect(slot)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_clip_rect(clip);
        egui::Frame::new()
            .inner_margin(Margin::same(CARD_PAD as i8))
            .show(&mut child, |ui| {
                ui.set_width(ui.available_width());
                if let Some(label) = label {
                    heading(ui, label);
                    ui.add_space(8.0);
                }
                add(ui);
            });

        self.slots[i] = Some((bg, child.min_rect()));
        self.next += 1;
    }
}

/// Two columns on the page grid, squared off to the same height.
///
/// Both cards reach the same bottom edge. Each card reserves its background
/// shape before drawing its content, and the row paints both to the final
/// height once it knows it. Feeding a measured height into the next frame
/// instead could only ever grow; this is correct on the first frame and
/// shrinks as readily as it grows.
///
/// Columns are allocated at an exact size: inside a `horizontal_top`,
/// `set_width` is only advisory, and rows came out several px short of a
/// full-width card below them.
pub fn two_columns(
    ui: &mut egui::Ui,
    id_source: &str,
    split: Split,
    build: impl FnOnce(&mut Cols<'_>),
) -> RowGeom {
    let id = ui.id().with(id_source);
    let prev: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);

    let content = (ui.available_width() - GUTTER).max(0.0);
    let lead_w = (content * split.lead_fraction()).max(0.0);
    let widths = [lead_w, (content - lead_w).max(0.0)];

    let origin = ui.cursor().min;
    let height = ui.available_height().max(0.0);
    let parent_clip = ui.clip_rect();

    let slots = {
        let mut cols = Cols {
            ui,
            origin,
            widths,
            height,
            parent_clip,
            next: 0,
            slots: [None, None],
        };
        build(&mut cols);
        cols.slots
    };

    let row_height = slots
        .iter()
        .flatten()
        .map(|(_, r)| r.height())
        .fold(0.0_f32, f32::max);

    for (bg, rect) in slots.iter().flatten() {
        let full = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), row_height));
        ui.painter().set(
            *bg,
            egui::epaint::RectShape::new(
                full,
                CornerRadius::same(CARD_RADIUS),
                theme::PANEL,
                Stroke::new(1.0_f32, theme::STROKE_SOFT),
                egui::StrokeKind::Inside,
            ),
        );
    }

    // The columns were drawn into child Uis, which do not move the parent's
    // cursor. Claim the row explicitly so whatever follows starts below it.
    ui.allocate_rect(
        egui::Rect::from_min_size(origin, egui::vec2(content + GUTTER, row_height)),
        egui::Sense::hover(),
    );

    // Stored purely so a caller converging its own content height has last
    // frame's row to aim at. Nothing in this function reads it back, so it
    // cannot influence layout.
    if (row_height - prev).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, row_height));
        ui.ctx().request_repaint();
    }

    RowGeom {
        lead_height: slots[0].map_or(0.0, |(_, r)| r.height()),
        row_height,
    }
}

/// A named group of rows aligned to the page margin.
///
/// No box: a heading and air do the grouping, as on the nav rail. [`section`]
/// and [`divider`] are the heavier tools for a page that needs more separation
/// than a heading gives it.
///
/// Groups share the page's left edge; the heading and spacing establish
/// hierarchy without nested indentation.
///
/// For a group that must square off against a neighbour, use [`Cols::card`]
/// inside [`two_columns`] instead.
pub fn group<R>(ui: &mut egui::Ui, label: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .inner_margin(Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            // Fill the column rather than shrink-wrapping the content, so a
            // card holding two sliders lines up with the card above it holding
            // a wide grid.
            ui.set_width(ui.available_width());
            if let Some(label) = label {
                heading(ui, label);
                ui.add_space(8.0);
            }
            add(ui)
        })
        .inner
}

/// Section heading: orange tick + title.
pub fn section(ui: &mut egui::Ui, label: &str) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_at_least(egui::vec2(3.0, 17.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, theme::ORANGE);
        ui.add_space(6.0);
        ui.label(
            RichText::new(label)
                .font(egui::FontId::new(theme::TEXT_XL, theme::semibold()))
                .color(theme::TEXT_HI),
        );
    });
    ui.add_space(6.0);
}

/// Key on the left, caller-drawn value on the right.
///
/// No hairline under the row: the gap does that job for a stack of short rows.
/// Where a page genuinely needs a cut, [`divider`] is the tool, once.
pub fn kv_row(ui: &mut egui::Ui, key: &str, value: impl FnOnce(&mut egui::Ui)) {
    // Capped at [`theme::ROW_MAX_WIDTH`], not the page's width: a right-to-left
    // value takes whatever it is given, which would put key and value at
    // opposite margins of the page.
    let width = ui.available_width().min(theme::ROW_MAX_WIDTH);
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_max_width(width);
            ui.label(RichText::new(key).size(theme::TEXT_BODY).color(theme::DIM));
            ui.with_layout(Layout::right_to_left(Align::Center), value);
        },
    );
    ui.add_space(8.0);
}

/// Short settings use adjacent label/value columns, not opposite page margins.
/// Path-heavy rows can keep using `kv_row` to give the value more space.
pub fn inline_value(ui: &mut egui::Ui, key: &str, value: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(112.0);
            ui.add(
                egui::Label::new(RichText::new(key).size(theme::TEXT_BODY).color(theme::DIM))
                    .truncate(),
            );
        });
        ui.vertical(value);
    });
    ui.add_space(8.0);
}

/// Inset hairline between groups of rows — the page-body twin of
/// `shell::nav_separator`, cut to the same 8 px inset and 9 px of air.
///
/// The grouping tool of last resort, since [`group`] has no box.
pub fn divider(ui: &mut egui::Ui) {
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

/// Elide `text` from the left so it fits `max_width` in `font`.
///
/// Measured rather than counted: a character limit is only right for one
/// column width, and a right-aligned run that does not fit grows *leftwards*,
/// over its own label.
pub fn elide_to_width(ui: &egui::Ui, text: &str, font: &egui::FontId, max_width: f32) -> String {
    // Monospace, so one glyph measurement describes every character.
    let glyph = ui.fonts_mut(|f| f.glyph_width(font, '0')).max(1.0);
    let fits = (max_width / glyph).floor().max(0.0) as usize;
    elide_left(text, fits)
}

/// Trim to at most `max` characters, keeping the **end**.
///
/// These paths run to ~140 characters and the distinguishing part is always the
/// tail, so the head is what goes.
fn elide_left(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_owned();
    }
    if max <= 1 {
        return "…".repeat(max);
    }
    let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{tail}")
}

/// Fade the bottom edge of a scrolling region to signal content below.
///
/// Scrollbars here are floating, so a tab whose content runs past the window
/// otherwise looks truncated rather than scrollable. A fade costs no layout
/// space and keeps all four page insets equal.
///
/// `remaining` is how much content is still hidden; the fade eases in over the
/// first `FADE_H` of it so arriving at the bottom does not end with a hard
/// switch-off.
pub fn scroll_fade(painter: &egui::Painter, viewport: egui::Rect, remaining: f32) {
    const FADE_H: f32 = 28.0;

    if remaining <= 0.5 || viewport.height() <= FADE_H {
        return;
    }
    // Ease in over the last stretch so the hint disappears smoothly rather than
    // popping out on the final pixel of scroll.
    let strength = (remaining / FADE_H).clamp(0.0, 1.0);

    let top = viewport.bottom() - FADE_H;
    let clear =
        egui::Color32::from_rgba_unmultiplied(theme::BG.r(), theme::BG.g(), theme::BG.b(), 0);
    let solid = theme::BG.gamma_multiply(strength);

    // A vertex-coloured mesh rather than a stack of thin rectangles: egui
    // interpolates in premultiplied alpha, so this is one shape and a true
    // gradient instead of visible banding.
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(egui::pos2(viewport.left(), top), clear);
    mesh.colored_vertex(egui::pos2(viewport.right(), top), clear);
    mesh.colored_vertex(egui::pos2(viewport.left(), viewport.bottom()), solid);
    mesh.colored_vertex(egui::pos2(viewport.right(), viewport.bottom()), solid);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Run one frame of a root `Ui` with no window or GPU behind it.
    ///
    /// `Context::run_ui` is what makes the page grid testable at all: egui 0.35
    /// hands panels a `Ui` rather than a `Context`, and this is the headless
    /// version of the same thing.
    fn frame(size: egui::Vec2, build: impl FnMut(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        // The `FullOutput` is the tessellated frame; these tests only care
        // about the geometry the closure observed while producing it.
        let _ = ctx.run_ui(input, build);
    }

    /// A `kv_row` puts its value in a right-to-left layout, which takes every
    /// pixel it is offered, so uncapped the key and value sit at opposite
    /// margins of the page.
    ///
    /// The row is capped at [`theme::ROW_MAX_WIDTH`] whatever the page is, and
    /// it still gives way on a narrow window rather than overflowing.
    #[test]
    fn a_key_and_its_value_never_end_up_at_opposite_margins() {
        for page in [1600.0_f32, 900.0, 620.0, 400.0] {
            let seen: RefCell<Option<(f32, f32)>> = RefCell::new(None);
            frame(egui::vec2(page + 40.0, 400.0), |ui| {
                ui.set_max_width(page);
                kv_row(ui, "Wine binary", |ui| {
                    let r = ui.label("…/files/bin/wine");
                    *seen.borrow_mut() = Some((r.rect.left(), ui.min_rect().left()));
                });
            });
            let (value_left, _) = seen.into_inner().expect("the row should have drawn");
            let gap = value_left - theme::PAGE_INSET as f32;
            assert!(
                gap <= theme::ROW_MAX_WIDTH,
                "at a {page} px page the value starts {gap:.0} px in, past the \
                 {} px measure — the key and the value are back at opposite margins",
                theme::ROW_MAX_WIDTH
            );
        }
    }

    /// Lay out one row and report `(lead, trail, available_width)`, with the
    /// lead card asked to draw `lead_content_w` px of content.
    fn row_geometry(lead_content_w: f32) -> (egui::Rect, egui::Rect, f32) {
        let seen: RefCell<Vec<egui::Rect>> = RefCell::new(Vec::new());
        let avail = RefCell::new(0.0_f32);

        frame(egui::vec2(1000.0, 700.0), |ui| {
            *avail.borrow_mut() = ui.available_width();
            two_columns(ui, "t", Split::Lead, |cols| {
                cols.card(None, |ui| {
                    seen.borrow_mut().push(ui.max_rect());
                    ui.allocate_space(egui::vec2(lead_content_w, 20.0));
                });
                cols.card(None, |ui| {
                    seen.borrow_mut().push(ui.max_rect());
                });
            });
        });

        let seen = seen.into_inner();
        assert_eq!(seen.len(), 2, "both cards should have drawn");
        (seen[0], seen[1], avail.into_inner())
    }

    /// If the cursor advances by the width the first card actually *drew*, one
    /// over-wide child pushes the second column past the window edge, where it
    /// is clipped out of sight.
    ///
    /// Asserted as an invariant: the trailing column must land in the identical
    /// place whether the lead card holds 10 px of content or twice the page
    /// width.
    #[test]
    fn the_trailing_column_does_not_move_with_the_lead_cards_content() {
        let (narrow_lead, narrow_trail, _) = row_geometry(10.0);
        let (wide_lead, wide_trail, _) = row_geometry(2000.0);

        assert!(
            (narrow_trail.left() - wide_trail.left()).abs() < 0.5,
            "the trailing column moved {:.1} px when the lead card overflowed \
             (narrow={:.1}, wide={:.1})",
            (narrow_trail.left() - wide_trail.left()).abs(),
            narrow_trail.left(),
            wide_trail.left(),
        );
        assert!(
            (narrow_lead.width() - wide_lead.width()).abs() < 0.5,
            "the lead column changed width with its content"
        );
    }

    /// The row divides on the ratio `Split` promises, and stays inside the page.
    #[test]
    fn a_lead_row_is_58_percent_and_fits_the_page() {
        let (lead, trail, avail) = row_geometry(2000.0);
        let content = avail - GUTTER;
        let lead_w = content * Split::Lead.lead_fraction();

        assert!(
            (trail.left() - lead.left() - (lead_w + GUTTER)).abs() < 0.5,
            "trail column misplaced: lead.left={} trail.left={} expected gap={}",
            lead.left(),
            trail.left(),
            lead_w + GUTTER
        );
        assert!(
            trail.right() <= lead.left() - CARD_PAD + content + GUTTER + 0.5,
            "trail column ran past the content width: right={} limit={}",
            trail.right(),
            lead.left() - CARD_PAD + content + GUTTER
        );

        let lead_outer = lead.width() + 2.0 * CARD_PAD;
        assert!(
            (lead_outer / content - 0.58).abs() < 0.01,
            "lead column is {:.1}% of the row, expected 58%",
            100.0 * lead_outer / content
        );
    }

    /// A card must not paint outside its own column, even when its content
    /// overflows — otherwise the neighbour is drawn over rather than displaced,
    /// which is the same bug wearing a different hat.
    #[test]
    fn a_cards_clip_rect_is_its_own_column() {
        let seen: RefCell<Vec<(egui::Rect, egui::Rect)>> = RefCell::new(Vec::new());

        frame(egui::vec2(1000.0, 700.0), |ui| {
            two_columns(ui, "t", Split::Even, |cols| {
                cols.card(None, |ui| {
                    seen.borrow_mut().push((ui.max_rect(), ui.clip_rect()));
                });
                cols.card(None, |ui| {
                    seen.borrow_mut().push((ui.max_rect(), ui.clip_rect()));
                });
            });
        });

        let seen = seen.borrow();
        let (lead_rect, lead_clip) = seen[0];
        let (trail_rect, _) = seen[1];
        assert!(
            lead_clip.right() <= trail_rect.left() + 0.5,
            "the lead card can paint into the trailing column: clip right={} trail left={}",
            lead_clip.right(),
            trail_rect.left()
        );
        assert!(lead_clip.contains_rect(lead_rect.shrink(0.5)));
    }

    // ── Path elision ────────────────────────────────────────────────────────
    //
    // These need the *real* font, not egui's default, so they install the theme
    // before measuring.

    /// Measure what a string actually renders to, in the real font.
    fn measured(text: &str, font: &egui::FontId, ui: &egui::Ui) -> f32 {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                .rect
                .width()
        })
    }

    /// Run a closure inside a real headless `Ui`, so font metrics are genuine.
    fn with_ui<R>(f: impl FnOnce(&egui::Ui) -> R) -> R {
        // `run_ui` takes an `FnMut`, so the once-closure moves into an Option
        // that the frame callback takes from.
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1180.0, 760.0),
            )),
            ..Default::default()
        };
        let mut f = Some(f);
        let mut out = None;
        let _ = ctx.run_ui(input, |ui| {
            if let Some(f) = f.take() {
                out = Some(f(ui));
            }
        });
        out.expect("closure ran")
    }

    const LONG: &str =
        "/mnt/steam/SteamLibrary/steamapps/common/Proton - Experimental/files/bin/wine";

    /// A fixed 68 characters is ~449 px of 11 px monospace; inside a half-width
    /// card (~415 px of value room) the right-aligned run does not fit and grows
    /// leftwards over its own label. Asserted as a width, because a character
    /// count is only ever right for one column.
    #[test]
    fn an_elided_path_never_exceeds_the_width_it_was_given() {
        with_ui(|ui| {
            let font = egui::FontId::monospace(11.0);
            for room in [0.0, 12.0, 40.0, 120.0, 320.0, 415.0, 872.0, 4000.0] {
                let out = elide_to_width(ui, LONG, &font, room);
                let width = measured(&out, &font, ui);
                assert!(
                    width <= room + 0.5,
                    "elided to {width:.1} px for {room:.1} px of room: {out:?}"
                );
            }
        });
    }

    /// The tail is the half that identifies a path, so it is the half that must
    /// survive.
    #[test]
    fn elision_keeps_the_tail_and_marks_the_cut() {
        with_ui(|ui| {
            let font = egui::FontId::monospace(11.0);
            let out = elide_to_width(ui, LONG, &font, 320.0);
            assert!(out.starts_with('…'), "no ellipsis in {out:?}");
            assert!(out.ends_with("files/bin/wine"), "tail lost in {out:?}");
        });
    }

    /// A path that fits is shown whole, with no ellipsis.
    #[test]
    fn a_path_that_fits_is_left_alone() {
        with_ui(|ui| {
            let font = egui::FontId::monospace(11.0);
            let short = "/home/user";
            assert_eq!(elide_to_width(ui, short, &font, 872.0), short);
        });
    }

    /// A half-width card must yield visibly fewer characters than a fixed 68.
    #[test]
    fn a_half_width_card_takes_far_fewer_than_the_old_fixed_count() {
        with_ui(|ui| {
            let font = egui::FontId::monospace(11.0);
            let out = elide_to_width(ui, LONG, &font, 415.0);
            assert!(
                out.chars().count() < 68,
                "{} chars still fits the old assumption",
                out.chars().count()
            );
        });
    }

    #[test]
    fn short_paths_are_untouched() {
        assert_eq!(elide_left("/short/path", 40), "/short/path");
        assert_eq!(elide_left("", 0), "");
    }

    #[test]
    fn long_paths_keep_their_tail() {
        let out = elide_left(LONG, 40);
        assert!(out.starts_with('…'));
        assert!(out.ends_with("files/bin/wine"));
        assert_eq!(out.chars().count(), 40);
    }

    /// Degenerate widths must not panic or produce something longer than asked.
    #[test]
    fn tiny_budgets_degrade_gracefully() {
        assert_eq!(elide_left("/some/path", 0), "");
        assert_eq!(elide_left("/some/path", 1), "…");
        assert_eq!(elide_left("/some/path", 2).chars().count(), 2);
    }

    /// Multi-byte characters must not be split.
    #[test]
    fn elision_is_char_safe() {
        let s = "ααααααααααααααααααααααααααα";
        assert_eq!(elide_left(s, 10).chars().count(), 10);
    }
}
