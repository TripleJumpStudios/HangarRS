//! Test-only mesh capture for offline visual review of the production egui UI.
//! An offline renderer, not part of this repository, reads this small binary format.

use std::io::Write;

pub fn save(
    path: &std::path::Path,
    ctx: &egui::Context,
    output: egui::FullOutput,
    size: egui::Vec2,
) {
    let mut file = std::fs::File::create(path).unwrap();
    let uint =
        |file: &mut std::fs::File, n: usize| file.write_all(&(n as u32).to_le_bytes()).unwrap();
    uint(&mut file, size.x as usize);
    uint(&mut file, size.y as usize);
    let atlas = output
        .textures_delta
        .set
        .iter()
        .find(|(id, _)| *id == egui::TextureId::Managed(0))
        .unwrap();
    assert!(atlas.1.pos.is_none(), "capture needs a fresh egui context");
    let egui::ImageData::Color(image) = &atlas.1.image;
    uint(&mut file, image.size[0]);
    uint(&mut file, image.size[1]);
    for pixel in &image.pixels {
        file.write_all(&pixel.to_array()).unwrap();
    }
    let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
    uint(&mut file, primitives.len());
    for primitive in primitives {
        let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
            panic!("unexpected paint callback")
        };
        assert_eq!(mesh.texture_id, egui::TextureId::Managed(0));
        for f in [
            primitive.clip_rect.min.x,
            primitive.clip_rect.min.y,
            primitive.clip_rect.max.x,
            primitive.clip_rect.max.y,
        ] {
            file.write_all(&f.to_le_bytes()).unwrap();
        }
        uint(&mut file, mesh.vertices.len());
        uint(&mut file, mesh.indices.len());
        for v in mesh.vertices {
            for f in [v.pos.x, v.pos.y, v.uv.x, v.uv.y] {
                file.write_all(&f.to_le_bytes()).unwrap();
            }
            file.write_all(&v.color.to_array()).unwrap();
        }
        for index in mesh.indices {
            file.write_all(&index.to_le_bytes()).unwrap();
        }
    }
}
