use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::engine::spatial::{ContinuousSpatialHash, LatticeSpatialIndex};
use crate::math::{
    ArchimedeanSpiral, ArchimedeanSpots, AxialCoord, GEOM_EPS, HexSpiral, Point2, SquareCoord,
    SquareSpiral, TriangleCoord, TriangleSpiral, attack_circle_hits_body_distance_squared,
    attack_radius_from_move,
};
use crate::protocol::{
    ArmyPreset, BoardKind, ColorKey, ColorRule, ColorState, CustomPiece, EnemyMode, EngineSettings,
    EngineStats, PieceColor, PieceSignature, Placement, PlacementSearchMode, ShapeKind, SpotCoord,
    custom_army_effective_color_groups, custom_army_moves_match_for_board,
    custom_color_group_change_affects_generation, move_key_for_board,
    normalize_prime_modulo_divisor,
};

const CONTINUOUS_CENTER_STREAMS: usize = 1;
const SPECIAL_PRIME_COLOR_GROUP: u64 = 0;
const SPECIAL_PRIME_COLOR: &str = "#808080";
const ATTACK_RELEVANCE_RADIUS_MULTIPLIER: f64 = 4.0;
const CONTINUOUS_PLACEMENT_OVERLAP_TOLERANCE_FRACTION: f64 = 0.0045;
const CONTINUOUS_PLACEMENT_OVERLAP_TOLERANCE_ABSOLUTE: f64 = 1.0e-10;
const MAX_PREALLOCATED_SPIRAL_PATH_SPOTS: u64 = 12_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationMode {
    SpotSeeking,
    PieceSeeking,
}

impl SimulationMode {
    #[must_use]
    pub fn from_preset(preset: ArmyPreset) -> Self {
        match preset {
            ArmyPreset::CustomFinite => Self::SpotSeeking,
            ArmyPreset::PrimeKnight | ArmyPreset::PrimeGapper => Self::PieceSeeking,
        }
    }
}

#[derive(Clone, Debug)]
struct CandidatePiece {
    signature: PieceSignature,
    color: PieceColor,
    move_group: (u32, u32),
    color_group: u64,
    enemy_color_group: u64,
    attack_radius: f64,
}

#[derive(Clone, Debug)]
struct PlacedPiece {
    spot_index: u64,
    location: PlacedLocation,
    piece: PieceSignature,
    color_rule: ColorRule,
    fixed_color: StoredFixedColor,
    color_key: ColorKey,
    enemy_color_group: u64,
    attack_radius: f64,
}

#[derive(Clone, Copy, Debug)]
enum PlacedLocation {
    Lattice { a: i64, b: i64 },
    Continuous { center: Point2, theta: f64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoredFixedColor {
    None,
    SpecialPrime,
}

impl StoredFixedColor {
    fn from_color(color: &PieceColor) -> Self {
        if color.rule == ColorRule::Fixed && color.fixed_css == SPECIAL_PRIME_COLOR {
            Self::SpecialPrime
        } else {
            Self::None
        }
    }

    fn to_css(self) -> String {
        match self {
            Self::None => String::new(),
            Self::SpecialPrime => SPECIAL_PRIME_COLOR.to_string(),
        }
    }
}

impl PlacedPiece {
    fn to_protocol(&self, id: u64, board: BoardKind, shape: ShapeKind) -> Placement {
        let coord = match board {
            BoardKind::LatticeSquare => {
                let (x, y) = self.lattice_coord();
                SpotCoord::Square { x, y }
            }
            BoardKind::LatticeHex => {
                let (q, r) = self.lattice_coord();
                SpotCoord::Hex { q, r }
            }
            BoardKind::LatticeTriangle => {
                let (u, v) = self.lattice_coord();
                SpotCoord::Triangle { u, v }
            }
            BoardKind::ContinuousArchimedean => match self.location {
                PlacedLocation::Continuous { center, theta } => SpotCoord::Continuous {
                    x: center.x,
                    y: center.y,
                    theta,
                },
                PlacedLocation::Lattice { .. } => SpotCoord::Continuous {
                    x: 0.0,
                    y: 0.0,
                    theta: 0.0,
                },
            },
        };

        Placement {
            id,
            spot_index: self.spot_index,
            coord,
            piece: self.piece,
            color: PieceColor {
                rule: self.color_rule,
                fixed_css: self.fixed_color.to_css(),
                key: self.color_key,
            },
            shape,
        }
    }

    fn lattice_coord(&self) -> (i64, i64) {
        match self.location {
            PlacedLocation::Lattice { a, b } => (a, b),
            PlacedLocation::Continuous { .. } => (0, 0),
        }
    }

    fn continuous_center(&self) -> Option<Point2> {
        match self.location {
            PlacedLocation::Continuous { center, .. } => Some(center),
            PlacedLocation::Lattice { .. } => None,
        }
    }
}

fn preallocated_spiral_path_spots(settings: &EngineSettings) -> Option<usize> {
    if settings.placement_search != PlacementSearchMode::SpiralPath {
        return None;
    }

    let radius = settings.radius.max(0.0).floor() as u64;
    let count = match settings.board {
        BoardKind::LatticeSquare => {
            let side = radius.saturating_mul(2).saturating_add(1);
            side.saturating_mul(side)
        }
        BoardKind::LatticeHex => 1_u64.saturating_add(
            3_u64
                .saturating_mul(radius)
                .saturating_mul(radius.saturating_add(1)),
        ),
        BoardKind::LatticeTriangle => {
            let last_segment = radius.saturating_mul(3);
            last_segment
                .saturating_mul(last_segment.saturating_add(1))
                .saturating_div(2)
                .saturating_add(1)
        }
        BoardKind::ContinuousArchimedean => return None,
    };

    if count <= MAX_PREALLOCATED_SPIRAL_PATH_SPOTS {
        usize::try_from(count).ok()
    } else {
        None
    }
}

#[derive(Clone, Debug)]
enum BoardSpot {
    Square {
        index: u64,
        coord: SquareCoord,
    },
    Hex {
        index: u64,
        coord: AxialCoord,
    },
    Triangle {
        index: u64,
        coord: TriangleCoord,
        spiral_radius: u64,
    },
    Continuous {
        index: u64,
        theta: f64,
        center: Point2,
    },
}

#[derive(Clone, Debug)]
struct CenterQueueEntry {
    distance_squared: f64,
    spot_index: u64,
    continuous_stream: Option<usize>,
    spot: BoardSpot,
}

impl PartialEq for CenterQueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.distance_squared.total_cmp(&other.distance_squared) == Ordering::Equal
            && self.spot_index == other.spot_index
    }
}

impl Eq for CenterQueueEntry {}

impl PartialOrd for CenterQueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CenterQueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .distance_squared
            .total_cmp(&self.distance_squared)
            .then_with(|| other.spot_index.cmp(&self.spot_index))
    }
}

impl BoardSpot {
    #[must_use]
    fn index(&self) -> u64 {
        match self {
            Self::Square { index, .. }
            | Self::Hex { index, .. }
            | Self::Triangle { index, .. }
            | Self::Continuous { index, .. } => *index,
        }
    }

    #[must_use]
    fn lattice_coord(&self) -> Option<(i64, i64)> {
        match self {
            Self::Square { coord, .. } => Some((coord.x, coord.y)),
            Self::Hex { coord, .. } => Some((coord.q, coord.r)),
            Self::Triangle { coord, .. } => Some((coord.u, coord.v)),
            Self::Continuous { .. } => None,
        }
    }

    #[must_use]
    fn center(&self) -> Point2 {
        match self {
            Self::Square { coord, .. } => coord.to_point(),
            Self::Hex { coord, .. } => coord.to_point(),
            Self::Triangle { coord, .. } => coord.to_point(),
            Self::Continuous { center, .. } => *center,
        }
    }

    #[must_use]
    fn center_distance_squared(&self) -> f64 {
        let center = self.center();
        center.x.mul_add(center.x, center.y * center.y)
    }

    #[must_use]
    fn spot_coord(&self) -> SpotCoord {
        match self {
            Self::Square { coord, .. } => SpotCoord::Square {
                x: coord.x,
                y: coord.y,
            },
            Self::Hex { coord, .. } => SpotCoord::Hex {
                q: coord.q,
                r: coord.r,
            },
            Self::Triangle { coord, .. } => SpotCoord::Triangle {
                u: coord.u,
                v: coord.v,
            },
            Self::Continuous { theta, center, .. } => SpotCoord::Continuous {
                x: center.x,
                y: center.y,
                theta: *theta,
            },
        }
    }

    #[must_use]
    fn generation_radius(&self) -> f64 {
        match self {
            Self::Square { coord, .. } => coord.x.abs().max(coord.y.abs()) as f64,
            Self::Hex { coord, .. } => {
                let (x, y, z) = coord.cube();
                x.abs().max(y.abs()).max(z.abs()) as f64
            }
            Self::Triangle { spiral_radius, .. } => *spiral_radius as f64,
            Self::Continuous { center, .. } => center.radius(),
        }
    }
}

#[derive(Debug)]
pub struct SimulationEngine {
    settings: EngineSettings,
    mode: SimulationMode,
    stats: EngineStats,
    next_id: u64,
    next_army_index: u64,
    custom_spot_order_indices: Vec<u64>,
    next_prime_candidate_index: usize,
    shared_prime_spot_order_index: u64,
    prime_color_spot_order_indices: FxHashMap<u64, u64>,
    custom_enemy_color_groups: Vec<u64>,
    spots: Vec<BoardSpot>,
    spots_exhausted: bool,
    first_out_of_radius_spot: Option<BoardSpot>,
    center_ordered_spots: Vec<BoardSpot>,
    center_queue: BinaryHeap<CenterQueueEntry>,
    center_next_shell: u64,
    center_shells_exhausted: bool,
    continuous_spiral: ArchimedeanSpots,
    continuous_center_spirals: Vec<ArchimedeanSpots>,
    continuous_center_streams_exhausted: Vec<bool>,
    lattice_index: LatticeSpatialIndex,
    continuous_index: ContinuousSpatialHash,
    max_continuous_attack_radius: f64,
    placed: Vec<PlacedPiece>,
    prime_cache: Vec<u32>,
    min_gap_seen: Option<f64>,
    max_gap_seen: Option<f64>,
}

impl SimulationEngine {
    #[must_use]
    pub fn new(settings: EngineSettings) -> Self {
        let mode = SimulationMode::from_preset(settings.army_preset);
        let custom_army_len = settings.custom_army.len();
        let continuous_spiral = ArchimedeanSpiral::spots(settings.continuous_offset);
        let continuous_center_spirals = continuous_center_spirals(&settings);
        let continuous_center_streams_exhausted = vec![false; continuous_center_spirals.len()];
        let preallocated_spiral_path_spots = preallocated_spiral_path_spots(&settings);
        let mut spots = Vec::new();
        let mut placed = Vec::new();
        if let Some(capacity) = preallocated_spiral_path_spots {
            if settings.board == BoardKind::ContinuousArchimedean {
                let _ = spots.try_reserve_exact(capacity);
            }
            let _ = placed.try_reserve_exact(capacity);
        }
        let custom_enemy_color_groups = custom_army_effective_color_groups(&settings);
        Self {
            settings,
            mode,
            stats: EngineStats::default(),
            next_id: 0,
            next_army_index: 0,
            custom_spot_order_indices: vec![0; custom_army_len],
            next_prime_candidate_index: 0,
            shared_prime_spot_order_index: 0,
            prime_color_spot_order_indices: FxHashMap::default(),
            custom_enemy_color_groups,
            spots,
            spots_exhausted: false,
            first_out_of_radius_spot: None,
            center_ordered_spots: Vec::new(),
            center_queue: BinaryHeap::new(),
            center_next_shell: 0,
            center_shells_exhausted: false,
            continuous_spiral,
            continuous_center_spirals,
            continuous_center_streams_exhausted,
            lattice_index: LatticeSpatialIndex::new(),
            continuous_index: ContinuousSpatialHash::new(2.0),
            max_continuous_attack_radius: 0.0,
            placed,
            prime_cache: Vec::new(),
            min_gap_seen: None,
            max_gap_seen: None,
        }
    }

    pub fn reset(&mut self, settings: EngineSettings) {
        *self = Self::new(settings);
    }

    #[must_use]
    pub fn placement_count(&self) -> usize {
        self.placed.len()
    }

