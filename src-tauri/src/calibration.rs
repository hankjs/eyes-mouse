use serde::Serialize;

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

#[derive(Clone)]
pub struct Mapping {
    center: Features,
    scale: Features,
    coefficients: [Point; 5],
    correction: Option<[AxisCorrection; 2]>,
    eye_pose: Point,
}

#[derive(Clone, Copy)]
struct AxisCorrection {
    source: [f64; 3],
    target: [f64; 3],
}

impl AxisCorrection {
    fn predict(self, value: f64) -> f64 {
        let side = usize::from(value >= self.source[1]);
        let fraction = (value - self.source[side]) / (self.source[side + 1] - self.source[side]);
        self.target[side] + fraction * (self.target[side + 1] - self.target[side])
    }
}

impl Mapping {
    pub fn fit(samples: &[(Features, Point)]) -> Result<Self, &'static str> {
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
            correction: None,
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

        let error = samples
            .iter()
            .map(|(features, point)| {
                let prediction = mapping.predict(*features);
                (prediction[0] - point[0]).powi(2) + (prediction[1] - point[1]).powi(2)
            })
            .sum::<f64>()
            / samples.len() as f64;
        if error.sqrt() > 0.20 {
            return Err("这次校准位置偏差较大。请坐稳，并在每个点出现后看准再按空格。");
        }
        Ok(mapping)
    }

    pub fn reset_center(
        previous: Option<&Self>,
        samples: &[(Features, Point); 3],
    ) -> Result<Self, &'static str> {
        let [(center, _), (upper_left, _), (lower_right, _)] = *samples;
        if !reset_corner(center, upper_left, 1) || !reset_corner(center, lower_right, 2) {
            return Err("注视位置的变化不足。请按顺序看向中央、左上和右下，再试一次。");
        }
        // With no earlier calibration, the unmirrored camera's X axis points
        // opposite screen X. Y increases down in both frames of reference.
        let mut mapping = previous.cloned().unwrap_or(Self {
            center,
            scale: [8.0, 10.0, 1.0, 1.0],
            coefficients: [[0.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0; 2], [0.0; 2]],
            correction: None,
            eye_pose: [center[2], center[3]],
        });
        let [left, middle, right] =
            [upper_left, center, lower_right].map(|sample| mapping.predict_linear(sample));
        let target = [samples[1].1, samples[0].1, samples[2].1];
        for axis in 0..2 {
            if middle[axis] - left[axis] < 0.055
                || right[axis] - middle[axis] < 0.055
                || !(target[0][axis] < target[1][axis] && target[1][axis] < target[2][axis])
            {
                return Err("快速校准未能区分三个位置。请保持头部稳定，或重新做九点校准。");
            }
        }
        mapping.correction = Some(std::array::from_fn(|axis| AxisCorrection {
            source: [left[axis], middle[axis], right[axis]],
            target: target.map(|point| point[axis]),
        }));
        mapping.eye_pose = [center[2], center[3]];
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
        let point = self.predict_linear(features);
        self.correction.map_or(point, |correction| {
            std::array::from_fn(|axis| correction[axis].predict(point[axis]))
        })
    }

    pub fn eye_point(&self, mut features: Features) -> Point {
        features[2..].copy_from_slice(&self.eye_pose);
        self.predict(features)
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

pub fn reset_corner(center: Features, sample: Features, index: usize) -> bool {
    let direction = if index == 1 { 1.0 } else { -1.0 };
    direction * (sample[0] - center[0]) >= 0.012
        && direction * (center[1] - sample[1]) >= 0.009
        && (sample[2] - center[2]).hypot(sample[3] - center[3]) <= 0.14
}

#[derive(Default)]
pub struct Fixation {
    frames: Vec<(u64, Features)>,
}

impl Fixation {
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn observe(&mut self, timestamp: u64, features: Features) -> Option<Features> {
        const MAX_RANGE: Features = [0.020, 0.016, 0.075, 0.075];
        if self.frames.last().is_some_and(|(last, _)| {
            timestamp <= *last || timestamp - last > crate::gestures::MAX_FRAME_GAP_MS
        }) || MAX_RANGE.iter().enumerate().any(|(axis, range)| {
            self.frames
                .iter()
                .any(|(_, previous)| (features[axis] - previous[axis]).abs() > *range)
        }) {
            self.clear();
        }
        self.frames.push((timestamp, features));
        if self.frames.len() < 7 || timestamp - self.frames[0].0 < 650 {
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
    fn resets_each_axis_on_both_sides_of_center_and_preserves_a_full_mapping() {
        let features = |[x, y]: Point| [0.58 - 0.22 * x - 0.05 * y, 0.02 + 0.15 * y, 0.0, 0.0];
        let full = Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let move_camera =
            |[x, y]: Point| features([0.05 + (x - 0.50) * 0.8 + 0.50, (y - 0.50) * 0.9 + 0.45]);
        let anchors = RESET_POINTS.map(|point| (move_camera(point), point));
        let reset = Mapping::reset_center(Some(&full), &anchors).unwrap();
        for (sample, expected) in anchors {
            let result = reset.predict(sample);
            assert!((result[0] - expected[0]).abs() < 1e-12);
            assert!((result[1] - expected[1]).abs() < 1e-12);
        }
        let point = reset.predict(move_camera([0.79, 0.23]));
        assert!((point[0] - 0.79).abs() < 0.02);
        assert!((point[1] - 0.23).abs() < 0.02);
    }

    #[test]
    fn fits_uncalibrated_eyes_and_rejects_identical_or_swapped_anchors() {
        let features = |[x, y]: Point| [0.65 - 0.25 * x, 0.02 + 0.15 * y, 0.0, 0.0];
        let mut anchors = RESET_POINTS.map(|point| (features(point), point));
        let reset = Mapping::reset_center(None, &anchors).unwrap();
        let [x, y] = reset.predict(features([0.75, 0.3]));
        assert!((x - 0.75).abs() < 1e-12);
        assert!((y - 0.30).abs() < 1e-12);
        (anchors[1].0, anchors[2].0) = (anchors[2].0, anchors[1].0);
        assert!(Mapping::reset_center(None, &anchors).is_err());
        anchors[1].0 = anchors[0].0;
        assert!(Mapping::reset_center(None, &anchors).is_err());
    }

    #[test]
    fn rejects_three_stable_fixations_made_with_different_head_poses() {
        let anchors = [
            ([0.5, 0.06, 0.0, 0.0], RESET_POINTS[0]),
            ([0.514, 0.050, 0.40, 0.30], RESET_POINTS[1]),
            ([0.486, 0.070, -0.40, -0.30], RESET_POINTS[2]),
        ];
        assert!(!reset_corner(anchors[0].0, anchors[1].0, 1));
        assert!(!reset_corner(anchors[0].0, anchors[2].0, 2));
        assert!(Mapping::reset_center(None, &anchors).is_err());
    }

    #[test]
    fn fixation_restarts_on_motion_or_missing_frames() {
        let mut fixation = Fixation::default();
        for frame in 0..6 {
            assert_eq!(fixation.observe(frame * 100, [0.3, 0.1, 0.0, 0.0]), None);
        }
        for frame in 6..12 {
            assert_eq!(fixation.observe(frame * 100, [0.6, 0.1, 0.0, 0.0]), None);
        }
        for frame in 15..22 {
            assert_eq!(fixation.observe(frame * 100, [0.6, 0.1, 0.0, 0.0]), None);
        }
        assert_eq!(
            fixation.observe(2_200, [0.6, 0.1, 0.0, 0.0]),
            Some([0.6, 0.1, 0.0, 0.0])
        );
    }
}
