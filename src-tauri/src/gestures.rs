use crate::calibration::Point;
use std::f64::consts::{PI, TAU};

pub const MAX_FRAME_GAP_MS: u64 = 220;
const CIRCLE_DURATION_MS: std::ops::RangeInclusive<u64> = 750..=2_900;
const RING_RADIUS: std::ops::RangeInclusive<f64> = 0.16..=0.66;
const SCAN_LEFT_X: f64 = 0.34;
const SCAN_Y: std::ops::RangeInclusive<f64> = 0.18..=0.82;
const SCAN_VERTICAL_SPAN: f64 = 0.22;
const SCAN_FIXATION_MS: u64 = 45;
const SCAN_STEP_MS: u64 = 900;
const SCAN_DURATION_MS: u64 = 2_600;
const SCAN_REOPEN_MS: u64 = 200;
const SCAN_COOLDOWN_MS: u64 = 1_250;

#[derive(Debug, PartialEq)]
pub enum Gesture {
    TogglePointer,
    Exit,
    ResetCenter,
}

pub enum Eyes {
    Open { gaze: Option<Point>, pose: Point },
    Closed,
    Unreliable,
}

#[derive(Default)]
pub struct Detector {
    last_frame: Option<u64>,
    scan: Option<HorizontalScan>,
    scan_blocked_until: u64,
    circle: Option<Circle>,
    circle_blocked_until: Option<u64>,
    centered_since: Option<u64>,
    blink: BlinkSequence,
}

impl Detector {
    pub fn clear_progress(&mut self) {
        self.last_frame = None;
        self.scan = None;
        self.circle = None;
        self.centered_since = None;
        self.blink = BlinkSequence::default();
    }

    pub fn observe(&mut self, now: u64, eyes: Eyes) -> Option<Gesture> {
        if self
            .last_frame
            .is_some_and(|last| now <= last || now - last > MAX_FRAME_GAP_MS)
        {
            self.clear_progress();
        }
        self.last_frame = Some(now);

        let (gaze, pose) = match eyes {
            Eyes::Open { gaze, pose } => {
                if self.blink.open(now) {
                    self.clear_progress();
                    return Some(Gesture::ResetCenter);
                }
                (gaze, pose)
            }
            Eyes::Closed => {
                self.blink.close(now);
                if let Some(scan) = &mut self.scan {
                    scan.fixating_since = None;
                }
                self.circle = None;
                self.centered_since = None;
                return None;
            }
            Eyes::Unreliable => {
                self.clear_progress();
                return None;
            }
        };
        let Some(point) =
            gaze.filter(|point| point.iter().all(|coordinate| coordinate.is_finite()))
        else {
            self.scan = None;
            self.circle = None;
            self.centered_since = None;
            return None;
        };

        if self.observe_exit(now, point, pose) {
            self.clear_progress();
            self.circle_blocked_until = Some(now + 1_250);
            return Some(Gesture::Exit);
        }

        if now < self.scan_blocked_until || !SCAN_Y.contains(&point[1]) {
            self.scan = None;
            return None;
        }
        self.scan = self
            .scan
            .take()
            .filter(|scan| scan.accepts(now, point[1], pose))
            .or_else(|| {
                (point[0] <= SCAN_LEFT_X).then(|| HorizontalScan::new(now, point[1], pose))
            });
        if self
            .scan
            .as_mut()
            .is_some_and(|scan| scan.advance(now, point, pose))
        {
            self.clear_progress();
            self.scan_blocked_until = now + SCAN_COOLDOWN_MS;
            return Some(Gesture::TogglePointer);
        }
        None
    }

