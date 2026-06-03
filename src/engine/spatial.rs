use rustc_hash::FxHashMap;

use crate::math::Point2;
use crate::protocol::{BoardKind, EnemyMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatticePieceIdentity {
    pub placement_id: u64,
    pub color_group: u64,
    pub move_group: (u32, u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatticeOccupant {
    pub placement_id: u64,
    pub identity: LatticePieceIdentity,
}

#[derive(Clone, Debug)]
struct AttackRecord {
    first_color_group: u64,
    has_other_color_group: bool,
    first_move_group: (u32, u32),
    has_other_move_group: bool,
    first_composite: (u64, (u32, u32)),
    has_other_composite: bool,
}

impl AttackRecord {
    fn new(identity: LatticePieceIdentity) -> Self {
        Self {
            first_color_group: identity.color_group,
            has_other_color_group: false,
            first_move_group: identity.move_group,
            has_other_move_group: false,
            first_composite: (identity.color_group, identity.move_group),
            has_other_composite: false,
        }
    }

    fn add(&mut self, identity: LatticePieceIdentity) {
        self.has_other_color_group |= identity.color_group != self.first_color_group;
        self.has_other_move_group |= identity.move_group != self.first_move_group;
        self.has_other_composite |=
            (identity.color_group, identity.move_group) != self.first_composite;
    }

    fn has_enemy(&self, candidate: LatticePieceIdentity, enemy_mode: EnemyMode) -> bool {
        match enemy_mode {
            EnemyMode::FreeForAll => true,
            EnemyMode::Color => {
                self.first_color_group != candidate.color_group || self.has_other_color_group
            }
            EnemyMode::AttackSet => {
                self.first_move_group != candidate.move_group || self.has_other_move_group
            }
            EnemyMode::ColorAttackSet => {
                self.first_composite != (candidate.color_group, candidate.move_group)
                    || self.has_other_composite
            }
        }
    }
}

pub const MAX_DENSE_LATTICE_CELLS: usize = 125_000_000;
pub const MAX_DENSE_GROUP_BITS: usize = 8;

#[must_use]
pub fn dense_lattice_cell_count(board: BoardKind, radius: f64) -> Option<usize> {
    dense_lattice_geometry(board, radius).map(|(_, _, cell_count)| cell_count)
}

fn dense_lattice_geometry(board: BoardKind, radius: f64) -> Option<(i64, usize, usize)> {
    if matches!(board, BoardKind::ContinuousArchimedean) {
        return None;
    }

    let bound = radius.max(0.0).floor() as i64;
    let side = usize::try_from(bound.checked_mul(2)?.checked_add(1)?).ok()?;
    let cell_count = side.checked_mul(side)?;
    Some((bound, side, cell_count))
}

#[derive(Clone, Debug)]
struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    fn new(bit_count: usize) -> Option<Self> {
        let word_count = bit_count.checked_add(63)?.div_ceil(64);
        let mut words = Vec::new();
        words.try_reserve_exact(word_count).ok()?;
        words.resize(word_count, 0);
        Some(Self { words })
    }

    fn set(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        if let Some(value) = self.words.get_mut(word) {
            *value |= 1_u64 << bit;
        }
    }

    fn get(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        self.words
            .get(word)
            .is_some_and(|value| (*value & (1_u64 << bit)) != 0)
    }
}

#[derive(Clone, Debug)]
struct DenseLatticeSpatialIndex {
    board: BoardKind,
    bound: i64,
    side: usize,
    occupied: BitSet,
    attack_any: Option<BitSet>,
    attack_color_masks: Option<Vec<u8>>,
    attack_move_masks: Option<Vec<u8>>,
    color_bits: FxHashMap<u64, u8>,
    move_bits: FxHashMap<(u32, u32), u8>,
}

impl DenseLatticeSpatialIndex {
    fn new(
        board: BoardKind,
        radius: f64,
        color_groups: &[u64],
        move_groups: &[(u32, u32)],
        enemy_mode: EnemyMode,
    ) -> Option<Self> {
        let (bound, side, cell_count) = dense_lattice_geometry(board, radius)?;
        if cell_count > MAX_DENSE_LATTICE_CELLS {
            return None;
        }

        let needs_color_masks = matches!(enemy_mode, EnemyMode::Color | EnemyMode::ColorAttackSet);
        let needs_move_masks =
            matches!(enemy_mode, EnemyMode::AttackSet | EnemyMode::ColorAttackSet);
        let needs_any_mask = enemy_mode == EnemyMode::FreeForAll;
        let color_bits = dense_group_bits(color_groups)?;
        let move_bits = dense_group_bits(move_groups)?;
        if needs_color_masks && color_bits.is_empty() {
            return None;
        }
        if needs_move_masks && move_bits.is_empty() {
            return None;
        }

        let occupied = BitSet::new(cell_count)?;
        let attack_any = if needs_any_mask {
            Some(BitSet::new(cell_count)?)
        } else {
            None
        };
        let attack_color_masks = if needs_color_masks {
            Some(zeroed_byte_mask(cell_count)?)
        } else {
            None
        };
        let attack_move_masks = if needs_move_masks {
            Some(zeroed_byte_mask(cell_count)?)
        } else {
            None
        };

        Some(Self {
            board,
            bound,
            side,
            occupied,
            attack_any,
            attack_color_masks,
            attack_move_masks,
            color_bits,
            move_bits,
        })
    }

    fn cell_index(&self, coord: (i64, i64)) -> Option<usize> {
        let (a, b) = coord;
        if a < -self.bound || a > self.bound || b < -self.bound || b > self.bound {
            return None;
        }

        if self.board == BoardKind::LatticeHex {
            let c = -a - b;
            if c < -self.bound || c > self.bound {
                return None;
            }
        }

        let x = usize::try_from(a + self.bound).ok()?;
        let y = usize::try_from(b + self.bound).ok()?;
        y.checked_mul(self.side)?.checked_add(x)
    }

    fn insert_occupied(&mut self, coord: (i64, i64)) {
        if let Some(index) = self.cell_index(coord) {
            self.occupied.set(index);
        }
    }

    fn contains(&self, coord: (i64, i64)) -> bool {
        self.cell_index(coord)
            .is_some_and(|index| self.occupied.get(index))
    }

    fn add_attack(&mut self, coord: (i64, i64), identity: LatticePieceIdentity) {
        let Some(index) = self.cell_index(coord) else {
            return;
        };

        if let Some(bits) = &mut self.attack_any {
            bits.set(index);
        }

        if let Some(masks) = &mut self.attack_color_masks
            && let Some(bit) = self.color_bits.get(&identity.color_group)
        {
            masks[index] |= *bit;
        }

        if let Some(masks) = &mut self.attack_move_masks
            && let Some(bit) = self.move_bits.get(&identity.move_group)
        {
            masks[index] |= *bit;
        }
    }

    fn is_attacked_by_enemy(
        &self,
        coord: (i64, i64),
        candidate: LatticePieceIdentity,
        enemy_mode: EnemyMode,
    ) -> bool {
        let Some(index) = self.cell_index(coord) else {
            return false;
        };

        match enemy_mode {
            EnemyMode::FreeForAll => self.attack_any.as_ref().is_some_and(|bits| bits.get(index)),
            EnemyMode::Color => self.attack_color_masks.as_ref().is_some_and(|masks| {
                mask_has_enemy(
                    masks[index],
                    self.color_bits.get(&candidate.color_group).copied(),
                )
            }),
            EnemyMode::AttackSet => self.attack_move_masks.as_ref().is_some_and(|masks| {
                mask_has_enemy(
                    masks[index],
                    self.move_bits.get(&candidate.move_group).copied(),
                )
            }),
            EnemyMode::ColorAttackSet => {
                let color_enemy = self.attack_color_masks.as_ref().is_some_and(|masks| {
                    mask_has_enemy(
                        masks[index],
                        self.color_bits.get(&candidate.color_group).copied(),
                    )
                });
                let move_enemy = self.attack_move_masks.as_ref().is_some_and(|masks| {
                    mask_has_enemy(
                        masks[index],
                        self.move_bits.get(&candidate.move_group).copied(),
                    )
                });
                color_enemy || move_enemy
            }
        }
    }
}

fn zeroed_byte_mask(len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).ok()?;
    out.resize(len, 0);
    Some(out)
}

fn dense_group_bits<T>(groups: &[T]) -> Option<FxHashMap<T, u8>>
where
    T: Copy + Eq + std::hash::Hash,
{
    let mut out = FxHashMap::default();
    for group in groups {
        if out.contains_key(group) {
            continue;
        }
        if out.len() >= MAX_DENSE_GROUP_BITS {
            return None;
        }
        out.insert(*group, 1_u8 << out.len());
    }
    Some(out)
}

fn mask_has_enemy(mask: u8, candidate_bit: Option<u8>) -> bool {
    match candidate_bit {
        Some(bit) => mask & !bit != 0,
        None => mask != 0,
    }
}

#[derive(Clone, Debug, Default)]
pub struct LatticeSpatialIndex {
    occupied: FxHashMap<(i64, i64), LatticeOccupant>,
    attacks: FxHashMap<(i64, i64), AttackRecord>,
    dense: Option<DenseLatticeSpatialIndex>,
}

impl LatticeSpatialIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_dense(
        board: BoardKind,
        radius: f64,
        color_groups: &[u64],
        move_groups: &[(u32, u32)],
        enemy_mode: EnemyMode,
    ) -> Option<Self> {
        DenseLatticeSpatialIndex::new(board, radius, color_groups, move_groups, enemy_mode).map(
            |dense| Self {
                occupied: FxHashMap::default(),
                attacks: FxHashMap::default(),
                dense: Some(dense),
            },
        )
    }

    #[must_use]
    pub fn is_dense(&self) -> bool {
        self.dense.is_some()
    }

    pub fn clear(&mut self) {
        self.occupied.clear();
        self.attacks.clear();
        self.dense = None;
    }

    pub fn insert_occupied(
        &mut self,
        coord: (i64, i64),
        identity: LatticePieceIdentity,
    ) -> Option<LatticeOccupant> {
        if let Some(dense) = &mut self.dense {
            dense.insert_occupied(coord);
            return None;
        }

        self.occupied.insert(
            coord,
            LatticeOccupant {
                placement_id: identity.placement_id,
                identity,
            },
        )
    }

    pub fn add_attack(&mut self, coord: (i64, i64), identity: LatticePieceIdentity) {
        if let Some(dense) = &mut self.dense {
            dense.add_attack(coord, identity);
            return;
        }

        self.attacks
            .entry(coord)
            .and_modify(|record| record.add(identity))
            .or_insert_with(|| AttackRecord::new(identity));
    }

    #[must_use]
    pub fn get(&self, coord: (i64, i64)) -> Option<u64> {
        if self.dense.is_some() {
            return None;
        }

        self.occupied
            .get(&coord)
            .map(|occupant| occupant.placement_id)
    }

    #[must_use]
    pub fn occupant_at(&self, coord: (i64, i64)) -> Option<LatticeOccupant> {
        if self.dense.is_some() {
            return None;
        }

        self.occupied.get(&coord).copied()
    }

    #[must_use]
    pub fn contains(&self, coord: (i64, i64)) -> bool {
        if let Some(dense) = &self.dense {
            return dense.contains(coord);
        }

        self.occupied.contains_key(&coord)
    }

    #[must_use]
    pub fn is_attacked_by_enemy(
        &self,
        coord: (i64, i64),
        candidate: LatticePieceIdentity,
        enemy_mode: EnemyMode,
    ) -> bool {
        if let Some(dense) = &self.dense {
            return dense.is_attacked_by_enemy(coord, candidate, enemy_mode);
        }

        self.attacks
            .get(&coord)
            .is_some_and(|record| record.has_enemy(candidate, enemy_mode))
    }
}

