use rustc_hash::{FxHashMap, FxHashSet};

use crate::engine::state::{attack_radius_relevant_to_generation, lattice_attack_targets};
use crate::math::{AxialCoord, Point2, SquareCoord, TriangleCoord, attack_radius_from_move};
use crate::protocol::{
    ArmyPreset, AttackOverlayUpdate, BoardKind, ColorRule, ColorState, CustomPiece, EnemyMode,
    EngineSettings, PieceSignature, Placement, RgbColor, SpotCoord, VertexBufferUpdate,
    custom_army_effective_color_groups, move_key_for_board, normalize_prime_modulo_divisor,
    parse_hex_rgb, rainbow_rgb,
};

const ATTACK_OVERLAY_COLOR: RgbColor = RgbColor::new(
    247.0_f32 / 255.0_f32,
    247.0_f32 / 255.0_f32,
    247.0_f32 / 255.0_f32,
);
const CONTINUOUS_ATTACK_CIRCLE_VERTICES: usize = 6;
const FLOATS_PER_CONTINUOUS_ATTACK_CIRCLE_VERTEX: usize = 8;

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

#[derive(Clone, Debug, PartialEq)]
pub struct ColorContext {
    pub anchors: ColorAnchors,
    custom_overrides: Vec<Option<RgbColor>>,
}

impl ColorContext {
    #[must_use]
    pub fn from_settings(settings: &EngineSettings) -> Self {
        Self {
            anchors: ColorAnchors::from_settings(settings),
            custom_overrides: settings
                .custom_army
                .iter()
                .map(|piece| piece.color_override.as_deref().and_then(parse_hex_rgb))
                .collect(),
        }
    }

