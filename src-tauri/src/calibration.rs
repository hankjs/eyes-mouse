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

pub struct Mapping {
    center: Features,
    scale: Features,
    coefficients: [Point; 5],
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

    fn design_row(&self, features: Features) -> [f64; 5] {
        let mut row = [1.0; 5];
        for i in 0..4 {
            row[i + 1] = (features[i] - self.center[i]) * self.scale[i];
        }
        row
    }

    pub fn predict(&self, features: Features) -> Point {
        let mut point = [0.0; 2];
        for (value, coefficient) in self.design_row(features).iter().zip(self.coefficients) {
            for axis in 0..2 {
                point[axis] += value * coefficient[axis];
            }
        }
        point
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
}
