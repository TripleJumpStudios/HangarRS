//! Drawing helpers for meters and charts.

use crate::theme;
use egui::{Color32, Rect, Stroke, StrokeKind};

/// Draw the trough, centre tick, fill and knob of a bipolar bar.
///
/// Bipolar because the value is signed and zero is the middle of the track —
/// trim, a wind component, a deviation from plan. A unipolar quantity should
/// use a plain [`egui::ProgressBar`] rather than a bar whose centre tick lies
/// about where nothing is.
pub fn bipolar_bar(
    painter: &egui::Painter,
    rect: Rect,
    value: f32,
    max: f32,
    color: Color32,
    knob: bool,
) {
    painter.rect_filled(rect, 5.0, theme::WELL);
    painter.rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0_f32, theme::STROKE_SOFT),
        StrokeKind::Inside,
    );

    let center_x = rect.center().x;
    painter.line_segment(
        [
            egui::pos2(center_x, rect.top() - 2.0),
            egui::pos2(center_x, rect.bottom() + 2.0),
        ],
        Stroke::new(1.0_f32, theme::STROKE),
    );

    let norm = if max > 0.0 {
        (value / max).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let end_x = center_x + norm * rect.width() * 0.5;
    let fill = Rect::from_min_max(
        egui::pos2(center_x.min(end_x), rect.top() + 1.0),
        egui::pos2(center_x.max(end_x), rect.bottom() - 1.0),
    );
    if fill.width() > 0.5 {
        painter.rect_filled(fill, 4.0, color.gamma_multiply(0.85));
    }

    if knob {
        let pos = egui::pos2(end_x, rect.center().y);
        // Cheap glow: a wide translucent disc under the solid knob.
        painter.circle_filled(pos, 7.0, color.gamma_multiply(0.25));
        painter.circle_filled(pos, 4.0, color);
    }
}

/// Round a value up to a readable chart-axis maximum.
///
/// Guards against a non-finite or non-positive input, which would otherwise
/// yield a NaN axis and an empty chart.
pub fn nice_ceil(v: f32) -> f32 {
    if !v.is_finite() || v <= 0.0 {
        return 10.0;
    }
    let mag = 10_f32.powf(v.log10().floor());
    ((v / mag).ceil() * mag).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::nice_ceil;

    #[test]
    fn nice_ceil_rounds_up_to_a_readable_axis_max() {
        assert_eq!(nice_ceil(0.0), 10.0);
        assert_eq!(nice_ceil(f32::NAN), 10.0);
        assert_eq!(nice_ceil(-5.0), 10.0);
        assert_eq!(nice_ceil(42.0), 50.0);
        assert_eq!(nice_ceil(50.0), 50.0);
        assert_eq!(nice_ceil(3.2), 4.0);
    }
}