    #[must_use]
    pub fn placement_at(&self, index: usize) -> Option<Placement> {
        let shape = forced_shape_for_board(self.settings.board, self.settings.shape);
        self.placed
            .get(index)
            .map(|placement| placement.to_protocol(index as u64, self.settings.board, shape))
    }

    #[must_use]
    pub fn placements_in_range(&self, start: usize, end: usize) -> Vec<Placement> {
        let end = end.min(self.placed.len());
        if start >= end {
            return Vec::new();
        }

        let mut placements = Vec::with_capacity(end - start);
        let shape = forced_shape_for_board(self.settings.board, self.settings.shape);
        placements.extend(
            self.placed[start..end]
                .iter()
                .enumerate()
                .map(|(offset, placement)| {
                    placement.to_protocol((start + offset) as u64, self.settings.board, shape)
                }),
        );
        placements
    }

    pub fn update_settings(&mut self, settings: EngineSettings) -> bool {
        let radius_increased = settings.radius > self.settings.radius;
        if self.requires_reset_for_settings(&settings) {
            self.reset(settings);
            true
        } else {
            self.settings = settings;
            self.custom_enemy_color_groups = custom_army_effective_color_groups(&self.settings);
            if radius_increased {
                self.reopen_spiral_path_radius_bound();
            }
            false
        }
    }

    #[must_use]
    pub fn settings(&self) -> &EngineSettings {
        &self.settings
    }

    #[must_use]
    pub fn stats(&self) -> EngineStats {
        self.stats
    }

    #[must_use]
    pub fn color_state(&self) -> ColorState {
        match (self.min_gap_seen, self.max_gap_seen) {
            (Some(min_gap), Some(max_gap)) if max_gap > min_gap => ColorState { min_gap, max_gap },
            (Some(gap), _) => ColorState {
                min_gap: gap,
                max_gap: gap + 1.0,
            },
            _ => ColorState::default(),
        }
    }

    pub fn step_batch(&mut self, max_steps: u32) -> Vec<Placement> {
        self.step_budget(max_steps, u64::MAX / 4)
    }

    pub fn step_budget(&mut self, max_steps: u32, max_work: u64) -> Vec<Placement> {
        let count = max_steps.max(1);
        let mut placements = Vec::with_capacity(count as usize);
        let mut remaining_work = max_work.max(1);

        while placements.len() < count as usize && remaining_work > 0 {
            let placement = match self.mode {
                SimulationMode::SpotSeeking => {
                    self.next_spot_seeking_placement(&mut remaining_work)
                }
                SimulationMode::PieceSeeking => {
                    self.next_piece_seeking_placement(&mut remaining_work)
                }
            };

            match placement {
                Some(placement) => placements.push(placement),
                None => break,
            }
        }

        if !placements.is_empty() {
            self.stats.batches_emitted += 1;
        }

        placements
    }

    fn next_spot_seeking_placement(&mut self, remaining_work: &mut u64) -> Option<Placement> {
        let army_len = self.custom_army_len();
        if army_len == 0 {
            self.stats.exhausted = true;
            return None;
        }

        self.ensure_custom_spot_cursor_capacity(army_len);
        let army_index = (self.next_army_index as usize) % army_len;
        let piece = self.custom_candidate(army_index);
        let mut spot_order_index = self.custom_spot_order_indices[army_index];

        while *remaining_work > 0 {
            let Some(spot) = self.spot_at_search_order(spot_order_index) else {
                self.custom_spot_order_indices[army_index] = spot_order_index;
                self.stats.exhausted = true;
                return None;
            };
            spot_order_index += 1;
            self.custom_spot_order_indices[army_index] = spot_order_index;
            *remaining_work -= 1;

            self.stats.spots_tested += 1;
            if self.is_valid_candidate(&spot, &piece) {
                self.next_army_index += 1;
                return Some(self.place_piece(spot, piece));
            }

            self.stats.skipped_spots += 1;
        }

        None
    }

    fn next_piece_seeking_placement(&mut self, remaining_work: &mut u64) -> Option<Placement> {
        let piece = self.prime_candidate(self.next_prime_candidate_index);
        let mut spot_order_index = self.prime_spot_order_index(&piece);

        while *remaining_work > 0 {
            let Some(spot) = self.spot_at_search_order(spot_order_index) else {
                self.set_prime_spot_order_index(&piece, spot_order_index);
                self.stats.exhausted = true;
                return None;
            };
            spot_order_index += 1;
            self.set_prime_spot_order_index(&piece, spot_order_index);
            self.stats.piece_candidates_tested += 1;
            self.stats.spots_tested += 1;
            *remaining_work -= 1;

            if self.is_valid_candidate(&spot, &piece) {
                self.next_prime_candidate_index += 1;
                return Some(self.place_piece(spot, piece));
            }

            self.stats.skipped_spots += 1;
        }

        None
    }

    fn place_piece(&mut self, spot: BoardSpot, piece: CandidatePiece) -> Placement {
        let shape = forced_shape_for_board(self.settings.board, self.settings.shape);

        let center = spot.center();
        let lattice_coord = spot.lattice_coord();
        let coord = spot.spot_coord();
        let continuous_theta = match coord {
            SpotCoord::Continuous { theta, .. } => theta,
            _ => 0.0,
        };
        let spot_index = spot.index();
        let id = self.next_id;
        let fixed_color = StoredFixedColor::from_color(&piece.color);
        let color_rule = piece.color.rule;
        let color_key = piece.color.key;
        let enemy_color_group = piece.enemy_color_group;
        let location = lattice_coord.map_or(
            PlacedLocation::Continuous {
                center,
                theta: continuous_theta,
            },
            |(a, b)| PlacedLocation::Lattice { a, b },
        );

        let placed_piece = PlacedPiece {
            spot_index,
            location,
            piece: piece.signature,
            color_rule,
            fixed_color,
            color_key,
            enemy_color_group,
            attack_radius: piece.attack_radius,
        };

        if let Some(coord) = lattice_coord {
            self.lattice_index.insert_occupied(coord, id);
            if attack_radius_relevant_to_generation(&self.settings, piece.attack_radius) {
                for attack in lattice_attack_targets(self.settings.board, coord, piece.signature) {
                    self.lattice_index.add_attack(attack, id);
                }
            }
        } else {
            self.continuous_index.insert(id, center);
            self.max_continuous_attack_radius =
                self.max_continuous_attack_radius.max(piece.attack_radius);
        }

        if piece.color.rule == ColorRule::PrimeGapperBounds {
            let gap = piece.color.key.gradient_value;
            self.min_gap_seen = Some(self.min_gap_seen.map_or(gap, |current| current.min(gap)));
            self.max_gap_seen = Some(self.max_gap_seen.map_or(gap, |current| current.max(gap)));
        }

        self.placed.push(placed_piece);
        self.next_id += 1;
        self.stats.placements += 1;

        Placement {
            id,
            spot_index,
            coord,
            piece: piece.signature,
            color: piece.color,
            shape,
        }
    }

    fn is_valid_candidate(&mut self, spot: &BoardSpot, candidate: &CandidatePiece) -> bool {
        match self.settings.board {
            BoardKind::LatticeSquare | BoardKind::LatticeHex | BoardKind::LatticeTriangle => {
                self.is_valid_lattice_candidate(spot, candidate)
            }
            BoardKind::ContinuousArchimedean => self.is_valid_continuous_candidate(spot, candidate),
        }
    }

    fn is_valid_lattice_candidate(&mut self, spot: &BoardSpot, candidate: &CandidatePiece) -> bool {
        let Some(coord) = spot.lattice_coord() else {
            return false;
        };

        if self.lattice_index.contains(coord) {
            return false;
        }

        for attacker_id in self.lattice_index.attackers_at(coord) {
            let Some(attacker) = self.placed.get(*attacker_id as usize) else {
                continue;
            };

            if self.are_enemies(attacker, candidate) {
                self.stats.passive_rejections += 1;
                return false;
            }
        }

        if self.settings.proactive_attacking
            && attack_radius_relevant_to_generation(&self.settings, candidate.attack_radius)
        {
            for target_coord in
                lattice_attack_targets(self.settings.board, coord, candidate.signature)
            {
                if let Some(target_id) = self.lattice_index.get(target_coord) {
                    let Some(target) = self.placed.get(target_id as usize) else {
                        continue;
                    };

                    if self.are_enemies(target, candidate) {
                        self.stats.proactive_rejections += 1;
                        return false;
                    }
                }
            }
        }

        true
    }

    fn is_valid_continuous_candidate(
        &mut self,
        spot: &BoardSpot,
        candidate: &CandidatePiece,
    ) -> bool {
        let center = spot.center();
        let body_radius = self.settings.piece_radius;
        let body_probe_radius = 2.0 * body_radius;
        for id in self.continuous_index.nearby_ids(center, body_probe_radius) {
            let Some(existing) = self.placed.get(id as usize) else {
                continue;
            };
            let Some(existing_center) = existing.continuous_center() else {
                continue;
            };

            if continuous_bodies_overlap_for_placement_rejection(
                center,
                existing_center,
                body_radius,
            ) {
                return false;
            }
        }

        for id in self.continuous_passive_probe_ids(center, body_radius) {
            let Some(existing) = self.placed.get(id as usize) else {
                continue;
            };
            let Some(existing_center) = existing.continuous_center() else {
                continue;
            };

            if self.are_enemies(existing, candidate)
                && attack_circle_hits_body_distance_squared(
                    existing_center.squared_distance(center),
                    existing.attack_radius,
                    body_radius,
                )
            {
                self.stats.passive_rejections += 1;
                return false;
            }
        }

        if self.settings.proactive_attacking {
            for id in self.continuous_proactive_probe_ids(center, candidate, body_radius) {
                let Some(existing) = self.placed.get(id as usize) else {
                    continue;
                };
                let Some(existing_center) = existing.continuous_center() else {
                    continue;
                };

                if self.are_enemies(existing, candidate)
                    && attack_circle_hits_body_distance_squared(
                        center.squared_distance(existing_center),
                        candidate.attack_radius,
                        body_radius,
                    )
                {
                    self.stats.proactive_rejections += 1;
                    return false;
                }
            }
        }

        true
    }

    fn continuous_passive_probe_ids(&self, center: Point2, body_radius: f64) -> Vec<u64> {
        if self.max_continuous_attack_radius <= 0.0 {
            return Vec::new();
        }

        let bounded_attack_radius = self
            .max_continuous_attack_radius
            .min(max_relevant_attack_radius(&self.settings) + body_radius.max(0.0));
        if bounded_attack_radius <= 0.0 {
            return Vec::new();
        }

        self.continuous_index
            .nearby_ids(center, bounded_attack_radius + body_radius.max(0.0))
    }

    fn continuous_proactive_probe_ids(
        &self,
        center: Point2,
        candidate: &CandidatePiece,
        body_radius: f64,
    ) -> Vec<u64> {
        if !attack_radius_relevant_to_generation(&self.settings, candidate.attack_radius) {
            return Vec::new();
        }

        let max_possible_distance =
            center.radius() + self.settings.radius.max(0.0) + body_radius.max(0.0);
        if candidate.attack_radius > max_possible_distance {
            return Vec::new();
        }

        self.continuous_index.nearby_ids(
            center,
            candidate.attack_radius.max(0.0) + body_radius.max(0.0),
        )
    }

    fn are_enemies(&self, existing: &PlacedPiece, candidate: &CandidatePiece) -> bool {
        if self.enemy_mode_uses_color() && existing.enemy_color_group != candidate.enemy_color_group
        {
            return true;
        }

        self.enemy_mode_uses_attack_set()
            && move_group(self.settings.board, existing.piece) != candidate.move_group
    }

    fn enemy_mode_uses_color(&self) -> bool {
        matches!(
            self.settings.enemy_mode,
            EnemyMode::Color | EnemyMode::ColorAttackSet
        )
    }

    fn enemy_mode_uses_attack_set(&self) -> bool {
        matches!(
            self.settings.enemy_mode,
            EnemyMode::AttackSet | EnemyMode::ColorAttackSet
        )
    }

    fn custom_army_len(&self) -> usize {
        self.settings.custom_army.len()
    }

