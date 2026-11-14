//! Startup splash, in the same visual language as HeadtrackRS' loader so the
//! two read as one suite: rings, cardinal ticks, a rotating dashed inner ring,
//! corner telemetry and a blinking status line, with a swinging heading needle
//! in place of HeadtrackRS' sweeping dot.

use crate::theme;
use egui::{Align2, Color32, FontId, Pos2, Stroke, Vec2};
use std::time::Instant;

/// Minimum time the splash is shown regardless of how fast startup completes.
///
/// Without a floor, a warm start flashes the window and is read as a glitch.
pub const MIN_SPLASH_SECS: f32 = 0.5;

pub struct Splash {
    start_time: Instant,
    last_frame: Instant,
    /// Current rotation of the dashed inner ring.
    ring_angle: f32,
    /// Current needle heading, lerped toward its target.
    needle: f32,
}

impl Default for Splash {
    fn default() -> Self {
        Self::new()
    }
}

impl Splash {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start_time: now,
            last_frame: now,
            ring_angle: 0.0,
            needle: 0.0,
        }
    }

    /// Seconds since the splash first appeared.
    pub fn elapsed(&self) -> f32 {
        self.start_time.elapsed().as_secs_f32()
    }

    /// Paint a whole frame of the splash over the viewport.
    ///
    /// `status` is the caller's word for what startup is doing right now; it
    /// blinks until `ready`, then settles to a steady green line.
    pub fn show(&mut self, ui: &mut egui::Ui, ready: bool, status: &str) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_DARK))
            .show(ui, |ui| self.draw(ui, ready, status));
        // Drive the animation at the display refresh rate.
        ui.ctx().request_repaint();
    }

    fn draw(&mut self, ui: &mut egui::Ui, ready: bool, status: &str) {
        let now = Instant::now();
        // Clamped so a stalled frame cannot fling the needle across the dial.
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        let elapsed = now.duration_since(self.start_time).as_secs_f32();

        let painter = ui.painter();
        let rect = ui.max_rect();
        let center = rect.center();
        let outer_r = 120.0_f32;
        let inner_r = 96.0_f32;

        self.draw_rings_and_ticks(painter, center, outer_r, inner_r, dt);
        self.draw_needle(painter, center, inner_r - 14.0, elapsed, dt);
        draw_corner_telemetry(painter, rect, elapsed);
        draw_status_line(painter, center, outer_r, elapsed, ready, status);
    }

    /// Outer ring, cardinal ticks, and the rotating dashed inner ring.
    fn draw_rings_and_ticks(
        &mut self,
        painter: &egui::Painter,
        center: Pos2,
        outer_r: f32,
        inner_r: f32,
        dt: f32,
    ) {
        painter.circle_stroke(
            center,
            outer_r,
            Stroke::new(1.0_f32, Color32::from_white_alpha(20)),
        );

        let tick_stroke = Stroke::new(2.0_f32, theme::ORANGE);
        let tick_outer = outer_r - 2.0;
        let tick_inner = tick_outer - 16.0;
        for &deg in &[0.0_f32, 90.0, 180.0, 270.0] {
            let rad = deg.to_radians();
            let dir = Vec2::new(rad.cos(), rad.sin());
            painter.line_segment(
                [center + dir * tick_inner, center + dir * tick_outer],
                tick_stroke,
            );
        }

        self.ring_angle += 1.8 * dt;
        let dash_count = 36_usize;
        for i in (0..dash_count).step_by(2) {
            let a1 = self.ring_angle + i as f32 * std::f32::consts::TAU / dash_count as f32;
            let a2 = self.ring_angle + (i as f32 + 0.8) * std::f32::consts::TAU / dash_count as f32;
            let p1 = center + Vec2::new(a1.cos(), a1.sin()) * inner_r;
            let p2 = center + Vec2::new(a2.cos(), a2.sin()) * inner_r;
            painter.line_segment(
                [p1, p2],
                Stroke::new(1.5_f32, Color32::from_white_alpha(40)),
            );
        }
    }

    /// A heading needle settling onto a new bearing every 1.2 s.
    ///
    /// The lerp is deliberately underdamped-looking rather than instant: an
    /// indicator that snaps reads as a redraw, one that swings reads as an
    /// instrument.
    fn draw_needle(
        &mut self,
        painter: &egui::Painter,
        center: Pos2,
        r: f32,
        elapsed: f32,
        dt: f32,
    ) {
        // Four bearings on a 1.2 s loop, offset so the needle never rests on a
        // cardinal tick and disappear into it.
        let leg = (elapsed / 1.2) as u32 % 4;
        let target = match leg {
            0 => -55.0_f32,
            1 => 40.0,
            2 => 155.0,
            _ => -140.0,
        }
        .to_radians();

        // Shortest-way-round, so the needle never takes the long path home.
        let mut delta = target - self.needle;
        while delta > std::f32::consts::PI {
            delta -= std::f32::consts::TAU;
        }
        while delta < -std::f32::consts::PI {
            delta += std::f32::consts::TAU;
        }
        self.needle += delta * (6.0 * dt).min(1.0);

        let dir = Vec2::new(self.needle.cos(), self.needle.sin());
        let tail = center - dir * (r * 0.35);
        let head = center + dir * r;
        painter.line_segment([tail, head], Stroke::new(2.0_f32, theme::ORANGE));
        painter.circle_filled(head, 5.0, theme::ORANGE);
        painter.circle_filled(center, 4.0, Color32::from_white_alpha(90));
    }
}

/// Monospaced corner readouts — the loader's instrument framing.
fn draw_corner_telemetry(painter: &egui::Painter, rect: egui::Rect, elapsed: f32) {
    let mono = FontId::monospace(12.0);
    painter.text(
        rect.left_top() + Vec2::new(22.0, 22.0),
        Align2::LEFT_TOP,
        format!("SYS_UPTIME: {:.3}s", elapsed + 10.0),
        mono.clone(),
        theme::DIMMER,
    );
    painter.text(
        rect.right_top() + Vec2::new(-22.0, 22.0),
        Align2::RIGHT_TOP,
        "HANGARRS",
        mono,
        theme::DIMMER,
    );
}

/// The blinking status line under the dial.
fn draw_status_line(
    painter: &egui::Painter,
    center: Pos2,
    outer_r: f32,
    elapsed: f32,
    ready: bool,
    status: &str,
) {
    let pos = center + Vec2::new(0.0, outer_r + 86.0);
    let font = FontId::new(12.0, theme::mono_bold());

    if ready {
        painter.text(
            pos,
            Align2::CENTER_CENTER,
            theme::track_out("HANGAR OPEN"),
            font,
            theme::OK,
        );
    } else if ((elapsed * 3.0) as u32) % 2 == 0 {
        painter.text(
            pos,
            Align2::CENTER_CENTER,
            theme::track_out(status),
            font,
            theme::ORANGE,
        );
    }
}
