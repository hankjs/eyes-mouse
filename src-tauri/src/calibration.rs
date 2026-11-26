use crate::refinement::{CorrectionError, Refinement};
use serde::Serialize;
use std::collections::VecDeque;

pub type Point = [f64; 2];
pub type Features = [f64; 4];

pub const CALIBRATION_POINTS: [Point; 9] = [
    [0.50, 0.50],
    [0.10, 0.18],
    [0.50, 0.18],
    [0.90, 0.18],
    [0.90, 0.50],
    [0.90, 0.82],
    [0.50, 0.82],
    [0.10, 0.82],
    [0.10, 0.50],
];

pub const VALIDATION_POINTS: [Point; 5] = [
    [0.27, 0.34],
    [0.73, 0.64],
    [0.73, 0.34],
    [0.27, 0.64],
    [0.62, 0.50],
];

pub const RESET_POINTS: [Point; 3] = [[0.50, 0.50], [0.13, 0.16], [0.87, 0.84]];

const MAX_RESET_SHIFT: f64 = 0.12;
const MAX_RESET_CORNER_ERROR: f64 = 0.12;
const RESET_CORNER_NOISE: f64 = 0.025;
const RESET_ERROR: &str =
    "三个注视位置与九点校准偏差过大，已保留原校准。请保持头部稳定后重试，或重新做九点校准。";

#[derive(Clone)]
pub struct Mapping {
    center: Features,
    scale: Features,
    coefficients: [Point; 5],
    offset: Point,
    pointer_offset: Point,
    refinement: Refinement,
    eye_pose: Point,
}

impl Mapping {
    pub fn fit(samples: &[(Features, Point)]) -> Result<Self, &'static str> {
        let fit_error = "这次校准位置偏差较大。请坐稳，并在每个点出现后看准再按空格。";
        if samples.is_empty()
            || samples.iter().any(|(features, point)| {
                features.iter().chain(point).any(|value| !value.is_finite())
            })
        {
            return Err(fit_error);
        }
        let mut center = [0.0; 4];
        let mut scale = [0.0; 4];
        for i in 0..4 {
            center[i] =
                samples.iter().map(|(sample, _)| sample[i]).sum::<f64>() / samples.len() as f64;
            let variance = samples
                .iter()
                .map(|(sample, _)| (sample[i] - center[i]).powi(2))
                .sum::<f64>()
                / samples.len() as f64;
            // Keep tracker noise and tiny head movements from dominating the fit.
            let floor: f64 = if i < 2 { 0.012 } else { 0.12 };
            scale[i] = variance.sqrt().max(floor).recip();
        }

        if samples
            .iter()
            .all(|(features, _)| (features[0] - center[0]).abs() < 0.012)
            || samples
                .iter()
                .all(|(features, _)| (features[1] - center[1]).abs() < 0.008)
        {
            return Err("眼动变化太小，无法完成校准。请靠近一些，重新看向每个目标。");
        }

        let mut mapping = Self {
            center,
            scale,
            coefficients: [[0.0; 2]; 5],
            offset: [0.0; 2],
            pointer_offset: [0.0; 2],
            refinement: Refinement::default(),
            eye_pose: [center[2], center[3]],
        };
        let mut equations = [[0.0; 7]; 5];
        for (features, point) in samples {
            let row = mapping.design_row(*features);
            for i in 0..5 {
                for j in 0..5 {
                    equations[i][j] += row[i] * row[j];
                }
                for axis in 0..2 {
                    equations[i][5 + axis] += row[i] * point[axis];
                }
            }
        }

        for (i, row) in equations.iter_mut().enumerate().skip(1) {
            // Penalize head-pose coefficients more than the eye coefficients.
            row[i] += if i < 3 { 0.25 } else { 1.0 };
        }
        mapping.coefficients = solve(equations).ok_or("无法拟合屏幕坐标，请重新校准。")?;

