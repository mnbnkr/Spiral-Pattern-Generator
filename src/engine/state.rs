use rustc_hash::{FxHashMap, FxHashSet};

use crate::engine::spatial::{ContinuousSpatialHash, LatticeSpatialIndex};
use crate::math::{
    ArchimedeanSpiral, ArchimedeanSpots, AxialCoord, HexSpiral, Point2, SquareCoord, SquareSpiral,
    attack_circle_hits_body, attack_circle_hits_body_distance_squared, attack_radius_from_move,
    bodies_overlap,
};
use crate::protocol::{
    ArmyPreset, BoardKind, ColorKey, ColorRule, ColorState, CustomPiece, EnemyMode, EngineSettings,
    EngineStats, PieceColor, PieceSignature, Placement, ShapeKind, SpotCoord,
};

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
            ArmyPreset::PrimeKnight | ArmyPreset::PrimeGap => Self::PieceSeeking,
        }
    }
}

#[derive(Clone, Debug)]
struct CandidatePiece {
    signature: PieceSignature,
    color: PieceColor,
    move_group: (i32, i32),
    color_group: u64,
    attack_radius: f64,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct PlacedPiece {
    id: u64,
    spot_index: u64,
    lattice_coord: Option<(i64, i64)>,
    center: Point2,
    piece: PieceSignature,
    color: PieceColor,
    shape: ShapeKind,
    move_group: (i32, i32),
    color_group: u64,
    attack_radius: f64,
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
    Continuous {
        index: u64,
        theta: f64,
        center: Point2,
    },
}

impl BoardSpot {
    #[must_use]
    fn index(&self) -> u64 {
        match self {
            Self::Square { index, .. }
            | Self::Hex { index, .. }
            | Self::Continuous { index, .. } => *index,
        }
    }

    #[must_use]
    fn lattice_coord(&self) -> Option<(i64, i64)> {
        match self {
            Self::Square { coord, .. } => Some((coord.x, coord.y)),
            Self::Hex { coord, .. } => Some((coord.q, coord.r)),
            Self::Continuous { .. } => None,
        }
    }

    #[must_use]
    fn center(&self) -> Point2 {
        match self {
            Self::Square { coord, .. } => coord.to_point(),
            Self::Hex { coord, .. } => coord.to_point(),
            Self::Continuous { center, .. } => *center,
        }
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
            Self::Continuous { theta, center, .. } => SpotCoord::Continuous {
                x: center.x,
                y: center.y,
                theta: *theta,
            },
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
    spot_seek_scan_indices: Vec<u64>,
    next_piece_spot_index: u64,
    piece_seek_candidate_index: Option<usize>,
    spots: Vec<BoardSpot>,
    square_spiral: SquareSpiral,
    hex_spiral: HexSpiral,
    continuous_spiral: ArchimedeanSpots,
    occupied_spots: FxHashMap<u64, u64>,
    lattice_index: LatticeSpatialIndex,
    continuous_index: ContinuousSpatialHash,
    max_continuous_attack_radius: f64,
    placed: Vec<PlacedPiece>,
    prime_cache: Vec<u32>,
    prime_used: Vec<bool>,
    min_gap_seen: Option<f64>,
    max_gap_seen: Option<f64>,
}

impl SimulationEngine {
    #[must_use]
    pub fn new(settings: EngineSettings) -> Self {
        let mode = SimulationMode::from_preset(settings.army_preset);
        let continuous_spiral = ArchimedeanSpiral::spots(settings.continuous_offset);
        let custom_army_len = settings.custom_army.len();

        Self {
            settings,
            mode,
            stats: EngineStats::default(),
            next_id: 0,
            next_army_index: 0,
            spot_seek_scan_indices: vec![0; custom_army_len],
            next_piece_spot_index: 0,
            piece_seek_candidate_index: None,
            spots: Vec::new(),
            square_spiral: SquareSpiral::new(),
            hex_spiral: HexSpiral::new(),
            continuous_spiral,
            occupied_spots: FxHashMap::default(),
            lattice_index: LatticeSpatialIndex::new(),
            continuous_index: ContinuousSpatialHash::new(2.0),
            max_continuous_attack_radius: 0.0,
            placed: Vec::new(),
            prime_cache: Vec::new(),
            prime_used: Vec::new(),
            min_gap_seen: None,
            max_gap_seen: None,
        }
    }

