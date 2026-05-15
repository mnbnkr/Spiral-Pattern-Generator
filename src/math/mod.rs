pub mod archimedean;
pub mod circles;
pub mod constants;
pub mod hex;
pub mod point;
pub mod square;

pub use archimedean::{ArchimedeanSpiral, ArchimedeanSpots, ContinuousSpot, SolverError};
pub use circles::{
    attack_circle_hits_body, attack_circle_hits_body_distance_squared, attack_radius_from_move,
    bodies_overlap, body_collision_allowed,
};
pub use constants::{BODY_RADIUS, GEOM_EPS, PI, SOLVER_EPS, TAU, UNIT_TOUCH_DISTANCE};
pub use hex::{AxialCoord, HexSpiral};
pub use point::Point2;
pub use square::{SquareCoord, SquareSpiral};
