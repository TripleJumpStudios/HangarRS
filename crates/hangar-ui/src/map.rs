//! Rotatable raster map control, using walkers' existing tile cache.
//! Tile access is injected: `None` paints and handles gestures without I/O.
//! Geographic positions rotate; callers paint labels and readouts upright.

use egui::{pos2, vec2, Mesh, Pos2, Rect, Response, Sense, Ui, Vec2};
use walkers::{lat_lon, MapMemory, Position, Tile, TileId, Tiles};

const EARTH_CIRCUMFERENCE: f64 = 40_075_016.686;
const MERCATOR_LIMIT: f64 = 85.051_128_78;

/// A reversible projection whose bearing is the compass direction at screen top.
pub struct Projector {
    base: walkers::Projector,
    rect: Rect,
    center_pixels: (f64, f64),
    world: f64,
    zoom: f64,
    bearing: f32,
}

impl Projector {
    pub fn new(rect: Rect, memory: &MapMemory, position: Position, bearing: f32) -> Self {
        let center = valid_center(memory.detached().unwrap_or(position));
        let mut memory = memory.clone();
        memory.center_at(center);
        let zoom = memory.zoom();
        let pixels = walkers::mercator::project(center, zoom);
        Self {
            base: walkers::Projector::new(rect, &memory, center),
            rect,
            center_pixels: (pixels.x(), pixels.y()),
            world: 256.0 * 2_f64.powf(zoom),
            zoom,
            bearing: if bearing.is_finite() {
                bearing.rem_euclid(360.0)
            } else {
                0.0
            },
        }
    }

    pub fn bearing(&self) -> f32 {
        self.bearing
    }

    /// Nearest wrapped copy, computed in f64 before converting to screen pixels.
    pub fn project(&self, position: Position) -> Vec2 {
        let point = walkers::mercator::project(position, self.zoom);
        let mut dx = point.x() - self.center_pixels.0;
        dx -= self.world * (dx / self.world).round();
        self.project_offset(vec2(dx as f32, (point.y() - self.center_pixels.1) as f32))
            .to_vec2()
    }

    pub fn unproject(&self, screen: Vec2) -> Position {
        let offset = self.unproject_offset(screen.to_pos2());
        let position = self.base.unproject(self.rect.center().to_vec2() + offset);
        lat_lon(position.y(), wrap_longitude(position.x()))
    }

    pub fn scale_pixel_per_meter(&self, position: Position) -> f32 {
        (self.world / EARTH_CIRCUMFERENCE / position.y().to_radians().cos()) as f32
    }

    fn project_offset(&self, offset: Vec2) -> Pos2 {
        self.rect.center() + rotate(offset, -self.bearing)
    }

    fn unproject_offset(&self, screen: Pos2) -> Vec2 {
        rotate(screen - self.rect.center(), self.bearing)
    }
}

fn rotate(vector: Vec2, degrees: f32) -> Vec2 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    vec2(
        cos * vector.x - sin * vector.y,
        sin * vector.x + cos * vector.y,
    )
}

