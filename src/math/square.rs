use serde::{Deserialize, Serialize};

use super::Point2;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SquareCoord {
    pub x: i64,
    pub y: i64,
}

impl SquareCoord {
    #[must_use]
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub fn to_point(self) -> Point2 {
        Point2::new(self.x as f64, self.y as f64)
    }
}

#[derive(Clone, Debug)]
pub struct SquareSpiral {
    coord: SquareCoord,
    dir: (i64, i64),
    leg_len: u64,
    leg_progress: u64,
    legs_at_len: u8,
    emit_origin: bool,
}

impl Default for SquareSpiral {
    fn default() -> Self {
        Self {
            coord: SquareCoord::new(0, 0),
            dir: (1, 0),
            leg_len: 1,
            leg_progress: 0,
            legs_at_len: 0,
            emit_origin: true,
        }
    }
}

impl SquareSpiral {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn coord_at_index(index: u64) -> SquareCoord {
        Self::new()
            .nth(index as usize)
            .expect("square spiral is infinite")
    }

    fn rotate_counter_clockwise(&mut self) {
        let (dx, dy) = self.dir;
        self.dir = (-dy, dx);
    }
}

impl Iterator for SquareSpiral {
    type Item = SquareCoord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.emit_origin {
            self.emit_origin = false;
            return Some(self.coord);
        }

        self.coord.x += self.dir.0;
        self.coord.y += self.dir.1;
        self.leg_progress += 1;

        if self.leg_progress == self.leg_len {
            self.leg_progress = 0;
            self.legs_at_len += 1;
            self.rotate_counter_clockwise();

            if self.legs_at_len == 2 {
                self.legs_at_len = 0;
                self.leg_len += 1;
            }
        }

        Some(self.coord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_square_spiral_coordinates_match_reference() {
        let got: Vec<_> = SquareSpiral::new().take(12).collect();
        let expected = vec![
            SquareCoord::new(0, 0),
            SquareCoord::new(1, 0),
            SquareCoord::new(1, 1),
            SquareCoord::new(0, 1),
            SquareCoord::new(-1, 1),
            SquareCoord::new(-1, 0),
            SquareCoord::new(-1, -1),
            SquareCoord::new(0, -1),
            SquareCoord::new(1, -1),
            SquareCoord::new(2, -1),
            SquareCoord::new(2, 0),
            SquareCoord::new(2, 1),
        ];

        assert_eq!(got, expected);
    }

    #[test]
    fn coord_at_index_matches_iterator() {
        for i in 0..128 {
            assert_eq!(
                SquareSpiral::coord_at_index(i),
                SquareSpiral::new().nth(i as usize).unwrap()
            );
        }
    }
}
