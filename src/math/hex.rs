use serde::{Deserialize, Serialize};

use super::Point2;

const AXIAL_DIRECTIONS: [AxialCoord; 6] = [
    AxialCoord { q: 1, r: 0 },
    AxialCoord { q: 0, r: 1 },
    AxialCoord { q: -1, r: 1 },
    AxialCoord { q: -1, r: 0 },
    AxialCoord { q: 0, r: -1 },
    AxialCoord { q: 1, r: -1 },
];

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct AxialCoord {
    pub q: i64,
    pub r: i64,
}

impl AxialCoord {
    #[must_use]
    pub const fn new(q: i64, r: i64) -> Self {
        Self { q, r }
    }

    #[must_use]
    pub const fn add(self, other: Self) -> Self {
        Self::new(self.q + other.q, self.r + other.r)
    }

    #[must_use]
    pub const fn scale(self, factor: i64) -> Self {
        Self::new(self.q * factor, self.r * factor)
    }

    #[must_use]
    pub const fn cube(self) -> (i64, i64, i64) {
        (self.q, self.r, -self.q - self.r)
    }

    #[must_use]
    pub fn to_point(self) -> Point2 {
        let q = self.q as f64;
        let r = self.r as f64;
        Point2::new(3.0_f64.sqrt() * (q + r / 2.0), 1.5 * r)
    }
}

#[derive(Clone, Debug, Default)]
pub struct HexSpiral {
    emitted: u64,
    current_ring: u64,
    ring: Vec<AxialCoord>,
    ring_index: usize,
}

impl HexSpiral {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn ring(radius: u64) -> Vec<AxialCoord> {
        if radius == 0 {
            return vec![AxialCoord::new(0, 0)];
        }

        let r = radius as i64;
        let mut coord = AxialCoord::new(r, 1 - r);
        let mut out = Vec::with_capacity((radius * 6) as usize);
        out.push(coord);

        for _ in 0..radius.saturating_sub(1) {
            coord = coord.add(AXIAL_DIRECTIONS[1]);
            out.push(coord);
        }

        for direction in AXIAL_DIRECTIONS
            .iter()
            .skip(2)
            .chain(AXIAL_DIRECTIONS.iter().take(1))
        {
            for _ in 0..radius {
                coord = coord.add(*direction);
                out.push(coord);
            }
        }

        out.truncate((radius * 6) as usize);
        out
    }

    #[must_use]
    pub fn coord_at_index(index: u64) -> AxialCoord {
        if index == 0 {
            return AxialCoord::new(0, 0);
        }

        let mut radius = (((index as f64) / 3.0).sqrt().ceil() as u64).max(1);
        while hex_ring_max_index(radius) < index {
            radius += 1;
        }
        while radius > 0 && hex_ring_max_index(radius - 1) >= index {
            radius -= 1;
        }

        let ring_start = hex_ring_max_index(radius - 1) + 1;
        Self::coord_at_ring_offset(radius, index - ring_start)
    }

    #[must_use]
    fn coord_at_ring_offset(radius: u64, offset: u64) -> AxialCoord {
        let r = radius as i64;
        let mut coord = AxialCoord::new(r, 1 - r);
        if offset == 0 {
            return coord;
        }

        let first_side_remaining = radius - 1;
        if offset <= first_side_remaining {
            return coord.add(AXIAL_DIRECTIONS[1].scale(offset as i64));
        }
        coord = coord.add(AXIAL_DIRECTIONS[1].scale(first_side_remaining as i64));
        let mut remaining = offset - first_side_remaining;

        for direction in AXIAL_DIRECTIONS
            .iter()
            .skip(2)
            .chain(AXIAL_DIRECTIONS.iter().take(1))
        {
            let step = remaining.min(radius);
            coord = coord.add(direction.scale(step as i64));
            if remaining <= radius {
                return coord;
            }
            remaining -= radius;
        }

        coord
    }
}

#[must_use]
fn hex_ring_max_index(radius: u64) -> u64 {
    3_u64
        .saturating_mul(radius)
        .saturating_mul(radius.saturating_add(1))
}

impl Iterator for HexSpiral {
    type Item = AxialCoord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.emitted == 0 {
            self.emitted += 1;
            return Some(AxialCoord::new(0, 0));
        }

        if self.ring_index >= self.ring.len() {
            self.current_ring += 1;
            self.ring = Self::ring(self.current_ring);
            self.ring_index = 0;
        }

        let coord = self.ring[self.ring_index];
        self.ring_index += 1;
        self.emitted += 1;
        Some(coord)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn hex_rings_have_expected_counts_and_no_duplicates() {
        for radius in 0..8 {
            let ring = HexSpiral::ring(radius);
            let expected = if radius == 0 { 1 } else { radius * 6 } as usize;
            assert_eq!(ring.len(), expected);

            let unique: HashSet<_> = ring.iter().copied().collect();
            assert_eq!(unique.len(), expected);
        }
    }

    #[test]
    fn first_hex_spiral_coordinates_start_right_and_turn_counterclockwise() {
        let got: Vec<_> = HexSpiral::new().take(12).collect();
        let expected = vec![
            AxialCoord::new(0, 0),
            AxialCoord::new(1, 0),
            AxialCoord::new(0, 1),
            AxialCoord::new(-1, 1),
            AxialCoord::new(-1, 0),
            AxialCoord::new(0, -1),
            AxialCoord::new(1, -1),
            AxialCoord::new(2, -1),
            AxialCoord::new(2, 0),
            AxialCoord::new(1, 1),
            AxialCoord::new(0, 2),
            AxialCoord::new(-1, 2),
        ];

        assert_eq!(got, expected);
    }

    #[test]
    fn hex_spiral_ring_transitions_are_adjacent() {
        let got: Vec<_> = HexSpiral::new().take(128).collect();
        for pair in got.windows(2) {
            let delta = AxialCoord::new(pair[1].q - pair[0].q, pair[1].r - pair[0].r);
            assert!(
                AXIAL_DIRECTIONS.contains(&delta),
                "non-adjacent transition from {:?} to {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn coord_at_index_matches_iterator() {
        for i in 0..256 {
            assert_eq!(
                HexSpiral::coord_at_index(i),
                HexSpiral::new().nth(i as usize).unwrap()
            );
        }
    }
}
