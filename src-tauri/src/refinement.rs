use crate::calibration::Point;
use std::collections::VecDeque;

const MAX_SAMPLES: usize = 16;
const REPLACE_RADIUS: f64 = 0.045;
const MAX_OFFSET: f64 = 0.15;
const MAX_OFFSET_SLOPE: f64 = 0.70;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrectionError {
    InvalidPoint,
    TooFar,
    ConflictsWithSample,
}

#[derive(Clone)]
struct Sample {
    predicted: Point,
    offset: Point,
}

#[derive(Clone, Default)]
pub struct Refinement {
    samples: VecDeque<Sample>,
}

impl Refinement {
    pub fn record(
        &mut self,
        predicted: Point,
        confirmed: Point,
        translation: Point,
    ) -> Result<(), CorrectionError> {
        if predicted
            .iter()
            .chain(&translation)
            .any(|axis| !axis.is_finite())
            || !on_screen(confirmed)
        {
            return Err(CorrectionError::InvalidPoint);
        }
        // Learn in the original nine-point prediction space so a later recenter keeps the sample.
        let offset =
            std::array::from_fn(|axis| confirmed[axis] - (predicted[axis] + translation[axis]));
        if offset[0].hypot(offset[1]) > MAX_OFFSET {
            return Err(CorrectionError::TooFar);
        }

        let keep = |sample: &Sample| distance(predicted, sample.predicted) >= REPLACE_RADIUS;
        let evict_oldest = self.samples.len() == MAX_SAMPLES && self.samples.iter().all(keep);
        let conflicts = self
            .samples
            .iter()
            .filter(|sample| keep(sample))
            .skip(usize::from(evict_oldest))
            .any(|sample| {
                max_axis_distance(offset, sample.offset)
                    > MAX_OFFSET_SLOPE * max_axis_distance(predicted, sample.predicted)
            });
        if conflicts {
            return Err(CorrectionError::ConflictsWithSample);
        }

        self.samples.retain(keep);
        if evict_oldest {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample { predicted, offset });
        Ok(())
    }

    pub fn correct(&self, point: Point) -> Point {
        if point.iter().any(|axis| !axis.is_finite()) {
            return point;
        }

        let mut lower = [f64::NEG_INFINITY; 2];
        let mut upper = [f64::INFINITY; 2];
        // Bound how fast offsets vary so a change in gaze cannot reverse pointer movement.
        for sample in &self.samples {
            let margin = MAX_OFFSET_SLOPE * max_axis_distance(point, sample.predicted);
            for axis in 0..2 {
                lower[axis] = lower[axis].max(sample.offset[axis] - margin);
                upper[axis] = upper[axis].min(sample.offset[axis] + margin);
            }
        }
        // Use the smallest offset permitted by the confirmed samples on each axis.
        std::array::from_fn(|axis| point[axis] + 0.0_f64.max(lower[axis]).min(upper[axis]))
    }
}

fn on_screen(point: Point) -> bool {
    point.iter().all(|axis| (0.0..=1.0).contains(axis))
}

fn distance(left: Point, right: Point) -> f64 {
    (left[0] - right[0]).hypot(left[1] - right[1])
}

