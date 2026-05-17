use crate::math::{AxialCoord, Point2, SquareCoord, TriangleCoord};
use crate::protocol::{
    ColorRule, ColorState, EngineSettings, Placement, RgbColor, SpotCoord, parse_hex_rgb,
    rainbow_rgb,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorAnchors {
    pub a: RgbColor,
    pub b: RgbColor,
}

impl ColorAnchors {
    #[must_use]
    pub fn from_settings(settings: &EngineSettings) -> Self {
        Self {
            a: parse_hex_rgb(&settings.anchor_color_a)
                .unwrap_or_else(crate::protocol::default_anchor_a_rgb),
            b: parse_hex_rgb(&settings.anchor_color_b)
                .unwrap_or_else(crate::protocol::default_anchor_b_rgb),
        }
    }
}

#[must_use]
pub fn placement_center(placement: &Placement) -> Point2 {
    match placement.coord {
        SpotCoord::Square { x, y } => SquareCoord::new(x, y).to_point(),
        SpotCoord::Hex { q, r } => AxialCoord::new(q, r).to_point(),
        SpotCoord::Triangle { u, v } => TriangleCoord::new(u, v).to_point(),
        SpotCoord::Continuous { x, y, .. } => Point2::new(x, y),
    }
}

#[must_use]
pub fn color_for_placement_rgb(
    placement: &Placement,
    anchors: ColorAnchors,
    color_state: ColorState,
) -> RgbColor {
    match placement.color.rule {
        ColorRule::Fixed => parse_hex_rgb(&placement.color.fixed_css)
            .unwrap_or_else(|| RgbColor::new(238.0 / 255.0, 241.0 / 255.0, 244.0 / 255.0)),
        ColorRule::OrderRainbow | ColorRule::PrimeKnightModulo => {
            rainbow_rgb(anchors.a, anchors.b, placement.color.key.gradient_value)
        }
        ColorRule::PrimeGapBounds => {
            let min_gap = color_state.min_gap;
            let max_gap = color_state.max_gap.max(min_gap + 1.0);
            let t = ((placement.color.key.gradient_value - min_gap) / (max_gap - min_gap))
                .clamp(0.0, 1.0);
            rainbow_rgb(anchors.a, anchors.b, t)
        }
    }
}

#[must_use]
pub fn pack_vertices(
    placements: &[Placement],
    settings: &EngineSettings,
    color_state: ColorState,
) -> Vec<f32> {
    let anchors = ColorAnchors::from_settings(settings);
    let mut vertices = Vec::with_capacity(placements.len() * 5);
    append_vertices(&mut vertices, placements, anchors, color_state);
    vertices
}

pub fn append_vertices(
    vertices: &mut Vec<f32>,
    placements: &[Placement],
    anchors: ColorAnchors,
    color_state: ColorState,
) {
    vertices.reserve(placements.len() * 5);

    for placement in placements {
        let center = placement_center(placement);
        let color = color_for_placement_rgb(placement, anchors, color_state);
        vertices.extend_from_slice(&[center.x as f32, center.y as f32, color.r, color.g, color.b]);
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::{
        ColorKey, ColorRule, DEFAULT_ANCHOR_A, DEFAULT_ANCHOR_B, EngineSettings, PieceColor,
        PieceSignature, Placement, ShapeKind, SpotCoord,
    };

    use super::*;

    #[test]
    fn pack_vertices_contains_position_and_rgb_color() {
        let settings = EngineSettings {
            anchor_color_a: DEFAULT_ANCHOR_A.to_string(),
            anchor_color_b: DEFAULT_ANCHOR_B.to_string(),
            ..EngineSettings::default()
        };
        let placements = vec![Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Square { x: 2, y: -3 },
            piece: PieceSignature::new(2, 1),
            color: PieceColor {
                rule: ColorRule::OrderRainbow,
                fixed_css: String::new(),
                key: ColorKey {
                    group: 0,
                    gradient_value: 0.0,
                },
            },
            shape: ShapeKind::Square,
        }];

        let vertices = pack_vertices(&placements, &settings, ColorState::default());

        assert_eq!(vertices.len(), 5);
        assert_eq!(&vertices[0..2], &[2.0, -3.0]);
        assert_eq!(&vertices[2..5], &[1.0, 120.0 / 255.0, 0.0]);
    }
}