        let mut squared_error = 0.0;
        for &(features, point) in samples {
            let [x, y] = mapping.predict(features);
            let error = (x - point[0]).hypot(y - point[1]);
            // A single missed target can hide in the aggregate error.
            if !error.is_finite() || error > 0.28 {
                return Err(fit_error);
            }
            squared_error += error.powi(2);
        }
        if (squared_error / samples.len() as f64).sqrt() > 0.20 {
            return Err(fit_error);
        }
        Ok(mapping)
    }

    pub fn reset_center(&self, samples: &[(Features, Point); 3]) -> Result<Self, &'static str> {
        let [(center, target), (upper_left, _), (lower_right, _)] = *samples;
        if !reset_corner(center, upper_left, 1) || !reset_corner(center, lower_right, 2) {
            return Err(RESET_ERROR);
        }

        // Use the nine-point fit for gestures and the locally corrected fit for the pointer.
        let base = self.predict_linear(center);
        let corrected = self.refinement.correct(base);
        let offset = std::array::from_fn(|axis| target[axis] - base[axis]);
        let pointer_offset = std::array::from_fn(|axis| target[axis] - corrected[axis]);
        if !within_distance(offset, [0.0; 2], MAX_RESET_SHIFT)
            || !within_distance(pointer_offset, [0.0; 2], MAX_RESET_SHIFT)
        {
            return Err(RESET_ERROR);
        }
        let mut mapping = self.clone();
        mapping.offset = offset;
        mapping.pointer_offset = pointer_offset;
        mapping.eye_pose = [center[2], center[3]];
        let translations = [
            (offset, self.offset),
            (offset, self.offset),
            (pointer_offset, self.pointer_offset),
        ];
        let mut corner_offsets = [[0.0; 2]; 3];
        for &(features, target) in &samples[1..] {
            // Check the base fit, the center-pose eye prediction and the unclamped pointer.
            let predictions = [
                mapping.predict(features),
                mapping.eye_point(features),
                mapping.unclamped_pointer_point(features),
            ];
            for ((corner_offset, prediction), (translation, _)) in
                corner_offsets.iter_mut().zip(predictions).zip(translations)
            {
                if !within_distance(prediction, target, MAX_RESET_CORNER_ERROR) {
                    return Err(RESET_ERROR);
                }
                for axis in 0..2 {
                    corner_offset[axis] +=
                        (target[axis] - prediction[axis] + translation[axis]) / 2.0;
                }
            }
        }
        // The mean corner offset minimizes the squared error for a common translation.
        // Allow small differences from it, and require larger differences to improve the fit.
        if corner_offsets.into_iter().zip(translations).any(
            |(corner_offset, (current, previous))| {
                let error = distance(current, corner_offset);
                error > RESET_CORNER_NOISE && error >= distance(previous, corner_offset)
            },
        ) {
            return Err(RESET_ERROR);
        }
        Ok(mapping)
    }

    fn design_row(&self, features: Features) -> [f64; 5] {
        let mut row = [1.0; 5];
        for i in 0..4 {
            row[i + 1] = (features[i] - self.center[i]) * self.scale[i];
        }
        row
    }

    pub fn predict(&self, features: Features) -> Point {
        let [x, y] = self.predict_linear(features);
        [x + self.offset[0], y + self.offset[1]]
    }

    pub fn eye_point(&self, mut features: Features) -> Point {
        features[2..].copy_from_slice(&self.eye_pose);
        self.predict(features)
    }

    pub fn pointer_point(&self, features: Features) -> Point {
        self.unclamped_pointer_point(features)
            .map(|axis| axis.clamp(0.0, 1.0))
    }

    pub fn refine(&mut self, features: Features, confirmed: Point) -> Result<(), CorrectionError> {
        let predicted = self.predict_linear(features);
        self.refinement
            .record(predicted, confirmed, self.pointer_offset)
    }

    fn unclamped_pointer_point(&self, features: Features) -> Point {
        let point = self.refinement.correct(self.predict_linear(features));
        std::array::from_fn(|axis| point[axis] + self.pointer_offset[axis])
    }

    fn predict_linear(&self, features: Features) -> Point {
        let mut point = [0.0; 2];
        for (value, coefficient) in self.design_row(features).iter().zip(self.coefficients) {
            for axis in 0..2 {
                point[axis] += value * coefficient[axis];
            }
        }
        point
    }
}