    fn ensure_custom_spot_cursor_capacity(&mut self, army_len: usize) {
        match self.custom_spot_order_indices.len().cmp(&army_len) {
            Ordering::Less => self.custom_spot_order_indices.resize(army_len, 0),
            Ordering::Greater => self.custom_spot_order_indices.truncate(army_len),
            Ordering::Equal => {}
        }
    }

    fn prime_spot_order_index(&self, candidate: &CandidatePiece) -> u64 {
        if self.prime_uses_color_group_cursors() {
            self.prime_color_spot_order_indices
                .get(&candidate.color_group)
                .copied()
                .unwrap_or(0)
        } else {
            self.shared_prime_spot_order_index
        }
    }

    fn set_prime_spot_order_index(&mut self, candidate: &CandidatePiece, spot_order_index: u64) {
        if self.prime_uses_color_group_cursors() {
            self.prime_color_spot_order_indices
                .insert(candidate.color_group, spot_order_index);
        } else {
            self.shared_prime_spot_order_index = spot_order_index;
        }
    }

    fn prime_uses_color_group_cursors(&self) -> bool {
        self.settings.enemy_mode == EnemyMode::Color
    }

    fn requires_reset_for_settings(&self, next: &EngineSettings) -> bool {
        let current = &self.settings;

        current.board != next.board
            || self.radius_change_requires_reset(next)
            || continuous_piece_radius_changes_simulation(current, next)
            || current.proactive_attacking != next.proactive_attacking
            || current.enemy_mode != next.enemy_mode
            || current.placement_search != next.placement_search
            || current.army_preset != next.army_preset
            || !custom_army_moves_match_for_board(
                current.board,
                &current.custom_army,
                &next.custom_army,
            )
            || custom_color_group_change_affects_generation(current, next)
            || current.continuous_offset != next.continuous_offset
            || current.prime_modulo_divisor != next.prime_modulo_divisor
    }

    fn radius_change_requires_reset(&self, next: &EngineSettings) -> bool {
        let current = &self.settings;
        if current.radius == next.radius {
            return false;
        }

        current.placement_search != PlacementSearchMode::SpiralPath || next.radius < current.radius
    }

    fn custom_candidate(&self, index: usize) -> CandidatePiece {
        let fallback = CustomPiece::with_auto_color(2, 1);
        let piece = self.settings.custom_army.get(index).unwrap_or(&fallback);
        let signature = PieceSignature::new(piece.a, piece.b);
        let army_len = self.custom_army_len();
        let color_t = if army_len <= 1 {
            0.0
        } else {
            index as f64 / (army_len - 1) as f64
        };
        let color_group = index as u64;
        let enemy_color_group = self
            .custom_enemy_color_groups
            .get(index)
            .copied()
            .unwrap_or(color_group);

        CandidatePiece {
            signature,
            color: PieceColor {
                rule: ColorRule::OrderRainbow,
                fixed_css: String::new(),
                key: ColorKey {
                    group: color_group,
                    gradient_value: color_t,
                },
            },
            move_group: move_group(self.settings.board, signature),
            color_group,
            enemy_color_group,
            attack_radius: attack_radius_from_move(signature.a, signature.b),
        }
    }

    fn prime_candidate(&mut self, candidate_index: usize) -> CandidatePiece {
        match self.settings.army_preset {
            ArmyPreset::PrimeKnight => {
                let p = self.prime(candidate_index) as i32;
                let signature = PieceSignature::new(1, p);
                if candidate_index == 0 {
                    return CandidatePiece {
                        signature,
                        color: PieceColor {
                            rule: ColorRule::Fixed,
                            fixed_css: SPECIAL_PRIME_COLOR.to_string(),
                            key: ColorKey {
                                group: SPECIAL_PRIME_COLOR_GROUP,
                                gradient_value: 0.0,
                            },
                        },
                        move_group: move_group(self.settings.board, signature),
                        color_group: SPECIAL_PRIME_COLOR_GROUP,
                        enemy_color_group: SPECIAL_PRIME_COLOR_GROUP,
                        attack_radius: attack_radius_from_move(signature.a, signature.b),
                    };
                }

                let (bucket, t) = prime_knight_color_bucket(
                    candidate_index as u32 + 1,
                    self.settings.prime_modulo_divisor,
                );
                let color_group = bucket as u64 + 1;

                CandidatePiece {
                    signature,
                    color: PieceColor {
                        rule: ColorRule::PrimeKnightModulo,
                        fixed_css: String::new(),
                        key: ColorKey {
                            group: color_group,
                            gradient_value: t,
                        },
                    },
                    move_group: move_group(self.settings.board, signature),
                    color_group,
                    enemy_color_group: color_group,
                    attack_radius: attack_radius_from_move(signature.a, signature.b),
                }
            }
            ArmyPreset::PrimeGapper => {
                let a = self.prime(candidate_index) as i32;
                let b = self.prime(candidate_index + 1) as i32;
                let signature = PieceSignature::new(a, b);
                if candidate_index == 0 {
                    return CandidatePiece {
                        signature,
                        color: PieceColor {
                            rule: ColorRule::Fixed,
                            fixed_css: SPECIAL_PRIME_COLOR.to_string(),
                            key: ColorKey {
                                group: SPECIAL_PRIME_COLOR_GROUP,
                                gradient_value: 0.0,
                            },
                        },
                        move_group: move_group(self.settings.board, signature),
                        color_group: SPECIAL_PRIME_COLOR_GROUP,
                        enemy_color_group: SPECIAL_PRIME_COLOR_GROUP,
                        attack_radius: attack_radius_from_move(signature.a, signature.b),
                    };
                }

                let gap = signature_gap(signature);

                CandidatePiece {
                    signature,
                    color: PieceColor {
                        rule: ColorRule::PrimeGapperBounds,
                        fixed_css: String::new(),
                        key: ColorKey {
                            group: gap as u64,
                            gradient_value: gap as f64,
                        },
                    },
                    move_group: move_group(self.settings.board, signature),
                    color_group: gap as u64,
                    enemy_color_group: gap as u64,
                    attack_radius: attack_radius_from_move(signature.a, signature.b),
                }
            }
            ArmyPreset::CustomFinite => self.custom_candidate(0),
        }
    }

    fn spot_at_search_order(&mut self, order_index: u64) -> Option<BoardSpot> {
        match self.settings.placement_search {
            PlacementSearchMode::SpiralPath => self.spot_at(index_from_u64(order_index)?),
            PlacementSearchMode::CenterDistance => self.center_ordered_spot(order_index),
        }
    }

    fn center_ordered_spot(&mut self, order_index: u64) -> Option<BoardSpot> {
        if self.settings.board == BoardKind::ContinuousArchimedean {
            return self.continuous_center_ordered_spot(order_index);
        }

        let order_index = index_from_u64(order_index)?;
        while self.center_ordered_spots.len() <= order_index {
            if !self.advance_center_order() {
                return None;
            }
        }

        self.center_ordered_spots.get(order_index).cloned()
    }

    fn continuous_center_ordered_spot(&mut self, order_index: u64) -> Option<BoardSpot> {
        let order_index = index_from_u64(order_index)?;
        while self.center_ordered_spots.len() <= order_index {
            if !self.advance_continuous_center_order() {
                return None;
            }
        }

        self.center_ordered_spots.get(order_index).cloned()
    }

    fn advance_continuous_center_order(&mut self) -> bool {
        loop {
            if self.center_queue.is_empty() && !self.seed_continuous_center_frontier() {
                return false;
            }

            if let Some(entry) = self.center_queue.pop() {
                if let Some(stream_index) = entry.continuous_stream {
                    self.push_next_continuous_center_stream_spot(stream_index);
                }
                self.center_ordered_spots.push(entry.spot);
                return true;
            }
        }
    }

    fn seed_continuous_center_frontier(&mut self) -> bool {
        if self.center_shells_exhausted {
            return false;
        }

        for stream_index in 0..self.continuous_center_spirals.len() {
            self.push_next_continuous_center_stream_spot(stream_index);
        }

        !self.center_queue.is_empty()
    }

    fn push_next_continuous_center_stream_spot(&mut self, stream_index: usize) -> bool {
        if self
            .continuous_center_streams_exhausted
            .get(stream_index)
            .copied()
            .unwrap_or(true)
        {
            return false;
        }

        let Some(raw_spot) = self.continuous_center_spirals[stream_index].next() else {
            self.mark_continuous_center_stream_exhausted(stream_index);
            return false;
        };
        let spot_index = continuous_center_spot_index(stream_index, raw_spot.index);
        let spot = BoardSpot::Continuous {
            index: spot_index,
            theta: raw_spot.theta,
            center: raw_spot.center,
        };

        if !self.spot_within_generation_radius(&spot) {
            self.mark_continuous_center_stream_exhausted(stream_index);
            return false;
        }

        self.stats.current_radius = self.stats.current_radius.max(spot.generation_radius());
        self.center_queue.push(CenterQueueEntry {
            distance_squared: spot.center_distance_squared(),
            spot_index,
            continuous_stream: Some(stream_index),
            spot,
        });
        true
    }

    fn mark_continuous_center_stream_exhausted(&mut self, stream_index: usize) {
        if let Some(exhausted) = self
            .continuous_center_streams_exhausted
            .get_mut(stream_index)
        {
            *exhausted = true;
        }

        if self
            .continuous_center_streams_exhausted
            .iter()
            .all(|exhausted| *exhausted)
        {
            self.center_shells_exhausted = true;
            self.stats.current_radius =
                self.stats.current_radius.max(self.settings.radius.max(0.0));
        }
    }

    fn advance_center_order(&mut self) -> bool {
        loop {
            if self.center_queue.is_empty() {
                if !self.push_next_center_shell() {
                    return false;
                }
                continue;
            }

            while !self.center_shells_exhausted {
                let Some(peek) = self.center_queue.peek() else {
                    break;
                };
                let next_min = min_center_distance_squared_for_shell(
                    self.settings.board,
                    self.center_next_shell,
                );
                if next_min > peek.distance_squared {
                    break;
                }
                if !self.push_next_center_shell() {
                    break;
                }
            }

            if let Some(entry) = self.center_queue.pop() {
                self.center_ordered_spots.push(entry.spot);
                return true;
            }
        }
    }

    fn push_next_center_shell(&mut self) -> bool {
        if self.center_shells_exhausted {
            return false;
        }

        let bound = self.settings.radius.max(0.0).floor() as u64;
        if self.center_next_shell > bound {
            self.center_shells_exhausted = true;
            self.stats.current_radius =
                self.stats.current_radius.max(self.settings.radius.max(0.0));
            return false;
        }

        let shell = self.center_next_shell;
        self.center_next_shell += 1;
        for spot in center_shell_spots(self.settings.board, shell) {
            self.center_queue.push(CenterQueueEntry {
                distance_squared: spot.center_distance_squared(),
                spot_index: spot.index(),
                continuous_stream: None,
                spot,
            });
        }
        self.stats.current_radius = self.stats.current_radius.max(shell as f64);
        if shell == bound {
            self.center_shells_exhausted = true;
            self.stats.current_radius =
                self.stats.current_radius.max(self.settings.radius.max(0.0));
        }
        true
    }

    fn spot_at(&mut self, index: usize) -> Option<BoardSpot> {
        if self.settings.board != BoardKind::ContinuousArchimedean {
            return self.lattice_spiral_path_spot_at(index);
        }

        if self.spots_exhausted && self.spots.len() <= index {
            return None;
        }

        while self.spots.len() <= index {
            let next_index = self.spots.len() as u64;
            let spot = if let Some(spot) = self.first_out_of_radius_spot.take() {
                spot
            } else {
                let spot = self.continuous_spiral.next()?;
                BoardSpot::Continuous {
                    index: next_index,
                    theta: spot.theta,
                    center: spot.center,
                }
            };

            if !self.spot_within_generation_radius(&spot) {
                self.first_out_of_radius_spot = Some(spot);
                self.stats.current_radius =
                    self.stats.current_radius.max(self.settings.radius.max(0.0));
                self.spots_exhausted = true;
                return None;
            }

            self.stats.current_radius = self.stats.current_radius.max(spot.generation_radius());
            self.spots.push(spot);
        }

        self.spots.get(index).cloned()
    }