    fn observe_exit(&mut self, now: u64, point: Point, pose: Point) -> bool {
        let [x, y] = [point[0] - 0.5, point[1] - 0.5];
        let radius = x.hypot(y);
        if let Some(blocked_until) = self.circle_blocked_until {
            if radius < 0.13 {
                let start = *self.centered_since.get_or_insert(now);
                if now >= blocked_until && now - start >= 180 {
                    self.circle_blocked_until = None;
                    self.centered_since = None;
                }
            } else {
                self.centered_since = None;
            }
            return false;
        }
        if !RING_RADIUS.contains(&radius) {
            self.circle = None;
            return false;
        }

        let angle = y.atan2(x);
        let sector = 1 << ((angle.rem_euclid(TAU) / TAU * 16.0) as u32).min(15);
        if let Some(circle) = &mut self.circle {
            let delta = (angle - circle.last_angle + PI).rem_euclid(TAU) - PI;
            if now - circle.started <= *CIRCLE_DURATION_MS.end()
                && delta.abs() <= 0.60
                && distance(point, circle.last_point) <= 0.24
                && distance(pose, circle.start_pose) <= 0.14
                && distance(pose, circle.last_pose) <= 0.08
            {
                circle.last_angle = angle;
                circle.last_point = point;
                circle.last_pose = pose;
                circle.sectors |= sector;
                circle.clockwise += delta.max(0.0);
                circle.counterclockwise += (-delta).max(0.0);
                let moving = delta.abs() >= 0.025;
                if moving {
                    circle.movement_started.get_or_insert(circle.last_frame);
                    circle.movement_ended = now;
                    circle.moving_samples += 1;
                }
                circle.last_frame = now;

                if circle.clockwise <= 0.65 {
                    // Screen Y increases downward, so counterclockwise deltas are negative.
                    let turn = circle.counterclockwise - circle.clockwise;
                    let movement_duration = circle
                        .movement_started
                        .map_or(0, |start| circle.movement_ended - start);
                    if circle.sectors.count_ones() >= 14
                        && circle.moving_samples >= 16
                        && turn >= TAU - 0.33
                        && distance(point, circle.start_point) <= 0.15
                    {
                        if moving && CIRCLE_DURATION_MS.contains(&movement_duration) {
                            return true;
                        }
                        if turn >= TAU - 0.08 {
                            self.circle = None;
                        }
                    }
                    return false;
                }
            }
        }
        self.circle = Some(Circle {
            started: now,
            last_frame: now,
            start_point: point,
            last_point: point,
            last_angle: angle,
            start_pose: pose,
            last_pose: pose,
            sectors: sector,
            clockwise: 0.0,
            counterclockwise: 0.0,
            movement_started: None,
            movement_ended: now,
            moving_samples: 0,
        });
        false
    }
}

fn distance([x, y]: Point, [other_x, other_y]: Point) -> f64 {
    (x - other_x).hypot(y - other_y)
}

struct HorizontalScan {
    started: u64,
    last_step: u64,
    last_frame: u64,
    start_pose: Point,
    last_pose: Point,
    min_y: f64,
    max_y: f64,
    steps: u8,
    fixating_since: Option<u64>,
}

impl HorizontalScan {
    fn new(now: u64, y: f64, pose: Point) -> Self {
        Self {
            started: now,
            last_step: now,
            last_frame: now,
            start_pose: pose,
            last_pose: pose,
            min_y: y,
            max_y: y,
            steps: 0,
            fixating_since: None,
        }
    }

    fn accepts(&self, now: u64, y: f64, pose: Point) -> bool {
        now - self.started <= SCAN_DURATION_MS
            && now - self.last_step <= SCAN_STEP_MS
            && now - self.last_frame <= SCAN_REOPEN_MS
            && self.max_y.max(y) - self.min_y.min(y) <= SCAN_VERTICAL_SPAN
            && distance(pose, self.start_pose) <= 0.14
            && distance(pose, self.last_pose) <= 0.08
    }

