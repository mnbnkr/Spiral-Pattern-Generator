use serde::{Deserialize, Serialize};

pub const DEFAULT_ANCHOR_A: &str = "#ff7800";
pub const DEFAULT_ANCHOR_B: &str = "#ff0006";
pub const DEFAULT_RADIUS: f64 = 200.0;
pub const MIN_FREE_CAMERA_ZOOM: f64 = 1.0;
pub const MAX_FREE_CAMERA_ZOOM: f64 = 256.0;

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
    AttackSet,
    Color,
    ColorAttackSet,
    FreeForAll,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PlacementSearchMode {
    SpiralPath,
    CenterDistance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ArmyPreset {
    CustomFinite,
    PrimeKnight,
    PrimeGapper,
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
    pub color_override: Option<String>,
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
            color_override: None,
        }
    }

    #[must_use]
    pub fn with_auto_color(a: i32, b: i32) -> Self {
        Self::new(a, b, auto_piece_color(a, b))
    }

    #[must_use]
    pub fn with_color_override(a: i32, b: i32, color_override: impl Into<String>) -> Self {
        let mut piece = Self::with_auto_color(a, b);
        piece.color_override = Some(color_override.into());
        piece
    }
}

#[must_use]
pub fn move_key_for_board(board: BoardKind, piece: PieceSignature) -> (u32, u32) {
    let a = piece.a.unsigned_abs();
    let b = piece.b.unsigned_abs();
    if board == BoardKind::LatticeTriangle {
        (a, b)
    } else {
        (a.min(b), a.max(b))
    }
}

#[must_use]
pub fn custom_army_moves_match_for_board(
    board: BoardKind,
    left: &[CustomPiece],
    right: &[CustomPiece],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            move_key_for_board(board, PieceSignature::new(left.a, left.b))
                == move_key_for_board(board, PieceSignature::new(right.a, right.b))
        })
}

#[must_use]
pub fn custom_army_moves_match(left: &[CustomPiece], right: &[CustomPiece]) -> bool {
    custom_army_moves_match_for_board(BoardKind::LatticeSquare, left, right)
}

#[must_use]
pub fn custom_army_color_overrides_match(left: &[CustomPiece], right: &[CustomPiece]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.color_override == right.color_override)
}

#[must_use]
pub fn custom_piece_order_color(
    settings: &EngineSettings,
    index: usize,
    army_len: usize,
) -> String {
    let color_t = if army_len <= 1 {
        0.0
    } else {
        index as f64 / (army_len - 1) as f64
    };
    rainbow_color(&settings.anchor_color_a, &settings.anchor_color_b, color_t)
}

#[must_use]
pub fn custom_piece_effective_color(settings: &EngineSettings, index: usize) -> String {
    let army_len = settings.custom_army.len();
    settings
        .custom_army
        .get(index)
        .and_then(|piece| {
            piece
                .color_override
                .as_deref()
                .and_then(parse_hex_rgb)
                .map(RgbColor::to_css_hex)
        })
        .unwrap_or_else(|| custom_piece_order_color(settings, index, army_len))
}

#[must_use]
pub fn custom_army_effective_color_groups(settings: &EngineSettings) -> Vec<u64> {
    let colors = settings
        .custom_army
        .iter()
        .enumerate()
        .map(|(index, _)| custom_piece_effective_color(settings, index))
        .collect::<Vec<_>>();

    colors
        .iter()
        .enumerate()
        .map(|(index, color)| {
            colors
                .iter()
                .position(|candidate| candidate == color)
                .unwrap_or(index) as u64
        })
        .collect()
}

#[must_use]
pub fn custom_army_effective_color_groups_match(
    left: &EngineSettings,
    right: &EngineSettings,
) -> bool {
    custom_army_effective_color_groups(left) == custom_army_effective_color_groups(right)
}

#[must_use]
pub fn custom_color_groups_affect_generation(settings: &EngineSettings) -> bool {
    settings.army_preset == ArmyPreset::CustomFinite
        && matches!(
            settings.enemy_mode,
            EnemyMode::Color | EnemyMode::ColorAttackSet
        )
}