fn within_distance(a: Point, b: Point, limit: f64) -> bool {
    distance(a, b) <= limit
}

fn distance(a: Point, b: Point) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

pub fn reset_corner(center: Features, sample: Features, index: usize) -> bool {
    let direction = if index == 1 { 1.0 } else { -1.0 };
    direction * (sample[0] - center[0]) >= 0.012
        && direction * (center[1] - sample[1]) >= 0.009
        && (sample[2] - center[2]).hypot(sample[3] - center[3]) <= 0.075
}

#[derive(Default)]
pub struct Fixation {
    frames: VecDeque<(u64, Features)>,
}

impl Fixation {
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn observe(&mut self, timestamp: u64, features: Option<Features>) {
        // At an eye width of 30 pixels, a 0.020 span allows 0.6 pixels of iris jitter.
        const MAX_RANGE: Features = [0.020, 0.020, 0.075, 0.075];
        let Some(features) =
            features.filter(|features| features.iter().all(|value| value.is_finite()))
        else {
            self.clear();
            return;
        };
        if self.frames.back().is_some_and(|(last, _)| {
            timestamp <= *last || timestamp - last > crate::gestures::MAX_FRAME_GAP_MS
        }) {
            self.clear();
        }
        while self
            .frames
            .front()
            .is_some_and(|(first, _)| timestamp - first > 1_000)
        {
            self.frames.pop_front();
        }
        if MAX_RANGE.iter().enumerate().any(|(axis, range)| {
            self.frames
                .iter()
                .any(|(_, previous)| (features[axis] - previous[axis]).abs() > *range)
        }) {
            self.clear();
        }
        self.frames.push_back((timestamp, features));
    }

    pub fn sample(&self) -> Option<Features> {
        if self.frames.len() < 7 || self.frames.back()?.0 - self.frames.front()?.0 < 650 {
            return None;
        }
        Some(std::array::from_fn(|axis| {
            let mut values: Vec<_> = self
                .frames
                .iter()
                .map(|(_, features)| features[axis])
                .collect();
            median(&mut values)
        }))
    }

    pub fn recent_sample(&self, timestamp: u64) -> Option<Features> {
        if timestamp.abs_diff(self.frames.back()?.0) > crate::gestures::MAX_FRAME_GAP_MS {
            return None;
        }
        self.sample()
    }
}

fn solve(mut matrix: [[f64; 7]; 5]) -> Option<[Point; 5]> {
    for i in 0..5 {
        let pivot = (i..5).max_by(|a, b| matrix[*a][i].abs().total_cmp(&matrix[*b][i].abs()))?;
        matrix.swap(i, pivot);
        let divisor = matrix[i][i];
        if divisor.abs() < 1e-10 {
            return None;
        }
        for value in &mut matrix[i][i..] {
            *value /= divisor;
        }
        let normalized = matrix[i];
        for (index, row) in matrix.iter_mut().enumerate() {
            if index != i {
                let factor = row[i];
                for j in i..7 {
                    row[j] -= factor * normalized[j];
                }
            }
        }
    }
    Some(matrix.map(|row| [row[5], row[6]]))
}

pub fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    (values[(values.len() - 1) / 2] + values[middle]) / 2.0
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Validation {
    median_points: u32,
    p90_points: u32,
}