fn wrap_longitude(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

fn valid_center(position: Position) -> Position {
    if !position.x().is_finite() || !position.y().is_finite() {
        return lat_lon(0.0, 0.0);
    }
    lat_lon(
        position.y().clamp(-MERCATOR_LIMIT, MERCATOR_LIMIT),
        wrap_longitude(position.x()),
    )
}

/// Fill the available map area. Gestures are owned here, like other design-system controls.
/// Drag or scroll pans; Ctrl+wheel/pinch and double-click zoom about the pointer.
pub fn show(
    ui: &mut Ui,
    memory: &mut MapMemory,
    position: Position,
    bearing: f32,
    tiles: Option<&mut dyn Tiles>,
    overlay: impl FnOnce(&mut Ui, &Response, &Projector),
) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let mut changed = false;
    if response.hovered() {
        let zoom = ui.input(|i| i.zoom_delta());
        let step = if response.double_clicked_by(egui::PointerButton::Primary) {
            1.0
        } else if response.double_clicked_by(egui::PointerButton::Secondary) {
            -1.0
        } else if zoom.is_finite() && zoom > 0.0 {
            f64::from(zoom.log2())
        } else {
            0.0
        };
        if step.abs() > 0.0001 {
            let pointer = ui
                .input(|i| i.multi_touch().map(|t| t.center_pos))
                .or(response.hover_pos())
                .unwrap_or(rect.center());
            zoom_about(memory, rect, position, bearing, pointer, step);
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            changed = true;
        } else {
            let scroll = ui.input(|i| i.smooth_scroll_delta);
            if scroll != Vec2::ZERO {
                pan(memory, rect, position, bearing, scroll);
                ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
                changed = true;
            }
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) && !changed {
        pan(memory, rect, position, bearing, response.drag_delta());
        changed = true;
    }
    if changed {
        response.mark_changed();
        ui.ctx().request_repaint();
    }
    let projector = Projector::new(rect, memory, position, bearing);
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, crate::theme::BG);
    if let Some(tiles) = tiles {
        paint_tiles(&painter, &projector, tiles);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("map-overlay"));
    overlay(&mut child, &response, &projector);
    response
}

fn pan(memory: &mut MapMemory, rect: Rect, position: Position, bearing: f32, delta: Vec2) {
    let projection = Projector::new(rect, memory, position, bearing);
    memory.center_at(valid_center(
        projection.unproject(rect.center().to_vec2() - delta),
    ));
}

fn zoom_about(
    memory: &mut MapMemory,
    rect: Rect,
    position: Position,
    bearing: f32,
    pointer: Pos2,
    step: f64,
) {
    let before = Projector::new(rect, memory, position, bearing);
    let anchor = before.unproject(pointer.to_vec2());
    let _ = memory.set_zoom((memory.zoom() + step).clamp(0.0, 26.0));
    if (pointer - rect.center()).length() < 1.0 && memory.detached().is_none() {
        return;
    }
    let after = Projector::new(rect, memory, position, bearing);
    let center =
        after.unproject(rect.center().to_vec2() + after.project(anchor) - pointer.to_vec2());
    memory.center_at(valid_center(center));
}

/// Visible tile cells, including wrapped x cells, in the unrotated world grid.
fn tile_cells(projection: &Projector, source_size: u32) -> Vec<(TileId, Rect)> {
    let adjustment = (f64::from(source_size.max(256)) / 256.0).log2().floor() as u8;
    let zoom = (projection.zoom.round() as u8).saturating_sub(adjustment);
    let count = 1_i64 << zoom;
    let size = projection.world / count as f64;
    let corners = [
        projection.rect.left_top(),
        projection.rect.right_top(),
        projection.rect.left_bottom(),
        projection.rect.right_bottom(),
    ];
    let mut bounds = Rect::NOTHING;
    for corner in corners {
        bounds.extend_with(projection.unproject_offset(corner).to_pos2());
    }
    let left = ((projection.center_pixels.0 + f64::from(bounds.left())) / size).floor() as i64;
    let right = ((projection.center_pixels.0 + f64::from(bounds.right())) / size).floor() as i64;
    let top =
        (((projection.center_pixels.1 + f64::from(bounds.top())) / size).floor() as i64).max(0);
    let bottom = (((projection.center_pixels.1 + f64::from(bounds.bottom())) / size).floor()
        as i64)
        .min(count - 1);
    let mut cells = Vec::new();
    for y in top..=bottom {
        for x in left..=right {
            let rect = Rect::from_min_size(
                pos2(
                    (x as f64 * size - projection.center_pixels.0) as f32,
                    (y as f64 * size - projection.center_pixels.1) as f32,
                ),
                Vec2::splat(size as f32),
            );
            cells.push((
                TileId {
                    x: x.rem_euclid(count) as u32,
                    y: y as u32,
                    zoom,
                },
                rect,
            ));
        }
    }
    cells
}