fn max_axis_distance(left: Point, right: Point) -> f64 {
    (left[0] - right[0]).abs().max((left[1] - right[1]).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn improves_nearby_predictions_and_fades_to_zero_at_a_distance() {
        let mut refinement = Refinement::default();
        let anchor = [0.42, 0.53];
        let confirmed = [0.49, 0.50];
        refinement.record(anchor, confirmed, [0.0; 2]).unwrap();
        assert_eq!(refinement.correct(anchor), confirmed);

        let nearby = [0.433, 0.517];
        let expected = [0.503, 0.487];
        assert!(distance(refinement.correct(nearby), expected) < distance(nearby, expected) / 4.0);
        for point in [[0.61, 0.53], [0.75, 0.80]] {
            assert_eq!(refinement.correct(point), point);
        }
    }

    #[test]
    fn replaces_nearby_samples_without_stacking_offsets() {
        let mut refinement = Refinement::default();
        refinement
            .record([0.40, 0.50], [0.50, 0.50], [0.0; 2])
            .unwrap();
        refinement
            .record([0.42, 0.51], [0.38, 0.51], [0.0; 2])
            .unwrap();
        assert_eq!(refinement.samples.len(), 1);
        assert_eq!(refinement.correct([0.42, 0.51]), [0.38, 0.51]);
        assert!(refinement.correct([0.40, 0.50])[0] < 0.40);
    }

    #[test]
    fn evicts_old_samples_and_bounds_the_combined_offset() {
        let mut refinement = Refinement::default();
        refinement
            .record([0.10, 0.10], [0.20, 0.10], [0.0; 2])
            .unwrap();
        for index in 0..MAX_SAMPLES {
            let x = 0.30 + (index % 4) as f64 * 0.09;
            let y = 0.55 + (index / 4) as f64 * 0.09;
            refinement
                .record([x, y], [x + 0.12, y + 0.07], [0.0; 2])
                .unwrap();
        }
        assert_eq!(refinement.samples.len(), MAX_SAMPLES);
        assert_eq!(refinement.correct([0.10, 0.10]), [0.10, 0.10]);

        let point = [0.425, 0.685];
        assert!(distance(point, refinement.correct(point)) <= MAX_OFFSET);
    }

    #[test]
    fn records_a_visible_target_near_a_shifted_screen_edge() {
        let mut refinement = Refinement::default();
        let point = [-0.05, 0.5];
        refinement.record(point, [0.01, 0.5], [0.04, 0.0]).unwrap();
        assert!((refinement.correct(point)[0] + 0.04 - 0.01).abs() < 1e-12);
    }

    #[test]
    fn rejects_outliers_and_invalid_points_without_changing_the_mapping() {
        let mut refinement = Refinement::default();
        let predicted = [0.40, 0.50];
        let confirmed = [0.47, 0.48];
        refinement.record(predicted, confirmed, [0.0; 2]).unwrap();
        assert_eq!(
            refinement.record(predicted, [0.51, 0.61], [0.0; 2]),
            Err(CorrectionError::TooFar)
        );
        for point in [[f64::NAN, 0.0], [0.0, f64::INFINITY], [1.01, 0.5]] {
            assert_eq!(
                refinement.record(predicted, point, [0.0; 2]),
                Err(CorrectionError::InvalidPoint)
            );
        }
        assert_eq!(
            refinement.record([f64::NAN, 0.0], confirmed, [0.0; 2]),
            Err(CorrectionError::InvalidPoint)
        );
        assert_eq!(refinement.samples.len(), 1);
        assert_eq!(refinement.correct(predicted), confirmed);
    }

    #[test]
    fn strong_local_corrections_keep_pointer_movement_monotone_on_both_axes() {
        for axis in 0..2 {
            let position = |value| {
                let mut point = [0.50; 2];
                point[axis] = value;
                point
            };
            let mut refinement = Refinement::default();
            refinement
                .record(position(0.40), position(0.54), [0.0; 2])
                .unwrap();
            assert_eq!(refinement.correct(position(0.40)), position(0.54));
            let mut previous = refinement.correct(position(0.44));
            for coordinate in [0.45, 0.48, 0.50, 0.55, 0.60, 0.65] {
                let corrected = refinement.correct(position(coordinate));
                assert!(corrected[axis] > previous[axis]);
                assert_eq!(corrected[1 - axis], 0.50);
                previous = corrected;
            }
        }
    }

    #[test]
    fn rejects_a_conflict_without_removing_the_sample_it_would_replace() {
        let mut refinement = Refinement::default();
        let first = [0.40, 0.50];
        let second = [0.50, 0.50];
        refinement.record(first, [0.48, 0.50], [0.0; 2]).unwrap();
        refinement.record(second, [0.56, 0.50], [0.0; 2]).unwrap();
        assert_eq!(
            refinement.record([0.41, 0.50], [0.34, 0.50], [0.0; 2]),
            Err(CorrectionError::ConflictsWithSample)
        );
        assert_eq!(refinement.correct(first), [0.48, 0.50]);
        assert_eq!(refinement.correct(second), [0.56, 0.50]);
    }
}
