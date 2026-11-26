use crate::calibration::{solve, GazeSample, Point};

pub type HeadPosition = [f64; 3];

pub const SAMPLE_INTERVAL_MS: u64 = 110;
pub const MAX_SAMPLES: usize = 150;
pub const FIT_ERROR: &str =
    "头动补偿采样不足或注视偏离，已保留原校准。请持续注视中央亮点，慢慢做每个动作后重试。";

const MOTION_SCALE: [f64; 5] = [0.15, 0.13, 0.065, 0.05, 0.12];
const MAX_DELTA: [f64; 5] = [0.42, 0.36, 0.19, 0.16, 0.32];
const MAX_CORRECTION: f64 = 0.27;
const MIN_SAMPLES: usize = 27;

type Motion = [f64; 5];

#[derive(Clone)]
pub struct HeadCompensation {
    reference: Motion,
    reference_correction: Point,
    bounds: [Point; 5],
    coefficients: [Point; 5],
}

fn pose(sample: GazeSample) -> Option<Motion> {
    let [x, y, depth] = sample
        .head_position
        .filter(|&position| valid_position(position))?;
    let [_, _, yaw, pitch] = sample.features;
    (yaw.abs() <= 0.75 && pitch.abs() <= 0.75).then_some([yaw, pitch, x, y, depth])
}

pub fn valid_position(position: HeadPosition) -> bool {
    let [x, y, depth] = position;
    x.abs() <= 1.0 && y.abs() <= 1.0 && (0.0..=10_000_f64.ln()).contains(&depth)
}

pub fn continuous_motion(previous: GazeSample, current: GazeSample, gap_ms: u64) -> bool {
    let (Some(previous_pose), Some(current_pose)) = (pose(previous), pose(current)) else {
        return false;
    };
    let seconds = gap_ms as f64 / 1000.0;
    let max_change = [0.055, 0.05, 0.023, 0.023, 0.045];
    let max_speed = [2.0, 1.7, 0.85, 0.85, 1.9];
    (0..5).all(|axis| {
        (current_pose[axis] - previous_pose[axis]).abs()
            <= max_change[axis] + seconds * max_speed[axis]
    }) && (0..2).all(|axis| {
        (current.features[axis] - previous.features[axis]).abs() <= 0.04 + 0.45 * seconds
    })
}

