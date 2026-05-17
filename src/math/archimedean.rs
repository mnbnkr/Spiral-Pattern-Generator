use serde::{Deserialize, Serialize};

use super::constants::{MAX_BISECTION_ITERS, MAX_NEWTON_ITERS};
use super::{Point2, SOLVER_EPS, TAU, UNIT_TOUCH_DISTANCE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolverError {
    NonPositiveDistance,
    NonFiniteInput,
    BracketFailed,
    ConvergenceFailed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContinuousSpot {
    pub index: u64,
    pub theta: f64,
    pub center: Point2,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ArchimedeanSpiral;

impl ArchimedeanSpiral {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn radius(theta: f64) -> f64 {
        theta / TAU
    }

    #[must_use]
    pub fn position(theta: f64) -> Point2 {
        let r = Self::radius(theta);
        Point2::new(r * theta.cos(), r * theta.sin())
    }

    #[must_use]
    pub fn derivative(theta: f64) -> Point2 {
        let inv_tau = 1.0 / TAU;
        Point2::new(
            inv_tau * theta.cos() - Self::radius(theta) * theta.sin(),
            inv_tau * theta.sin() + Self::radius(theta) * theta.cos(),
        )
    }

    #[must_use]
    pub fn squared_distance(theta0: f64, theta1: f64) -> f64 {
        let r0 = Self::radius(theta0);
        let r1 = Self::radius(theta1);
        let dr = r1 - r0;
        let half_delta = 0.5 * (theta1 - theta0);
        dr.mul_add(dr, 4.0 * r0 * r1 * half_delta.sin().powi(2))
    }

    #[must_use]
    pub fn squared_distance_derivative(theta0: f64, theta1: f64) -> f64 {
        let inv_tau = 1.0 / TAU;
        let r0 = Self::radius(theta0);
        let r1 = Self::radius(theta1);
        let delta = theta1 - theta0;
        let sin_half = (0.5 * delta).sin();
        let sin_half_squared = sin_half * sin_half;
        2.0 * (r1 - r0) * inv_tau
            + 4.0 * r0 * inv_tau * sin_half_squared
            + 2.0 * r0 * r1 * delta.sin()
    }

    #[must_use]
    pub fn arc_length_from_origin(theta: f64) -> f64 {
        let theta = theta.max(0.0);
        (theta * (theta.mul_add(theta, 1.0)).sqrt() + theta.asinh()) / (2.0 * TAU)
    }

    pub fn theta_for_arc_length_from_origin(distance: f64) -> Result<f64, SolverError> {
        if !distance.is_finite() {
            return Err(SolverError::NonFiniteInput);
        }

        if distance < 0.0 {
            return Err(SolverError::NonPositiveDistance);
        }

        if distance == 0.0 {
            return Ok(0.0);
        }

        let mut low = 0.0;
        let mut high = TAU.max(distance * TAU);
        let mut expansions = 0;
        while Self::arc_length_from_origin(high) < distance && expansions < 64 {
            high *= 2.0;
            expansions += 1;
        }

        if Self::arc_length_from_origin(high) < distance {
            return Err(SolverError::BracketFailed);
        }

        for _ in 0..MAX_BISECTION_ITERS {
            let mid = 0.5 * (low + high);
            let value = Self::arc_length_from_origin(mid);
            if (value - distance).abs() <= SOLVER_EPS {
                return Ok(mid);
            }

            if value > distance {
                high = mid;
            } else {
                low = mid;
            }
        }

        Ok(0.5 * (low + high))
    }

    pub fn theta_for_chord_from(theta0: f64, distance: f64) -> Result<f64, SolverError> {
        if !theta0.is_finite() || !distance.is_finite() {
            return Err(SolverError::NonFiniteInput);
        }

        if distance <= 0.0 {
            return Err(SolverError::NonPositiveDistance);
        }

        let target = distance * distance;
        let mut low = theta0;
        let mut high = Self::initial_high(theta0, distance);
        let mut high_value = Self::squared_distance(theta0, high) - target;
        let mut expansions = 0;

        while high_value < 0.0 && expansions < 96 {
            let width = high - low;
            high += width.max(1.0e-9);
            high_value = Self::squared_distance(theta0, high) - target;
            expansions += 1;
        }

        if high_value < 0.0 {
            return Err(SolverError::BracketFailed);
        }

        let mut theta = theta0 + (high - theta0).min(Self::local_step_estimate(theta0, distance));
        theta = theta.clamp(low, high);
        if theta <= low || theta >= high {
            theta = 0.5 * (low + high);
        }

        let mut best_theta = theta;
        let mut best_error = f64::INFINITY;

        for _ in 0..MAX_NEWTON_ITERS {
            let value = Self::squared_distance(theta0, theta) - target;
            let error = value.abs();
            if error < best_error {
                best_error = error;
                best_theta = theta;
            }
            if error <= SOLVER_EPS * target.max(1.0) {
                return Ok(theta);
            }

            if value > 0.0 {
                high = theta;
            } else {
                low = theta;
            }

            let derivative = Self::squared_distance_derivative(theta0, theta);
            let candidate = if derivative.abs() > f64::EPSILON && derivative.is_finite() {
                theta - value / derivative
            } else {
                f64::NAN
            };

            theta = if candidate.is_finite() && candidate > low && candidate < high {
                candidate
            } else {
                0.5 * (low + high)
            };
        }

        for _ in 0..MAX_BISECTION_ITERS {
            theta = 0.5 * (low + high);
            if theta <= low || theta >= high {
                return Ok(best_theta);
            }

            let value = Self::squared_distance(theta0, theta) - target;
            let error = value.abs();
            if error < best_error {
                best_error = error;
                best_theta = theta;
            }

            if error <= SOLVER_EPS * target.max(1.0) {
                return Ok(theta);
            }
            if (high - low).abs() <= f64::EPSILON * high.abs().max(1.0) {
                return Ok(best_theta);
            }

            if value > 0.0 {
                high = theta;
            } else {
                low = theta;
            }
        }

        Ok(best_theta)
    }

    #[must_use]
    pub fn spots(offset: f64) -> ArchimedeanSpots {
        ArchimedeanSpots::new(offset)
    }

    fn local_step_estimate(theta0: f64, distance: f64) -> f64 {
        let speed = Self::derivative(theta0).radius().max(1.0 / TAU);
        (distance / speed).clamp(1.0e-9, TAU)
    }

    fn initial_high(theta0: f64, distance: f64) -> f64 {
        theta0 + Self::local_step_estimate(theta0, distance).max(1.0e-9)
    }
}

#[derive(Clone, Debug)]
pub struct ArchimedeanSpots {
    next_index: u64,
    next_theta: f64,
    failed: bool,
}

impl ArchimedeanSpots {
    #[must_use]
    pub fn new(offset: f64) -> Self {
        let clamped = offset.clamp(0.0, 1.0);
        let next_theta =
            ArchimedeanSpiral::theta_for_arc_length_from_origin(clamped).unwrap_or(clamped * TAU);

        Self {
            next_index: 0,
            next_theta,
            failed: false,
        }
    }
}

impl Iterator for ArchimedeanSpots {
    type Item = ContinuousSpot;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }

        let theta = self.next_theta;
        let spot = ContinuousSpot {
            index: self.next_index,
            theta,
            center: ArchimedeanSpiral::position(theta),
        };

        match ArchimedeanSpiral::theta_for_chord_from(theta, UNIT_TOUCH_DISTANCE) {
            Ok(next_theta) => {
                self.next_theta = next_theta;
                self.next_index += 1;
                Some(spot)
            }
            Err(_) => {
                self.failed = true;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archimedean_one_turn_radial_spacing_is_one() {
        for theta in [0.0, 1.25, TAU, 17.0] {
            let r0 = ArchimedeanSpiral::position(theta).radius();
            let r1 = ArchimedeanSpiral::position(theta + TAU).radius();
            assert!(((r1 - r0) - 1.0).abs() <= 1.0e-12);
        }
    }

    #[test]
    fn chord_solver_produces_unit_distance() {
        for theta0 in [0.0, 0.5, TAU, 11.7, 40.0] {
            let theta1 =
                ArchimedeanSpiral::theta_for_chord_from(theta0, UNIT_TOUCH_DISTANCE).unwrap();
            let distance =
                ArchimedeanSpiral::position(theta0).distance(ArchimedeanSpiral::position(theta1));
            assert!(
                (distance - UNIT_TOUCH_DISTANCE).abs() <= 1.0e-12,
                "theta0={theta0}, theta1={theta1}, distance={distance}"
            );
        }
    }

    #[test]
    fn chord_solver_handles_large_theta_precision_limit() {
        let theta0 = 511.990_204_480_847_35;
        let theta1 = ArchimedeanSpiral::theta_for_chord_from(theta0, UNIT_TOUCH_DISTANCE).unwrap();
        let distance =
            ArchimedeanSpiral::position(theta0).distance(ArchimedeanSpiral::position(theta1));
        assert!((distance - UNIT_TOUCH_DISTANCE).abs() <= 1.0e-10);
    }

    #[test]
    fn chord_solver_handles_very_high_radius_spots() {
        for radius in [1_500.0, 3_000.0, 5_000.0] {
            let theta0 = radius * TAU;
            let theta1 =
                ArchimedeanSpiral::theta_for_chord_from(theta0, UNIT_TOUCH_DISTANCE).unwrap();
            let distance = ArchimedeanSpiral::squared_distance(theta0, theta1).sqrt();
            assert!(
                (distance - UNIT_TOUCH_DISTANCE).abs() <= 2.0e-8,
                "radius={radius}, theta0={theta0}, theta1={theta1}, distance={distance}"
            );
        }
    }

    #[test]
    fn offsets_are_fractional_initial_arc_lengths() {
        for offset in [0.0, 0.5, 1.0] {
            let first = ArchimedeanSpots::new(offset).next().unwrap();
            let arc_length = ArchimedeanSpiral::arc_length_from_origin(first.theta);
            assert!((arc_length - offset).abs() <= 1.0e-12);
        }
    }

    #[test]
    fn iterator_uses_unit_chord_steps() {
        let spots: Vec<_> = ArchimedeanSpots::new(0.25).take(10).collect();
        for pair in spots.windows(2) {
            let distance = pair[0].center.distance(pair[1].center);
            assert!((distance - 1.0).abs() <= 1.0e-12);
        }
    }
}
