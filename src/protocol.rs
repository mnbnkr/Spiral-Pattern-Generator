use serde::{Deserialize, Serialize};

pub const DEFAULT_ANCHOR_A: &str = "#ff7800";
pub const DEFAULT_ANCHOR_B: &str = "#ff0006";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BoardKind {
    LatticeSquare,
    LatticeHex,
    LatticeTriangle,
    ContinuousArchimedean,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShapeKind {
    Square,
    Circle,
    Hex,
    Triangle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DisplayMode {
    FitScreen,
    PixelOneToOne,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EnemyMode {
    MoveSet,
    Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ArmyPreset {
    CustomFinite,
    PrimeKnight,
    PrimeGap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SpeedMode {
    PerSecond(u16),
    Fastest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomPiece {
    pub a: i32,
    pub b: i32,
    pub color: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RgbColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl RgbColor {
    #[must_use]
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub fn from_u8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
        }
    }

    #[must_use]
    pub fn to_u8_tuple(self) -> (u8, u8, u8) {
        (
            channel_f32_to_u8(self.r),
            channel_f32_to_u8(self.g),
            channel_f32_to_u8(self.b),
        )
    }

    #[must_use]
    pub fn to_css_hex(self) -> String {
        format_rgb(self.to_u8_tuple())
    }
}

impl CustomPiece {
    #[must_use]
    pub fn new(a: i32, b: i32, color: impl Into<String>) -> Self {
        Self {
            a,
            b,
            color: color.into(),
        }
    }

    #[must_use]
    pub fn with_auto_color(a: i32, b: i32) -> Self {
        Self::new(a, b, auto_piece_color(a, b))
    }
}

#[must_use]
pub fn piece_gap(a: i32, b: i32) -> u32 {
    a.abs_diff(b)
}

#[must_use]
pub fn auto_piece_color(a: i32, b: i32) -> String {
    auto_gap_color(piece_gap(a, b))
}

#[must_use]
pub fn auto_gap_color(gap: u32) -> String {
    const PALETTE: [&str; 12] = [
        "#c7d0d9", "#55a7ff", "#7ddc8a", "#ffb454", "#d884ff", "#ff6b6b", "#4dd8c8", "#f2e85d",
        "#b5a0ff", "#ff8ac1", "#8fd3ff", "#f0a35d",
    ];

    if let Some(color) = PALETTE.get(gap as usize) {
        (*color).to_string()
    } else {
        let hue = (gap as u64 * 137 + 43) % 360;
        format!("hsl({hue}, 78%, 62%)")
    }
}

#[must_use]
pub fn rainbow_color(anchor_a: &str, anchor_b: &str, t: f64) -> String {
    let left = parse_hex_rgb(anchor_a).unwrap_or_else(default_anchor_a_rgb);
    let right = parse_hex_rgb(anchor_b).unwrap_or_else(default_anchor_b_rgb);
    rainbow_rgb(left, right, t).to_css_hex()
}

#[must_use]
pub fn rainbow_rgb(anchor_a: RgbColor, anchor_b: RgbColor, t: f64) -> RgbColor {
    let left = anchor_a.to_u8_tuple();
    let right = anchor_b.to_u8_tuple();
    let t = t.clamp(0.0, 1.0);

    if t <= f64::EPSILON {
        return RgbColor::from_u8(left.0, left.1, left.2);
    }

    if (1.0 - t) <= f64::EPSILON {
        return RgbColor::from_u8(right.0, right.1, right.2);
    }

    let (mut h0, s0, v0) = rgb_to_hsv(left);
    let (mut h1, s1, v1) = rgb_to_hsv(right);

    if s0 <= f64::EPSILON {
        h0 = h1;
    }
    if s1 <= f64::EPSILON {
        h1 = h0;
    }

    let short_delta = normalized_hue_delta(h0, h1);
    let hue_delta = if short_delta >= 0.0 {
        short_delta - 360.0
    } else {
        short_delta + 360.0
    };
    let hue = wrap_hue(h0 + hue_delta * t);
    let saturation = lerp(s0, s1, t).clamp(0.0, 1.0);
    let value = lerp(v0, v1, t).clamp(0.0, 1.0);

    let (r, g, b) = hsv_to_rgb(hue, saturation, value);
    RgbColor::from_u8(r, g, b)
}

#[must_use]
pub fn parse_hex_rgb(input: &str) -> Option<RgbColor> {
    let (r, g, b) = parse_hex_color(input)?;
    Some(RgbColor::from_u8(r, g, b))
}

fn parse_hex_color(input: &str) -> Option<(u8, u8, u8)> {
    let value = input.strip_prefix('#').unwrap_or(input);
    if value.len() != 6 {
        return None;
    }

    let r = u8::from_str_radix(&value[0..2], 16).ok()?;
    let g = u8::from_str_radix(&value[2..4], 16).ok()?;
    let b = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some((r, g, b))
}

fn format_rgb((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn rgb_to_hsv((r, g, b): (u8, u8, u8)) -> (f64, f64, f64) {
    let r = r as f64 / 255.0;
    let g = g as f64 / 255.0;
    let b = b as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let hue = if delta <= f64::EPSILON {
        0.0
    } else if (max - r).abs() <= f64::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() <= f64::EPSILON {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };

    let saturation = if max <= f64::EPSILON {
        0.0
    } else {
        delta / max
    };

    (wrap_hue(hue), saturation, max)
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> (u8, u8, u8) {
    let chroma = value * saturation;
    let h = wrap_hue(hue) / 60.0;
    let x = chroma * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = if h < 1.0 {
        (chroma, x, 0.0)
    } else if h < 2.0 {
        (x, chroma, 0.0)
    } else if h < 3.0 {
        (0.0, chroma, x)
    } else if h < 4.0 {
        (0.0, x, chroma)
    } else if h < 5.0 {
        (x, 0.0, chroma)
    } else {
        (chroma, 0.0, x)
    };
    let m = value - chroma;

    (
        channel_to_u8(r1 + m),
        channel_to_u8(g1 + m),
        channel_to_u8(b1 + m),
    )
}

fn channel_to_u8(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn channel_f32_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[must_use]
pub fn default_anchor_a_rgb() -> RgbColor {
    parse_hex_rgb(DEFAULT_ANCHOR_A).expect("default anchor A is a valid CSS hex color")
}

#[must_use]
pub fn default_anchor_b_rgb() -> RgbColor {
    parse_hex_rgb(DEFAULT_ANCHOR_B).expect("default anchor B is a valid CSS hex color")
}

fn normalized_hue_delta(from: f64, to: f64) -> f64 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

fn wrap_hue(hue: f64) -> f64 {
    hue.rem_euclid(360.0)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineSettings {
    pub board: BoardKind,
    pub shape: ShapeKind,
    pub radius: f64,
    pub piece_radius: f64,
    pub visual_progress: bool,
    pub speed: SpeedMode,
    pub display_mode: DisplayMode,
    pub zoom: u8,
    pub track_opacity: f32,
    pub proactive_attacking: bool,
    pub enemy_mode: EnemyMode,
    pub army_preset: ArmyPreset,
    pub custom_army: Vec<CustomPiece>,
    pub continuous_offset: f64,
    pub prime_modulo_divisor: u32,
    pub anchor_color_a: String,
    pub anchor_color_b: String,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            board: BoardKind::LatticeSquare,
            shape: ShapeKind::Square,
            radius: 100.0,
            piece_radius: 0.5,
            visual_progress: true,
            speed: SpeedMode::Fastest,
            display_mode: DisplayMode::FitScreen,
            zoom: 4,
            track_opacity: 0.0,
            proactive_attacking: false,
            enemy_mode: EnemyMode::Color,
            army_preset: ArmyPreset::CustomFinite,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(2, 1),
            ],
            continuous_offset: 0.0,
            prime_modulo_divisor: 12,
            anchor_color_a: DEFAULT_ANCHOR_A.to_string(),
            anchor_color_b: DEFAULT_ANCHOR_B.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PieceSignature {
    pub a: i32,
    pub b: i32,
}

impl PieceSignature {
    #[must_use]
    pub const fn new(a: i32, b: i32) -> Self {
        Self { a, b }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpotCoord {
    Square { x: i64, y: i64 },
    Hex { q: i64, r: i64 },
    Triangle { u: i64, v: i64 },
    Continuous { x: f64, y: f64, theta: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorKey {
    pub group: u64,
    pub gradient_value: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ColorRule {
    Fixed,
    OrderRainbow,
    PrimeKnightModulo,
    PrimeGapBounds,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PieceColor {
    pub rule: ColorRule,
    pub fixed_css: String,
    pub key: ColorKey,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub id: u64,
    pub spot_index: u64,
    pub coord: SpotCoord,
    pub piece: PieceSignature,
    pub color: PieceColor,
    pub shape: ShapeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EngineStats {
    pub placements: u64,
    pub skipped_spots: u64,
    pub batches_emitted: u64,
    pub spots_tested: u64,
    pub piece_candidates_tested: u64,
    pub passive_rejections: u64,
    pub proactive_rejections: u64,
    pub current_radius: f64,
    pub exhausted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorState {
    pub min_gap: f64,
    pub max_gap: f64,
}

impl Default for ColorState {
    fn default() -> Self {
        Self {
            min_gap: 0.0,
            max_gap: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AppToWorker {
    Initialize { settings: EngineSettings },
    Reset { settings: EngineSettings },
    UpdateSettings { settings: EngineSettings },
    Start,
    Pause,
    RunTick,
    StepBatch { max_steps: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorkerToApp {
    Ready,
    Batch {
        log_placements: Vec<Placement>,
        vertex_update: VertexBufferUpdate,
        stats: EngineStats,
        color_state: ColorState,
    },
    Stats {
        stats: EngineStats,
        color_state: ColorState,
        vertex_update: VertexBufferUpdate,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VertexBufferUpdate {
    None,
    Append(Vec<f32>),
    Replace(Vec<f32>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_serialize_through_binary_transport() {
        let msg = AppToWorker::Initialize {
            settings: EngineSettings::default(),
        };
        let value = bincode::serialize(&msg).unwrap();
        let round_trip: AppToWorker = bincode::deserialize(&value).unwrap();
        assert_eq!(round_trip, msg);
    }

    #[test]
    fn rainbow_color_keeps_anchors_and_avoids_rgb_midpoint_blend() {
        assert_eq!(
            rainbow_color(DEFAULT_ANCHOR_A, DEFAULT_ANCHOR_B, 0.0),
            "#ff7800"
        );
        assert_eq!(
            rainbow_color(DEFAULT_ANCHOR_A, DEFAULT_ANCHOR_B, 1.0),
            "#ff0006"
        );
        assert_ne!(
            rainbow_color(DEFAULT_ANCHOR_A, DEFAULT_ANCHOR_B, 0.5),
            "#ff3c03"
        );
    }

    #[test]
    fn default_anchor_colors_match_requested_rgb_values() {
        assert_eq!(EngineSettings::default().anchor_color_a, "#ff7800");
        assert_eq!(EngineSettings::default().anchor_color_b, "#ff0006");
    }

    #[test]
    fn visual_progress_defaults_to_enabled() {
        assert!(EngineSettings::default().visual_progress);
    }
}