impl HeadCompensation {
    pub fn fit(
        reference: GazeSample,
        samples: &[GazeSample],
        predict: impl Fn(GazeSample) -> Option<Point>,
        previous: Option<&Self>,
    ) -> Option<Self> {
        let origin = pose(reference)?;
        let reference_point = predict(reference)?;
        let reference_correction = previous
            .and_then(|model| model.correction(reference))
            .unwrap_or([0.0; 2]);
        let observations: Vec<_> = samples
            .iter()
            .filter_map(|&sample| {
                let sample_pose = pose(sample)?;
                let delta: Motion = std::array::from_fn(|axis| sample_pose[axis] - origin[axis]);
                let point = predict(sample)?;
                let residual = std::array::from_fn(|axis| reference_point[axis] - point[axis]);
                if (0..5).any(|axis| delta[axis].abs() > MAX_DELTA[axis])
                    || length(residual) > 0.36
                    || residual.iter().any(|value| !value.is_finite())
                {
                    return None;
                }
                Some((
                    std::array::from_fn(|axis| delta[axis] / MOTION_SCALE[axis]),
                    residual,
                    sample,
                ))
            })
            .collect();
        if observations.len() < MIN_SAMPLES {
            return None;
        }

        // Hold out interleaved samples so every movement in the exercise can contribute to
        // both the fit and its accuracy check.
        let (training, evaluation): (Vec<_>, Vec<_>) = observations
            .into_iter()
            .enumerate()
            .partition(|(index, _)| index % 3 != 0);
        let mut model = Self {
            reference: origin,
            reference_correction,
            bounds: [[0.0; 2]; 5],
            coefficients: [[0.0; 2]; 5],
        };
        let mut independent_motion: Vec<Vec<f64>> = Vec::new();
        for axis in 0..5 {
            let values: Vec<_> = training.iter().map(|(_, (row, _, _))| row[axis]).collect();
            let mut ordered = values.clone();
            ordered.sort_by(f64::total_cmp);
            // Require at least three samples in each direction and leave the most extreme
            // measurements outside the range used for live extrapolation.
            let min = ordered[2];
            let max = ordered[ordered.len() - 3];
            if min > -0.18 || max < 0.18 {
                continue;
            }

            let energy = values.iter().map(|value| value.powi(2)).sum::<f64>();
            let mut independent = values;
            for direction in &independent_motion {
                let projection = independent
                    .iter()
                    .zip(direction)
                    .map(|(value, basis)| value * basis)
                    .sum::<f64>();
                for (value, basis) in independent.iter_mut().zip(direction) {
                    *value -= projection * basis;
                }
            }
            let independent_energy = independent.iter().map(|value| value.powi(2)).sum::<f64>();
            if independent_energy < 0.25 * energy {
                continue;
            }
            let norm = independent_energy.sqrt();
            independent.iter_mut().for_each(|value| *value /= norm);
            independent_motion.push(independent);
            model.bounds[axis] = [min, max];
        }
        if independent_motion.is_empty()
            || previous.is_some_and(|previous| {
                previous
                    .bounds
                    .iter()
                    .zip(model.bounds)
                    .any(|(old, new)| *old != [0.0; 2] && new == [0.0; 2])
            })
        {
            return None;
        }

        let mut equations = [[0.0; 7]; 5];
        for (_, (row, residual, _)) in &training {
            let row = model.bounded_row(*row);
            for (i, equation) in equations.iter_mut().enumerate() {
                for (entry, value) in equation.iter_mut().zip(row) {
                    *entry += row[i] * value;
                }
                for axis in 0..2 {
                    equation[5 + axis] += row[i] * residual[axis];
                }
            }
        }
        for (i, equation) in equations.iter_mut().enumerate() {
            equation[i] += 0.025 * training.len() as f64;
        }
        model.coefficients = solve(equations)?;

        let mut baseline_error = 0.0;
        let mut compensated_error = 0.0;
        let mut previous_error = 0.0;
        for (_, (_, residual, sample)) in &evaluation {
            let correction = model.correction(*sample)?;
            let adjusted = std::array::from_fn(|axis| {
                residual[axis] - (correction[axis] - reference_correction[axis])
            });
            let previous_correction = previous
                .and_then(|previous| previous.correction(*sample))
                .unwrap_or([0.0; 2]);
            let previous_adjusted = std::array::from_fn(|axis| {
                residual[axis] - (previous_correction[axis] - reference_correction[axis])
            });
            baseline_error += length(*residual).powi(2);
            compensated_error += length(adjusted).powi(2);
            previous_error += length(previous_adjusted).powi(2);
        }
        let baseline_rms = (baseline_error / evaluation.len() as f64).sqrt();
        let compensated_rms = (compensated_error / evaluation.len() as f64).sqrt();
        let previous_rms = (previous_error / evaluation.len() as f64).sqrt();
        (baseline_rms >= 0.015
            && compensated_rms <= 0.085
            && compensated_rms <= 0.78 * baseline_rms
            && compensated_rms <= previous_rms + 0.008)
            .then_some(model)
    }

    pub fn correction(&self, sample: GazeSample) -> Option<Point> {
        let current = pose(sample)?;
        let row = self.bounded_row(std::array::from_fn(|axis| {
            (current[axis] - self.reference[axis]) / MOTION_SCALE[axis]
        }));
        let mut correction = self.reference_correction;
        for (value, coefficient) in row.into_iter().zip(self.coefficients) {
            for axis in 0..2 {
                correction[axis] += value * coefficient[axis];
            }
        }
        let scale = (length(correction) / MAX_CORRECTION).max(1.0);
        Some(correction.map(|value| value / scale))
    }

    fn bounded_row(&self, row: Motion) -> Motion {
        std::array::from_fn(|axis| {
            let [min, max] = self.bounds[axis];
            row[axis].clamp(min, max)
        })
    }
}

#[derive(Default)]
pub struct RecentCorrection {
    last: Option<(u64, Point)>,
}

impl RecentCorrection {
    pub fn clear(&mut self) {
        self.last = None;
    }

    pub fn observe(&mut self, timestamp: u64, correction: Option<Point>) -> Point {
        const HOLD_MS: u64 = 150;
        const FADE_MS: u64 = 160;
        if let Some(correction) = correction {
            self.last = Some((timestamp, correction));
            return correction;
        }
        let Some((last_time, last_correction)) = self.last else {
            return [0.0; 2];
        };
        let Some(age) = timestamp.checked_sub(last_time) else {
            self.clear();
            return [0.0; 2];
        };
        if age >= HOLD_MS + FADE_MS {
            self.clear();
            return [0.0; 2];
        }
        let remaining = 1.0 - age.saturating_sub(HOLD_MS) as f64 / FADE_MS as f64;
        last_correction.map(|value| value * remaining)
    }
}

fn length([x, y]: Point) -> f64 {
    x.hypot(y)
}