    pub fn reset(&mut self, settings: EngineSettings) {
        *self = Self::new(settings);
    }

    pub fn update_settings(&mut self, settings: EngineSettings) -> bool {
        if self.requires_reset_for_settings(&settings) {
            self.reset(settings);
            true
        } else {
            self.settings = settings;
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

        if self.spot_seek_scan_indices.len() != army_len {
            self.spot_seek_scan_indices.resize(army_len, 0);
        }

        let army_index = (self.next_army_index as usize) % army_len;
        let piece = self.custom_candidate(army_index);
        let mut scan_index = self.spot_seek_scan_indices[army_index];

        while *remaining_work > 0 {
            let spot = self.spot_at(scan_index)?;
            scan_index += 1;
            *remaining_work -= 1;

            if self.occupied_spots.contains_key(&spot.index()) {
                continue;
            }

            self.stats.spots_tested += 1;

            if self.is_valid_candidate(&spot, &piece) {
                self.next_army_index += 1;
                self.spot_seek_scan_indices[army_index] = scan_index;
                return Some(self.place_piece(spot, piece));
            }

            self.stats.skipped_spots += 1;
        }

        self.spot_seek_scan_indices[army_index] = scan_index;
        None
    }

    fn next_piece_seeking_placement(&mut self, remaining_work: &mut u64) -> Option<Placement> {
        while *remaining_work > 0 {
            let spot = self.spot_at(self.next_piece_spot_index)?;
            if !self.should_skip_piece_seeking_spot(&spot) {
                break;
            }

            self.next_piece_spot_index += 1;
            self.piece_seek_candidate_index = None;
            self.stats.spots_tested += 1;
            self.stats.skipped_spots += 1;
            *remaining_work -= 1;
        }

        if *remaining_work == 0 {
            return None;
        }

        let spot = self.spot_at(self.next_piece_spot_index)?;
        let mut candidate_index = self
            .piece_seek_candidate_index
            .unwrap_or_else(|| self.lowest_unused_prime_index());

        while *remaining_work > 0 {
            self.ensure_prime_used_capacity(candidate_index);
            if self.prime_used[candidate_index] {
                candidate_index += 1;
                continue;
            }

            let piece = self.prime_candidate(candidate_index);
            self.stats.piece_candidates_tested += 1;
            *remaining_work -= 1;

            if self.is_valid_candidate(&spot, &piece) {
                self.prime_used[candidate_index] = true;
                self.next_piece_spot_index += 1;
                self.piece_seek_candidate_index = None;
                return Some(self.place_piece(spot, piece));
            }

            candidate_index += 1;
        }

        self.piece_seek_candidate_index = Some(candidate_index);
        None
    }

    fn should_skip_piece_seeking_spot(&self, spot: &BoardSpot) -> bool {
        match self.settings.board {
            BoardKind::LatticeSquare | BoardKind::LatticeHex => {
                let Some(coord) = spot.lattice_coord() else {
                    return false;
                };
                if self.lattice_index.contains(coord) {
                    return true;
                }

                match self.settings.enemy_mode {
                    EnemyMode::MoveSet => !self.lattice_index.attackers_at(coord).is_empty(),
                    EnemyMode::Color => {
                        let mut groups = FxHashSet::default();
                        for attacker_id in self.lattice_index.attackers_at(coord) {
                            if let Some(attacker) = self.placed.get(*attacker_id as usize) {
                                groups.insert(attacker.color_group);
                                if groups.len() > 1 {
                                    return true;
                                }
                            }
                        }
                        self.required_color_group_is_unavailable(&groups)
                    }
                }
            }
            BoardKind::ContinuousArchimedean => {
                let center = spot.center();
                let body_radius = self.settings.piece_radius;
                let mut passive_color_groups = FxHashSet::default();

                for id in self.continuous_body_probe_ids(center, body_radius) {
                    let Some(existing) = self.placed.get(id as usize) else {
                        continue;
                    };

                    if bodies_overlap(center, existing.center, body_radius) {
                        return true;
                    }
                }

                for id in self.continuous_passive_probe_ids(center, body_radius) {
                    let Some(existing) = self.placed.get(id as usize) else {
                        continue;
                    };

                    if attack_circle_hits_body(
                        existing.center,
                        center,
                        existing.attack_radius,
                        body_radius,
                    ) {
                        match self.settings.enemy_mode {
                            EnemyMode::MoveSet => return true,
                            EnemyMode::Color => {
                                passive_color_groups.insert(existing.color_group);
                                if passive_color_groups.len() > 1 {
                                    return true;
                                }
                            }
                        }
                    }
                }

                self.required_color_group_is_unavailable(&passive_color_groups)
            }
        }
    }

    fn required_color_group_is_unavailable(&self, passive_groups: &FxHashSet<u64>) -> bool {
        if passive_groups.len() > 1 {
            return true;
        }

        let Some(&required_group) = passive_groups.iter().next() else {
            return false;
        };

        match self.settings.army_preset {
            ArmyPreset::PrimeKnight => false,
            ArmyPreset::PrimeGap => {
                required_group == 1 && self.prime_used.first().copied().unwrap_or(false)
            }
            ArmyPreset::CustomFinite => false,
        }
    }

    fn place_piece(&mut self, spot: BoardSpot, piece: CandidatePiece) -> Placement {
        let shape = if self.settings.board == BoardKind::ContinuousArchimedean {
            ShapeKind::Circle
        } else {
            self.settings.shape
        };

        let center = spot.center();
        let lattice_coord = spot.lattice_coord();
        let coord = spot.spot_coord();
        let spot_index = spot.index();
        let id = self.next_id;

        let placed_piece = PlacedPiece {
            id,
            spot_index,
            lattice_coord,
            center,
            piece: piece.signature,
            color: piece.color.clone(),
            shape,
            move_group: piece.move_group,
            color_group: piece.color_group,
            attack_radius: piece.attack_radius,
        };

        self.occupied_spots.insert(spot_index, id);

        if let Some(coord) = lattice_coord {
            self.lattice_index.insert_occupied(coord, id);
            for attack in lattice_attack_targets(self.settings.board, coord, piece.signature) {
                self.lattice_index.add_attack(attack, id);
            }
        } else {
            self.continuous_index.insert(id, center);
            self.max_continuous_attack_radius =
                self.max_continuous_attack_radius.max(piece.attack_radius);
        }

        if piece.color.rule == ColorRule::PrimeGapBounds {
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
        if self.occupied_spots.contains_key(&spot.index()) {
            return false;
        }

        match self.settings.board {
            BoardKind::LatticeSquare | BoardKind::LatticeHex => {
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

        if self.settings.proactive_attacking {
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

            if bodies_overlap(center, existing.center, body_radius) {
                return false;
            }
        }

        for id in self.continuous_passive_probe_ids(center, body_radius) {
            let Some(existing) = self.placed.get(id as usize) else {
                continue;
            };

            if self.are_enemies(existing, candidate)
                && attack_circle_hits_body_distance_squared(
                    existing.center.squared_distance(center),
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

                if self.are_enemies(existing, candidate)
                    && attack_circle_hits_body_distance_squared(
                        center.squared_distance(existing.center),
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

    fn continuous_body_probe_ids(&self, center: Point2, body_radius: f64) -> Vec<u64> {
        self.continuous_index
            .nearby_ids(center, 2.0 * body_radius.max(0.0))
    }

    fn continuous_passive_probe_ids(&self, center: Point2, body_radius: f64) -> Vec<u64> {
        if self.max_continuous_attack_radius <= 0.0 {
            return Vec::new();
        }

        self.continuous_index.nearby_ids(
            center,
            self.max_continuous_attack_radius + body_radius.max(0.0),
        )
    }

    fn continuous_proactive_probe_ids(
        &self,
        center: Point2,
        candidate: &CandidatePiece,
        body_radius: f64,
    ) -> Vec<u64> {
        self.continuous_index.nearby_ids(
            center,
            candidate.attack_radius.max(0.0) + body_radius.max(0.0),
        )
    }

    fn are_enemies(&self, existing: &PlacedPiece, candidate: &CandidatePiece) -> bool {
        match self.settings.enemy_mode {
            EnemyMode::MoveSet => existing.move_group != candidate.move_group,
            EnemyMode::Color => existing.color_group != candidate.color_group,
        }
    }

    fn custom_army_len(&self) -> usize {
        self.settings.custom_army.len()
    }

    fn requires_reset_for_settings(&self, next: &EngineSettings) -> bool {
        let current = &self.settings;

        current.board != next.board
            || current.radius != next.radius
            || continuous_piece_radius_changes_simulation(current, next)
            || current.proactive_attacking != next.proactive_attacking
            || current.enemy_mode != next.enemy_mode
            || current.army_preset != next.army_preset
            || current.custom_army != next.custom_army
            || current.continuous_offset != next.continuous_offset
            || current.prime_modulo_divisor != next.prime_modulo_divisor
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
            move_group: move_group(signature),
            color_group,
            attack_radius: attack_radius_from_move(signature.a, signature.b),
        }
    }

    fn prime_candidate(&mut self, candidate_index: usize) -> CandidatePiece {
        match self.settings.army_preset {
            ArmyPreset::PrimeKnight => {
                let p = self.prime(candidate_index) as i32;
                let signature = PieceSignature::new(1, p);
                let (bucket, t) = prime_knight_color_bucket(
                    candidate_index as u32 + 1,
                    self.settings.prime_modulo_divisor,
                );

                CandidatePiece {
                    signature,
                    color: PieceColor {
                        rule: ColorRule::PrimeKnightModulo,
                        fixed_css: String::new(),
                        key: ColorKey {
                            group: bucket as u64,
                            gradient_value: t,
                        },
                    },
                    move_group: move_group(signature),
                    color_group: bucket as u64,
                    attack_radius: attack_radius_from_move(signature.a, signature.b),
                }
            }
            ArmyPreset::PrimeGap => {
                let a = self.prime(candidate_index) as i32;
                let b = self.prime(candidate_index + 1) as i32;
                let signature = PieceSignature::new(a, b);
                let gap = signature_gap(signature);

                CandidatePiece {
                    signature,
                    color: PieceColor {
                        rule: ColorRule::PrimeGapBounds,
                        fixed_css: String::new(),
                        key: ColorKey {
                            group: gap as u64,
                            gradient_value: gap as f64,
                        },
                    },
                    move_group: move_group(signature),
                    color_group: gap as u64,
                    attack_radius: attack_radius_from_move(signature.a, signature.b),
                }
            }
            ArmyPreset::CustomFinite => self.custom_candidate(0),
        }
    }

    fn spot_at(&mut self, index: u64) -> Option<BoardSpot> {
        if self.stats.exhausted && self.spots.len() <= index as usize {
            return None;
        }

        while self.spots.len() <= index as usize {
            let next_index = self.spots.len() as u64;
            let spot = match self.settings.board {
                BoardKind::LatticeSquare => {
                    let coord = self.square_spiral.next()?;
                    BoardSpot::Square {
                        index: next_index,
                        coord,
                    }
                }
                BoardKind::LatticeHex => {
                    let coord = self.hex_spiral.next()?;
                    BoardSpot::Hex {
                        index: next_index,
                        coord,
                    }
                }
                BoardKind::ContinuousArchimedean => {
                    let spot = self.continuous_spiral.next()?;
                    BoardSpot::Continuous {
                        index: next_index,
                        theta: spot.theta,
                        center: spot.center,
                    }
                }
            };

            if !self.spot_within_generation_radius(&spot) {
                self.stats.exhausted = true;
                return None;
            }

            self.spots.push(spot);
        }

        self.spots.get(index as usize).cloned()
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
            BoardSpot::Continuous { center, .. } => center.radius() <= radius + 1.0e-9,
        }
    }

    fn ensure_prime_used_capacity(&mut self, index: usize) {
        if self.prime_used.len() <= index {
            self.prime_used.resize(index + 1, false);
        }
    }

    fn lowest_unused_prime_index(&mut self) -> usize {
        let mut index = 0;
        loop {
            self.ensure_prime_used_capacity(index);
            if !self.prime_used[index] {
                return index;
            }
            index += 1;
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

fn lattice_attack_targets(
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
        BoardKind::ContinuousArchimedean => Vec::new(),
    }
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

fn rotate_cube_right((x, y, z): (i64, i64, i64)) -> (i64, i64, i64) {
    (-z, -x, -y)
}

fn move_group(piece: PieceSignature) -> (i32, i32) {
    let a = piece.a.unsigned_abs() as i32;
    let b = piece.b.unsigned_abs() as i32;
    (a.min(b), a.max(b))
}

fn signature_gap(piece: PieceSignature) -> u32 {
    piece.a.abs_diff(piece.b)
}

fn continuous_piece_radius_changes_simulation(
    current: &EngineSettings,
    next: &EngineSettings,
) -> bool {
    (current.board == BoardKind::ContinuousArchimedean
        || next.board == BoardKind::ContinuousArchimedean)
        && current.piece_radius != next.piece_radius
}

fn prime_knight_color_bucket(value: u32, divisor: u32) -> (u32, f64) {
    let divisor = divisor.max(2);
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
    fn radius_limits_continuous_generation() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            radius: 1.0,
            piece_radius: 0.25,
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
    fn proactive_rule_rejects_candidate_that_attacks_enemy() {
        let mut passive = EngineSettings {
            enemy_mode: EnemyMode::MoveSet,
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
        assert_ne!(active_batch[1].spot_index, 1);
        assert!(active_engine.stats().proactive_rejections > 0);
    }

    #[test]
    fn proactive_rule_changes_lattice_hex_placements() {
        let mut passive = EngineSettings {
            board: BoardKind::LatticeHex,
            enemy_mode: EnemyMode::MoveSet,
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
        assert_ne!(active_batch[1].spot_index, 1);
        assert!(active_engine.stats().proactive_rejections > 0);
    }

    #[test]
    fn proactive_rule_changes_continuous_placements() {
        let mut passive = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            piece_radius: 0.25,
            enemy_mode: EnemyMode::MoveSet,
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
        assert_ne!(active_batch[1].spot_index, 1);
        assert!(active_engine.stats().proactive_rejections > 0);
    }

    #[test]
    fn piece_seeking_prime_knights_fill_every_spot() {
        let settings = EngineSettings {
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::Color,
            prime_modulo_divisor: 2,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_batch(16);
        let indices: Vec<_> = batch.iter().map(|p| p.spot_index).collect();
        assert_eq!(indices, (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn piece_seeking_move_set_skips_passively_attacked_spots() {
        let settings = EngineSettings {
            board: BoardKind::LatticeHex,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::MoveSet,
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
    fn continuous_prime_knight_uses_piece_radius_and_keeps_progressing() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            piece_radius: 0.25,
            army_preset: ArmyPreset::PrimeKnight,
            enemy_mode: EnemyMode::MoveSet,
            ..EngineSettings::default()
        };
        let mut engine = SimulationEngine::new(settings);
        let batch = engine.step_budget(32, 500_000);

        assert_eq!(batch.len(), 32);
        assert!(engine.stats().skipped_spots > 0);
    }

    #[test]
    fn continuous_prime_color_modes_keep_progressing() {
        for army_preset in [ArmyPreset::PrimeKnight, ArmyPreset::PrimeGap] {
            let settings = EngineSettings {
                board: BoardKind::ContinuousArchimedean,
                shape: ShapeKind::Circle,
                piece_radius: 0.25,
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
    fn lattice_hex_prime_color_modes_keep_progressing() {
        for army_preset in [ArmyPreset::PrimeKnight, ArmyPreset::PrimeGap] {
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
