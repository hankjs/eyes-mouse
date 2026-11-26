use crate::calibration::{solve, GazeSample, Point, CALIBRATION_POINTS};
use crate::head_compensation::valid_position;

type Vector = [f64; 3];

#[derive(Clone)]
pub struct GeometricGaze {
    screen_cm: Point,
    reference_depth: f64,
    coefficients: [Point; 3],
}

impl GeometricGaze {
    pub fn fit(screen_cm: Point, samples: &[(GazeSample, Point)]) -> Option<(Self, f64)> {
        if samples.len() != CALIBRATION_POINTS.len() {
            return None;
        }
        let observations: Vec<_> = samples
            .iter()
            .map(|&(sample, [x, y])| {
                let pose = Pose::from_sample(sample)?;
                // The webcam is near the center of the top edge. Camera X points
                // toward the user's left and camera Y points up.
                let target = [(0.5 - x) * screen_cm[0], -y * screen_cm[1], 0.0];
                let ray = std::array::from_fn(|axis| target[axis] - pose.eye[axis]);
                let [x, y, z] = pose.axes.map(|axis| dot(axis, ray));
                (z >= 15.0).then_some((iris_row(sample), [x / z, y / z], -pose.eye[2]))
            })
            .collect::<Option<_>>()?;
        let reference_depth =
            observations.iter().map(|row| row.2).sum::<f64>() / observations.len() as f64;
        let data = observations.iter().map(|&(iris, slope, _)| (iris, slope));
        let model = Self {
            screen_cm,
            reference_depth,
            coefficients: fit_direction(data.clone())?,
        };

        // Validate predictions for targets left out of the fit.
        let mut squared_error = 0.0;
        for (held_out, &(sample, target)) in samples.iter().enumerate() {
            let coefficients = fit_direction(
                data.clone()
                    .enumerate()
                    .filter_map(|(index, row)| (index != held_out).then_some(row)),
            )?;
            let [x, y] = model.intersect(sample, &coefficients)?;
            let error = (x - target[0]).hypot(y - target[1]);
            if error > 0.17 {
                return None;
            }
            squared_error += error.powi(2);
        }
        let rms = (squared_error / samples.len() as f64).sqrt();
        (rms <= 0.085).then_some((model, rms))
    }

    pub fn predict(&self, sample: GazeSample) -> Option<Point> {
        self.intersect(sample, &self.coefficients)
    }

    fn intersect(&self, sample: GazeSample, coefficients: &[Point; 3]) -> Option<Point> {
        let pose = Pose::from_sample(sample)?;
        let depth = -pose.eye[2];
        if !(0.65..=1.45).contains(&(depth / self.reference_depth)) {
            return None;
        }
        let row = iris_row(sample);
        let [x, y]: Point =
            std::array::from_fn(|axis| dot(row, coefficients.map(|coefficient| coefficient[axis])));
        if x.hypot(y) > 1.0 {
            return None;
        }
        let [x, y, z] = pose.in_camera([x, y, 1.0]);
        if z < 0.6 {
            return None;
        }
        let point = [
            0.5 - (pose.eye[0] + depth * x / z) / self.screen_cm[0],
            -(pose.eye[1] + depth * y / z) / self.screen_cm[1],
        ];
        // Allow modest overscan for edge and circular gestures.
        point
            .iter()
            .all(|value| (-0.30..=1.30).contains(value))
            .then_some(point)
    }
}

struct Pose {
    eye: Vector,
    axes: [Vector; 3],
}

impl Pose {
    fn from_sample(sample: GazeSample) -> Option<Self> {
        let [x, y, log_depth] = sample.head_position.filter(|&p| valid_position(p))?;
        let [_, _, yaw, pitch] = sample.features;
        let roll = sample.head_roll?;
        let depth = log_depth.exp();
        if !(25.0..=125.0).contains(&depth)
            || x.abs() > 0.55
            || y.abs() > 0.45
            || yaw.hypot(pitch) > 0.48
            || !(-0.35..=0.35).contains(&roll)
        {
            return None;
        }

        // MediaPipe uses a virtual camera looking down -Z and a canonical face in cm.
        // The columns of Ry * Rx * Rz are the head's axes in camera space.
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let (sr, cr) = roll.sin_cos();
        let x_axis = [cy, 0.0, -sy];
        let y_axis = [sy * sp, cp, cy * sp];
        let mut pose = Self {
            eye: [x * depth, y * depth, -depth],
            axes: [
                std::array::from_fn(|axis| cr * x_axis[axis] + sr * y_axis[axis]),
                std::array::from_fn(|axis| -sr * x_axis[axis] + cr * y_axis[axis]),
                [sy * cp, -sp, cy * cp],
            ],
        };
        // Use the midpoint of MediaPipe's canonical eye corners for a single binocular ray.
        let offset = pose.in_camera([0.0, 2.6, 3.5]);
        for (axis, offset) in pose.eye.iter_mut().zip(offset) {
            *axis += offset;
        }
        Some(pose)
    }