    fn lattice_spiral_path_spot_at(&mut self, index: usize) -> Option<BoardSpot> {
        let index = index as u64;
        if self
            .first_out_of_radius_spot
            .as_ref()
            .is_some_and(|spot| index >= spot.index())
            && self.spots_exhausted
        {
            return None;
        }

        let spot = match self.settings.board {
            BoardKind::LatticeSquare => BoardSpot::Square {
                index,
                coord: SquareSpiral::coord_at_index(index),
            },
            BoardKind::LatticeHex => BoardSpot::Hex {
                index,
                coord: HexSpiral::coord_at_index(index),
            },
            BoardKind::LatticeTriangle => BoardSpot::Triangle {
                index,
                coord: TriangleSpiral::coord_at_index(index),
                spiral_radius: TriangleSpiral::radius_for_index(index),
            },
            BoardKind::ContinuousArchimedean => return None,
        };

        if !self.spot_within_generation_radius(&spot) {
            self.first_out_of_radius_spot = Some(spot);
            self.stats.current_radius =
                self.stats.current_radius.max(self.settings.radius.max(0.0));
            self.spots_exhausted = true;
            return None;
        }

        self.stats.current_radius = self.stats.current_radius.max(spot.generation_radius());
        Some(spot)
    }

    fn spot_within_generation_radius(&self, spot: &BoardSpot) -> bool {
        let radius = self.settings.radius.max(0.0);
        match spot {
            BoardSpot::Square { coord, .. } => {
                let bound = radius.floor() as i64;
                coord.x.abs().max(coord.y.abs()) <= bound
            }
            BoardSpot::Hex { coord, .. } => {
                let bound = radius.floor() as i64;
                let (x, y, z) = coord.cube();
                x.abs().max(y.abs()).max(z.abs()) <= bound
            }
            BoardSpot::Triangle { spiral_radius, .. } => *spiral_radius <= radius.floor() as u64,
            BoardSpot::Continuous { center, .. } => center.radius() <= radius + 1.0e-9,
        }
    }

    fn reopen_spiral_path_radius_bound(&mut self) {
        if self.settings.placement_search != PlacementSearchMode::SpiralPath {
            return;
        }

        let Some(spot) = self.first_out_of_radius_spot.as_ref() else {
            return;
        };

        if self.spot_within_generation_radius(spot) {
            self.spots_exhausted = false;
            self.stats.exhausted = false;
        }
    }

    fn prime(&mut self, index: usize) -> u32 {
        let mut candidate = if let Some(last) = self.prime_cache.last() {
            last + if *last == 2 { 1 } else { 2 }
        } else {
            2
        };

        while self.prime_cache.len() <= index {
            if is_prime_with_cache(candidate, &self.prime_cache) {
                self.prime_cache.push(candidate);
            }
            candidate += if candidate == 2 { 1 } else { 2 };
        }

        self.prime_cache[index]
    }
}

pub(crate) fn lattice_attack_targets(
    board: BoardKind,
    origin: (i64, i64),
    piece: PieceSignature,
) -> Vec<(i64, i64)> {
    match board {
        BoardKind::LatticeSquare => square_attack_offsets(piece)
            .into_iter()
            .map(|(dx, dy)| (origin.0 + dx, origin.1 + dy))
            .collect(),
        BoardKind::LatticeHex => hex_attack_offsets(piece)
            .into_iter()
            .map(|offset| (origin.0 + offset.q, origin.1 + offset.r))
            .collect(),
        BoardKind::LatticeTriangle => triangle_attack_offsets(piece)
            .into_iter()
            .map(|offset| (origin.0 + offset.u, origin.1 + offset.v))
            .collect(),
        BoardKind::ContinuousArchimedean => Vec::new(),
    }
}

fn center_shell_spots(board: BoardKind, shell: u64) -> Vec<BoardSpot> {
    match board {
        BoardKind::LatticeSquare => center_square_shell_spots(shell),
        BoardKind::LatticeHex => center_hex_shell_spots(shell),
        BoardKind::LatticeTriangle => center_triangle_shell_spots(shell),
        BoardKind::ContinuousArchimedean => Vec::new(),
    }
}

fn continuous_center_spirals(settings: &EngineSettings) -> Vec<ArchimedeanSpots> {
    (0..CONTINUOUS_CENTER_STREAMS)
        .map(|stream_index| {
            ArchimedeanSpiral::spots(continuous_center_stream_offset(
                settings.continuous_offset,
                stream_index,
            ))
        })
        .collect()
}

fn continuous_center_stream_offset(base_offset: f64, stream_index: usize) -> f64 {
    let base_offset = base_offset.clamp(0.0, 1.0);
    if stream_index == 0 {
        return base_offset;
    }

    (base_offset + stream_index as f64 / CONTINUOUS_CENTER_STREAMS as f64).rem_euclid(1.0)
}

fn continuous_center_spot_index(stream_index: usize, local_index: u64) -> u64 {
    local_index
        .saturating_mul(CONTINUOUS_CENTER_STREAMS as u64)
        .saturating_add(stream_index as u64)
}

fn center_square_shell_spots(shell: u64) -> Vec<BoardSpot> {
    if shell == 0 {
        return vec![BoardSpot::Square {
            index: 0,
            coord: SquareCoord::new(0, 0),
        }];
    }

    let start = shell.saturating_mul(2).saturating_sub(1).saturating_pow(2);
    let end = shell
        .saturating_mul(2)
        .saturating_add(1)
        .saturating_pow(2)
        .saturating_sub(1);
    (start..=end)
        .map(|index| BoardSpot::Square {
            index,
            coord: SquareSpiral::coord_at_index(index),
        })
        .collect()
}

fn center_hex_shell_spots(shell: u64) -> Vec<BoardSpot> {
    if shell == 0 {
        return vec![BoardSpot::Hex {
            index: 0,
            coord: AxialCoord::new(0, 0),
        }];
    }

    let start = hex_ring_max_index(shell - 1).saturating_add(1);
    let end = hex_ring_max_index(shell);
    (start..=end)
        .map(|index| BoardSpot::Hex {
            index,
            coord: HexSpiral::coord_at_index(index),
        })
        .collect()
}

fn center_triangle_shell_spots(shell: u64) -> Vec<BoardSpot> {
    if shell == 0 {
        return vec![BoardSpot::Triangle {
            index: 0,
            coord: TriangleCoord::new(0, 0),
            spiral_radius: 0,
        }];
    }

    let start = triangular_number(3_u64.saturating_mul(shell - 1)).saturating_add(1);
    let end = triangular_number(3_u64.saturating_mul(shell));
    (start..=end)
        .map(|index| BoardSpot::Triangle {
            index,
            coord: TriangleSpiral::coord_at_index(index),
            spiral_radius: shell,
        })
        .collect()
}

fn min_center_distance_squared_for_shell(board: BoardKind, shell: u64) -> f64 {
    match board {
        BoardKind::LatticeSquare => (shell as f64).powi(2),
        BoardKind::LatticeHex => min_hex_center_distance_squared_for_shell(shell),
        BoardKind::LatticeTriangle => min_triangle_center_distance_squared_for_shell(shell),
        BoardKind::ContinuousArchimedean => 0.0,
    }
}

fn min_hex_center_distance_squared_for_shell(shell: u64) -> f64 {
    let shell = shell as f64;
    let leg = (shell / 2.0).floor();
    3.0 * (shell * shell - shell * leg + leg * leg)
}

fn min_triangle_center_distance_squared_for_shell(shell: u64) -> f64 {
    if shell == 0 {
        return 0.0;
    }
    if shell == 1 {
        return 1.0;
    }

    let n = shell - 1;
    let u = (n / 2) as f64;
    let v = -(n as f64);
    u.mul_add(u, u * v + v * v)
}

fn triangular_number(n: u64) -> u64 {
    n.saturating_mul(n.saturating_add(1)) / 2
}

fn hex_ring_max_index(radius: u64) -> u64 {
    3_u64
        .saturating_mul(radius)
        .saturating_mul(radius.saturating_add(1))
}

fn square_attack_offsets(piece: PieceSignature) -> Vec<(i64, i64)> {
    let a = piece.a.unsigned_abs() as i64;
    let b = piece.b.unsigned_abs() as i64;
    let mut offsets = FxHashSet::default();

    for (x, y) in [(a, b), (b, a)] {
        for sx in [-1_i64, 1] {
            for sy in [-1_i64, 1] {
                offsets.insert((sx * x, sy * y));
            }
        }
    }

    offsets.remove(&(0, 0));
    offsets.into_iter().collect()
}

fn hex_attack_offsets(piece: PieceSignature) -> Vec<AxialCoord> {
    let mut out = FxHashSet::default();
    let a = piece.a.unsigned_abs() as i64;
    let b = piece.b.unsigned_abs() as i64;
    let bases = if a == b {
        vec![AxialCoord::new(a, b)]
    } else {
        vec![AxialCoord::new(a, b), AxialCoord::new(b, a)]
    };

    for base in bases {
        let mut cube = base.cube();
        for _ in 0..6 {
            out.insert(AxialCoord::new(cube.0, cube.1));
            cube = rotate_cube_right(cube);
        }
    }

    out.remove(&AxialCoord::new(0, 0));
    out.into_iter().collect()
}

fn triangle_attack_offsets(piece: PieceSignature) -> Vec<TriangleCoord> {
    const DIRECTIONS: [TriangleCoord; 6] = [
        TriangleCoord { u: 1, v: 0 },
        TriangleCoord { u: 0, v: 1 },
        TriangleCoord { u: -1, v: 1 },
        TriangleCoord { u: -1, v: 0 },
        TriangleCoord { u: 0, v: -1 },
        TriangleCoord { u: 1, v: -1 },
    ];
    const PRIMARY_AXES: [usize; 3] = [0, 2, 4];

    let a = piece.a.unsigned_abs() as i64;
    let b = piece.b.unsigned_abs() as i64;
    let mut out = FxHashSet::default();

    for axis in PRIMARY_AXES {
        let primary = DIRECTIONS[axis];
        if b == 0 {
            out.insert(primary.scale(a));
            continue;
        }

        let (base, left, right) = if a % 2 == 0 {
            (
                primary.scale(a),
                DIRECTIONS[(axis + 1) % DIRECTIONS.len()].scale(b),
                DIRECTIONS[(axis + DIRECTIONS.len() - 1) % DIRECTIONS.len()].scale(b),
            )
        } else {
            (
                primary.scale(a.saturating_sub(1)),
                primary.scale(b),
                DIRECTIONS[(axis + 1) % DIRECTIONS.len()].scale(b),
            )
        };
        out.insert(base.add(left));
        out.insert(base.add(right));
    }

    out.remove(&TriangleCoord::new(0, 0));
    out.into_iter().collect()
}

fn rotate_cube_right((x, y, z): (i64, i64, i64)) -> (i64, i64, i64) {
    (-z, -x, -y)
}

fn move_group(board: BoardKind, piece: PieceSignature) -> (u32, u32) {
    move_key_for_board(board, piece)
}

fn forced_shape_for_board(board: BoardKind, requested: ShapeKind) -> ShapeKind {
    match board {
        BoardKind::ContinuousArchimedean => ShapeKind::Circle,
        BoardKind::LatticeTriangle => match requested {
            ShapeKind::Circle => ShapeKind::Circle,
            _ => ShapeKind::Triangle,
        },
        BoardKind::LatticeSquare | BoardKind::LatticeHex => requested,
    }
}

fn signature_gap(piece: PieceSignature) -> u32 {
    piece.a.abs_diff(piece.b)
}

fn index_from_u64(index: u64) -> Option<usize> {
    usize::try_from(index).ok()
}

fn continuous_piece_radius_changes_simulation(
    current: &EngineSettings,
    next: &EngineSettings,
) -> bool {
    (current.board == BoardKind::ContinuousArchimedean
        || next.board == BoardKind::ContinuousArchimedean)
        && current.piece_radius != next.piece_radius
}

pub(crate) fn max_relevant_attack_radius(settings: &EngineSettings) -> f64 {
    ATTACK_RELEVANCE_RADIUS_MULTIPLIER * settings.radius.max(0.0)
}

pub(crate) fn attack_radius_relevant_to_generation(
    settings: &EngineSettings,
    attack_radius: f64,
) -> bool {
    attack_radius <= max_relevant_attack_radius(settings) + f64::EPSILON
}