#[derive(Clone, Debug)]
pub struct ContinuousSpatialHash {
    cell_size: f64,
    cells: FxHashMap<(i64, i64), Vec<u64>>,
    centers: FxHashMap<u64, Point2>,
}

impl ContinuousSpatialHash {
    #[must_use]
    pub fn new(cell_size: f64) -> Self {
        Self {
            cell_size: cell_size.max(1.0),
            cells: FxHashMap::default(),
            centers: FxHashMap::default(),
        }
    }

    pub fn clear(&mut self) {
        self.cells.clear();
        self.centers.clear();
    }

    pub fn insert(&mut self, id: u64, center: Point2) {
        let cell = self.cell_for(center);
        self.cells.entry(cell).or_default().push(id);
        self.centers.insert(id, center);
    }

    #[must_use]
    pub fn nearby_ids(&self, center: Point2, radius: f64) -> Vec<u64> {
        let (cx, cy) = self.cell_for(center);
        let reach = (radius / self.cell_size).ceil() as i64 + 1;
        let mut out = Vec::new();

        for y in (cy - reach)..=(cy + reach) {
            for x in (cx - reach)..=(cx + reach) {
                if let Some(ids) = self.cells.get(&(x, y)) {
                    out.extend(ids.iter().copied());
                }
            }
        }

        out
    }

