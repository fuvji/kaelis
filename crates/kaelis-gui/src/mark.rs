//! The Kaelis mark: two folded planes that read as one launch dart.
//!
//! A right-pointing arrowhead carries a V notch at its back and a hairline
//! seam from the notch to the nose. At 16 px the seam closes and the mark is a
//! solid, unmistakable launch arrow; at 512 px the seam reveals the fold. The
//! glyph is drawn in the product accent with a lighter upper plane, on a fully
//! transparent background, so it sits on any surface without a plate or a
//! letterform.

use eframe::egui;

/// Opening of the fold at the back notch, as a fraction of the tile edge. It
/// tapers to nothing at the nose, so the point stays sharp.
const SEAM: f32 = 0.021;

/// The three points of the dart: tail top, nose, tail bottom.
const TAIL_TOP: egui::Pos2 = egui::pos2(0.1, 0.14);
const NOSE: egui::Pos2 = egui::pos2(0.9, 0.5);
const TAIL_BOTTOM: egui::Pos2 = egui::pos2(0.1, 0.86);
/// How far the back notch bites into the dart.
const NOTCH_X: f32 = 0.3;

/// Upper plane: the light face catching the light.
const PLANE_LIGHT: egui::Color32 = egui::Color32::from_rgb(112, 165, 255);
/// Lower plane: the accent face, one step deeper so the fold reads without a
/// stroke or a shadow.
const PLANE_DARK: egui::Color32 = egui::Color32::from_rgb(46, 112, 232);

/// The two folded planes. They meet at the nose and open by `SEAM` at the
/// notch, so the silhouette stays unbroken while the fold stays visible.
fn facets() -> [[egui::Pos2; 3]; 2] {
    [
        [TAIL_TOP, NOSE, egui::pos2(NOTCH_X, 0.5 - SEAM * 0.5)],
        [egui::pos2(NOTCH_X, 0.5 + SEAM * 0.5), NOSE, TAIL_BOTTOM],
    ]
}