#[must_use]
pub fn custom_color_group_change_affects_generation(
    left: &EngineSettings,
    right: &EngineSettings,
) -> bool {
    (custom_color_groups_affect_generation(left) || custom_color_groups_affect_generation(right))
        && !custom_army_effective_color_groups_match(left, right)
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

#[must_use]
pub fn normalize_prime_modulo_divisor(value: u32) -> u32 {
    let rounded = value.saturating_add(3) / 6 * 6;
    rounded.max(6)
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
    pub zoom: f64,
    pub track_opacity: f32,
    pub attack_overlay_opacity: f32,
    pub proactive_attacking: bool,
    pub enemy_mode: EnemyMode,
    pub placement_search: PlacementSearchMode,
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
            radius: DEFAULT_RADIUS,
            piece_radius: 0.5,
            visual_progress: true,
            speed: SpeedMode::Fastest,
            display_mode: DisplayMode::FitScreen,
            zoom: 1.0,
            track_opacity: 0.1,
            attack_overlay_opacity: 0.0,
            proactive_attacking: false,
            enemy_mode: EnemyMode::Color,
            placement_search: PlacementSearchMode::SpiralPath,
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
    PrimeGapperBounds,
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
    Initialize {
        epoch: u64,
        settings: EngineSettings,
    },
    Reset {
        epoch: u64,
        settings: EngineSettings,
    },
    UpdateSettings {
        epoch: u64,
        settings: EngineSettings,
    },
    Start {
        epoch: u64,
    },
    Pause {
        epoch: u64,
    },
    RunTick {
        epoch: u64,
    },
    StepBatch {
        epoch: u64,
        max_steps: u32,
    },
    BuildAttackOverlay {
        epoch: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorkerToApp {
    Ready,
    Batch {
        epoch: u64,
        log_placements: Vec<Placement>,
        vertex_update: VertexBufferUpdate,
        attack_overlay_update: AttackOverlayUpdate,
        attack_overlay_pending: bool,
        stats: EngineStats,
        color_state: ColorState,
    },
    Stats {
        epoch: u64,
        stats: EngineStats,
        color_state: ColorState,
        vertex_update: VertexBufferUpdate,
        attack_overlay_update: AttackOverlayUpdate,
        attack_overlay_pending: bool,
    },
    Error {
        epoch: u64,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VertexBufferUpdate {
    None,
    Append(Vec<f32>),
    Replace(Vec<f32>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttackOverlayUpdate {
    pub spots: VertexBufferUpdate,
    pub hits: VertexBufferUpdate,
    pub circles: VertexBufferUpdate,
}

impl AttackOverlayUpdate {
    #[must_use]
    pub fn none() -> Self {
        Self {
            spots: VertexBufferUpdate::None,
            hits: VertexBufferUpdate::None,
            circles: VertexBufferUpdate::None,
        }
    }

    #[must_use]
    pub fn replace_empty() -> Self {
        Self {
            spots: VertexBufferUpdate::Replace(Vec::new()),
            hits: VertexBufferUpdate::Replace(Vec::new()),
            circles: VertexBufferUpdate::Replace(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_serialize_through_binary_transport() {
        let msg = AppToWorker::Initialize {
            epoch: 7,
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
    fn custom_color_overrides_share_effective_enemy_groups_without_changing_order() {
        let settings = EngineSettings {
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_color_override(3, 1, "#ff7800"),
                CustomPiece::with_auto_color(4, 1),
            ],
            ..EngineSettings::default()
        };

        assert_ne!(
            custom_piece_order_color(&settings, 1, 3),
            custom_piece_effective_color(&settings, 1)
        );
        assert_eq!(
            custom_piece_effective_color(&settings, 1),
            custom_piece_order_color(&settings, 0, 3)
        );
        assert_eq!(custom_army_effective_color_groups(&settings), vec![0, 0, 2]);
    }

    #[test]
    fn move_keys_are_symmetric_except_on_triangle_board() {
        let left = PieceSignature::new(3, 0);
        let right = PieceSignature::new(0, 3);
        let min_leg = PieceSignature::new(i32::MIN, 0);

        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::ContinuousArchimedean,
        ] {
            assert_eq!(
                move_key_for_board(board, left),
                move_key_for_board(board, right)
            );
            assert!(custom_army_moves_match_for_board(
                board,
                &[CustomPiece::with_auto_color(3, 0)],
                &[CustomPiece::with_auto_color(0, 3)]
            ));
        }

        assert_ne!(
            move_key_for_board(BoardKind::LatticeTriangle, left),
            move_key_for_board(BoardKind::LatticeTriangle, right)
        );
        assert!(!custom_army_moves_match_for_board(
            BoardKind::LatticeTriangle,
            &[CustomPiece::with_auto_color(3, 0)],
            &[CustomPiece::with_auto_color(0, 3)]
        ));
        assert_eq!(
            move_key_for_board(BoardKind::LatticeSquare, min_leg),
            (0, 2_147_483_648)
        );
    }

    #[test]
    fn visual_progress_defaults_to_enabled() {
        assert!(EngineSettings::default().visual_progress);
    }

    #[test]
    fn prime_modulo_divisor_normalizes_to_multiples_of_six() {
        assert_eq!(normalize_prime_modulo_divisor(0), 6);
        assert_eq!(normalize_prime_modulo_divisor(2), 6);
        assert_eq!(normalize_prime_modulo_divisor(8), 6);
        assert_eq!(normalize_prime_modulo_divisor(10), 12);
        assert_eq!(normalize_prime_modulo_divisor(15), 18);
    }
}