    fn advance(&mut self, now: u64, [x, y]: Point, pose: Point) -> bool {
        // A dropped frame can leave the scan intact, but it cannot fill a fixation.
        if now - self.last_frame > 130 {
            self.fixating_since = None;
        }
        self.last_frame = now;
        self.last_pose = pose;
        self.min_y = self.min_y.min(y);
        self.max_y = self.max_y.max(y);

        // Use the same boundary looking left or right, with some room for jitter after entering.
        let side_x = if matches!(self.steps, 0 | 2) {
            x
        } else {
            1.0 - x
        };
        let margin = if self.fixating_since.is_some() {
            0.04
        } else {
            0.0
        };
        if side_x > SCAN_LEFT_X + margin {
            self.fixating_since = None;
            if self.steps == 1 && x <= SCAN_LEFT_X {
                // The first look can be held until the user starts moving right.
                *self = Self {
                    steps: 1,
                    ..Self::new(now, y, pose)
                };
            }
            return false;
        }

        let start = *self.fixating_since.get_or_insert(now);
        if now - start < SCAN_FIXATION_MS {
            return false;
        }
        self.fixating_since = None;
        self.last_step = now;
        self.steps += 1;
        self.steps == 4
    }
}

struct Circle {
    started: u64,
    last_frame: u64,
    start_point: Point,
    last_point: Point,
    last_angle: f64,
    start_pose: Point,
    last_pose: Point,
    sectors: u16,
    clockwise: f64,
    counterclockwise: f64,
    movement_started: Option<u64>,
    movement_ended: u64,
    moving_samples: usize,
}

#[derive(Default)]
struct BlinkSequence {
    last_open: Option<u64>,
    closed_at: Option<u64>,
    first_blink: Option<u64>,
    last_blink: Option<u64>,
    count: u8,
}

impl BlinkSequence {
    fn close(&mut self, now: u64) {
        if self.last_open.is_none() {
            return;
        }
        let closed_at = *self.closed_at.get_or_insert(now);
        if now - closed_at > 280 {
            *self = Self::default();
        }
    }