/// Whether a unit-square point falls inside one triangle, including its edges.
fn inside_triangle(point: egui::Pos2, triangle: &[egui::Pos2; 3]) -> bool {
    let cross = |from: egui::Pos2, to: egui::Pos2| {
        let edge = to - from;
        let offset = point - from;
        edge.x * offset.y - edge.y * offset.x
    };
    let d1 = cross(triangle[0], triangle[1]);
    let d2 = cross(triangle[1], triangle[2]);
    let d3 = cross(triangle[2], triangle[0]);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

/// Glyph coverage for the geometry tests; the rasteriser composites each plane
/// separately so it can shade the fold.
#[cfg(test)]
fn inside_glyph(point: egui::Pos2) -> bool {
    facets().iter().any(|facet| inside_triangle(point, facet))
}

/// Paint the mark into `rect`. The background stays transparent: only the two
/// planes are drawn, each with its own shade.
pub(crate) fn paint(painter: &egui::Painter, rect: egui::Rect) {
    for (facet, color) in facets().iter().zip([PLANE_LIGHT, PLANE_DARK]) {
        let points = facet
            .iter()
            .map(|point| rect.min + egui::vec2(point.x * rect.width(), point.y * rect.height()))
            .collect();
        painter.add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// Rasterise the mark for the window and tray icons. Rendered at 4x and box
/// filtered, so the glyph is anti-aliased without an image decoder. Every
/// pixel outside the dart keeps `alpha = 0`.
pub(crate) fn icon(size: u32) -> egui::IconData {
    const SUPERSAMPLE: u32 = 4;
    const PLANES: [egui::Color32; 2] = [PLANE_LIGHT, PLANE_DARK];
    let high = size * SUPERSAMPLE;
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut hits = [0_u32; 2];
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let px = (x * SUPERSAMPLE + sx) as f32 + 0.5;
                    let py = (y * SUPERSAMPLE + sy) as f32 + 0.5;
                    let point = egui::pos2(px / high as f32, py / high as f32);
                    for (plane, facet) in facets().iter().enumerate() {
                        if inside_triangle(point, facet) {
                            hits[plane] += 1;
                        }
                    }
                }
            }
            let total = hits[0] + hits[1];
            if total == 0 {
                continue;
            }
            let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
            let index = ((y * size + x) * 4) as usize;
            // Each sample contributes its plane's colour, so the seam stays a
            // soft transition instead of a grey fringe.
            let cover = total as f32 / samples;
            let blend = |channel: fn(egui::Color32) -> u8| {
                let sum = hits[0] as f32 * f32::from(channel(PLANES[0]))
                    + hits[1] as f32 * f32::from(channel(PLANES[1]));
                (sum / total as f32).round() as u8
            };
            rgba[index] = blend(|color| color.r());
            rgba[index + 1] = blend(|color| color.g());
            rgba[index + 2] = blend(|color| color.b());
            rgba[index + 3] = (255.0 * cover).round() as u8;
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

/// The accent dart with no plate: the mark used by the app, the tray, and the
/// site.
pub(crate) fn primary_icon(size: u32) -> egui::IconData {
    icon(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facets_stay_inside_the_canvas_and_clear_of_its_edges() {
        for facet in facets() {
            for point in facet {
                assert!(point.x > 0.05 && point.x < 0.95);
                assert!(point.y > 0.1 && point.y < 0.9);
            }
        }
    }

    #[test]
    fn mark_reads_as_a_launch_dart_at_every_size() {
        // Both wings are solid, and the hairline fold runs between them.
        assert!(inside_glyph(egui::pos2(0.55, 0.38)));
        assert!(inside_glyph(egui::pos2(0.55, 0.62)));
        assert!(inside_glyph(egui::pos2(0.80, 0.47)));
        // The back notch and the fold stay open, so no vertical stem forms a
        // letter at small sizes.
        assert!(!inside_glyph(egui::pos2(0.20, 0.50)));
        assert!(!inside_glyph(egui::pos2(0.30, 0.50)));
        assert!(!inside_glyph(egui::pos2(0.80, 0.50)));
        // Outside the dart remains clear.
        assert!(!inside_glyph(egui::pos2(0.95, 0.50)));
        assert!(!inside_glyph(egui::pos2(0.50, 0.05)));
        assert!(!inside_glyph(egui::pos2(0.50, 0.95)));
    }

    #[test]
    fn rasterised_icon_is_square_with_a_transparent_background() {
        let icon = primary_icon(64);
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba[3], 0, "top-left corner must be transparent");
        // The plate is gone: the left margin outside the dart stays empty.
        let margin = ((32 * 64 + 2) * 4) as usize;
        assert_eq!(icon.rgba[margin + 3], 0, "no background plate");
        // Sample the upper plane, clear of the hairline seam that crosses the
        // exact centre of the glyph.
        let plane = ((26 * 64 + 32) * 4) as usize;
        assert_eq!(icon.rgba[plane + 3], 255);
        assert!(
            icon.rgba[plane + 2] > icon.rgba[plane],
            "the dart is blue, not black"
        );
    }

    #[test]
    fn sixteen_pixel_icon_keeps_the_glyph_and_a_clear_background() {
        let icon = primary_icon(16);
        let (mut solid, mut clear, mut dark) = (0_u32, 0_u32, 0_u32);
        for pixel in icon.rgba.as_chunks::<4>().0 {
            if pixel[3] == 0 {
                clear += 1;
            } else if pixel[3] > 128 {
                solid += 1;
                if u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2]) < 120 {
                    dark += 1;
                }
            }
        }
        assert!(solid > 30, "16 px icon lost the glyph: {solid} pixels");
        assert!(
            clear > 20,
            "16 px icon lost its transparency: {clear} pixels"
        );
        assert_eq!(dark, 0, "the glyph stays in the accent family");
    }
}
