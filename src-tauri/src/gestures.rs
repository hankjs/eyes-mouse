use crate::calibration::Point;
use std::f64::consts::{PI, TAU};

pub const MAX_FRAME_GAP_MS: u64 = 220;
const CIRCLE_DURATION_MS: std::ops::RangeInclusive<u64> = 750..=2_900;
const RING_RADIUS: std::ops::RangeInclusive<f64> = 0.16..=0.66;

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
    circle: Option<Circle>,
    circle_blocked_until: Option<u64>,
    centered_since: Option<u64>,
    blink: BlinkSequence,
}

impl Detector {
    pub fn clear_progress(&mut self) {
        self.last_frame = None;
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
                self.circle = None;
                self.centered_since = None;
                return None;
            }
            Eyes::Unreliable => {
                self.clear_progress();
                return None;
            }
        };
        let Some(point) = gaze else {
            self.circle = None;
            return None;
        };
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
            return None;
        }
        if !RING_RADIUS.contains(&radius) {
            self.circle = None;
            return None;
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

                if circle.clockwise.min(circle.counterclockwise) <= 0.65 {
                    let turn = circle.clockwise - circle.counterclockwise;
                    let movement_duration = circle
                        .movement_started
                        .map_or(0, |start| circle.movement_ended - start);
                    if circle.sectors.count_ones() >= 14
                        && circle.moving_samples >= 16
                        && turn.abs() >= TAU - 0.33
                        && distance(point, circle.start_point) <= 0.15
                    {
                        if moving && CIRCLE_DURATION_MS.contains(&movement_duration) {
                            self.clear_progress();
                            self.circle_blocked_until = Some(now + 1_250);
                            // Screen Y increases downward, so the right-to-down turn is positive.
                            return Some(if turn > 0.0 {
                                Gesture::TogglePointer
                            } else {
                                Gesture::Exit
                            });
                        }
                        if turn.abs() >= TAU - 0.08 {
                            self.circle = None;
                        }
                    }
                    return None;
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
        None
    }
}

fn distance([x, y]: Point, [other_x, other_y]: Point) -> f64 {
    (x - other_x).hypot(y - other_y)
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
    fn recognizes_each_direction_from_any_start_and_requires_center_before_repeating() {
        for (turn, gesture) in [(TAU, Gesture::TogglePointer), (-TAU, Gesture::Exit)] {
            let mut detector = Detector::default();
            let ring = |index: u32| point(0.71 + turn * f64::from(index) / 30.0, 0.32);
            let mut observed = Vec::new();
            for frame in 0..=100 {
                observed
                    .extend(detector.observe(500 + 45 * u64::from(frame), open(Some(ring(frame)))));
            }
            assert_eq!(observed, vec![gesture]);
            for frame in 0..=6 {
                assert_eq!(
                    detector.observe(5_100 + frame * 45, open(Some([0.5; 2]))),
                    None
                );
            }
            assert_eq!(
                (0..=30)
                    .filter_map(|frame| detector
                        .observe(5_420 + 45 * u64::from(frame), open(Some(ring(frame)))))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn rejects_small_fast_broken_reversing_and_head_driven_movements() {
        for scenario in 0..7 {
            let mut detector = Detector::default();
            let mut observed = Vec::new();
            for frame in 0_u32..=30 {
                let travel = match scenario {
                    2 => TAU * f64::from(frame / 7) / 4.0,
                    3 => TAU * f64::from(frame.min(30 - frame)) / 20.0,
                    6 => TAU * f64::from(frame / 3) / 10.0,
                    _ => TAU * f64::from(frame) / 30.0,
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