    fn open(&mut self, now: u64) -> bool {
        self.last_open = Some(now);
        let Some(closed_at) = self.closed_at.take() else {
            return false;
        };
        if !(34..=280).contains(&(now - closed_at)) {
            *self = Self {
                last_open: Some(now),
                ..Self::default()
            };
            return false;
        }

        if self
            .last_blink
            .is_none_or(|last| !(85..=520).contains(&(closed_at - last)))
            || self.first_blink.is_some_and(|first| now - first > 1_500)
        {
            self.first_blink = Some(closed_at);
            self.count = 0;
        }
        self.last_blink = Some(now);
        self.count += 1;
        self.count == 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(point: Option<Point>) -> Eyes {
        Eyes::Open {
            gaze: point,
            pose: [0.0; 2],
        }
    }

    fn point(angle: f64, radius: f64) -> Point {
        [0.5 + radius * angle.cos(), 0.5 + radius * angle.sin()]
    }

    #[test]
    fn exits_on_a_counterclockwise_circle_and_requires_center_before_repeating() {
        for start in [0.71, 2.8, 4.1] {
            let mut detector = Detector::default();
            let ring = |index: u32| point(start - TAU * f64::from(index) / 30.0, 0.32);
            let mut observed = Vec::new();
            for frame in 0..=100 {
                observed
                    .extend(detector.observe(500 + 45 * u64::from(frame), open(Some(ring(frame)))));
            }
            assert_eq!(observed, vec![Gesture::Exit]);
            for frame in 0..=6 {
                assert_eq!(
                    detector.observe(5_100 + frame * 45, open(Some([0.5; 2]))),
                    None
                );
            }
            assert_eq!(
                (0..=30_u32)
                    .filter_map(|frame| detector
                        .observe(5_420 + 45 * u64::from(frame), open(Some(ring(frame)))))
                    .collect::<Vec<_>>(),
                vec![Gesture::Exit]
            );
        }
    }

    #[test]
    fn ignores_repeated_clockwise_circles() {
        for radius in [0.18, 0.24, 0.32] {
            let mut detector = Detector::default();
            for frame in 0..=110_u32 {
                let angle = PI + TAU * f64::from(frame) / 30.0;
                assert_eq!(
                    detector.observe(45 * u64::from(frame), open(Some(point(angle, radius)))),
                    None
                );
            }
        }
    }

    #[test]
    fn rejects_small_fast_broken_reversing_and_head_driven_movements() {
        for scenario in 0..7 {
            let mut detector = Detector::default();
            let mut observed = Vec::new();
            for frame in 0_u32..=30 {
                let travel = match scenario {
                    2 => -TAU * f64::from(frame / 7) / 4.0,
                    3 => -TAU * f64::from(frame.min(30 - frame)) / 20.0,
                    6 => -TAU * f64::from(frame / 3) / 10.0,
                    _ => -TAU * f64::from(frame) / 30.0,
                };
                let radius = if scenario == 0 { 0.095 } else { 0.30 };
                let pose = if scenario == 4 {
                    [0.21 * travel.cos(), 0.21 * travel.sin()]
                } else {
                    [0.0; 2]
                };
                let step = if scenario == 1 { 18 } else { 45 };
                let gap = if scenario == 5 && frame >= 15 { 280 } else { 0 };
                observed.extend(detector.observe(
                    u64::from(frame) * step + gap,
                    Eyes::Open {
                        gaze: Some(point(travel, radius)),
                        pose,
                    },
                ));
            }
            assert!(
                observed.is_empty(),
                "scenario {scenario} produced {observed:?}"
            );
        }
    }

    #[test]
    fn rejects_fast_circles_with_a_stationary_start_or_finish() {
        for turn in [TAU, -TAU] {
            for delay in [0, 4] {
                let mut detector = Detector::default();
                for frame in 0_u32..=20 {
                    let index = frame.saturating_sub(delay).min(16);
                    let angle = if frame > 16 + delay {
                        turn + turn.signum() * 0.04 * f64::from(frame % 2)
                    } else {
                        turn * f64::from(index) / 16.0
                    };
                    assert_eq!(
                        detector.observe(u64::from(frame) * 42, open(Some(point(angle, 0.32)))),
                        None
                    );
                }
            }
        }
    }

    #[test]
    fn toggles_on_left_right_left_right_with_jitter_and_a_short_blink() {
        let mut detector = Detector::default();
        let mut observed = Vec::new();
        for frame in 0..28 {
            observed.extend(detector.observe(frame * 46, open(Some([0.29, 0.55]))));
        }
        for (offset, point) in [
            (0, Some([0.48, 0.50])),
            (42, Some([0.68, 0.51])),
            (89, Some([0.64, 0.55])),
            (133, None),
            (180, None),
            (222, Some([0.32, 0.53])),
            (306, Some([0.36, 0.51])),
            (348, Some([0.52, 0.58])),
            (390, Some([0.72, 0.56])),
            (438, Some([0.74, 0.52])),
        ] {
            observed.extend(detector.observe(
                1_288 + offset,
                point.map_or(Eyes::Closed, |point| open(Some(point))),
            ));
        }
        assert_eq!(observed, vec![Gesture::TogglePointer]);
        detector.clear_progress();

        for (index, x) in [0.30, 0.72, 0.30, 0.73].into_iter().enumerate() {
            let start = 1_775 + index as u64 * 145;
            for now in [start, start + 48] {
                observed.extend(detector.observe(now, open(Some([x, 0.52]))));
            }
        }
        for frame in 0..12 {
            observed.extend(detector.observe(2_305 + frame * 45, open(Some([0.70, 0.50]))));
        }
        assert_eq!(observed, vec![Gesture::TogglePointer]);

        for (index, x) in [0.30, 0.72, 0.30, 0.73].into_iter().enumerate() {
            let start = 3_000 + index as u64 * 145;
            for now in [start, start + 48] {
                observed.extend(detector.observe(now, open(Some([x, 0.52]))));
            }
        }
        assert_eq!(
            observed,
            vec![Gesture::TogglePointer, Gesture::TogglePointer]
        );
    }

    #[test]
    fn ignores_incomplete_reversed_small_vertical_and_diagonal_glances() {
        for path in [
            &[[0.30, 0.50], [0.70, 0.50]][..],
            &[[0.30, 0.50], [0.70, 0.50], [0.30, 0.50]],
            &[[0.70, 0.50], [0.30, 0.50], [0.70, 0.50], [0.30, 0.50]],
            &[[0.42, 0.50], [0.58, 0.50], [0.42, 0.50], [0.58, 0.50]],
            &[[0.50, 0.30], [0.50, 0.70], [0.50, 0.30], [0.50, 0.70]],
            &[[0.30, 0.34], [0.70, 0.65], [0.30, 0.35], [0.70, 0.64]],
        ] {
            let mut detector = Detector::default();
            for (step, &point) in path.iter().enumerate() {
                for frame in 0..3 {
                    assert_eq!(
                        detector.observe(step as u64 * 180 + frame * 45, open(Some(point))),
                        None,
                        "path {path:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scan_discards_partial_progress_across_head_movement_or_tracking_interruptions() {
        for scenario in 0..8 {
            let mut detector = Detector::default();
            for frame in 0..16 {
                let x = if matches!(frame, 0..=3 | 8..=11) {
                    0.30
                } else {
                    0.70
                };
                let gaze = Some([x, 0.50]);
                let now = frame * 47 + if scenario == 3 && frame >= 8 { 235 } else { 0 };
                let eyes = match (scenario, frame) {
                    (0, 7) => Eyes::Unreliable,
                    (1, 7) => open(None),
                    (2, 7..=11) => Eyes::Closed,
                    (4, 7) => Eyes::Open {
                        gaze,
                        pose: [0.10, 0.0],
                    },
                    (5, 5..=9) => Eyes::Open {
                        gaze,
                        pose: [(frame - 4) as f64 * 0.05, 0.0],
                    },
                    (6, 5 | 7) => open(Some([0.50, 0.50])),
                    (7, 7) => open(Some([f64::NAN, 0.50])),
                    _ => open(gaze),
                };
                assert_eq!(detector.observe(now, eyes), None, "scenario {scenario}");
            }
        }
    }

    #[test]
    fn scan_expires_during_a_pause_despite_reliable_frames() {
        let mut detector = Detector::default();
        for frame in 0..40 {
            let x = match frame {
                0..=2 | 31..=33 => 0.30,
                _ => 0.70,
            };
            assert_eq!(detector.observe(frame * 47, open(Some([x, 0.50]))), None);
        }
    }

    #[test]
    fn triple_blink_aborts_a_scan_and_requests_reset() {
        let mut detector = Detector::default();
        let mut observed = Vec::new();
        for frame in 0..17 {
            let eyes = match frame {
                6 | 9 | 12 => Eyes::Closed,
                2 | 3 | 13.. => open(Some([0.70, 0.50])),
                _ => open(Some([0.30, 0.50])),
            };
            observed.extend(detector.observe(frame * 47, eyes));
        }
        assert_eq!(observed, vec![Gesture::ResetCenter]);
    }

    #[test]
    fn recognizes_three_distinct_blinks_without_a_mapping() {
        let mut detector = Detector::default();
        let mut observed = Vec::new();
        for frame in 0..22 {
            let eyes = match frame {
                3 | 4 | 9 | 10 | 15 | 16 => Eyes::Closed,
                _ => open(None),
            };
            observed.extend(detector.observe(frame * 45, eyes));
        }
        assert_eq!(observed, vec![Gesture::ResetCenter]);
    }

    #[test]
    fn rejects_slow_blinks_long_closures_tracking_loss_and_stream_gaps() {
        for scenario in 0..5 {
            let mut detector = Detector::default();
            let mut observed = Vec::new();
            for frame in 0..50 {
                let eyes = match (scenario, frame) {
                    (0, 3 | 19 | 35) | (1, 3..=12 | 16 | 22) => Eyes::Closed,
                    (2, 4) => Eyes::Unreliable,
                    (2..=4, 3 | 9 | 15) => Eyes::Closed,
                    _ => open(None),
                };
                let adjustment = match scenario {
                    3 if frame > 3 => 280,
                    4 if frame == 4 => 0,
                    _ => 45,
                };
                observed.extend(detector.observe(frame * 45 + adjustment, eyes));
            }
            assert!(
                observed.is_empty(),
                "scenario {scenario} produced {observed:?}"
            );
        }
    }
}
