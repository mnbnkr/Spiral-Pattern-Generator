use serde::{Deserialize, Serialize};

use super::Point2;

const AXIAL_DIRECTIONS: [AxialCoord; 6] = [
    AxialCoord { q: 1, r: 0 },
    AxialCoord { q: 1, r: -1 },
    AxialCoord { q: 0, r: -1 },
    AxialCoord { q: -1, r: 0 },
    AxialCoord { q: -1, r: 1 },
    AxialCoord { q: 0, r: 1 },
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

        let radius_i64 = radius as i64;
        let mut coord = AXIAL_DIRECTIONS[4].scale(radius_i64);
        let mut out = Vec::with_capacity((radius * 6) as usize);

        for direction in AXIAL_DIRECTIONS {
            for _ in 0..radius {
                out.push(coord);
                coord = coord.add(direction);
            }
        }

        out
    }

    #[must_use]
    pub fn coord_at_index(index: u64) -> AxialCoord {
        Self::new()
            .nth(index as usize)
            .expect("hex spiral is infinite")
    }
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
    fn first_hex_spiral_coordinates_are_deterministic() {
        let got: Vec<_> = HexSpiral::new().take(8).collect();
        let expected = vec![
            AxialCoord::new(0, 0),
            AxialCoord::new(-1, 1),
            AxialCoord::new(0, 1),
            AxialCoord::new(1, 0),
            AxialCoord::new(1, -1),
            AxialCoord::new(0, -1),
            AxialCoord::new(-1, 0),
            AxialCoord::new(-2, 2),
        ];

        assert_eq!(got, expected);
    }
}
