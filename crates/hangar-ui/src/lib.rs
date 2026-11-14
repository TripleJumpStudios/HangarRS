//! Shared visual language for HangarRS.
//!
//! egui is immediate-mode: there is no retained widget tree to restyle, so
//! "the design system" is this crate. Panels compose these primitives instead
//! of hand-rolling frames and painters, which is what keeps tabs from drifting
//! apart visually.
//!
//! Two house rules:
//!
//! * **Zero raw `ui.button` / `egui::Button::new` call sites outside this
//!   crate.** Everything interactive routes through [`controls`] or [`shell`].
//! * **No crate below `hangar-app` depends on egui.** This crate is the sole
//!   exception, by definition — it *is* the toolkit layer.

pub mod console;
pub mod controls;
pub mod gauge;
pub mod layout;
pub mod map;
pub mod reorder;
pub mod shell;
pub mod splash;
pub mod theme;

pub use console::{console_header, console_view, level_color, ConsoleLevel, ConsoleLine};
pub use controls::{
    activity, dot, icon, icon_button, icon_text, mark, param_cell, segment, select, state, stepper,
    text_field, toggle_chip, tool_button, tool_button_enabled, ButtonKind, Step, Tone,
};
pub use gauge::{bipolar_bar, nice_ceil};
pub use layout::{
    divider, elide_to_width, group, heading, inline_value, kv_row, page_header, scroll_fade,
    section, subtitle, two_columns, Cols, RowGeom, Split, GUTTER, ROW_GAP,
};
pub use reorder::reorder_row;
pub use shell::{
    brand_block, nav_item, nav_separator, status_dot, status_mono, status_sep, status_text,
    wordmark, NAV_WIDTH, STATUS_HEIGHT,
};
pub use splash::{Splash, MIN_SPLASH_SECS};

/// Wrap a central panel's content in the project's page geometry: the standard
/// inset, a width-capped column, and the overflow fade under a floating
/// scrollbar.
///
/// Every tab goes through this, so the gap to the nav rail, to the right edge
/// and to the status bar all match, and no tab can quietly grow a different
/// content width.
pub fn page(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> PageMetrics {
    page_with_width(ui, theme::CONTENT_MAX_WIDTH, add)
}

/// Workspaces can fill the window; reading and form views keep a bounded width.
pub fn page_with_width(
    ui: &mut egui::Ui,
    max_width: f32,
    add: impl FnOnce(&mut egui::Ui),
) -> PageMetrics {
    let out = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let width = ui
                .available_width()
                .clamp(theme::CONTENT_MIN_WIDTH, max_width);
            ui.set_max_width(width);
            add(ui);
        });

    // Overflow affordance. The scrollbars float, so without this a tab that
    // runs past the window looks truncated rather than scrollable.
    let hidden_below = out.content_size.y - out.state.offset.y - out.inner_rect.height();
    layout::scroll_fade(ui.painter(), out.inner_rect, hidden_below);

    PageMetrics {
        content_width: out.content_size.x,
        available_width: out.inner_rect.width(),
    }
}

/// What a page's content actually measured.
///
/// **A panel that runs past the right edge cannot be seen in a headless render
/// test**: egui clips it and the test still passes. The usual cause is a label
/// inside `ui.horizontal`, which does not wrap but extends the row. Measuring
/// the content turns that into a failing test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageMetrics {
    /// How wide the content wanted to be.
    pub content_width: f32,
    /// How wide it was allowed to be.
    pub available_width: f32,
}

impl PageMetrics {
    /// How far past the right edge the content ran. Zero when it fits.
    ///
    /// A pixel of slack: rounding in a scroll area's own measurement is not a
    /// layout bug.
    pub fn overflow(&self) -> f32 {
        (self.content_width - self.available_width - 1.0).max(0.0)
    }

    pub fn fits(&self) -> bool {
        self.overflow() <= 0.0
    }
}
