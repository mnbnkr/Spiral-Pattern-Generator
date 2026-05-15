use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub fn squared_distance(self, other: Self) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx.mul_add(dx, dy * dy)
    }

    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        self.squared_distance(other).sqrt()
    }

    #[must_use]
    pub fn radius(self) -> f64 {
        self.x.hypot(self.y)
    }
}