impl Validation {
    pub fn new(mut errors: Vec<f64>) -> Self {
        let middle = median(&mut errors);
        let p90 = (errors.len() as f64 * 0.9).ceil() as usize - 1;
        Self {
            median_points: middle.round() as u32,
            p90_points: errors[p90].round() as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_learned_center_shift_that_would_worsen_both_pointer_corners() {
        let features = |[x, y]: Point| [0.61 - 0.24 * x, 0.02 + 0.16 * y, 0.0, 0.0];
        let mut mapping =
            Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let anchors = RESET_POINTS.map(|point| (features(point), point));
        let corners = [anchors[1].0, anchors[2].0];
        let before = corners.map(|features| mapping.pointer_point(features));
        assert!(mapping.reset_center(&anchors).is_ok());

        let center = [0.60, 0.50];
        mapping.refine(anchors[0].0, center).unwrap();
        assert!(mapping.reset_center(&anchors).is_err());
        assert_eq!(
            corners.map(|features| mapping.pointer_point(features)),
            before
        );
        assert_eq!(mapping.pointer_point(anchors[0].0), center);
    }

    #[test]
    fn local_corrections_survive_recenter_and_keep_gesture_coordinates_independent() {
        let features = |[x, y]: Point| [0.61 - 0.24 * x, 0.02 + 0.16 * y, 0.0, 0.0];
        let full = Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let mut learned = full.clone();
        let center = features([0.56, 0.51]);
        let outlying = features([0.73, 0.42]);
        let center_confirmed = [0.59, 0.50];
        let outlying_confirmed = [0.76, 0.42];
        learned.refine(center, center_confirmed).unwrap();
        learned.refine(outlying, outlying_confirmed).unwrap();
        assert!(within_distance(
            learned.pointer_point(center),
            center_confirmed,
            1e-12
        ));
        assert_eq!(learned.eye_point(center), full.eye_point(center));

        let anchors = RESET_POINTS.map(|[x, y]| (features([x + 0.06, y + 0.01]), [x, y]));
        let recentered = learned.reset_center(&anchors).unwrap();
        let unlearned = full.reset_center(&anchors).unwrap();
        assert!(within_distance(
            recentered.pointer_point(center),
            [0.5; 2],
            1e-12
        ));
        assert_eq!(
            recentered.eye_point(outlying),
            unlearned.eye_point(outlying)
        );
        assert!(within_distance(
            recentered.pointer_point(outlying),
            [0.67, 0.42],
            1e-12
        ));
    }

    #[test]
    fn fits_known_gaze_mapping_at_an_unseen_position() {
        let features = |[x, y]: Point| [0.4 + 0.2 * x, -0.08 + 0.16 * y, 0.0, 0.0];
        let samples = CALIBRATION_POINTS.map(|point| (features(point), point));
        let fit = Mapping::fit(&samples).unwrap();
        let [x, y] = fit.predict(features([0.68, 0.27]));
        assert!((x - 0.68).abs() < 0.015);
        assert!((y - 0.27).abs() < 0.015);
    }

    #[test]
    fn rejects_repeated_gaze() {
        let samples = CALIBRATION_POINTS.map(|point| ([0.5, 0.0, 0.0, 0.0], point));
        assert!(Mapping::fit(&samples).is_err());
    }

    #[test]
    fn rejects_one_gross_miss_in_an_otherwise_consistent_fit() {
        let features = |[x, y]: Point| [0.60 - 0.21 * x - 0.03 * y, 0.03 + 0.16 * y, 0.0, 0.0];
        let mut samples = CALIBRATION_POINTS.map(|point| (features(point), point));
        samples[0].0 = features([0.89, 0.81]);
        assert!(Mapping::fit(&samples).is_err());
    }

    #[test]
    fn center_reset_translates_the_entire_nine_point_fit() {
        let features = |[x, y]: Point| [0.58 - 0.22 * x - 0.05 * y, 0.02 + 0.15 * y, 0.0, 0.0];
        let full = Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let move_camera = |[x, y]: Point| features([x + 0.05, y - 0.04]);
        let anchors = RESET_POINTS.map(|point| (move_camera(point), point));
        let reset = full.reset_center(&anchors).unwrap();
        assert!(within_distance(
            reset.predict(anchors[0].0),
            anchors[0].1,
            1e-12
        ));
        let center_before = full.predict(anchors[0].0);
        for target in [[0.79, 0.23], [0.10, 0.78], [1.0, 0.0]] {
            let sample = move_camera(target);
            let before = full.predict(sample);
            let after = reset.predict(sample);
            assert!(within_distance(after, target, 0.025));
            for axis in 0..2 {
                assert!(
                    (after[axis] - before[axis] - anchors[0].1[axis] + center_before[axis]).abs()
                        < 1e-12
                );
            }
        }
    }

    #[test]
    fn later_resets_replace_the_offset_and_stay_bounded_against_the_original_fit() {
        let features = |[x, y]: Point| [0.65 - 0.25 * x, 0.02 + 0.15 * y, 0.0, 0.0];
        let full = Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let anchors = |shift| RESET_POINTS.map(|[x, y]| (features([x + shift, y]), [x, y]));
        let first = full.reset_center(&anchors(0.09)).unwrap();
        let second = first.reset_center(&anchors(-0.04)).unwrap();
        let from_full = full.reset_center(&anchors(-0.04)).unwrap();
        for point in VALIDATION_POINTS {
            assert_eq!(
                second.predict(features(point)),
                from_full.predict(features(point))
            );
        }
        assert!(first.reset_center(&anchors(0.17)).is_err());
    }

    #[test]
    fn exact_corners_reject_a_missed_center_but_allow_small_noise() {
        let features = |[x, y]: Point| [0.59 - 0.18 * x, 0.02 + 0.16 * y, 0.0, 0.0];
        let full = Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let mut anchors = RESET_POINTS.map(|point| (features(point), point));
        anchors[0].0 = features([0.60, 0.50]);
        assert!(full.reset_center(&anchors).is_err());
        anchors[0].0 = features([0.515, 0.50]);
        assert!(full.reset_center(&anchors).is_ok());
    }

    #[test]
    fn rejects_compressed_swapped_and_head_driven_corners() {
        let features = |[x, y]: Point| [0.65 - 0.25 * x, 0.02 + 0.15 * y, 0.0, 0.0];
        let full = Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let mut anchors = RESET_POINTS.map(|[x, y]| {
            let slight_gaze = [0.5 + 0.27 * (x - 0.5), 0.5 + 0.27 * (y - 0.5)];
            (features(slight_gaze), [x, y])
        });
        assert!(reset_corner(anchors[0].0, anchors[1].0, 1));
        assert!(reset_corner(anchors[0].0, anchors[2].0, 2));
        assert!(full.reset_center(&anchors).is_err());
        anchors = RESET_POINTS.map(|point| (features(point), point));
        (anchors[1].0, anchors[2].0) = (anchors[2].0, anchors[1].0);
        assert!(full.reset_center(&anchors).is_err());
        anchors = RESET_POINTS.map(|point| (features(point), point));
        anchors[1].0[2] = 0.08;
        assert!(full.reset_center(&anchors).is_err());
    }

    #[test]
    fn rejects_corners_when_head_pose_changes_which_translation_fits() {
        let full = Mapping {
            center: [0.5, 0.1, 0.0, 0.0],
            scale: [10.0, 10.0, 1.0, 1.0],
            coefficients: [[0.5, 0.5], [-1.0, 0.0], [0.0, 1.0], [2.0, 0.0], [0.0, 2.0]],
            offset: [0.0; 2],
            pointer_offset: [0.0; 2],
            refinement: Refinement::default(),
            eye_pose: [0.0; 2],
        };
        let anchors = [
            ([0.500, 0.100, 0.000, 0.000], RESET_POINTS[0]),
            ([0.528, 0.075, -0.045, -0.045], RESET_POINTS[1]),
            ([0.472, 0.125, 0.045, 0.045], RESET_POINTS[2]),
        ];
        for (index, &(sample, target)) in anchors.iter().enumerate().skip(1) {
            assert!(reset_corner(anchors[0].0, sample, index));
            assert!(within_distance(full.predict(sample), target, 1e-12));
        }
        assert!(full.reset_center(&anchors).is_err());

        let anchors = [
            ([0.490, 0.100, 0.000, 0.000], RESET_POINTS[0]),
            ([0.527, 0.066, -0.050, 0.000], RESET_POINTS[1]),
            ([0.453, 0.134, -0.050, 0.000], RESET_POINTS[2]),
        ];
        for (index, &(sample, target)) in anchors.iter().enumerate().skip(1) {
            assert!(reset_corner(anchors[0].0, sample, index));
            assert!(within_distance(full.predict(sample), target, 1e-12));
        }
        assert!(full.reset_center(&anchors).is_err());
    }

    #[test]
    fn fixation_restarts_on_motion_missing_frames_or_tracking_loss() {
        let mut fixation = Fixation::default();
        let first = Some([0.3, 0.1, 0.0, 0.0]);
        let second = Some([0.6, 0.1, 0.0, 0.0]);
        for frame in 0..6 {
            fixation.observe(frame * 100, first);
            assert_eq!(fixation.sample(), None);
        }
        for frame in 6..12 {
            fixation.observe(frame * 100, second);
            assert_eq!(fixation.sample(), None);
        }
        for frame in 15..22 {
            fixation.observe(frame * 100, second);
            assert_eq!(fixation.sample(), None);
        }
        fixation.observe(2_200, second);
        assert_eq!(fixation.sample(), second);
        fixation.observe(2_300, None);
        fixation.observe(2_400, second);
        assert_eq!(fixation.sample(), None);
    }

    #[test]
    fn fixation_accepts_subpixel_vertical_iris_jitter_at_24_fps() {
        let mut fixation = Fixation::default();
        for frame in 0..=18 {
            let jitter = (std::f64::consts::TAU * 3.0 * frame as f64 / 24.0).sin() * 0.009;
            fixation.observe(frame * 1_000 / 24, Some([0.5, 0.10 + jitter, 0.0, 0.0]));
        }
        assert!((fixation.sample().unwrap()[1] - 0.10).abs() < 0.002);
    }

    #[test]
    fn using_the_settled_fixation_improves_predictions_after_late_saccades() {
        let features = |[x, y]: Point| [0.59 - 0.20 * x - 0.035 * y, 0.02 + 0.16 * y, 0.0, 0.0];
        let mut all_frames = Vec::new();
        let mut stable_frames = Vec::new();
        for target in CALIBRATION_POINTS {
            let intermediate = target.map(|value| 0.5 + 0.62 * (value - 0.5));
            let mut fixation = Fixation::default();
            let mut early = Vec::new();
            for timestamp in (270..=1_400).step_by(45) {
                let mut sample = features(if timestamp < 675 {
                    intermediate
                } else {
                    target
                });
                let jitter = ((timestamp / 45 % 3) as f64 - 1.0) * 0.002;
                sample[0] += jitter;
                sample[1] -= jitter;
                fixation.observe(timestamp, Some(sample));
                if timestamp <= 960 {
                    early.push(sample);
                }
            }
            let old = std::array::from_fn(|axis| {
                median(
                    &mut early
                        .iter()
                        .map(|features| features[axis])
                        .collect::<Vec<_>>(),
                )
            });
            all_frames.push((old, target));
            stable_frames.push((fixation.sample().unwrap(), target));
        }
        let before = Mapping::fit(&all_frames).unwrap();
        let after = Mapping::fit(&stable_frames).unwrap();
        for target in VALIDATION_POINTS {
            let sample = features(target);
            assert!(!within_distance(before.predict(sample), target, 0.06));
            assert!(within_distance(after.predict(sample), target, 0.015));
        }
    }
}