    fn custom_override(&self, group: u64) -> Option<RgbColor> {
        usize::try_from(group)
            .ok()
            .and_then(|index| self.custom_overrides.get(index))
            .copied()
            .flatten()
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
    context: &ColorContext,
    color_state: ColorState,
) -> RgbColor {
    match placement.color.rule {
        ColorRule::Fixed => parse_hex_rgb(&placement.color.fixed_css)
            .unwrap_or_else(|| RgbColor::new(238.0 / 255.0, 241.0 / 255.0, 244.0 / 255.0)),
        ColorRule::OrderRainbow => context
            .custom_override(placement.color.key.group)
            .unwrap_or_else(|| {
                rainbow_rgb(
                    context.anchors.a,
                    context.anchors.b,
                    placement.color.key.gradient_value,
                )
            }),
        ColorRule::PrimeKnightModulo => rainbow_rgb(
            context.anchors.a,
            context.anchors.b,
            placement.color.key.gradient_value,
        ),
        ColorRule::PrimeGapperBounds => {
            let min_gap = color_state.min_gap;
            let max_gap = color_state.max_gap.max(min_gap + 1.0);
            let t = ((placement.color.key.gradient_value - min_gap) / (max_gap - min_gap))
                .clamp(0.0, 1.0);
            rainbow_rgb(context.anchors.a, context.anchors.b, t)
        }
    }
}

#[must_use]
pub fn pack_vertices(
    placements: &[Placement],
    settings: &EngineSettings,
    color_state: ColorState,
) -> Vec<f32> {
    let context = ColorContext::from_settings(settings);
    let mut vertices = Vec::with_capacity(placements.len() * 5);
    append_vertices(&mut vertices, placements, &context, color_state);
    vertices
}

pub fn append_vertices(
    vertices: &mut Vec<f32>,
    placements: &[Placement],
    context: &ColorContext,
    color_state: ColorState,
) {
    vertices.reserve(placements.len() * 5);

    for placement in placements {
        let center = placement_center(placement);
        let color = color_for_placement_rgb(placement, context, color_state);
        vertices.extend_from_slice(&[center.x as f32, center.y as f32, color.r, color.g, color.b]);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttackOverlayBuffers {
    pub spots: Vec<f32>,
    pub hits: Vec<f32>,
    pub circles: Vec<f32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttackOverlayBuildStage {
    Occupied,
    Attacks,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OverlayCandidate {
    signature: PieceSignature,
    move_group: (u32, u32),
    enemy_color_group: u64,
}

#[derive(Clone, Debug)]
pub struct AttackOverlayCache {
    settings: EngineSettings,
    occupied: FxHashSet<(i64, i64)>,
    open_attacks: FxHashSet<(i64, i64)>,
    occupied_attacks: FxHashSet<(i64, i64)>,
    placement_count: usize,
}

#[derive(Clone, Debug)]
pub struct AttackOverlayBuildJob {
    cache: AttackOverlayCache,
    stage: AttackOverlayBuildStage,
    next_index: usize,
    placement_limit: usize,
}

#[must_use]
pub fn attack_overlay_requires_full_rebuild_for_new_placements(settings: &EngineSettings) -> bool {
    settings.proactive_attacking
        && matches!(
            settings.board,
            BoardKind::LatticeSquare | BoardKind::LatticeHex | BoardKind::LatticeTriangle
        )
}

impl AttackOverlayCache {
    #[must_use]
    pub fn new(settings: EngineSettings) -> Self {
        Self {
            settings,
            occupied: FxHashSet::default(),
            open_attacks: FxHashSet::default(),
            occupied_attacks: FxHashSet::default(),
            placement_count: 0,
        }
    }

    #[must_use]
    pub fn placement_count(&self) -> usize {
        self.placement_count
    }

    #[must_use]
    pub fn append_placements(&mut self, placements: &[Placement]) -> AttackOverlayUpdate {
        if placements.is_empty() {
            return AttackOverlayUpdate::none();
        }

        if self.settings.board == BoardKind::ContinuousArchimedean {
            self.placement_count = self.placement_count.saturating_add(placements.len());
            return attack_overlay_update_from_buffers(AttackOverlayBuffers {
                spots: Vec::new(),
                hits: Vec::new(),
                circles: continuous_attack_circle_vertices(placements, &self.settings),
            });
        }

        let mut buffers = AttackOverlayBuffers::default();
        for placement in placements {
            if let Some(coord) = lattice_coord(placement) {
                self.occupied.insert(coord);
                if self.open_attacks.contains(&coord) && self.occupied_attacks.insert(coord) {
                    self.open_attacks.remove(&coord);
                    append_attack_spot_vertices(
                        &mut buffers.hits,
                        std::iter::once(coord),
                        self.settings.board,
                    );
                }
            }
        }

        self.placement_count = self.placement_count.saturating_add(placements.len());
        let candidates = if self.settings.proactive_attacking {
            overlay_candidates(&self.settings, self.placement_count)
        } else {
            Vec::new()
        };
        let enemy_color_groups = custom_army_effective_color_groups(&self.settings);
        for placement in placements {
            self.append_lattice_placement_overlay(
                placement,
                &mut buffers,
                &candidates,
                &enemy_color_groups,
            );
        }

        attack_overlay_update_from_buffers(buffers)
    }

    fn append_lattice_placement_overlay(
        &mut self,
        placement: &Placement,
        buffers: &mut AttackOverlayBuffers,
        candidates: &[OverlayCandidate],
        enemy_color_groups: &[u64],
    ) {
        let Some(origin) = lattice_coord(placement) else {
            return;
        };

        if attack_radius_relevant_to_generation(
            &self.settings,
            attack_radius_from_move(placement.piece.a, placement.piece.b),
        ) {
            for target in lattice_attack_targets(self.settings.board, origin, placement.piece) {
                self.add_lattice_attack_coord(target, buffers);
            }
        }

        if !self.settings.proactive_attacking {
            return;
        }

        for &candidate in candidates {
            if !attack_radius_relevant_to_generation(
                &self.settings,
                attack_radius_from_move(candidate.signature.a, candidate.signature.b),
            ) {
                continue;
            }
            if !overlay_candidate_is_enemy(&self.settings, placement, candidate, enemy_color_groups)
            {
                continue;
            }
            for offset in lattice_attack_targets(self.settings.board, (0, 0), candidate.signature) {
                let candidate_origin = (origin.0 - offset.0, origin.1 - offset.1);
                if self.occupied.contains(&candidate_origin)
                    || !lattice_coord_within_radius(
                        self.settings.board,
                        candidate_origin,
                        self.settings.radius,
                    )
                {
                    continue;
                }
                self.add_lattice_open_attack_coord(candidate_origin, buffers);
            }
        }
    }

    fn add_lattice_attack_coord(&mut self, coord: (i64, i64), buffers: &mut AttackOverlayBuffers) {
        if self.occupied.contains(&coord) {
            if self.occupied_attacks.insert(coord) {
                self.open_attacks.remove(&coord);
                append_attack_spot_vertices(
                    &mut buffers.hits,
                    std::iter::once(coord),
                    self.settings.board,
                );
            }
        } else {
            self.add_lattice_open_attack_coord(coord, buffers);
        }
    }

    fn add_lattice_open_attack_coord(
        &mut self,
        coord: (i64, i64),
        buffers: &mut AttackOverlayBuffers,
    ) {
        if !self.occupied_attacks.contains(&coord) && self.open_attacks.insert(coord) {
            append_attack_spot_vertices(
                &mut buffers.spots,
                std::iter::once(coord),
                self.settings.board,
            );
        }
    }
}

impl AttackOverlayBuildJob {
    #[must_use]
    pub fn new(settings: EngineSettings, placement_limit: usize) -> Self {
        let stage = if settings.board == BoardKind::ContinuousArchimedean {
            AttackOverlayBuildStage::Attacks
        } else {
            AttackOverlayBuildStage::Occupied
        };
        Self {
            cache: AttackOverlayCache::new(settings),
            stage,
            next_index: 0,
            placement_limit,
        }
    }

    pub fn process_chunk(
        &mut self,
        placements: &[Placement],
        chunk_size: usize,
    ) -> (AttackOverlayUpdate, bool) {
        self.process_chunk_from(placements.len(), chunk_size, |start, end| {
            placements[start..end].to_vec()
        })
    }

    pub fn process_chunk_from<F>(
        &mut self,
        placement_count: usize,
        chunk_size: usize,
        mut placements_in_range: F,
    ) -> (AttackOverlayUpdate, bool)
    where
        F: FnMut(usize, usize) -> Vec<Placement>,
    {
        let limit = self.placement_limit.min(placement_count);
        let chunk_size = chunk_size.max(1);

        match self.stage {
            AttackOverlayBuildStage::Occupied => {
                let end = self.next_index.saturating_add(chunk_size).min(limit);
                let chunk = placements_in_range(self.next_index, end);
                for placement in &chunk {
                    if let Some(coord) = lattice_coord(placement) {
                        self.cache.occupied.insert(coord);
                    }
                }
                self.next_index = end;
                if self.next_index >= limit {
                    self.cache.placement_count = limit;
                    self.stage = AttackOverlayBuildStage::Attacks;
                    self.next_index = 0;
                }
                (
                    AttackOverlayUpdate::none(),
                    self.stage != AttackOverlayBuildStage::Done,
                )
            }
            AttackOverlayBuildStage::Attacks => {
                let end = self.next_index.saturating_add(chunk_size).min(limit);
                let mut buffers = AttackOverlayBuffers::default();
                let chunk = placements_in_range(self.next_index, end);
                if self.cache.settings.board == BoardKind::ContinuousArchimedean {
                    buffers.circles =
                        continuous_attack_circle_vertices(&chunk, &self.cache.settings);
                } else {
                    let candidates = if self.cache.settings.proactive_attacking {
                        overlay_candidates(&self.cache.settings, self.cache.placement_count)
                    } else {
                        Vec::new()
                    };
                    let enemy_color_groups =
                        custom_army_effective_color_groups(&self.cache.settings);
                    for placement in &chunk {
                        self.cache.append_lattice_placement_overlay(
                            placement,
                            &mut buffers,
                            &candidates,
                            &enemy_color_groups,
                        );
                    }
                }
                self.next_index = end;
                if self.next_index >= limit {
                    self.cache.placement_count = limit;
                    self.stage = AttackOverlayBuildStage::Done;
                }
                (
                    attack_overlay_update_from_buffers(buffers),
                    self.stage != AttackOverlayBuildStage::Done,
                )
            }
            AttackOverlayBuildStage::Done => (AttackOverlayUpdate::none(), false),
        }
    }

    #[must_use]
    pub fn into_cache(self) -> AttackOverlayCache {
        self.cache
    }
}

#[must_use]
pub fn pack_attack_overlay_buffers(
    placements: &[Placement],
    settings: &EngineSettings,
) -> AttackOverlayBuffers {
    if placements.is_empty() {
        return AttackOverlayBuffers::default();
    }

    match settings.board {
        BoardKind::LatticeSquare | BoardKind::LatticeHex | BoardKind::LatticeTriangle => {
            pack_lattice_attack_overlay_buffers(placements, settings)
        }
        BoardKind::ContinuousArchimedean => {
            pack_continuous_attack_overlay_buffers(placements, settings)
        }
    }
}

#[must_use]
pub fn attack_overlay_replace_update(
    placements: &[Placement],
    settings: &EngineSettings,
) -> AttackOverlayUpdate {
    let buffers = pack_attack_overlay_buffers(placements, settings);
    AttackOverlayUpdate {
        spots: VertexBufferUpdate::Replace(buffers.spots),
        hits: VertexBufferUpdate::Replace(buffers.hits),
        circles: VertexBufferUpdate::Replace(buffers.circles),
    }
}

fn pack_lattice_attack_overlay_buffers(
    placements: &[Placement],
    settings: &EngineSettings,
) -> AttackOverlayBuffers {
    let mut occupied = FxHashMap::default();
    for placement in placements {
        if let Some(coord) = lattice_coord(placement) {
            occupied.insert(coord, placement.id);
        }
    }

    let mut open_attacks = FxHashSet::default();
    let mut occupied_attacks = FxHashSet::default();
    let proactive_candidates = if settings.proactive_attacking {
        overlay_candidates(settings, placements.len())
    } else {
        Vec::new()
    };
    let enemy_color_groups = custom_army_effective_color_groups(settings);
    for placement in placements {
        let Some(origin) = lattice_coord(placement) else {
            continue;
        };

        if attack_radius_relevant_to_generation(
            settings,
            attack_radius_from_move(placement.piece.a, placement.piece.b),
        ) {
            for target in lattice_attack_targets(settings.board, origin, placement.piece) {
                if occupied.contains_key(&target) {
                    occupied_attacks.insert(target);
                } else {
                    open_attacks.insert(target);
                }
            }
        }
        if settings.proactive_attacking {
            for &candidate in &proactive_candidates {
                if !attack_radius_relevant_to_generation(
                    settings,
                    attack_radius_from_move(candidate.signature.a, candidate.signature.b),
                ) {
                    continue;
                }
                if !overlay_candidate_is_enemy(settings, placement, candidate, &enemy_color_groups)
                {
                    continue;
                }
                for offset in lattice_attack_targets(settings.board, (0, 0), candidate.signature) {
                    let candidate_origin = (origin.0 - offset.0, origin.1 - offset.1);
                    if occupied.contains_key(&candidate_origin)
                        || !lattice_coord_within_radius(
                            settings.board,
                            candidate_origin,
                            settings.radius,
                        )
                    {
                        continue;
                    }
                    open_attacks.insert(candidate_origin);
                }
            }
        }
    }
    for target in &occupied_attacks {
        open_attacks.remove(target);
    }

    let mut spots = Vec::with_capacity(open_attacks.len() * 5);
    let mut hits = Vec::with_capacity(occupied_attacks.len() * 5);
    append_attack_spot_vertices(&mut spots, open_attacks.into_iter(), settings.board);
    append_attack_spot_vertices(&mut hits, occupied_attacks.into_iter(), settings.board);

    AttackOverlayBuffers {
        spots,
        hits,
        circles: Vec::new(),
    }
}

fn pack_continuous_attack_overlay_buffers(
    placements: &[Placement],
    settings: &EngineSettings,
) -> AttackOverlayBuffers {
    let circles = continuous_attack_circle_vertices(placements, settings);

    AttackOverlayBuffers {
        spots: Vec::new(),
        hits: Vec::new(),
        circles,
    }
}

fn continuous_attack_circle_vertices(
    placements: &[Placement],
    settings: &EngineSettings,
) -> Vec<f32> {
    let mut circles = Vec::new();
    for placement in placements {
        let SpotCoord::Continuous { x, y, .. } = placement.coord else {
            continue;
        };
        let radius = attack_radius_from_move(placement.piece.a, placement.piece.b);
        if radius <= f64::EPSILON || !attack_radius_relevant_to_generation(settings, radius) {
            continue;
        }

        circles.reserve(
            CONTINUOUS_ATTACK_CIRCLE_VERTICES * FLOATS_PER_CONTINUOUS_ATTACK_CIRCLE_VERTEX,
        );
        append_attack_circle_quad(&mut circles, x, y, radius);
    }

    circles
}

fn append_attack_circle_quad(vertices: &mut Vec<f32>, x: f64, y: f64, radius: f64) {
    for (corner_x, corner_y) in [
        (-1.0, -1.0),
        (1.0, -1.0),
        (1.0, 1.0),
        (-1.0, -1.0),
        (1.0, 1.0),
        (-1.0, 1.0),
    ] {
        push_attack_circle_vertex(vertices, corner_x, corner_y, x, y, radius);
    }
}

fn push_attack_circle_vertex(
    vertices: &mut Vec<f32>,
    x: f64,
    y: f64,
    center_x: f64,
    center_y: f64,
    radius: f64,
) {
    vertices.extend_from_slice(&[
        x as f32,
        y as f32,
        center_x as f32,
        center_y as f32,
        radius as f32,
        ATTACK_OVERLAY_COLOR.r,
        ATTACK_OVERLAY_COLOR.g,
        ATTACK_OVERLAY_COLOR.b,
    ]);
}

fn attack_overlay_update_from_buffers(buffers: AttackOverlayBuffers) -> AttackOverlayUpdate {
    AttackOverlayUpdate {
        spots: vertex_update_for_append(buffers.spots),
        hits: vertex_update_for_append(buffers.hits),
        circles: vertex_update_for_append(buffers.circles),
    }
}

fn vertex_update_for_append(vertices: Vec<f32>) -> VertexBufferUpdate {
    if vertices.is_empty() {
        VertexBufferUpdate::None
    } else {
        VertexBufferUpdate::Append(vertices)
    }
}

fn lattice_coord(placement: &Placement) -> Option<(i64, i64)> {
    match placement.coord {
        SpotCoord::Square { x, y } => Some((x, y)),
        SpotCoord::Hex { q, r } => Some((q, r)),
        SpotCoord::Triangle { u, v } => Some((u, v)),
        SpotCoord::Continuous { .. } => None,
    }
}

fn overlay_candidates(settings: &EngineSettings, placement_count: usize) -> Vec<OverlayCandidate> {
    match settings.army_preset {
        ArmyPreset::CustomFinite => {
            let enemy_color_groups = custom_army_effective_color_groups(settings);
            settings
                .custom_army
                .iter()
                .enumerate()
                .map(|(index, piece)| {
                    overlay_candidate_for_custom_piece(
                        settings.board,
                        piece,
                        enemy_color_groups
                            .get(index)
                            .copied()
                            .unwrap_or(index as u64),
                    )
                })
                .collect()
        }
        ArmyPreset::PrimeKnight => {
            vec![overlay_candidate_for_prime_knight(
                settings.board,
                placement_count,
                settings.prime_modulo_divisor,
            )]
        }
        ArmyPreset::PrimeGapper => {
            vec![overlay_candidate_for_prime_gapper(
                settings.board,
                placement_count,
            )]
        }
    }
}

fn overlay_candidate_for_custom_piece(
    board: BoardKind,
    piece: &CustomPiece,
    enemy_color_group: u64,
) -> OverlayCandidate {
    let signature = PieceSignature::new(piece.a, piece.b);
    OverlayCandidate {
        signature,
        move_group: move_group(board, signature),
        enemy_color_group,
    }
}

fn overlay_candidate_for_prime_knight(
    board: BoardKind,
    index: usize,
    divisor: u32,
) -> OverlayCandidate {
    let prime = nth_prime(index) as i32;
    let signature = PieceSignature::new(1, prime);
    let color_group = if index == 0 {
        0
    } else {
        let (bucket, _) = prime_knight_color_bucket(index as u32 + 1, divisor);
        bucket as u64 + 1
    };
    OverlayCandidate {
        signature,
        move_group: move_group(board, signature),
        enemy_color_group: color_group,
    }
}

fn overlay_candidate_for_prime_gapper(board: BoardKind, index: usize) -> OverlayCandidate {
    let a = nth_prime(index) as i32;
    let b = nth_prime(index + 1) as i32;
    let signature = PieceSignature::new(a, b);
    let color_group = if index == 0 {
        0
    } else {
        signature.a.abs_diff(signature.b) as u64
    };
    OverlayCandidate {
        signature,
        move_group: move_group(board, signature),
        enemy_color_group: color_group,
    }
}

fn overlay_candidate_is_enemy(
    settings: &EngineSettings,
    placement: &Placement,
    candidate: OverlayCandidate,
    enemy_color_groups: &[u64],
) -> bool {
    if matches!(
        settings.enemy_mode,
        EnemyMode::Color | EnemyMode::ColorAttackSet
    ) && placement_enemy_color_group(settings, placement, enemy_color_groups)
        != candidate.enemy_color_group
    {
        return true;
    }

    matches!(
        settings.enemy_mode,
        EnemyMode::AttackSet | EnemyMode::ColorAttackSet
    ) && move_group(settings.board, placement.piece) != candidate.move_group
}

fn placement_enemy_color_group(
    settings: &EngineSettings,
    placement: &Placement,
    enemy_color_groups: &[u64],
) -> u64 {
    if settings.army_preset != ArmyPreset::CustomFinite
        || placement.color.rule != ColorRule::OrderRainbow
    {
        return placement.color.key.group;
    }

    usize::try_from(placement.color.key.group)
        .ok()
        .and_then(|index| enemy_color_groups.get(index).copied())
        .unwrap_or(placement.color.key.group)
}

fn move_group(board: BoardKind, piece: PieceSignature) -> (u32, u32) {
    move_key_for_board(board, piece)
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

fn nth_prime(index: usize) -> u32 {
    let mut primes = Vec::new();
    let mut candidate = 2_u32;
    while primes.len() <= index {
        if is_prime_with_cache(candidate, &primes) {
            primes.push(candidate);
        }
        candidate += if candidate == 2 { 1 } else { 2 };
    }
    primes[index]
}

fn is_prime_with_cache(n: u32, primes: &[u32]) -> bool {
    if n < 2 {
        return false;
    }
    for &prime in primes {
        if prime * prime > n {
            break;
        }
        if n.is_multiple_of(prime) {
            return false;
        }
    }
    true
}

fn lattice_coord_within_radius(board: BoardKind, coord: (i64, i64), radius: f64) -> bool {
    let bound = radius.max(0.0).floor() as i64;
    match board {
        BoardKind::LatticeSquare => coord.0.abs().max(coord.1.abs()) <= bound,
        BoardKind::LatticeHex => {
            let z = -coord.0 - coord.1;
            coord.0.abs().max(coord.1.abs()).max(z.abs()) <= bound
        }
        BoardKind::LatticeTriangle => TriangleCoord::new(coord.0, coord.1).shell_radius() <= bound,
        BoardKind::ContinuousArchimedean => false,
    }
}

fn append_attack_spot_vertices(
    vertices: &mut Vec<f32>,
    coords: impl Iterator<Item = (i64, i64)>,
    board: BoardKind,
) {
    for (a, b) in coords {
        let center = match board {
            BoardKind::LatticeSquare => SquareCoord::new(a, b).to_point(),
            BoardKind::LatticeHex => AxialCoord::new(a, b).to_point(),
            BoardKind::LatticeTriangle => TriangleCoord::new(a, b).to_point(),
            BoardKind::ContinuousArchimedean => continue,
        };
        push_attack_vertex(vertices, center.x, center.y);
    }
}

fn push_attack_vertex(vertices: &mut Vec<f32>, x: f64, y: f64) {
    vertices.extend_from_slice(&[
        x as f32,
        y as f32,
        ATTACK_OVERLAY_COLOR.r,
        ATTACK_OVERLAY_COLOR.g,
        ATTACK_OVERLAY_COLOR.b,
    ]);
}

#[cfg(test)]
mod tests {
    use crate::protocol::{
        ColorKey, ColorRule, CustomPiece, DEFAULT_ANCHOR_A, DEFAULT_ANCHOR_B, EnemyMode,
        EngineSettings, PieceColor, PieceSignature, Placement, ShapeKind, SpotCoord,
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

    #[test]
    fn custom_color_override_recolors_order_rainbow_without_changing_group() {
        let settings = EngineSettings {
            custom_army: vec![CustomPiece::with_color_override(2, 1, "#55a7ff")],
            ..EngineSettings::default()
        };
        let placements = vec![Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Square { x: 0, y: 0 },
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

        assert_eq!(&vertices[2..5], &[85.0 / 255.0, 167.0 / 255.0, 1.0]);
        assert_eq!(placements[0].color.key.group, 0);
    }

    #[test]
    fn lattice_attack_overlay_separates_open_spots_and_occupied_hits() {
        let settings = EngineSettings {
            board: crate::protocol::BoardKind::LatticeSquare,
            custom_army: vec![],
            ..EngineSettings::default()
        };
        let placements = vec![
            Placement {
                id: 0,
                spot_index: 0,
                coord: SpotCoord::Square { x: 0, y: 0 },
                piece: PieceSignature::new(1, 0),
                color: PieceColor {
                    rule: ColorRule::Fixed,
                    fixed_css: "#ffffff".to_string(),
                    key: ColorKey {
                        group: 0,
                        gradient_value: 0.0,
                    },
                },
                shape: ShapeKind::Square,
            },
            Placement {
                id: 1,
                spot_index: 1,
                coord: SpotCoord::Square { x: 1, y: 0 },
                piece: PieceSignature::new(1, 0),
                color: PieceColor {
                    rule: ColorRule::Fixed,
                    fixed_css: "#ffffff".to_string(),
                    key: ColorKey {
                        group: 1,
                        gradient_value: 0.0,
                    },
                },
                shape: ShapeKind::Square,
            },
        ];

        let overlay = pack_attack_overlay_buffers(&placements, &settings);

        assert_eq!(overlay.hits.len(), 10);
        assert!(overlay.spots.len() >= 15);
        assert!(overlay.circles.is_empty());
    }

    #[test]
    fn proactive_lattice_overlay_marks_candidate_origins_that_can_attack_enemies() {
        let mut settings = EngineSettings {
            board: crate::protocol::BoardKind::LatticeSquare,
            radius: 4.0,
            proactive_attacking: true,
            enemy_mode: EnemyMode::Color,
            custom_army: vec![
                CustomPiece::with_auto_color(1, 0),
                CustomPiece::with_auto_color(2, 0),
            ],
            ..EngineSettings::default()
        };
        let placements = vec![Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Square { x: 0, y: 0 },
            piece: PieceSignature::new(1, 0),
            color: PieceColor {
                rule: ColorRule::Fixed,
                fixed_css: "#ffffff".to_string(),
                key: ColorKey {
                    group: 0,
                    gradient_value: 0.0,
                },
            },
            shape: ShapeKind::Square,
        }];

        let proactive = pack_attack_overlay_buffers(&placements, &settings);
        assert!(
            vertices_contain_xy(&proactive.spots, 2.0, 0.0),
            "proactive candidate origin (2,0) should be marked"
        );

        settings.proactive_attacking = false;
        let passive = pack_attack_overlay_buffers(&placements, &settings);
        assert!(
            !vertices_contain_xy(&passive.spots, 2.0, 0.0),
            "passive overlay should not add candidate-origin shadows"
        );
    }

    #[test]
    fn proactive_lattice_overlay_uses_full_rebuild_path_for_new_placements() {
        let lattice = EngineSettings {
            board: crate::protocol::BoardKind::LatticeHex,
            proactive_attacking: true,
            ..EngineSettings::default()
        };
        assert!(attack_overlay_requires_full_rebuild_for_new_placements(
            &lattice
        ));

        let passive_lattice = EngineSettings {
            proactive_attacking: false,
            ..lattice.clone()
        };
        assert!(!attack_overlay_requires_full_rebuild_for_new_placements(
            &passive_lattice
        ));

        let continuous = EngineSettings {
            board: crate::protocol::BoardKind::ContinuousArchimedean,
            proactive_attacking: true,
            ..EngineSettings::default()
        };
        assert!(!attack_overlay_requires_full_rebuild_for_new_placements(
            &continuous
        ));
    }

    #[test]
    fn attack_overlay_skips_attacks_beyond_four_times_radius() {
        let lattice_settings = EngineSettings {
            board: crate::protocol::BoardKind::LatticeSquare,
            radius: 2.0,
            ..EngineSettings::default()
        };
        let lattice_placements = vec![Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Square { x: 0, y: 0 },
            piece: PieceSignature::new(9, 0),
            color: PieceColor {
                rule: ColorRule::Fixed,
                fixed_css: "#ffffff".to_string(),
                key: ColorKey {
                    group: 0,
                    gradient_value: 0.0,
                },
            },
            shape: ShapeKind::Square,
        }];
        let lattice_overlay = pack_attack_overlay_buffers(&lattice_placements, &lattice_settings);
        assert!(lattice_overlay.spots.is_empty());
        assert!(lattice_overlay.hits.is_empty());

        let continuous_settings = EngineSettings {
            board: crate::protocol::BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            radius: 2.0,
            ..EngineSettings::default()
        };
        let continuous_placements = vec![Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Continuous {
                x: 0.0,
                y: 0.0,
                theta: 0.0,
            },
            piece: PieceSignature::new(9, 0),
            color: PieceColor {
                rule: ColorRule::Fixed,
                fixed_css: "#ffffff".to_string(),
                key: ColorKey {
                    group: 0,
                    gradient_value: 0.0,
                },
            },
            shape: ShapeKind::Circle,
        }];
        let continuous_overlay =
            pack_attack_overlay_buffers(&continuous_placements, &continuous_settings);
        assert!(continuous_overlay.circles.is_empty());
    }

    #[test]
    fn continuous_attack_overlay_packs_one_quad_per_circle() {
        let settings = EngineSettings {
            board: crate::protocol::BoardKind::ContinuousArchimedean,
            shape: ShapeKind::Circle,
            radius: 8.0,
            ..EngineSettings::default()
        };
        let placements = vec![Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Continuous {
                x: 3.0,
                y: -2.0,
                theta: 0.0,
            },
            piece: PieceSignature::new(1, 0),
            color: PieceColor {
                rule: ColorRule::Fixed,
                fixed_css: "#ffffff".to_string(),
                key: ColorKey {
                    group: 0,
                    gradient_value: 0.0,
                },
            },
            shape: ShapeKind::Circle,
        }];

        let overlay = pack_attack_overlay_buffers(&placements, &settings);
        assert_eq!(overlay.circles.len(), 6 * 8);
        for vertex in overlay.circles.chunks_exact(8) {
            assert!(vertex[0] == -1.0 || vertex[0] == 1.0);
            assert!(vertex[1] == -1.0 || vertex[1] == 1.0);
            assert_eq!(vertex[2], 3.0);
            assert_eq!(vertex[3], -2.0);
            assert_eq!(vertex[4], 1.0);
        }
    }

    #[test]
    fn incremental_attack_overlay_cache_skips_attacks_beyond_four_times_radius() {
        let settings = EngineSettings {
            board: crate::protocol::BoardKind::LatticeSquare,
            radius: 2.0,
            ..EngineSettings::default()
        };
        let placement = Placement {
            id: 0,
            spot_index: 0,
            coord: SpotCoord::Square { x: 0, y: 0 },
            piece: PieceSignature::new(9, 0),
            color: PieceColor {
                rule: ColorRule::Fixed,
                fixed_css: "#ffffff".to_string(),
                key: ColorKey {
                    group: 0,
                    gradient_value: 0.0,
                },
            },
            shape: ShapeKind::Square,
        };

        let mut cache = AttackOverlayCache::new(settings);
        let update = cache.append_placements(&[placement]);

        assert!(matches!(update.spots, VertexBufferUpdate::None));
        assert!(matches!(update.hits, VertexBufferUpdate::None));
        assert!(matches!(update.circles, VertexBufferUpdate::None));
    }

    fn vertices_contain_xy(vertices: &[f32], x: f32, y: f32) -> bool {
        vertices
            .chunks_exact(5)
            .any(|vertex| vertex[0] == x && vertex[1] == y)
    }
}
