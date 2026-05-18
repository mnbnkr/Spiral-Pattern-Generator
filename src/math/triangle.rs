use serde::{Deserialize, Serialize};

use super::Point2;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct TriangleCoord {
    pub u: i64,
    pub v: i64,
}

impl TriangleCoord {
    #[must_use]
    pub const fn new(u: i64, v: i64) -> Self {
        Self { u, v }
    }

    #[must_use]
    pub const fn add(self, other: Self) -> Self {
        Self::new(self.u + other.u, self.v + other.v)
    }

    #[must_use]
    pub const fn scale(self, factor: i64) -> Self {
        Self::new(self.u * factor, self.v * factor)
    }

    #[must_use]
    pub const fn cube(self) -> (i64, i64, i64) {
        (self.u, self.v, -self.u - self.v)
    }

    #[must_use]
    pub fn shell_radius(self) -> i64 {
        let (x, y, z) = self.cube();
        x.abs().max(y.abs()).max(z.abs())
    }

    #[must_use]
    pub fn to_point(self) -> Point2 {
        let u = self.u as f64;
        let v = self.v as f64;
        Point2::new(u + v / 2.0, 3.0_f64.sqrt() * 0.5 * v)
    }
}

const TRIANGLE_SPIRAL_DIRECTIONS: [TriangleCoord; 3] = [
    TriangleCoord { u: 1, v: 0 },
    TriangleCoord { u: -1, v: 1 },
    TriangleCoord { u: 0, v: -1 },
];

#[derive(Clone, Debug, Default)]
pub struct TriangleSpiral {
    emitted: u64,
    coord: TriangleCoord,
    direction_index: usize,
    segment_length: u64,
    segment_progress: u64,
}

impl TriangleSpiral {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn ring(radius: u64) -> Vec<TriangleCoord> {
        if radius == 0 {
            return vec![TriangleCoord::new(0, 0)];
        }

        let first_index = triangular_number(3 * (radius - 1)) + 1;
        let last_index = triangular_number(3 * radius);
        Self::new()
            .enumerate()
            .skip(first_index as usize)
            .take((last_index - first_index + 1) as usize)
            .map(|(_, coord)| coord)
            .collect()
    }

    #[must_use]
    pub fn coord_at_index(index: u64) -> TriangleCoord {
        if index == 0 {
            return TriangleCoord::new(0, 0);
        }

        let segment = segment_for_index(index);
        let previous_segments = segment - 1;
        let completed_cycles = previous_segments / 3;
        let remainder = previous_segments % 3;
        let mut coord = TriangleCoord::new(-(completed_cycles as i64), -(completed_cycles as i64));

        let first_length = 3 * completed_cycles + 1;
        if remainder >= 1 {
            coord = coord.add(TRIANGLE_SPIRAL_DIRECTIONS[0].scale(first_length as i64));
        }
        if remainder >= 2 {
            coord = coord.add(TRIANGLE_SPIRAL_DIRECTIONS[1].scale((first_length + 1) as i64));
        }

        let offset = index - triangular_number(segment - 1);
        coord.add(TRIANGLE_SPIRAL_DIRECTIONS[remainder as usize].scale(offset as i64))
    }

    #[must_use]
    pub fn radius_for_index(index: u64) -> u64 {
        if index == 0 {
            return 0;
        }

        segment_for_index(index).div_ceil(3)
    }
}

impl Iterator for TriangleSpiral {
    type Item = TriangleCoord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.emitted == 0 {
            self.emitted += 1;
            self.segment_length = 1;
            return Some(TriangleCoord::new(0, 0));
        }

        if self.segment_progress >= self.segment_length {
            self.direction_index = (self.direction_index + 1) % TRIANGLE_SPIRAL_DIRECTIONS.len();
            self.segment_length += 1;
            self.segment_progress = 0;
        }

