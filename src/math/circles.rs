use super::{GEOM_EPS, Point2};

#[must_use]
pub fn bodies_overlap(a: Point2, b: Point2, body_radius: f64) -> bool {
    let minimum_distance = (2.0 * body_radius.max(0.0)).max(0.0);
    a.distance(b) < minimum_distance - GEOM_EPS
}

#[must_use]
pub fn body_collision_allowed(a: Point2, b: Point2, body_radius: f64) -> bool {
    !bodies_overlap(a, b, body_radius)
}

#[must_use]
pub fn attack_radius_from_move(a: i32, b: i32) -> f64 {
    (a as f64).hypot(b as f64)
}

#[must_use]
pub fn attack_circle_hits_body(
    attacker: Point2,
    target: Point2,
    attack_radius: f64,
    body_radius: f64,
) -> bool {
    attack_circle_hits_body_distance_squared(
        attacker.squared_distance(target),
        attack_radius,
        body_radius,
    )
}

#[must_use]
pub fn attack_circle_hits_body_distance_squared(
    center_distance_squared: f64,
    attack_radius: f64,
    body_radius: f64,
) -> bool {
    if !center_distance_squared.is_finite()
        || !attack_radius.is_finite()
        || !body_radius.is_finite()
    {
        return false;
    }

    let tolerance = body_radius.max(0.0) + GEOM_EPS;
    let min_distance = (attack_radius - tolerance).max(0.0);
    let max_distance = (attack_radius + tolerance).max(0.0);
    center_distance_squared >= min_distance * min_distance
        && center_distance_squared <= max_distance * max_distance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::BODY_RADIUS;

    #[test]
    fn body_collision_accepts_tangency_and_rejects_overlap() {
        let origin = Point2::new(0.0, 0.0);
        assert!(body_collision_allowed(
            origin,
            Point2::new(1.0, 0.0),
            BODY_RADIUS
        ));
        assert!(!body_collision_allowed(
            origin,
            Point2::new(1.0 - 1.0e-9, 0.0),
            BODY_RADIUS
        ));
        assert!(body_collision_allowed(origin, Point2::new(0.5, 0.0), 0.25));
    }

    #[test]
    fn attack_circle_intersection_cases() {
        let attacker = Point2::new(0.0, 0.0);
        let radius = 2.0;

        assert!(attack_circle_hits_body(
            attacker,
            Point2::new(2.5, 0.0),
            radius,
            BODY_RADIUS
        ));
        assert!(attack_circle_hits_body(
            attacker,
            Point2::new(2.0, 0.0),
            radius,
            BODY_RADIUS
        ));
        assert!(attack_circle_hits_body(
            attacker,
            Point2::new(1.5, 0.0),
            radius,
            BODY_RADIUS
        ));
        assert!(!attack_circle_hits_body(
            attacker,
            Point2::new(2.500_000_1, 0.0),
            radius,
            BODY_RADIUS
        ));
        assert!(!attack_circle_hits_body(
            attacker,
            Point2::new(1.499_999_9, 0.0),
            radius,
            BODY_RADIUS
        ));
        assert!(!attack_circle_hits_body(
            attacker,
            Point2::new(2.26, 0.0),
            radius,
            0.25
        ));
    }

    #[test]
    fn attack_radius_comes_from_euclidean_move_vector() {
        assert_eq!(attack_radius_from_move(3, 4), 5.0);
    }
}