fn tile_mesh(projection: &Projector, rect: Rect, uv: Rect, texture: egui::TextureId) -> Mesh {
    let mut mesh = Mesh::with_texture(texture);
    mesh.add_rect_with_uv(rect, uv, egui::Color32::WHITE);
    for vertex in &mut mesh.vertices {
        vertex.pos = projection.project_offset(vertex.pos.to_vec2());
    }
    mesh
}

fn paint_tiles(painter: &egui::Painter, projection: &Projector, tiles: &mut dyn Tiles) {
    for (id, rect) in tile_cells(projection, tiles.tile_size()) {
        if let Some(piece) = tiles.at(id) {
            // HangarRS uses raster tiles; enabling walkers' MVT feature requires
            // adding an explicit vector path here rather than silently skipping it.
            let Tile::Raster(texture) = piece.tile;
            painter.add(egui::Shape::mesh(tile_mesh(
                projection,
                rect,
                piece.uv,
                texture.id(),
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))
    }
    fn memory() -> MapMemory {
        let mut memory = MapMemory::default();
        memory.set_zoom(10.0).unwrap();
        memory
    }

    #[test]
    fn east_is_up_and_north_is_left_at_eastbound_track() {
        let p = Projector::new(rect(), &memory(), lat_lon(0.0, 0.0), 90.0);
        let east = p.project(lat_lon(0.0, 0.1)) - rect().center().to_vec2();
        let north = p.project(lat_lon(0.1, 0.0)) - rect().center().to_vec2();
        assert!(east.y < -1.0 && east.x.abs() < 0.001);
        assert!(north.x < -1.0 && north.y.abs() < 0.001);
    }

    #[test]
    fn rotated_projection_roundtrips_and_wraps_at_the_date_line() {
        for bearing in [0.0, 37.0, 90.0, 180.0, 270.0, 359.0] {
            let p = Projector::new(rect(), &memory(), lat_lon(47.5, 179.9), bearing);
            let position = lat_lon(47.6, -179.9);
            let pixel = p.project(position);
            assert!(rect().contains(pixel.to_pos2()));
            let roundtrip = p.unproject(pixel);
            assert!((roundtrip.x() - position.x()).abs() < 0.00001);
            assert!((roundtrip.y() - position.y()).abs() < 0.00001);
        }
    }

    #[test]
    fn dragging_moves_geography_with_the_pointer_at_every_bearing() {
        for bearing in [0.0, 45.0, 90.0, 180.0, 270.0] {
            let mut memory = memory();
            let position = lat_lon(47.5, -122.3);
            let delta = vec2(67.0, -39.0);
            pan(&mut memory, rect(), position, bearing, delta);
            let after = Projector::new(rect(), &memory, position, bearing);
            assert!((after.project(position) - rect().center().to_vec2() - delta).length() < 0.01);
        }
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer_fixed() {
        for bearing in [0.0, 45.0, 90.0, 180.0, 270.0] {
            let mut memory = memory();
            let position = lat_lon(47.5, 179.9);
            let pointer = pos2(543.0, 215.0);
            let anchor =
                Projector::new(rect(), &memory, position, bearing).unproject(pointer.to_vec2());
            zoom_about(&mut memory, rect(), position, bearing, pointer, 1.25);
            let after = Projector::new(rect(), &memory, position, bearing);
            assert!((after.project(anchor) - pointer.to_vec2()).length() < 0.01);
        }
    }

    #[test]
    fn rotated_tile_cells_cover_every_viewport_corner() {
        for bearing in [0.0, 45.0, 90.0, 135.0, 270.0] {
            for longitude in [-179.9, 0.0, 179.9] {
                let projection =
                    Projector::new(rect(), &memory(), lat_lon(47.5, longitude), bearing);
                let cells = tile_cells(&projection, 256);
                for corner in [
                    rect().left_top(),
                    rect().right_top(),
                    rect().left_bottom(),
                    rect().right_bottom(),
                    rect().center(),
                ] {
                    let north = projection.unproject_offset(corner).to_pos2();
                    assert!(cells
                        .iter()
                        .any(|(_, rect)| rect.expand(0.01).contains(north)));
                }
                assert!(cells
                    .iter()
                    .all(|(id, _)| id.x < 1 << id.zoom && id.y < 1 << id.zoom));
            }
        }
    }

    #[test]
    fn raster_mesh_rotates_positions_and_preserves_parent_tile_uvs() {
        let projection = Projector::new(rect(), &memory(), lat_lon(0.0, 0.0), 90.0);
        let uv = Rect::from_min_max(pos2(0.25, 0.5), pos2(0.5, 0.75));
        let mesh = tile_mesh(
            &projection,
            Rect::from_min_size(pos2(0.0, 0.0), Vec2::splat(256.0)),
            uv,
            egui::TextureId::Managed(42),
        );
        assert_eq!(mesh.texture_id, egui::TextureId::Managed(42));
        assert_eq!(mesh.vertices[0].uv, uv.left_top());
        assert!(mesh
            .vertices
            .iter()
            .any(|v| (v.pos - (rect().center() + vec2(0.0, -256.0))).length() < 0.01));
        assert!(mesh.vertices.iter().all(|v| uv.contains(v.uv)));
    }

    #[test]
    fn pointer_drag_reaches_the_control_and_detaches_without_network_access() {
        let ctx = egui::Context::default();
        let mut memory = memory();
        let position = lat_lon(47.5, -122.3);
        let mut viewport = rect();
        let mut frame = |events: Vec<egui::Event>, memory: &mut MapMemory| {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect()),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let response = show(ui, memory, position, 90.0, None, |_, _, _| {});
                    viewport = response.rect;
                },
            );
            viewport
        };
        let viewport = frame(Vec::new(), &mut memory);
        let at = viewport.center();
        frame(
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            &mut memory,
        );
        let delta = vec2(60.0, 30.0);
        frame(vec![egui::Event::PointerMoved(at + delta)], &mut memory);
        assert!(memory.detached().is_some());
        let projector = Projector::new(viewport, &memory, position, 90.0);
        assert!((projector.project(position) - at.to_vec2() - delta).length() < 0.1);
    }

    #[test]
    fn raster_tiles_are_injected_and_drawn_as_rotated_meshes() {
        struct FixtureTiles {
            texture: egui::TextureHandle,
            requests: Vec<TileId>,
        }
        impl Tiles for FixtureTiles {
            fn at(&mut self, id: TileId) -> Option<walkers::TilePiece> {
                self.requests.push(id);
                Some(walkers::TilePiece::new(
                    Tile::Raster(self.texture.clone()),
                    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                ))
            }
            fn tile_size(&self) -> u32 {
                256
            }
            fn attribution(&self) -> walkers::sources::Attribution {
                walkers::sources::Attribution {
                    text: "Synthetic tiles",
                    url: "",
                    logo_light: None,
                    logo_dark: None,
                }
            }
        }
        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "synthetic-map",
            egui::ColorImage::filled([2, 2], egui::Color32::GREEN),
            Default::default(),
        );
        let mut tiles = FixtureTiles {
            texture,
            requests: Vec::new(),
        };
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect()),
                ..Default::default()
            },
            |ui| {
                show(
                    ui,
                    &mut memory(),
                    lat_lon(47.5, 179.9),
                    45.0,
                    Some(&mut tiles),
                    |_, _, _| {},
                );
            },
        );
        assert!(!tiles.requests.is_empty());
        let meshes: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Mesh(mesh) => Some(mesh),
                _ => None,
            })
            .collect();
        assert_eq!(meshes.len(), tiles.requests.len());
        for mesh in meshes {
            assert_eq!(mesh.texture_id, tiles.texture.id());
            let edge = mesh.vertices[1].pos - mesh.vertices[0].pos;
            assert!(edge.x.abs() > 1.0 && edge.y.abs() > 1.0);
        }
    }
}