fn continuous_bodies_overlap_for_placement_rejection(
    a: Point2,
    b: Point2,
    body_radius: f64,
) -> bool {
    let minimum_distance = (2.0 * body_radius.max(0.0)).max(0.0);
    let tolerance = (minimum_distance * CONTINUOUS_PLACEMENT_OVERLAP_TOLERANCE_FRACTION)
        .max(CONTINUOUS_PLACEMENT_OVERLAP_TOLERANCE_ABSOLUTE);
    a.distance(b) < minimum_distance - tolerance - GEOM_EPS
}

fn prime_knight_color_bucket(value: u32, divisor: u32) -> (u32, f64) {
    let divisor = normalize_prime_modulo_divisor(divisor);
    let half = (divisor / 2).max(1);
    let rem = value % divisor;
    let bucket = rem.min(divisor - rem);
    let bucket = if bucket == half { 0 } else { bucket };
    let t = if bucket == 0 {
        0.0
    } else {
        (bucket as f64 / half as f64).clamp(0.0, 1.0)
    };

    (bucket, t)
}

fn is_prime_with_cache(n: u32, primes: &[u32]) -> bool {
    if n < 2 {
        return false;
    }

    if n == 2 {
        return true;
    }

    if n.is_multiple_of(2) {
        return false;
    }

    for &factor in primes {
        if factor == 2 {
            continue;
        }

        if (factor as u64) * (factor as u64) > n as u64 {
            return true;
        }

        if n.is_multiple_of(factor) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{attack_circle_hits_body, bodies_overlap};

    #[test]
    fn engine_emits_square_batch() {
        let mut engine = SimulationEngine::new(EngineSettings::default());
        let batch = engine.step_batch(64);
        assert_eq!(batch.len(), 64);
        assert_eq!(batch[0].spot_index, 0);
        assert_eq!(engine.stats().placements, 64);
    }

    #[test]
    fn custom_finite_counts_spots_not_prime_candidates() {
        let mut engine = SimulationEngine::new(EngineSettings::default());
        let batch = engine.step_batch(24);
        let stats = engine.stats();

        assert_eq!(batch.len(), 24);
        assert_eq!(stats.piece_candidates_tested, 0);
        assert!(stats.spots_tested >= stats.placements);
    }

    #[test]
    fn empty_custom_army_is_a_valid_empty_state() {
        let settings = EngineSettings {
            custom_army: Vec::new(),
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_batch(8);

        assert!(batch.is_empty());
        assert!(engine.stats().exhausted);
    }

    #[test]
    fn radius_limits_square_generation() {
        let settings = EngineSettings {
            radius: 1.0,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(20, 10_000);

        assert_eq!(batch.len(), 9);
        assert!(engine.stats().exhausted);
        assert!(batch.iter().all(|placement| match placement.coord {
            SpotCoord::Square { x, y } => x.abs().max(y.abs()) <= 1,
            _ => false,
        }));
    }

    #[test]
    fn increasing_spiral_path_radius_resumes_from_first_previous_outside_spot() {
        let mut settings = EngineSettings {
            radius: 1.0,
            placement_search: PlacementSearchMode::SpiralPath,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings.clone());
        let first = engine.step_budget(20, 10_000);

        assert_eq!(first.len(), 9);
        assert!(engine.stats().exhausted);

        settings.radius = 2.0;
        assert!(!engine.update_settings(settings));
        assert!(!engine.stats().exhausted);
        let resumed = engine.step_budget(1, 10_000);

        assert_eq!(resumed[0].spot_index, 9);
        assert_eq!(resumed[0].coord, SpotCoord::Square { x: 2, y: -1 });
    }

    #[test]
    fn decreasing_radius_still_resets_generation() {
        let mut settings = EngineSettings {
            radius: 2.0,
            placement_search: PlacementSearchMode::SpiralPath,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings.clone());
        let first = engine.step_budget(10, 10_000);

        assert_eq!(first.last().map(|placement| placement.spot_index), Some(9));

        settings.radius = 1.0;
        assert!(engine.update_settings(settings));
        let restarted = engine.step_budget(1, 10_000);

        assert_eq!(restarted[0].spot_index, 0);
    }

    #[test]
    fn radius_limits_hex_generation() {
        let settings = EngineSettings {
            board: BoardKind::LatticeHex,
            radius: 1.0,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(20, 10_000);

        assert_eq!(batch.len(), 7);
        assert!(engine.stats().exhausted);
        assert!(batch.iter().all(|placement| match placement.coord {
            SpotCoord::Hex { q, r } => {
                let cube = AxialCoord::new(q, r).cube();
                cube.0.abs().max(cube.1.abs()).max(cube.2.abs()) <= 1
            }
            _ => false,
        }));
    }

    #[test]
    fn radius_limits_triangle_generation() {
        let settings = EngineSettings {
            board: BoardKind::LatticeTriangle,
            shape: ShapeKind::Triangle,
            radius: 1.0,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(20, 10_000);

        assert_eq!(batch.len(), 7);
        assert!(engine.stats().exhausted);
        assert!(batch.iter().all(|placement| placement.spot_index <= 6
            && matches!(placement.coord, SpotCoord::Triangle { .. })));
    }

    #[test]
    fn radius_limits_continuous_generation() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            radius: 1.0,
            piece_radius: 0.50,
            custom_army: vec![CustomPiece::with_auto_color(100, 0)],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(20, 10_000);

        assert_eq!(batch.len(), 2);
        assert!(engine.stats().exhausted);
        assert!(batch.iter().all(|placement| match placement.coord {
            SpotCoord::Continuous { x, y, .. } => Point2::new(x, y).radius() <= 1.0 + 1.0e-9,
            _ => false,
        }));
    }

    #[test]
    fn generation_bounds_exhaust_across_boards_presets_enemy_modes_and_search_modes() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
            BoardKind::ContinuousArchimedean,
        ] {
            for placement_search in [
                PlacementSearchMode::SpiralPath,
                PlacementSearchMode::CenterDistance,
            ] {
                for army_preset in [
                    ArmyPreset::CustomFinite,
                    ArmyPreset::PrimeKnight,
                    ArmyPreset::PrimeGapper,
                ] {
                    for enemy_mode in [
                        EnemyMode::AttackSet,
                        EnemyMode::Color,
                        EnemyMode::ColorAttackSet,
                    ] {
                        let radius = if board == BoardKind::ContinuousArchimedean {
                            1.5
                        } else {
                            1.0
                        };
                        let mut engine = SimulationEngine::new(EngineSettings {
                            board,
                            shape: if board == BoardKind::LatticeTriangle {
                                ShapeKind::Triangle
                            } else if board == BoardKind::ContinuousArchimedean {
                                ShapeKind::Circle
                            } else {
                                ShapeKind::Square
                            },
                            radius,
                            piece_radius: 0.50,
                            placement_search,
                            army_preset,
                            enemy_mode,
                            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
                            ..EngineSettings::default()
                        });
                        let mut placements = Vec::new();
                        for _ in 0..128 {
                            let batch = engine.step_budget(128, 2_000_000);
                            placements.extend(batch);
                            if engine.stats().exhausted {
                                break;
                            }
                        }

                        assert!(
                            engine.stats().exhausted,
                            "board={board:?}, search={placement_search:?}, preset={army_preset:?}, enemy={enemy_mode:?}"
                        );
                        assert!(
                            placements
                                .iter()
                                .all(|placement| placement_within_radius(placement, board, radius)),
                            "board={board:?}, search={placement_search:?}, preset={army_preset:?}, enemy={enemy_mode:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn custom_colors_are_order_based_not_gap_based() {
        let settings = EngineSettings {
            custom_army: vec![
                CustomPiece::new(2, 4, "#000000"),
                CustomPiece::new(1, 3, "#ffffff"),
            ],
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_batch(2);

        assert_eq!(batch[0].color.rule, ColorRule::OrderRainbow);
        assert_eq!(batch[1].color.rule, ColorRule::OrderRainbow);
        assert_eq!(batch[0].color.key.group, 0);
        assert_eq!(batch[1].color.key.group, 1);
        assert_eq!(batch[0].color.key.gradient_value, 0.0);
        assert_eq!(batch[1].color.key.gradient_value, 1.0);
    }

    #[test]
    fn custom_color_overrides_do_not_change_automatic_order_groups() {
        let mut engine = SimulationEngine::new(EngineSettings {
            enemy_mode: EnemyMode::AttackSet,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(3, 1),
            ],
            ..EngineSettings::default()
        });
        let first = engine.step_batch(4);
        let mut next_settings = engine.settings().clone();
        next_settings.custom_army[1].color_override = Some("#55a7ff".to_string());

        assert!(!engine.update_settings(next_settings));
        assert_eq!(engine.stats().placements, first.len() as u64);

        let next = engine.step_batch(2);
        assert_eq!(next[0].color.rule, ColorRule::OrderRainbow);
        assert_eq!(next[0].color.key.group, 0);
        assert_eq!(next[1].color.rule, ColorRule::OrderRainbow);
        assert_eq!(next[1].color.key.group, 1);
    }

    #[test]
    fn custom_color_override_changes_color_enemy_identity() {
        for enemy_mode in [EnemyMode::Color, EnemyMode::ColorAttackSet] {
            let settings = EngineSettings {
                board: BoardKind::LatticeSquare,
                placement_search: PlacementSearchMode::SpiralPath,
                enemy_mode,
                custom_army: vec![
                    CustomPiece::with_auto_color(2, 1),
                    CustomPiece::with_color_override(2, 1, "#ff7800"),
                ],
                ..EngineSettings::default()
            };
            let enemy_groups = custom_army_effective_color_groups(&settings);
            assert_eq!(enemy_groups, vec![0, 0]);

            let mut engine = SimulationEngine::new(settings);
            let batch = engine.step_budget(24, 1_000_000);
            let mut earlier: Vec<&Placement> = Vec::new();
            let mut allowed_copied_color_attack = false;

            for placement in &batch {
                for attacker in &earlier {
                    let automatic_groups_differ =
                        attacker.color.key.group != placement.color.key.group;
                    if automatic_groups_differ
                        && move_group(BoardKind::LatticeSquare, attacker.piece)
                            == move_group(BoardKind::LatticeSquare, placement.piece)
                        && placement_attacks(attacker, placement, BoardKind::LatticeSquare, 0.5)
                    {
                        allowed_copied_color_attack = true;
                    }
                }
                earlier.push(placement);
            }

            assert!(
                allowed_copied_color_attack,
                "enemy_mode={enemy_mode:?} did not allow same visible-color allies"
            );
        }
    }

    #[test]
    fn custom_color_override_identity_change_requires_reset_in_color_modes() {
        let mut engine = SimulationEngine::new(EngineSettings {
            enemy_mode: EnemyMode::Color,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(2, 1),
            ],
            ..EngineSettings::default()
        });
        let first = engine.step_batch(4);
        let mut next_settings = engine.settings().clone();
        next_settings.custom_army[1].color_override = Some("#ff7800".to_string());

        assert!(engine.update_settings(next_settings));
        assert_eq!(engine.stats().placements, 0);

        let next = engine.step_batch(2);
        assert_eq!(first[0].color.key.group, next[0].color.key.group);
        assert_eq!(next[0].color.key.group, 0);
        assert_eq!(next[1].color.key.group, 1);
    }

    #[test]
    fn continuous_placement_tolerance_does_not_change_strict_body_geometry() {
        let origin = Point2::new(0.0, 0.0);
        let placement_tolerated_overlap = Point2::new(1.0 - 0.004, 0.0);
        let visible_overlap = Point2::new(1.0 - 0.01, 0.0);

        assert!(bodies_overlap(origin, placement_tolerated_overlap, 0.5));
        assert!(!continuous_bodies_overlap_for_placement_rejection(
            origin,
            placement_tolerated_overlap,
            0.5
        ));
        assert!(continuous_bodies_overlap_for_placement_rejection(
            origin,
            visible_overlap,
            0.5
        ));
    }

    #[test]
    fn custom_color_mode_rejects_enemy_attacks_but_allows_ally_attacks() {
        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeSquare,
            placement_search: PlacementSearchMode::SpiralPath,
            enemy_mode: EnemyMode::Color,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(2, 1),
            ],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(64, 1_000_000);

        let mut earlier: Vec<&Placement> = Vec::new();
        let mut allowed_on_ally_attacked_spot = false;
        for placement in &batch {
            let SpotCoord::Square { x, y } = placement.coord else {
                panic!("expected square placement");
            };
            for attacker in &earlier {
                let SpotCoord::Square { x: ax, y: ay } = attacker.coord else {
                    panic!("expected square placement");
                };
                let attacked = square_attack_offsets(attacker.piece)
                    .into_iter()
                    .any(|(dx, dy)| ax + dx == x && ay + dy == y);
                if attacked && attacker.color.key.group == placement.color.key.group {
                    allowed_on_ally_attacked_spot = true;
                }
                assert!(
                    !attacked || attacker.color.key.group == placement.color.key.group,
                    "spot {} at ({x},{y}) was attacked by enemy color group {} from spot {}",
                    placement.spot_index,
                    attacker.color.key.group,
                    attacker.spot_index
                );
            }
            earlier.push(placement);
        }

        assert!(
            allowed_on_ally_attacked_spot,
            "test did not exercise an own-color attacked placement"
        );
    }

    #[test]
    fn custom_two_color_square_knights_keep_per_row_forward_cursors() {
        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeSquare,
            placement_search: PlacementSearchMode::SpiralPath,
            enemy_mode: EnemyMode::Color,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(2, 1),
            ],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(96, 2_000_000);
        assert_eq!(batch.len(), 96);

        let chronological = batch
            .iter()
            .map(|placement| placement.spot_index)
            .collect::<Vec<_>>();

        assert_eq!(&chronological[..8], &[0, 1, 2, 3, 5, 4, 9, 6]);
        assert!(
            chronological.windows(2).any(|pair| pair[1] < pair[0]),
            "per-row cursors should allow later rows to place behind the previous row"
        );
        for color_group in [0, 1] {
            let row_indices = batch
                .iter()
                .filter(|placement| placement.color.key.group == color_group)
                .map(|placement| placement.spot_index)
                .collect::<Vec<_>>();
            assert!(
                row_indices.windows(2).all(|pair| pair[1] > pair[0]),
                "color_group={color_group}, row_indices={row_indices:?}"
            );
        }
        assert!(engine.stats().passive_rejections > 0);
    }

    #[test]
    fn custom_enemy_modes_never_accept_enemy_attacked_spots_on_any_board() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
            BoardKind::ContinuousArchimedean,
        ] {
            for (enemy_mode, custom_army) in [
                (
                    EnemyMode::Color,
                    vec![
                        CustomPiece::with_auto_color(2, 1),
                        CustomPiece::with_auto_color(2, 1),
                    ],
                ),
                (
                    EnemyMode::AttackSet,
                    vec![
                        CustomPiece::with_auto_color(2, 1),
                        CustomPiece::with_auto_color(3, 1),
                    ],
                ),
                (
                    EnemyMode::ColorAttackSet,
                    vec![
                        CustomPiece::with_auto_color(2, 1),
                        CustomPiece::with_auto_color(3, 1),
                    ],
                ),
            ] {
                let mut engine = SimulationEngine::new(EngineSettings {
                    board,
                    shape: if board == BoardKind::LatticeTriangle {
                        ShapeKind::Triangle
                    } else if board == BoardKind::ContinuousArchimedean {
                        ShapeKind::Circle
                    } else {
                        ShapeKind::Square
                    },
                    placement_search: PlacementSearchMode::SpiralPath,
                    enemy_mode,
                    piece_radius: 0.5,
                    custom_army,
                    ..EngineSettings::default()
                });
                let batch = engine.step_budget(48, 2_000_000);
                assert_eq!(batch.len(), 48, "board={board:?}, mode={enemy_mode:?}");

                let mut earlier: Vec<&Placement> = Vec::new();
                for placement in &batch {
                    for attacker in &earlier {
                        if !placements_are_enemies(board, enemy_mode, attacker, placement) {
                            continue;
                        }
                        assert!(
                            !placement_attacks(attacker, placement, board, 0.5),
                            "board={board:?}, mode={enemy_mode:?}, spot {} was attacked by enemy spot {}",
                            placement.spot_index,
                            attacker.spot_index
                        );
                    }
                    earlier.push(placement);
                }
            }
        }
    }

    #[test]
    fn custom_spiral_path_alternates_rows_while_each_row_searches_forward() {
        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeSquare,
            placement_search: PlacementSearchMode::SpiralPath,
            enemy_mode: EnemyMode::Color,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(2, 1),
            ],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(8, 1_000_000);

        let indices: Vec<_> = batch.iter().map(|placement| placement.spot_index).collect();
        let color_groups: Vec<_> = batch
            .iter()
            .map(|placement| placement.color.key.group)
            .collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 5, 4, 9, 6]);
        assert_eq!(color_groups, vec![0, 1, 0, 1, 0, 1, 0, 1]);
        for color_group in [0, 1] {
            let row_indices = batch
                .iter()
                .filter(|placement| placement.color.key.group == color_group)
                .map(|placement| placement.spot_index)
                .collect::<Vec<_>>();
            assert!(
                row_indices.windows(2).all(|pair| pair[1] > pair[0]),
                "color_group={color_group}, row_indices={row_indices:?}"
            );
        }
    }

    #[test]
    fn color_attack_set_requires_matching_color_and_attack_set() {
        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeSquare,
            placement_search: PlacementSearchMode::SpiralPath,
            enemy_mode: EnemyMode::ColorAttackSet,
            custom_army: vec![
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(2, 1),
                CustomPiece::with_auto_color(3, 1),
            ],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(24, 2_000_000);
        assert_eq!(batch.len(), 24);

        let mut earlier: Vec<&Placement> = Vec::new();
        for placement in &batch {
            for attacker in &earlier {
                if placement_attacks(attacker, placement, BoardKind::LatticeSquare, 0.5) {
                    assert_eq!(
                        attacker.color.key.group, placement.color.key.group,
                        "spot {} was attacked by a different color group",
                        placement.spot_index
                    );
                    assert_eq!(
                        move_group(BoardKind::LatticeSquare, attacker.piece),
                        move_group(BoardKind::LatticeSquare, placement.piece),
                        "spot {} was attacked by a different attack set",
                        placement.spot_index
                    );
                }
            }
            earlier.push(placement);
        }
    }

    #[test]
    fn proactive_rule_rejects_candidate_that_attacks_enemy() {
        let mut passive = EngineSettings {
            enemy_mode: EnemyMode::AttackSet,
            custom_army: vec![
                CustomPiece::with_auto_color(3, 0),
                CustomPiece::with_auto_color(1, 0),
            ],
            ..EngineSettings::default()
        };
        passive.proactive_attacking = false;
        let mut active = passive.clone();
        active.proactive_attacking = true;

        let mut passive_engine = SimulationEngine::new(passive);
        let mut active_engine = SimulationEngine::new(active);
        let passive_batch = passive_engine.step_batch(2);
        let active_batch = active_engine.step_batch(2);

        assert_eq!(passive_batch[1].spot_index, 1);
        assert_eq!(passive_batch[1].piece, PieceSignature::new(1, 0));
        assert!(active_batch[1].spot_index > passive_batch[1].spot_index);
        assert_eq!(active_batch[1].piece, PieceSignature::new(1, 0));
        assert!(active_engine.stats().proactive_rejections > 0);
    }

    #[test]
    fn proactive_rule_changes_lattice_hex_placements() {
        let mut passive = EngineSettings {
            board: BoardKind::LatticeHex,
            enemy_mode: EnemyMode::AttackSet,
            custom_army: vec![
                CustomPiece::with_auto_color(3, 0),
                CustomPiece::with_auto_color(1, 0),
            ],
            ..EngineSettings::default()
        };
        passive.proactive_attacking = false;
        let mut active = passive.clone();
        active.proactive_attacking = true;

        let mut passive_engine = SimulationEngine::new(passive);
        let mut active_engine = SimulationEngine::new(active);
        let passive_batch = passive_engine.step_batch(2);
        let active_batch = active_engine.step_batch(2);

        assert_eq!(passive_batch[1].spot_index, 1);
        assert_eq!(passive_batch[1].piece, PieceSignature::new(1, 0));
        assert!(active_batch[1].spot_index > passive_batch[1].spot_index);
        assert_eq!(active_batch[1].piece, PieceSignature::new(1, 0));
        assert!(active_engine.stats().proactive_rejections > 0);
    }

    #[test]
    fn proactive_rule_changes_continuous_placements() {
        let mut passive = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            piece_radius: 0.50,
            enemy_mode: EnemyMode::AttackSet,
            custom_army: vec![
                CustomPiece::with_auto_color(3, 0),
                CustomPiece::with_auto_color(1, 0),
            ],
            ..EngineSettings::default()
        };
        passive.proactive_attacking = false;
        let mut active = passive.clone();
        active.proactive_attacking = true;

        let mut passive_engine = SimulationEngine::new(passive);
        let mut active_engine = SimulationEngine::new(active);
        let passive_batch = passive_engine.step_batch(2);
        let active_batch = active_engine.step_batch(2);

        assert_eq!(passive_batch[1].spot_index, 1);
        assert_eq!(passive_batch[1].piece, PieceSignature::new(1, 0));
        assert!(active_batch[1].spot_index > passive_batch[1].spot_index);
        assert_eq!(active_batch[1].piece, PieceSignature::new(1, 0));
        assert!(active_engine.stats().proactive_rejections > 0);
    }

    #[test]
    fn spiral_path_custom_mode_keeps_each_army_entry_moving_forward() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
            BoardKind::ContinuousArchimedean,
        ] {
            let mut engine = SimulationEngine::new(EngineSettings {
                board,
                shape: if board == BoardKind::LatticeTriangle {
                    ShapeKind::Triangle
                } else if board == BoardKind::ContinuousArchimedean {
                    ShapeKind::Circle
                } else {
                    ShapeKind::Square
                },
                placement_search: PlacementSearchMode::SpiralPath,
                custom_army: vec![
                    CustomPiece::with_auto_color(3, 0),
                    CustomPiece::with_auto_color(1, 0),
                ],
                ..EngineSettings::default()
            });
            let batch = engine.step_budget(40, 2_000_000);
            assert!(batch.len() >= 20, "board={board:?}");
            for color_group in [0, 1] {
                let indices = batch
                    .iter()
                    .filter(|placement| placement.color.key.group == color_group)
                    .map(|placement| placement.spot_index)
                    .collect::<Vec<_>>();
                assert!(
                    indices.windows(2).all(|pair| pair[1] > pair[0]),
                    "board={board:?}, color_group={color_group}, indices={indices:?}"
                );
            }
        }
    }

    #[test]
    fn center_distance_custom_mode_uses_origin_distance_then_spiral_index() {
        let mut engine = SimulationEngine::new(EngineSettings {
            placement_search: PlacementSearchMode::CenterDistance,
            radius: 2.0,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(9, 10_000);
        let indices: Vec<_> = batch.iter().map(|placement| placement.spot_index).collect();

        assert_eq!(indices, vec![0, 1, 3, 5, 7, 2, 4, 6, 8]);
    }

    #[test]
    fn center_distance_prime_mode_orders_spots_by_origin_distance() {
        let mut engine = SimulationEngine::new(EngineSettings {
            placement_search: PlacementSearchMode::CenterDistance,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::Color,
            prime_modulo_divisor: 6,
            radius: 2.0,
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(5, 100_000);
        let indices: Vec<_> = batch.iter().map(|placement| placement.spot_index).collect();

        assert_eq!(indices, vec![0, 1, 3, 5, 7]);
    }

    #[test]
    fn prime_modes_place_strict_prime_sequence() {
        for placement_search in [
            PlacementSearchMode::SpiralPath,
            PlacementSearchMode::CenterDistance,
        ] {
            for (army_preset, expected) in [
                (ArmyPreset::PrimeKnight, first_prime_knight_signatures(24)),
                (ArmyPreset::PrimeGapper, first_prime_gapper_signatures(24)),
            ] {
                let mut engine = SimulationEngine::new(EngineSettings {
                    board: BoardKind::LatticeHex,
                    shape: ShapeKind::Hex,
                    radius: 20.0,
                    placement_search,
                    army_preset,
                    enemy_mode: EnemyMode::AttackSet,
                    ..EngineSettings::default()
                });
                let batch = engine.step_budget(expected.len() as u32, 20_000_000);
                let pieces = batch
                    .iter()
                    .map(|placement| placement.piece)
                    .collect::<Vec<_>>();

                assert_eq!(
                    pieces, expected,
                    "search={placement_search:?}, army={army_preset:?}"
                );
            }
        }
    }

    #[test]
    fn center_distance_lattice_order_matches_bruteforce_sort() {
        let radius = 8_u64;
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
        ] {
            let expected = brute_force_center_distance_order(board, radius);
            let mut engine = SimulationEngine::new(EngineSettings {
                board,
                shape: if board == BoardKind::LatticeTriangle {
                    ShapeKind::Triangle
                } else {
                    ShapeKind::Square
                },
                radius: radius as f64,
                placement_search: PlacementSearchMode::CenterDistance,
                custom_army: vec![CustomPiece::with_auto_color(0, 0)],
                ..EngineSettings::default()
            });
            let batch = engine.step_budget(expected.len() as u32, 1_000_000);
            let got = batch
                .iter()
                .map(|placement| placement.spot_index)
                .collect::<Vec<_>>();

            assert_eq!(got, expected, "board={board:?}");
        }
    }

    #[test]
    fn hex_center_distance_includes_next_shell_side_points_before_farther_corners() {
        let radius = 8_u64;
        let expected = brute_force_center_distance_order(BoardKind::LatticeHex, radius);
        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeHex,
            shape: ShapeKind::Hex,
            radius: radius as f64,
            placement_search: PlacementSearchMode::CenterDistance,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(expected.len() as u32, 2_000_000);
        let got = batch
            .iter()
            .map(|placement| placement.spot_index)
            .collect::<Vec<_>>();

        assert_eq!(got, expected);
        assert!(
            got.windows(2).any(|pair| HexSpiral::coord_at_index(pair[1])
                .cube()
                .0
                .abs()
                .max(HexSpiral::coord_at_index(pair[1]).cube().1.abs())
                .max(HexSpiral::coord_at_index(pair[1]).cube().2.abs())
                > HexSpiral::coord_at_index(pair[0])
                    .cube()
                    .0
                    .abs()
                    .max(HexSpiral::coord_at_index(pair[0]).cube().1.abs())
                    .max(HexSpiral::coord_at_index(pair[0]).cube().2.abs())),
            "expected at least one closer next-shell side spot before an older shell corner"
        );
    }

    #[test]
    fn center_distance_high_radius_first_step_does_not_prebuild_full_lattice() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
        ] {
            let mut engine = SimulationEngine::new(EngineSettings {
                board,
                shape: if board == BoardKind::LatticeTriangle {
                    ShapeKind::Triangle
                } else {
                    ShapeKind::Square
                },
                radius: 1_500.0,
                placement_search: PlacementSearchMode::CenterDistance,
                custom_army: vec![CustomPiece::with_auto_color(0, 0)],
                ..EngineSettings::default()
            });
            let batch = engine.step_budget(1, 1);

            assert_eq!(batch.len(), 1, "board={board:?}");
            assert_eq!(batch[0].spot_index, 0, "board={board:?}");
            assert!(
                engine.stats().current_radius < 10.0,
                "board={board:?} prebuilt too much radius: {}",
                engine.stats().current_radius
            );
            assert!(!engine.stats().exhausted, "board={board:?}");
        }
    }

    #[test]
    fn high_radius_spiral_path_preallocates_lattice_without_caching_spots() {
        let radius = 1_500.0;
        for (board, expected_capacity) in [
            (BoardKind::LatticeSquare, 9_006_001_usize),
            (BoardKind::LatticeHex, 6_754_501_usize),
            (BoardKind::LatticeTriangle, 10_127_251_usize),
        ] {
            let settings = EngineSettings {
                board,
                shape: if board == BoardKind::LatticeTriangle {
                    ShapeKind::Triangle
                } else {
                    ShapeKind::Square
                },
                radius,
                placement_search: PlacementSearchMode::SpiralPath,
                custom_army: vec![CustomPiece::with_auto_color(0, 0)],
                ..EngineSettings::default()
            };
            assert_eq!(
                preallocated_spiral_path_spots(&settings),
                Some(expected_capacity),
                "board={board:?}"
            );

            let mut engine = SimulationEngine::new(settings);
            let batch = engine.step_budget(8, 8);
            assert_eq!(batch.len(), 8, "board={board:?}");
            assert!(
                engine.spots.is_empty(),
                "lattice SpiralPath should compute spots by index instead of caching every spot"
            );
        }
    }

    #[test]
    fn continuous_spiral_path_does_not_preallocate_large_spot_cache() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            radius: 1_500.0,
            piece_radius: 0.50,
            placement_search: PlacementSearchMode::SpiralPath,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };

        assert_eq!(preallocated_spiral_path_spots(&settings), None);
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(8, 8);
        assert_eq!(batch.len(), 8);
        assert_eq!(engine.spots.len(), 8);
    }

    #[test]
    fn continuous_center_distance_high_radius_starts_without_prebuilding_full_radius() {
        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            radius: 1_500.0,
            piece_radius: 0.50,
            placement_search: PlacementSearchMode::CenterDistance,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(1, 1);

        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].spot_index, 0);
        assert!(engine.stats().current_radius < 10.0);
        assert!(!engine.stats().exhausted);
    }

    #[test]
    fn continuous_center_distance_preserves_exact_primary_offset() {
        assert_eq!(continuous_center_stream_offset(1.0, 0), 1.0);
        assert_eq!(continuous_center_stream_offset(0.0, 0), 0.0);
    }

    #[test]
    fn triangle_center_shell_minimum_matches_shell_scan() {
        for shell in 0..128 {
            let scanned = center_triangle_shell_spots(shell)
                .iter()
                .map(BoardSpot::center_distance_squared)
                .min_by(f64::total_cmp)
                .unwrap();
            let formula = min_triangle_center_distance_squared_for_shell(shell);

            assert!(
                (scanned - formula).abs() <= 1.0e-9,
                "shell={shell}, scanned={scanned}, formula={formula}"
            );
        }
    }

    #[test]
    fn center_distance_lattice_modes_exhaust_full_radius_bounds() {
        let radius = 16_u64;
        let triangle_last = (3 * radius) * (3 * radius + 1) / 2;
        for (board, expected_count, last_spiral_index) in [
            (
                BoardKind::LatticeSquare,
                (2 * radius + 1).pow(2),
                (2 * radius + 1).pow(2) - 1,
            ),
            (
                BoardKind::LatticeHex,
                1 + 3 * radius * (radius + 1),
                3 * radius * (radius + 1),
            ),
            (BoardKind::LatticeTriangle, triangle_last + 1, triangle_last),
        ] {
            let mut engine = SimulationEngine::new(EngineSettings {
                board,
                shape: if board == BoardKind::LatticeTriangle {
                    ShapeKind::Triangle
                } else {
                    ShapeKind::Square
                },
                radius: radius as f64,
                placement_search: PlacementSearchMode::CenterDistance,
                custom_army: vec![CustomPiece::with_auto_color(0, 0)],
                ..EngineSettings::default()
            });
            let batch = engine.step_budget(expected_count as u32 + 1, 10_000_000);
            let indices = batch
                .iter()
                .map(|placement| placement.spot_index)
                .collect::<FxHashSet<_>>();

            assert_eq!(batch.len(), expected_count as usize, "board={board:?}");
            assert!(engine.stats().exhausted, "board={board:?}");
            assert!(
                indices.contains(&last_spiral_index),
                "board={board:?} did not include the outer boundary's final spiral spot"
            );
        }
    }

    #[test]
    fn continuous_center_distance_uses_valid_configured_unit_chord_spots() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            radius: 6.0,
            piece_radius: 0.50,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        };
        let mut spiral_path = SimulationEngine::new(EngineSettings {
            placement_search: PlacementSearchMode::SpiralPath,
            ..settings.clone()
        });
        let mut center_distance = SimulationEngine::new(EngineSettings {
            placement_search: PlacementSearchMode::CenterDistance,
            ..settings
        });

        let spiral_batch = spiral_path.step_budget(128, 1_000_000);
        let center_batch = center_distance.step_budget(128, 1_000_000);
        let spiral_indices = spiral_batch
            .iter()
            .map(|placement| placement.spot_index)
            .collect::<Vec<_>>();
        let center_indices = center_batch
            .iter()
            .map(|placement| placement.spot_index)
            .collect::<Vec<_>>();
        let center_points = center_batch
            .iter()
            .map(|placement| match placement.coord {
                SpotCoord::Continuous { x, y, .. } => Point2::new(x, y),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let spiral_points = spiral_batch
            .iter()
            .map(|placement| match placement.coord {
                SpotCoord::Continuous { x, y, .. } => Point2::new(x, y),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();

        assert_eq!(center_indices, spiral_indices);
        assert_eq!(center_points, spiral_points);
        assert_eq!(spiral_indices[0], 0);
        assert_eq!(center_indices[0], 0);
        assert!(center_distance.stats().exhausted);
    }

    #[test]
    fn continuous_spiral_path_inert_piece_fills_every_unit_chord_spot() {
        let radius = 200.0;
        let mut expected = Vec::new();
        for spot in ArchimedeanSpiral::spots(0.0) {
            if spot.center.radius() > radius + 1.0e-9 {
                break;
            }
            expected.push(spot.index);
        }

        let mut engine = SimulationEngine::new(EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            radius,
            piece_radius: 0.50,
            placement_search: PlacementSearchMode::SpiralPath,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(expected.len() as u32 + 1, 20_000_000);
        let actual = batch
            .iter()
            .map(|placement| placement.spot_index)
            .collect::<Vec<_>>();

        assert_eq!(actual.len(), expected.len());
        for (position, expected_index) in expected.iter().enumerate() {
            assert_eq!(
                actual.get(position),
                Some(expected_index),
                "first missing or reordered continuous spot at placement position {position}"
            );
        }
        assert!(engine.stats().exhausted);
    }

    #[test]
    fn prime_knight_color_mode_keeps_strict_prime_order_and_group_cursors() {
        let settings = EngineSettings {
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::Color,
            prime_modulo_divisor: 6,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(16, 5_000_000);
        let pieces: Vec<_> = batch.iter().map(|p| p.piece).collect();

        assert_eq!(batch.len(), 16);
        assert_eq!(
            &pieces[..6],
            &[
                PieceSignature::new(1, 2),
                PieceSignature::new(1, 3),
                PieceSignature::new(1, 5),
                PieceSignature::new(1, 7),
                PieceSignature::new(1, 11),
                PieceSignature::new(1, 13),
            ]
        );
        for color_group in [1, 2, 3] {
            let indices = batch
                .iter()
                .filter(|placement| placement.color.key.group == color_group)
                .map(|placement| placement.spot_index)
                .collect::<Vec<_>>();
            assert!(
                indices.windows(2).all(|pair| pair[1] > pair[0]),
                "color_group={color_group}, indices={indices:?}"
            );
        }
    }

    #[test]
    fn first_prime_knight_entry_uses_special_fixed_grey_group() {
        let mut engine = SimulationEngine::new(EngineSettings {
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::Color,
            prime_modulo_divisor: 6,
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(8, 5_000_000);

        assert_eq!(batch[0].piece, PieceSignature::new(1, 2));
        assert_eq!(batch[0].color.rule, ColorRule::Fixed);
        assert_eq!(batch[0].color.fixed_css, "#808080");
        assert_eq!(batch[0].color.key.group, 0);
        assert!(
            batch[1..]
                .iter()
                .all(|placement| placement.color.key.group != 0)
        );
    }

    #[test]
    fn first_prime_gapper_entry_uses_special_fixed_grey_group() {
        let mut engine = SimulationEngine::new(EngineSettings {
            army_preset: ArmyPreset::PrimeGapper,
            enemy_mode: EnemyMode::Color,
            ..EngineSettings::default()
        });
        let batch = engine.step_budget(8, 5_000_000);

        assert_eq!(batch[0].piece, PieceSignature::new(2, 3));
        assert_eq!(batch[0].color.rule, ColorRule::Fixed);
        assert_eq!(batch[0].color.fixed_css, "#808080");
        assert_eq!(batch[0].color.key.group, 0);
        assert!(
            batch[1..]
                .iter()
                .all(|placement| placement.color.key.group != 0)
        );
    }

    #[test]
    fn prime_gapper_color_mode_keeps_strict_gap_order_and_gap_group_cursors() {
        let settings = EngineSettings {
            army_preset: ArmyPreset::PrimeGapper,
            enemy_mode: EnemyMode::Color,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(24, 5_000_000);
        let pieces: Vec<_> = batch.iter().map(|p| p.piece).collect();

        assert_eq!(batch.len(), 24);
        assert_eq!(
            &pieces[..6],
            &[
                PieceSignature::new(2, 3),
                PieceSignature::new(3, 5),
                PieceSignature::new(5, 7),
                PieceSignature::new(7, 11),
                PieceSignature::new(11, 13),
                PieceSignature::new(13, 17),
            ]
        );

        let mut groups: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
        for placement in &batch {
            groups
                .entry(placement.color.key.group)
                .or_default()
                .push(placement.spot_index);
        }
        assert!(
            groups.values().any(|indices| indices.len() > 1),
            "test should include at least one repeated gap group"
        );
        for (color_group, indices) in groups {
            assert!(
                indices.windows(2).all(|pair| pair[1] > pair[0]),
                "gap_group={color_group}, indices={indices:?}"
            );
        }
    }

    #[test]
    fn piece_seeking_attack_set_skips_passively_attacked_spots() {
        let settings = EngineSettings {
            board: BoardKind::LatticeHex,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::AttackSet,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(24, 500_000);
        let stats = engine.stats();

        assert_eq!(batch.len(), 24);
        assert!(stats.skipped_spots > 0);
        assert!(
            batch
                .windows(2)
                .any(|pair| { pair[1].spot_index > pair[0].spot_index + 1 })
        );
    }

    #[test]
    fn hex_attack_offsets_use_rotations_and_optional_swap() {
        assert_eq!(hex_attack_offsets(PieceSignature::new(1, 2)).len(), 12);
        assert_eq!(hex_attack_offsets(PieceSignature::new(2, 2)).len(), 6);
    }

    #[test]
    fn hex_attack_offsets_match_straight_then_sixty_degree_turn_rule() {
        assert_eq!(
            hex_attack_offsets(PieceSignature::new(1, 2))
                .into_iter()
                .collect::<FxHashSet<_>>(),
            straight_then_turn_hex_offsets(2, 1)
        );
        assert_eq!(
            hex_attack_offsets(PieceSignature::new(7, 11))
                .into_iter()
                .collect::<FxHashSet<_>>(),
            straight_then_turn_hex_offsets(11, 7)
        );
    }

    #[test]
    fn triangle_attack_offsets_have_three_primary_rays_and_two_side_choices() {
        for piece in [
            PieceSignature::new(1, 1),
            PieceSignature::new(2, 1),
            PieceSignature::new(3, 1),
        ] {
            let offsets = triangle_attack_offsets(piece)
                .into_iter()
                .collect::<FxHashSet<_>>();
            assert_eq!(offsets.len(), 6, "piece={piece:?}");
            assert!(!offsets.contains(&TriangleCoord::new(0, 0)));
        }

        let one_one = triangle_attack_offsets(PieceSignature::new(1, 1))
            .into_iter()
            .collect::<FxHashSet<_>>();
        assert_eq!(
            one_one,
            [
                TriangleCoord::new(1, 0),
                TriangleCoord::new(0, 1),
                TriangleCoord::new(-1, 1),
                TriangleCoord::new(-1, 0),
                TriangleCoord::new(0, -1),
                TriangleCoord::new(1, -1),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn triangle_board_forces_triangle_or_circle_shape() {
        let mut square_request = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeTriangle,
            shape: ShapeKind::Square,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        });
        let batch = square_request.step_batch(1);
        assert_eq!(batch[0].shape, ShapeKind::Triangle);

        let mut circle_request = SimulationEngine::new(EngineSettings {
            board: BoardKind::LatticeTriangle,
            shape: ShapeKind::Circle,
            custom_army: vec![CustomPiece::with_auto_color(0, 0)],
            ..EngineSettings::default()
        });
        let batch = circle_request.step_batch(1);
        assert_eq!(batch[0].shape, ShapeKind::Circle);
    }

    fn placement_attacks(
        attacker: &Placement,
        target: &Placement,
        board: BoardKind,
        body_radius: f64,
    ) -> bool {
        match (attacker.coord, target.coord) {
            (SpotCoord::Square { x: ax, y: ay }, SpotCoord::Square { x, y }) => {
                lattice_attack_targets(board, (ax, ay), attacker.piece).contains(&(x, y))
            }
            (SpotCoord::Hex { q: aq, r: ar }, SpotCoord::Hex { q, r }) => {
                lattice_attack_targets(board, (aq, ar), attacker.piece).contains(&(q, r))
            }
            (SpotCoord::Triangle { u: au, v: av }, SpotCoord::Triangle { u, v }) => {
                lattice_attack_targets(board, (au, av), attacker.piece).contains(&(u, v))
            }
            (SpotCoord::Continuous { x: ax, y: ay, .. }, SpotCoord::Continuous { x, y, .. }) => {
                attack_circle_hits_body(
                    Point2::new(ax, ay),
                    Point2::new(x, y),
                    attack_radius_from_move(attacker.piece.a, attacker.piece.b),
                    body_radius,
                )
            }
            _ => false,
        }
    }

    fn first_prime_knight_signatures(count: usize) -> Vec<PieceSignature> {
        first_primes(count)
            .into_iter()
            .map(|prime| PieceSignature::new(1, prime as i32))
            .collect()
    }

    fn first_prime_gapper_signatures(count: usize) -> Vec<PieceSignature> {
        let primes = first_primes(count + 1);
        primes
            .windows(2)
            .take(count)
            .map(|pair| PieceSignature::new(pair[0] as i32, pair[1] as i32))
            .collect()
    }

    fn first_primes(count: usize) -> Vec<u32> {
        let mut primes = Vec::with_capacity(count);
        let mut candidate = 2;
        while primes.len() < count {
            if is_prime_with_cache(candidate, &primes) {
                primes.push(candidate);
            }
            candidate += if candidate == 2 { 1 } else { 2 };
        }
        primes
    }

    fn placement_within_radius(placement: &Placement, board: BoardKind, radius: f64) -> bool {
        match placement.coord {
            SpotCoord::Square { x, y } => x.abs().max(y.abs()) <= radius.floor() as i64,
            SpotCoord::Hex { q, r } => {
                let (x, y, z) = AxialCoord::new(q, r).cube();
                x.abs().max(y.abs()).max(z.abs()) <= radius.floor() as i64
            }
            SpotCoord::Triangle { .. } => {
                placement.spot_index <= triangular_number(3 * radius.floor() as u64)
            }
            SpotCoord::Continuous { x, y, .. } => {
                board == BoardKind::ContinuousArchimedean
                    && Point2::new(x, y).radius() <= radius + 1.0e-9
            }
        }
    }

    fn brute_force_center_distance_order(board: BoardKind, radius: u64) -> Vec<u64> {
        let mut spots = (0..=radius)
            .flat_map(|shell| center_shell_spots(board, shell))
            .collect::<Vec<_>>();
        spots.sort_by(|left, right| {
            left.center_distance_squared()
                .total_cmp(&right.center_distance_squared())
                .then_with(|| left.index().cmp(&right.index()))
        });
        spots.into_iter().map(|spot| spot.index()).collect()
    }

    fn placements_are_enemies(
        board: BoardKind,
        enemy_mode: EnemyMode,
        attacker: &Placement,
        target: &Placement,
    ) -> bool {
        let different_color = attacker.color.key.group != target.color.key.group;
        let different_attack_set =
            move_group(board, attacker.piece) != move_group(board, target.piece);
        match enemy_mode {
            EnemyMode::Color => different_color,
            EnemyMode::AttackSet => different_attack_set,
            EnemyMode::ColorAttackSet => different_color || different_attack_set,
        }
    }

    fn straight_then_turn_hex_offsets(long: i64, short: i64) -> FxHashSet<AxialCoord> {
        const DIRECTIONS: [AxialCoord; 6] = [
            AxialCoord { q: 1, r: 0 },
            AxialCoord { q: 1, r: -1 },
            AxialCoord { q: 0, r: -1 },
            AxialCoord { q: -1, r: 0 },
            AxialCoord { q: -1, r: 1 },
            AxialCoord { q: 0, r: 1 },
        ];

        let mut offsets = FxHashSet::default();
        for index in 0..DIRECTIONS.len() {
            let straight = DIRECTIONS[index].scale(long);
            let left = DIRECTIONS[(index + DIRECTIONS.len() - 1) % DIRECTIONS.len()].scale(short);
            let right = DIRECTIONS[(index + 1) % DIRECTIONS.len()].scale(short);
            offsets.insert(straight.add(left));
            offsets.insert(straight.add(right));
        }
        offsets.remove(&AxialCoord::new(0, 0));
        offsets
    }

    #[test]
    fn continuous_prime_knight_uses_piece_radius_and_keeps_progressing() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            piece_radius: 0.50,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::AttackSet,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(32, 500_000);

        assert_eq!(batch.len(), 32);
        assert!(engine.stats().skipped_spots > 0);
    }

    #[test]
    fn continuous_prime_color_modes_keep_progressing() {
        for army_preset in [ArmyPreset::PrimeKnight, ArmyPreset::PrimeGapper] {
            let settings = EngineSettings {
                board: BoardKind::ContinuousArchimedean,
                shape: ShapeKind::Circle,
                piece_radius: 0.50,
                army_preset,
                enemy_mode: EnemyMode::Color,
                ..EngineSettings::default()
            };
            let mut engine = SimulationEngine::new(settings);
            let batch = engine.step_budget(64, 5_000_000);
            assert_eq!(
                batch.len(),
                64,
                "continuous {army_preset:?} with color mode stalled at {} placements after testing {} candidates",
                batch.len(),
                engine.stats().piece_candidates_tested
            );
        }
    }

    #[test]
    fn continuous_prime_modes_make_progress_on_interactive_work_budget() {
        for army_preset in [ArmyPreset::PrimeKnight, ArmyPreset::PrimeGapper] {
            let settings = EngineSettings {
                board: BoardKind::ContinuousArchimedean,
                shape: ShapeKind::Circle,
                piece_radius: 0.50,
                army_preset,
                enemy_mode: EnemyMode::Color,
                ..EngineSettings::default()
            };
            let mut engine = SimulationEngine::new(settings);
            let batch = engine.step_budget(16, 200_000);
            assert!(
                batch.len() >= 8,
                "continuous {army_preset:?} only placed {} pieces after testing {} candidates",
                batch.len(),
                engine.stats().piece_candidates_tested
            );
        }
    }

    #[test]
    fn lattice_hex_prime_color_modes_keep_progressing() {
        for army_preset in [ArmyPreset::PrimeKnight, ArmyPreset::PrimeGapper] {
            let settings = EngineSettings {
                board: BoardKind::LatticeHex,
                army_preset,
                enemy_mode: EnemyMode::Color,
                ..EngineSettings::default()
            };
            let mut engine = SimulationEngine::new(settings);
            let batch = engine.step_budget(64, 5_000_000);
            assert_eq!(batch.len(), 64);
        }
    }

    #[test]
    fn continuous_piece_radius_changes_prime_color_sequence() {
        let mut small = SimulationEngine::new(EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            piece_radius: 0.10,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::Color,
            ..EngineSettings::default()
        });
        let mut large = SimulationEngine::new(EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            piece_radius: 0.50,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::Color,
            ..EngineSettings::default()
        });

        let small_batch = small.step_budget(80, 5_000_000);
        let large_batch = large.step_budget(80, 5_000_000);
        let small_spots: Vec<_> = small_batch.iter().map(|p| p.spot_index).collect();
        let large_spots: Vec<_> = large_batch.iter().map(|p| p.spot_index).collect();

        assert_eq!(small_batch.len(), 80);
        assert_eq!(large_batch.len(), 80);
        assert_ne!(small_spots, large_spots);
    }

    #[test]
    fn continuous_board_forces_circle_shape() {
        let mut settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Square,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings.clone());
        let batch = engine.step_batch(1);
        assert_eq!(batch[0].shape, ShapeKind::Circle);

        settings.board = BoardKind::LatticeHex;
        engine.update_settings(settings);
        let batch = engine.step_batch(1);
        assert_eq!(batch[0].shape, ShapeKind::Square);
    }

    #[test]
    fn prime_knight_modulo_bounces() {
        let buckets: Vec<_> = (1..=12)
            .map(|value| prime_knight_color_bucket(value, 12).0)
            .collect();
        assert_eq!(buckets, vec![1, 2, 3, 4, 5, 0, 5, 4, 3, 2, 1, 0]);
    }
}