        self.coord = self
            .coord
            .add(TRIANGLE_SPIRAL_DIRECTIONS[self.direction_index]);
        self.segment_progress += 1;
        self.emitted += 1;
        Some(self.coord)
    }
}

#[must_use]
fn triangular_number(n: u64) -> u64 {
    n.saturating_mul(n.saturating_add(1)) / 2
}

#[must_use]
fn segment_for_index(index: u64) -> u64 {
    let mut lo = 1_u64;
    let mut hi = 1_u64;
    while triangular_number(hi) < index {
        hi = hi.saturating_mul(2).max(2);
    }

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if triangular_number(mid) >= index {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }

    lo
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn triangle_rings_have_expected_counts_and_no_duplicates() {
        for radius in 0..8 {
            let ring = TriangleSpiral::ring(radius);
            let expected = if radius == 0 {
                1
            } else {
                (triangular_number(3 * radius) - triangular_number(3 * (radius - 1))) as usize
            };
            assert_eq!(ring.len(), expected);

            let unique: HashSet<_> = ring.iter().copied().collect();
            assert_eq!(unique.len(), expected);
        }
    }

    #[test]
    fn first_triangle_spiral_coordinates_use_triangular_turn_lengths() {
        let got: Vec<_> = TriangleSpiral::new().take(16).collect();
        let expected = vec![
            TriangleCoord::new(0, 0),
            TriangleCoord::new(1, 0),
            TriangleCoord::new(0, 1),
            TriangleCoord::new(-1, 2),
            TriangleCoord::new(-1, 1),
            TriangleCoord::new(-1, 0),
            TriangleCoord::new(-1, -1),
            TriangleCoord::new(0, -1),
            TriangleCoord::new(1, -1),
            TriangleCoord::new(2, -1),
            TriangleCoord::new(3, -1),
            TriangleCoord::new(2, 0),
            TriangleCoord::new(1, 1),
            TriangleCoord::new(0, 2),
            TriangleCoord::new(-1, 3),
            TriangleCoord::new(-2, 4),
        ];

        assert_eq!(got, expected);
    }

    #[test]
    fn triangle_spiral_turns_at_triangular_numbers() {
        let coords: Vec<_> = TriangleSpiral::new().take(22).collect();
        let deltas: Vec<_> = coords
            .windows(2)
            .map(|pair| TriangleCoord::new(pair[1].u - pair[0].u, pair[1].v - pair[0].v))
            .collect();

        assert_eq!(deltas[0], TRIANGLE_SPIRAL_DIRECTIONS[0]);
        assert_eq!(deltas[1], TRIANGLE_SPIRAL_DIRECTIONS[1]);
        assert_eq!(deltas[2], TRIANGLE_SPIRAL_DIRECTIONS[1]);
        assert_eq!(deltas[3], TRIANGLE_SPIRAL_DIRECTIONS[2]);
        assert_eq!(deltas[4], TRIANGLE_SPIRAL_DIRECTIONS[2]);
        assert_eq!(deltas[5], TRIANGLE_SPIRAL_DIRECTIONS[2]);
        assert_eq!(deltas[6], TRIANGLE_SPIRAL_DIRECTIONS[0]);
        assert_eq!(deltas[9], TRIANGLE_SPIRAL_DIRECTIONS[0]);
    }

    #[test]
    fn triangle_radius_follows_three_segment_shells() {
        let radii: Vec<_> = (0..=22).map(TriangleSpiral::radius_for_index).collect();
        assert_eq!(&radii[0..=6], &[0, 1, 1, 1, 1, 1, 1]);
        assert!(radii[7..=21].iter().all(|radius| *radius == 2));
        assert_eq!(radii[22], 3);
    }

    #[test]
    fn coord_at_index_matches_iterator() {
        for i in 0..256 {
            assert_eq!(
                TriangleSpiral::coord_at_index(i),
                TriangleSpiral::new().nth(i as usize).unwrap()
            );
        }
    }
}
