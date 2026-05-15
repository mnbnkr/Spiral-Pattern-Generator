use rustc_hash::FxHashMap;

use crate::math::Point2;

#[derive(Clone, Debug, Default)]
pub struct LatticeSpatialIndex {
    occupied: FxHashMap<(i64, i64), u64>,
    attacks: FxHashMap<(i64, i64), Vec<u64>>,
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

    pub fn insert_occupied(&mut self, coord: (i64, i64), placement_id: u64) -> Option<u64> {
        self.occupied.insert(coord, placement_id)
    }

    pub fn add_attack(&mut self, coord: (i64, i64), attacker_id: u64) {
        self.attacks.entry(coord).or_default().push(attacker_id);
    }

    #[must_use]
    pub fn get(&self, coord: (i64, i64)) -> Option<u64> {
        self.occupied.get(&coord).copied()
    }

    #[must_use]
    pub fn contains(&self, coord: (i64, i64)) -> bool {
        self.occupied.contains_key(&coord)
    }

    #[must_use]
    pub fn attackers_at(&self, coord: (i64, i64)) -> &[u64] {
        self.attacks
            .get(&coord)
            .map_or(&[] as &[u64], Vec::as_slice)
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
        assert!(!index.contains((3, -2)));
        index.insert_occupied((3, -2), 9);
        assert!(index.contains((3, -2)));
        assert_eq!(index.get((3, -2)), Some(9));
    }

    #[test]
    fn lattice_index_tracks_attack_cells() {
        let mut index = LatticeSpatialIndex::new();
        index.add_attack((2, 1), 7);
        index.add_attack((2, 1), 9);
        assert_eq!(index.attackers_at((2, 1)), &[7, 9]);
        assert!(index.attackers_at((0, 0)).is_empty());
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