    #[must_use]
    pub fn center(&self, id: u64) -> Option<Point2> {
        self.centers.get(&id).copied()
    }

    #[must_use]
    fn cell_for(&self, point: Point2) -> (i64, i64) {
        (
            (point.x / self.cell_size).floor() as i64,
            (point.y / self.cell_size).floor() as i64,
        )
    }
}

impl Default for ContinuousSpatialHash {
    fn default() -> Self {
        Self::new(2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_index_tracks_occupied_cells() {
        let mut index = LatticeSpatialIndex::new();
        let identity = LatticePieceIdentity {
            placement_id: 9,
            color_group: 1,
            move_group: (1, 2),
        };
        assert!(!index.contains((3, -2)));
        index.insert_occupied((3, -2), identity);
        assert!(index.contains((3, -2)));
        assert_eq!(index.get((3, -2)), Some(9));
        assert_eq!(
            index.occupant_at((3, -2)).map(|occupant| occupant.identity),
            Some(identity)
        );
    }

    #[test]
    fn lattice_index_compacts_attack_identities() {
        let mut index = LatticeSpatialIndex::new();
        let red_knight = LatticePieceIdentity {
            placement_id: 7,
            color_group: 1,
            move_group: (1, 2),
        };
        let blue_knight = LatticePieceIdentity {
            placement_id: 9,
            color_group: 2,
            move_group: (1, 2),
        };
        let red_bishop = LatticePieceIdentity {
            placement_id: 10,
            color_group: 1,
            move_group: (1, 1),
        };
        index.add_attack((2, 1), red_knight);
        index.add_attack((2, 1), red_knight);
        index.add_attack((2, 1), blue_knight);
        index.add_attack((3, 1), red_bishop);

        assert!(index.is_attacked_by_enemy((2, 1), red_knight, EnemyMode::Color));
        assert!(!index.is_attacked_by_enemy((2, 1), red_knight, EnemyMode::AttackSet));
        assert!(index.is_attacked_by_enemy((2, 1), red_knight, EnemyMode::ColorAttackSet));
        assert!(index.is_attacked_by_enemy((2, 1), red_bishop, EnemyMode::ColorAttackSet));
        assert!(index.is_attacked_by_enemy((3, 1), red_knight, EnemyMode::AttackSet));
        assert!(index.is_attacked_by_enemy((2, 1), red_knight, EnemyMode::FreeForAll));
        assert!(!index.is_attacked_by_enemy((0, 0), red_knight, EnemyMode::FreeForAll));
    }

    #[test]
    fn lattice_index_attack_summaries_handle_many_groups() {
        let mut index = LatticeSpatialIndex::new();
        let candidate = LatticePieceIdentity {
            placement_id: u64::MAX,
            color_group: 130,
            move_group: (5, 8),
        };

        for group in 0..512 {
            index.add_attack(
                (4, -3),
                LatticePieceIdentity {
                    placement_id: group,
                    color_group: 1_000 + group,
                    move_group: (group as u32, group as u32 + 1),
                },
            );
        }

        assert!(index.is_attacked_by_enemy((4, -3), candidate, EnemyMode::Color));
        assert!(index.is_attacked_by_enemy((4, -3), candidate, EnemyMode::AttackSet));
        assert!(index.is_attacked_by_enemy((4, -3), candidate, EnemyMode::ColorAttackSet));
        assert!(index.is_attacked_by_enemy((4, -3), candidate, EnemyMode::FreeForAll));

        let mut allied_index = LatticeSpatialIndex::new();
        for id in 0..512 {
            allied_index.add_attack(
                (0, 0),
                LatticePieceIdentity {
                    placement_id: id,
                    color_group: candidate.color_group,
                    move_group: candidate.move_group,
                },
            );
        }

        assert!(!allied_index.is_attacked_by_enemy((0, 0), candidate, EnemyMode::Color));
        assert!(!allied_index.is_attacked_by_enemy((0, 0), candidate, EnemyMode::AttackSet));
        assert!(!allied_index.is_attacked_by_enemy((0, 0), candidate, EnemyMode::ColorAttackSet));
        assert!(allied_index.is_attacked_by_enemy((0, 0), candidate, EnemyMode::FreeForAll));
    }

    #[test]
    fn dense_lattice_index_matches_color_and_attack_set_enemy_masks() {
        let red_knight = LatticePieceIdentity {
            placement_id: 1,
            color_group: 0,
            move_group: (1, 2),
        };
        let blue_knight = LatticePieceIdentity {
            placement_id: 2,
            color_group: 1,
            move_group: (1, 2),
        };
        let red_bishop = LatticePieceIdentity {
            placement_id: 3,
            color_group: 0,
            move_group: (1, 1),
        };
        let mut index = LatticeSpatialIndex::with_dense(
            BoardKind::LatticeSquare,
            5_000.0,
            &[0, 1],
            &[(1, 2), (1, 1)],
            EnemyMode::ColorAttackSet,
        )
        .expect("radius 5000 square should fit dense masks");

        index.insert_occupied((3, -2), red_knight);
        assert!(index.contains((3, -2)));
        assert!(!index.contains((5_001, 0)));

        index.add_attack((4, -1), red_knight);
        assert!(!index.is_attacked_by_enemy((4, -1), red_knight, EnemyMode::Color));
        assert!(!index.is_attacked_by_enemy((4, -1), red_knight, EnemyMode::AttackSet));
        assert!(index.is_attacked_by_enemy((4, -1), blue_knight, EnemyMode::Color));
        assert!(index.is_attacked_by_enemy((4, -1), red_bishop, EnemyMode::AttackSet));
        assert!(index.is_attacked_by_enemy((4, -1), blue_knight, EnemyMode::ColorAttackSet));
        assert!(index.is_attacked_by_enemy((4, -1), red_bishop, EnemyMode::ColorAttackSet));
    }

    #[test]
    fn continuous_hash_finds_nearby_ids() {
        let mut index = ContinuousSpatialHash::new(1.0);
        index.insert(1, Point2::new(0.0, 0.0));
        index.insert(2, Point2::new(10.0, 10.0));

        let ids = index.nearby_ids(Point2::new(0.25, 0.25), 1.0);
        assert!(ids.contains(&1));
        assert!(!ids.contains(&2));
    }
}