    fn in_camera(&self, local: Vector) -> Vector {
        std::array::from_fn(|axis| dot(self.axes.map(|row| row[axis]), local))
    }
}

fn fit_direction(observations: impl Iterator<Item = (Vector, Point)>) -> Option<[Point; 3]> {
    let mut equations = [[0.0; 5]; 3];
    for (row, target) in observations {
        for (i, equation) in equations.iter_mut().enumerate() {
            for (entry, value) in equation.iter_mut().zip(row.into_iter().chain(target)) {
                *entry += row[i] * value;
            }
        }
    }
    let coefficients = solve(equations)?;
    let [_, horizontal, vertical] = coefficients;
    // Increasing iris X points image-right. Increasing iris Y points image-down.
    // Reject fits that amplify small iris shifts excessively.
    (horizontal[0] > 0.0
        && vertical[1] < 0.0
        && horizontal[0].hypot(horizontal[1]) <= 1.2
        && vertical[0].hypot(vertical[1]) <= 1.2)
        .then_some(coefficients)
}

fn iris_row(sample: GazeSample) -> Vector {
    [
        1.0,
        (sample.features[0] - 0.5) * 10.0,
        sample.features[1] * 10.0,
    ]
}

fn dot(a: Vector, b: Vector) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{Mapping, VALIDATION_POINTS};
    use crate::head_compensation::RecentCorrection;

    const SCREEN: Point = [28.65, 17.90];
    const HEAD: Vector = [2.0, -10.0, 58.0];

    fn sample([x, y]: Point, [tx, ty, depth]: Vector, [yaw, pitch, roll]: Vector) -> GazeSample {
        // Inverse rotation in three steps, independent of the production pose basis.
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let (sr, cr) = roll.sin_cos();
        let x = (0.5 - x) * SCREEN[0] - tx;
        let y = -y * SCREEN[1] - ty;
        let z = depth;
        let (x, z) = (cy * x - sy * z, sy * x + cy * z);
        let (y, z) = (cp * y + sp * z, cp * z - sp * y);
        let (x, y) = (cr * x + sr * y, cr * y - sr * x - 2.6);
        let z = z - 3.5;
        GazeSample {
            features: [0.52 + 0.34 * x / z, -0.04 - 0.31 * y / z, yaw, pitch],
            head_position: Some([tx / depth, ty / depth, depth.ln()]),
            head_roll: Some(roll),
        }
    }

    fn calibration() -> [(GazeSample, Point); 9] {
        CALIBRATION_POINTS.map(|point| (sample(point, HEAD, [0.0; 3]), point))
    }

    fn error([x, y]: Point, [tx, ty]: Point) -> f64 {
        (x - tx).hypot(y - ty)
    }

    #[test]
    fn nine_point_gaze_stays_on_target_when_head_moves() {
        let data = calibration();
        let legacy = Mapping::fit(&data).unwrap();
        let geometry = Mapping::fit_for_screen(&data, Some(SCREEN)).unwrap();
        assert!(geometry.has_geometric_gaze());

        for point in VALIDATION_POINTS {
            let neutral = sample(point, HEAD, [0.0; 3]);
            assert!(error(geometry.predict(neutral), point) < 1e-10);
            assert!(error(legacy.predict(neutral), point) < 0.012);
            for (translation, rotation) in [
                ([6.0, -10.0, 58.0], [0.0; 3]),
                ([-2.0, -7.0, 58.0], [0.0; 3]),
                ([2.0, -10.0, 68.0], [0.0; 3]),
                (HEAD, [0.08, -0.05, 0.22]),
            ] {
                let moving = sample(point, translation, rotation);
                let fixed_pose_error = error(legacy.predict(moving), point);
                let geometry_error = error(geometry.predict(moving), point);
                assert!(fixed_pose_error > 0.01, "{point:?}: {fixed_pose_error}");
                assert!(geometry_error < 1e-10, "{point:?}: {geometry_error}");
            }
        }
    }

    #[test]
    fn invalid_geometry_falls_back_and_live_preview_fades_to_it() {
        let data = calibration();
        let legacy = Mapping::fit(&data).unwrap();
        let geometry = Mapping::fit_for_screen(&data, Some(SCREEN)).unwrap();
        let moved = sample([0.52, 0.48], [6.0, -10.0, 58.0], [0.0; 3]);
        let missing = GazeSample {
            head_position: None,
            ..moved
        };
        let missing_roll = GazeSample {
            head_roll: None,
            ..moved
        };
        let extreme = GazeSample {
            head_roll: Some(0.6),
            ..moved
        };
        let far = sample([0.52, 0.48], [6.0, -10.0, 95.0], [0.0; 3]);
        for unavailable in [missing, missing_roll, extreme, far] {
            assert_eq!(geometry.predict(unavailable), legacy.predict(unavailable));
            assert!(!geometry.has_required_head_pose(unavailable));
        }

        let mut recent = RecentCorrection::default();
        let first = geometry.live_pointer_point(moved, 1_000, &mut recent);
        let held = geometry.live_pointer_point(missing, 1_100, &mut recent);
        let faded = geometry.live_pointer_point(missing, 1_230, &mut recent);
        let fallback = geometry.live_pointer_point(missing, 1_320, &mut recent);
        assert_eq!(held, first);
        assert_eq!(fallback, legacy.pointer_point(missing));
        assert!(error(faded, first) > 0.05);
        assert!(error(faded, first) < error(fallback, first));
    }

    #[test]
    fn calibration_rejects_missing_pose_bad_parity_and_inconsistent_geometry() {
        let data = calibration();
        let mut missing = data;
        missing[1].0.head_roll = None;
        let mut inconsistent = data;
        inconsistent[4].0.head_position = Some([0.24, -0.17, 58_f64.ln()]);
        let mirrored = data.map(|(mut sample, point)| {
            sample.features[0] = 1.0 - sample.features[0];
            (sample, point)
        });
        for data in [missing, inconsistent, mirrored] {
            let legacy = Mapping::fit(&data).unwrap();
            let mapping = Mapping::fit_for_screen(&data, Some(SCREEN)).unwrap();
            assert!(!mapping.has_geometric_gaze());
            assert_eq!(mapping.predict(data[0].0), legacy.predict(data[0].0));
        }
    }

    #[test]
    fn calibration_keeps_legacy_when_noisy_pose_worsens_held_out_error() {
        let mut data = calibration();
        for (index, (sample, _)) in data.iter_mut().enumerate() {
            let noise = if index % 2 == 0 { 1.0 } else { -1.0 };
            sample.head_position = Some([
                (HEAD[0] + noise) / HEAD[2],
                (HEAD[1] + 0.4 * noise) / HEAD[2],
                HEAD[2].ln(),
            ]);
        }
        assert!(GeometricGaze::fit(SCREEN, &data).is_some());
        let mapping = Mapping::fit_for_screen(&data, Some(SCREEN)).unwrap();
        assert!(!mapping.has_geometric_gaze());
    }

    #[test]
    fn manual_compensation_learns_only_the_error_remaining_after_geometry() {
        let calibration = calibration();
        let geometry = Mapping::fit_for_screen(&calibration, Some(SCREEN)).unwrap();
        let target = [0.5; 2];
        let reference = sample(target, HEAD, [0.0; 3]);
        let moved = |shift| sample(target, [HEAD[0] + shift, HEAD[1], HEAD[2]], [0.0; 3]);
        let shifts = (0..61).map(|index| 5.0 * (f64::from(index) * 0.4).sin());
        let exact: Vec<_> = shifts.clone().map(moved).collect();
        assert!(geometry
            .compensate_head_motion(reference, target, &exact)
            .is_err());

        // Simulate a tracker that consistently overestimates sideways translation.
        let noisy = |shift| {
            let mut gaze = moved(shift);
            gaze.head_position.as_mut().unwrap()[0] += 0.55 * shift / HEAD[2];
            gaze
        };
        let samples: Vec<_> = shifts.map(noisy).collect();
        let refined = geometry
            .compensate_head_motion(reference, target, &samples)
            .unwrap();
        assert!(refined.has_geometric_gaze());
        assert!(refined.has_head_compensation());
        for gaze in [noisy(-3.0), noisy(3.0)] {
            let before = error(geometry.predict(gaze), target);
            let after = error(refined.predict(gaze), target);
            assert!(before > 0.05 && after < 0.015, "{before} -> {after}");
        }
    }
}
