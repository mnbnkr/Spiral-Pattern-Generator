use rustc_hash::FxHashMap;

use crate::math::Point2;
use crate::protocol::EnemyMode;

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

#[derive(Clone, Debug, Default)]
pub struct LatticeSpatialIndex {
    occupied: FxHashMap<(i64, i64), LatticeOccupant>,
    attacks: FxHashMap<(i64, i64), AttackRecord>,
}

impl LatticeSpatialIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.occupied.clear();
        self.attacks.clear();
    }

    pub fn insert_occupied(
        &mut self,
        coord: (i64, i64),
        identity: LatticePieceIdentity,
    ) -> Option<LatticeOccupant> {
        self.occupied.insert(
            coord,
            LatticeOccupant {
                placement_id: identity.placement_id,
                identity,
            },
        )
    }

    pub fn add_attack(&mut self, coord: (i64, i64), identity: LatticePieceIdentity) {
        self.attacks
            .entry(coord)
            .and_modify(|record| record.add(identity))
            .or_insert_with(|| AttackRecord::new(identity));
    }

    #[must_use]
    pub fn get(&self, coord: (i64, i64)) -> Option<u64> {
        self.occupied
            .get(&coord)
            .map(|occupant| occupant.placement_id)
    }

    #[must_use]
    pub fn occupant_at(&self, coord: (i64, i64)) -> Option<LatticeOccupant> {
        self.occupied.get(&coord).copied()
    }

    #[must_use]
    pub fn contains(&self, coord: (i64, i64)) -> bool {
        self.occupied.contains_key(&coord)
    }

    #[must_use]
    pub fn is_attacked_by_enemy(
        &self,
        coord: (i64, i64),
        candidate: LatticePieceIdentity,
        enemy_mode: EnemyMode,
    ) -> bool {
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
    fn continuous_hash_finds_nearby_ids() {
        let mut index = ContinuousSpatialHash::new(1.0);
        index.insert(1, Point2::new(0.0, 0.0));
        index.insert(2, Point2::new(10.0, 10.0));

        let ids = index.nearby_ids(Point2::new(0.25, 0.25), 1.0);
        assert!(ids.contains(&1));
        assert!(!ids.contains(&2));
    }
}
