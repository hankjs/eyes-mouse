use crate::calibration::{
    median, reset_corner, Features, Fixation, Mapping, Point, Validation, CALIBRATION_POINTS,
    RESET_POINTS, VALIDATION_POINTS,
};
use crate::gestures::{Detector, Eyes, Gesture};
use crate::macos::{self, Screen};
use crate::snake;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::oneshot;

const STREAM_STALE: Duration = Duration::from_millis(350);
const TRACKING_LOST: Duration = Duration::from_millis(850);
const HEAD_MAX_SPEED: f64 = 875.0;
const HEAD_MAX_FRAME_TIME: Duration = Duration::from_millis(64);
const HEAD_CAPTURE_SETTLE: Duration = Duration::from_millis(250);
const HEAD_CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
const HEAD_CAPTURE_SAMPLES: usize = 8;
const RESET_TARGET_TIMEOUT: Duration = Duration::from_secs(15);
const RESET_SETTLE: Duration = Duration::from_millis(500);
const EYE_DAMPING_FILE: &str = "eye-damping.json";
const CONTROL_MODE_FILE: &str = "control-mode.json";

type HeadPose = [f64; 2];

#[derive(Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ControlMode {
    #[default]
    Eye,
    Head,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "u8")]
struct EyeDamping(u8);

impl Default for EyeDamping {
    fn default() -> Self {
        Self(50)
    }
}

impl TryFrom<u8> for EyeDamping {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value > 100 {
            return Err("眼动阻尼必须在 0 到 100 之间。");
        }
        Ok(Self(value))
    }
}

impl EyeDamping {
    fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn weight(self, distance: f64) -> f64 {
        let weight = (0.12 + 3.0 * distance).min(0.68);
        // Scale the adaptive weight from 1.4× at minimum damping to 0.6× at maximum damping.
        let adjustment = f64::from(Self::default().0) - f64::from(self.0);
        weight * (1.0 + adjustment / 125.0)
    }
}

fn preference_path(app: &AppHandle, name: &str) -> Result<PathBuf, tauri::Error> {
    Ok(app.path().app_config_dir()?.join(name))
}

fn write_preference(path: &Path, value: impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = serde_json::to_vec(&value)?;
    fs::create_dir_all(path.parent().ok_or("无法读取设置目录")?)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    if let Err(error) = fs::write(&temporary, bytes).and_then(|_| fs::rename(&temporary, path)) {
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    Ok(())
}

fn head_velocity([yaw, pitch]: HeadPose, [neutral_yaw, neutral_pitch]: HeadPose) -> Point {
    const DEAD_ZONE: f64 = 0.08;
    let response = |angle: f64, full_speed: f64| {
        let strength = (angle.abs() - DEAD_ZONE) / (full_speed - DEAD_ZONE);
        angle.signum() * strength.clamp(0.0, 1.0)
    };
    // The camera frame is unmirrored. Rightward turns decrease yaw, and nodding down increases pitch.
    let x = response(neutral_yaw - yaw, 0.35);
    let y = response(pitch - neutral_pitch, 0.28);
    let scale = x.hypot(y).max(1.0);
    [x / scale, y / scale]
}

#[derive(Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Camera {
    #[default]
    Off,
    Starting,
    Running,
    Error,
}

#[derive(Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Quality {
    #[default]
    Searching,
    EyesClosed,
    Tracked,
    Stale,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PointerAccess {
    #[default]
    Missing,
    Waiting,
    Granted,
    RestartRequired,
    Restarting,
}

impl PointerAccess {
    fn current() -> Self {
        if macos::can_move_pointer() {
            Self::Granted
        } else {
            Self::without_post_access()
        }
    }

    fn without_post_access() -> Self {
        if macos::accessibility_trusted() {
            Self::RestartRequired
        } else {
            Self::Missing
        }
    }

    fn in_progress(self) -> bool {
        matches!(self, Self::Waiting | Self::Restarting)
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Packet {
    Ready,
    Sample {
        timestamp: u64,
        quality: Quality,
        features: Option<Features>,
        pose: Option<HeadPose>,
        #[serde(default)]
        bilateral_closed: bool,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    camera: Camera,
    quality: Quality,
    control_mode: ControlMode,
    eye_damping: EyeDamping,
    calibrated: bool,
    pointer_enabled: bool,
    pointer_access: PointerAccess,
    pause_available: bool,
    point: Option<Point>,
    validation: Option<Validation>,
    message: Option<String>,
    reset_exercise: Option<ResetStep>,
    snake_open: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Exercise {
    Calibrate,
    Verify,
}

impl Exercise {
    fn targets(self) -> &'static [Point] {
        match self {
            Self::Calibrate => &CALIBRATION_POINTS,
            Self::Verify => &VALIDATION_POINTS,
        }
    }
}

struct Session {
    mode: Exercise,
    screen: Screen,
    samples: Vec<(Features, Point)>,
    capture: Option<Instant>,
}

#[derive(Clone, Serialize)]
struct Step {
    point: Point,
    index: usize,
    total: usize,
}

#[derive(Clone, Serialize)]
struct ResetStep {
    id: u32,
    #[serde(flatten)]
    step: Step,
}

struct ResetSession {
    id: u32,
    screen: Screen,
    visible: bool,
    expires: Instant,
    target: Option<(Point, Instant)>,
    samples: Vec<(Features, Point)>,
    fixation: Fixation,
}

impl ResetSession {
    fn new(id: u32, screen: Screen, now: Instant) -> Self {
        Self {
            id,
            screen,
            visible: false,
            expires: now + RESET_TARGET_TIMEOUT,
            target: None,
            samples: Vec::new(),
            fixation: Fixation::default(),
        }
    }

    fn step(&self) -> Option<ResetStep> {
        Some(ResetStep {
            id: self.id,
            step: Step {
                point: *RESET_POINTS.get(self.samples.len())?,
                index: self.samples.len() + 1,
                total: RESET_POINTS.len(),
            },
        })
    }

    fn track(&mut self, now: Instant, timestamp: u64, features: Option<Features>) -> bool {
        let Some((target, ready)) = self.target else {
            return false;
        };
        let Some(features) = features.filter(|features| {
            self.samples
                .first()
                .is_none_or(|(center, _)| reset_corner(*center, *features, self.samples.len()))
        }) else {
            self.fixation.clear();
            return false;
        };
        if now - ready < RESET_SETTLE {
            return false;
        }
        let Some(sample) = self.fixation.observe(timestamp, features) else {
            return false;
        };
        self.samples.push((sample, target));
        self.target = None;
        self.fixation.clear();
        self.expires = now + RESET_TARGET_TIMEOUT;
        true
    }
}

enum MainAction {
    OpenReset(u32),
    CloseReset(u32),
    Exit(u64),
    RestartForPointerAccess,
}

impl Session {
    fn step(&self) -> Option<Step> {
        Some(Step {
            point: *self.mode.targets().get(self.samples.len())?,
            index: self.samples.len() + 1,
            total: self.mode.targets().len(),
        })
    }
}

#[derive(Serialize)]
struct CaptureResult {
    next: Option<Step>,
    message: Option<String>,
}

#[derive(Default)]
struct Runtime {
    camera: Camera,
    quality: Quality,
    control_mode: ControlMode,
    eye_damping: EyeDamping,
    child: Option<Child>,
    generation: u64,
    camera_started: Option<Instant>,
    last_frame: Option<Instant>,
    last_tracked: Option<Instant>,
    last_publish: Option<Instant>,
    recent: VecDeque<(Instant, Features)>,
    head_samples: Vec<HeadPose>,
    head_capture: Option<Instant>,
    neutral: Option<(Screen, HeadPose)>,
    head_motion_at: Option<Instant>,
    stream: Option<Channel<Snapshot>>,
    session: Option<Session>,
    closing_exercise: bool,
    gestures: Detector,
    reset_sequence: u32,
    reset: Option<ResetSession>,
    reset_fullscreen: Option<u32>,
    exiting: bool,
    mapping: Option<(Screen, Mapping)>,
    validation: Option<Validation>,
    point: Option<Point>,
    pointer_enabled: bool,
    pointer_access: PointerAccess,
    pointer_access_request: u64,
    activation_generation: u64,
    pause_available: bool,
    message: Option<String>,
    snake: snake::Status,
}

type Shared = Mutex<Runtime>;
type PermissionChecks = Mutex<macos::PermissionChecks>;

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

impl Runtime {
    fn publish(&mut self) {
        self.last_publish = Some(Instant::now());
        let snapshot = Snapshot {
            camera: self.camera,
            quality: self.quality,
            control_mode: self.control_mode,
            eye_damping: self.eye_damping,
            calibrated: self.calibrated(),
            pointer_enabled: self.pointer_enabled,
            pointer_access: self.pointer_access,
            pause_available: self.pause_available,
            point: self.point.filter(|_| self.quality == Quality::Tracked),
            validation: self
                .validation
                .filter(|_| self.control_mode == ControlMode::Eye),
            message: self.message.clone(),
            reset_exercise: self
                .reset
                .as_ref()
                .filter(|reset| reset.visible)
                .and_then(ResetSession::step),
            snake_open: matches!(self.snake, snake::Status::Open { .. }),
        };
        if let Some(channel) = &self.stream {
            if channel.send(snapshot).is_err() {
                self.disarm();
                self.stream = None;
            }
        }
    }

    fn calibrated(&self) -> bool {
        match self.control_mode {
            ControlMode::Eye => self.mapping.is_some(),
            ControlMode::Head => self.neutral.is_some(),
        }
    }

    fn request_pointer_access(
        &mut self,
        current: impl FnOnce() -> PointerAccess,
        prompt: impl FnOnce(),
    ) -> Result<(), String> {
        if self.exiting || self.pointer_access.in_progress() {
            return Ok(());
        }
        if self.calibration_active() || self.head_capture.is_some() {
            return Err("请先结束校准或采样。".into());
        }
        self.pointer_access = current();
        if self.pointer_access == PointerAccess::Missing {
            self.disarm();
            self.pointer_access = PointerAccess::Waiting;
            self.pointer_access_request = self.pointer_access_request.wrapping_add(1);
            self.message = None;
            prompt();
        }
        self.publish();
        Ok(())
    }

    fn cancel_pointer_access(&mut self) {
        if self.pointer_access == PointerAccess::Waiting {
            self.pointer_access = PointerAccess::Missing;
            self.publish();
        }
    }

    fn pending_pointer_access(&self) -> Option<u64> {
        (!self.exiting && self.pointer_access == PointerAccess::Waiting)
            .then_some(self.pointer_access_request)
    }

    fn observe_pointer_grant(&mut self, request: u64) -> Option<MainAction> {
        if self.pending_pointer_access() != Some(request) {
            return None;
        }
        self.pointer_access = PointerAccess::Restarting;
        self.publish();
        Some(MainAction::RestartForPointerAccess)
    }

    fn set_eye_damping(
        &mut self,
        path: &Path,
        eye_damping: EyeDamping,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.eye_damping != eye_damping {
            write_preference(path, eye_damping)?;
            self.eye_damping = eye_damping;
            self.publish();
        }
        Ok(())
    }

    fn disarm(&mut self) {
        self.pointer_enabled = false;
        self.head_motion_at = None;
        self.activation_generation = self.activation_generation.wrapping_add(1);
        self.gestures.clear_progress();
    }

    fn arm(&mut self) {
        self.disarm();
        self.pointer_enabled = true;
        self.head_motion_at = Some(Instant::now());
        self.message = None;
    }

    fn clear_tracking(&mut self, quality: Quality) {
        self.disarm();
        self.quality = quality;
        self.point = None;
        self.last_tracked = None;
        self.recent.clear();
        self.head_samples.clear();
        self.head_capture = None;
    }

    fn can_sample(&self) -> bool {
        self.camera == Camera::Running
            && self.quality == Quality::Tracked
            && self
                .last_tracked
                .is_some_and(|time| time.elapsed() < STREAM_STALE)
    }

    fn calibration_active(&self) -> bool {
        self.session.is_some()
            || self.closing_exercise
            || self.reset.is_some()
            || self.reset_fullscreen.is_some()
    }

    fn cancel_reset(&mut self) -> Option<MainAction> {
        self.reset.take()?;
        self.gestures.clear_progress();
        self.reset_fullscreen.map(MainAction::CloseReset)
    }

    fn stop(&mut self, message: Option<String>) -> Option<MainAction> {
        self.generation = self.generation.wrapping_add(1);
        let action = self.cancel_reset();
        if let Some(mut child) = self.child.take() {
            // Let the PyInstaller bootloader forward the signal and remove its temporary files.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(700);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if !matches!(child.try_wait(), Ok(Some(_))) {
                // A stuck bootloader and its worker share their own process group.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
            }
        }
        self.clear_tracking(Quality::Searching);
        self.neutral = None;
        self.camera_started = None;
        self.last_frame = None;
        self.camera = if message.is_some() {
            Camera::Error
        } else {
            Camera::Off
        };
        self.message = message;
        self.publish();
        action
    }

    fn watchdog(&mut self) -> Option<MainAction> {
        if self.camera == Camera::Starting
            && self
                .camera_started
                .is_some_and(|time| time.elapsed() > Duration::from_secs(45))
        {
            return self.stop(Some("相机启动超时。请检查系统相机权限，然后重试。".into()));
        }
        if self.camera != Camera::Running {
            return None;
        }
        let action = if self
            .reset
            .as_ref()
            .is_some_and(|reset| reset.expires <= Instant::now() || self.stream.is_none())
        {
            self.message = Some("快速校准超时。请在相机中保持双眼可见，再眨眼三次重试。".into());
            let action = self.cancel_reset();
            self.publish();
            action
        } else {
            None
        };
        let mut changed = false;
        if self
            .last_frame
            .is_some_and(|time| time.elapsed() > STREAM_STALE)
            && self.quality != Quality::Stale
        {
            if self.control_mode == ControlMode::Head {
                self.clear_tracking(Quality::Stale);
            } else {
                self.quality = Quality::Stale;
                self.point = None;
                self.gestures.clear_progress();
                if let Some(reset) = &mut self.reset {
                    reset.fixation.clear();
                }
            }
            changed = true;
        }
        if self
            .last_tracked
            .is_some_and(|time| time.elapsed() > TRACKING_LOST)
        {
            self.clear_tracking(self.quality);
            changed = true;
        }
        if changed {
            self.publish();
        }
        action
    }

    fn accept(&mut self, packet: Packet) -> Option<MainAction> {
        match packet {
            Packet::Ready => {
                self.camera = Camera::Running;
                self.last_frame = Some(Instant::now());
                self.publish();
            }
            Packet::Error { message } => return self.stop(Some(message)),
            Packet::Sample {
                timestamp,
                quality,
                features,
                pose,
                bilateral_closed,
            } => {
                let elapsed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                if (elapsed.as_millis() as u64).abs_diff(timestamp)
                    > STREAM_STALE.as_millis() as u64
                {
                    self.gestures.clear_progress();
                    if let Some(reset) = &mut self.reset {
                        reset.fixation.clear();
                    }
                    return None;
                }
                let now = Instant::now();
                let previous_quality = self.quality;
                let was_enabled = self.pointer_enabled;
                if (self.control_mode == ControlMode::Head
                    && self
                        .last_frame
                        .is_some_and(|time| now - time > STREAM_STALE))
                    || self
                        .last_tracked
                        .is_some_and(|time| now - time > TRACKING_LOST)
                {
                    self.clear_tracking(Quality::Stale);
                }
                self.last_frame = Some(now);
                let mut action = None;
                match self.control_mode {
                    ControlMode::Eye => {
                        self.quality = quality;
                        let features = features.filter(|features| {
                            quality == Quality::Tracked
                                && features.iter().all(|value| value.is_finite())
                        });
                        if let Some(features) = features {
                            self.last_tracked = Some(now);
                            self.recent.push_back((now, features));
                        } else {
                            self.point = None;
                        }
                        action = if self.reset.is_some() {
                            self.advance_reset(now, timestamp, features)
                        } else {
                            self.observe_gesture(timestamp, features, pose, bilateral_closed)
                        };
                        if let Some(features) = features {
                            self.update_point(features);
                        }
                        while self
                            .recent
                            .front()
                            .is_some_and(|(time, _)| now - *time > Duration::from_secs(2))
                        {
                            self.recent.pop_front();
                        }
                    }
                    ControlMode::Head => {
                        if let Some(pose) =
                            pose.filter(|pose| pose.iter().all(|angle| angle.is_finite()))
                        {
                            self.track_head(pose, now);
                        } else {
                            self.clear_tracking(Quality::Searching);
                        }
                    }
                }
                if previous_quality != self.quality
                    || was_enabled != self.pointer_enabled
                    || self
                        .last_publish
                        .is_none_or(|time| time.elapsed() > Duration::from_millis(62))
                {
                    self.publish();
                }
                return action;
            }
        }
        None
    }

    fn gesture_available(&self) -> bool {
        self.camera == Camera::Running
            && self.control_mode == ControlMode::Eye
            && self.stream.is_some()
            && !self.calibration_active()
            && !self.pointer_access.in_progress()
            && !self.exiting
    }

    fn observe_gesture(
        &mut self,
        timestamp: u64,
        features: Option<Features>,
        pose: Option<HeadPose>,
        bilateral_closed: bool,
    ) -> Option<MainAction> {
        if !self.gesture_available() {
            return None;
        }
        let pose = pose.filter(|pose| pose.iter().all(|angle| angle.abs() <= 0.55));
        let eyes = match (pose, features) {
            (Some(pose), Some(features)) => Eyes::Open {
                gaze: self
                    .mapping
                    .as_ref()
                    .filter(|(screen, _)| *screen == Screen::primary())
                    .map(|(_, mapping)| mapping.eye_point(features)),
                pose,
            },
            (Some(_), None) if self.quality == Quality::EyesClosed && bilateral_closed => {
                Eyes::Closed
            }
            _ => Eyes::Unreliable,
        };
        match self.gestures.observe(timestamp, eyes)? {
            Gesture::TogglePointer => {
                if self.pointer_enabled {
                    self.disarm();
                    self.message = Some("已用左右眼动手势暂停指针。".into());
                } else {
                    self.message = match check_pointer_ready(self) {
                        Err(message) => Some(message),
                        Ok(()) if !macos::can_move_pointer() => {
                            self.pointer_access = PointerAccess::without_post_access();
                            Some("请先在主窗口完成辅助功能授权。".into())
                        }
                        Ok(()) => {
                            self.pointer_access = PointerAccess::Granted;
                            self.arm();
                            Some("已用左右眼动手势启用指针。".into())
                        }
                    };
                }
                self.publish();
                None
            }
            Gesture::Exit => {
                self.disarm();
                Some(MainAction::Exit(self.activation_generation))
            }
            Gesture::ResetCenter => {
                self.disarm();
                self.reset_sequence = self.reset_sequence.wrapping_add(1);
                self.reset = Some(ResetSession::new(
                    self.reset_sequence,
                    Screen::primary(),
                    Instant::now(),
                ));
                self.message = None;
                self.publish();
                Some(MainAction::OpenReset(self.reset_sequence))
            }
        }
    }

    fn advance_reset(
        &mut self,
        now: Instant,
        timestamp: u64,
        features: Option<Features>,
    ) -> Option<MainAction> {
        let reset = self.reset.as_mut()?;
        if self.stream.is_none() || reset.expires <= now {
            return None;
        }
        if reset.screen != Screen::primary() {
            self.message = Some("主显示器已改变，请重新校准。".into());
            let action = self.cancel_reset();
            self.publish();
            return action;
        }
        if !reset.track(now, timestamp, features) {
            return None;
        }
        if let Ok(samples) = <&[(Features, Point); 3]>::try_from(reset.samples.as_slice()) {
            let screen = reset.screen;
            let previous = self
                .mapping
                .as_ref()
                .filter(|(previous_screen, _)| *previous_screen == screen)
                .map(|(_, mapping)| mapping);
            self.message = Some(
                match Mapping::reset_center(previous, samples) {
                    Ok(mapping) => {
                        self.mapping = Some((screen, mapping));
                        self.validation = None;
                        self.point = None;
                        "快速校准已完成。指针已暂停。"
                    }
                    Err(error) => error,
                }
                .into(),
            );
            let action = self.cancel_reset();
            self.publish();
            return action;
        }
        self.publish();
        None
    }

    fn update_point(&mut self, features: Features) {
        let Some((screen, mapping)) = &self.mapping else {
            return;
        };
        if *screen != Screen::primary() {
            self.mapping = None;
            self.validation = None;
            self.disarm();
            self.point = None;
            self.message = Some("主显示器已改变，请重新校准。".into());
            return;
        }
        let mut point = mapping
            .predict(features)
            .map(|position| position.clamp(0.0, 1.0));
        if let Some(last) = self.point {
            let distance = (point[0] - last[0]).hypot(point[1] - last[1]);
            let weight = self.eye_damping.weight(distance);
            for axis in 0..2 {
                point[axis] = last[axis] + (point[axis] - last[axis]) * weight;
            }
        }
        self.point = Some(point);
        if self.pointer_enabled
            && (!macos::can_move_pointer() || screen.move_pointer(point).is_err())
        {
            self.pointer_access_error();
        }
    }

    fn track_head(&mut self, pose: HeadPose, now: Instant) {
        self.quality = Quality::Tracked;
        self.last_tracked = Some(now);
        if let Some(started) = self.head_capture {
            if (HEAD_CAPTURE_SETTLE..=HEAD_CAPTURE_TIMEOUT).contains(&(now - started))
                && self.head_samples.len() < HEAD_CAPTURE_SAMPLES
            {
                self.head_samples.push(pose);
            }
        } else {
            self.update_head(pose, now);
        }
    }

    fn update_head(&mut self, pose: HeadPose, now: Instant) {
        let Some((screen, neutral)) = self.neutral else {
            return;
        };
        if screen != Screen::primary() {
            self.neutral = None;
            self.disarm();
            self.point = None;
            self.message = Some("主显示器已改变，请重新设定正面位置。".into());
            return;
        }
        let velocity = head_velocity(pose, neutral);
        self.point = Some(velocity.map(|component| 0.5 + 0.44 * component));
        if !self.pointer_enabled {
            return;
        }
        let elapsed = now - self.head_motion_at.replace(now).unwrap_or(now);
        if velocity == [0.0; 2] {
            return;
        }
        let distance = HEAD_MAX_SPEED * elapsed.min(HEAD_MAX_FRAME_TIME).as_secs_f64();
        let offset = velocity.map(|component| component * distance);
        if !macos::can_move_pointer() {
            self.pointer_access_error();
            return;
        }
        match screen.move_pointer_by(offset) {
            Ok(true) => {}
            Ok(false) => {
                self.disarm();
                self.message = Some("指针已移出主显示器。请移回主显示器，再启用指针。".into());
            }
            Err(()) => self.pointer_access_error(),
        }
    }

    fn pointer_access_error(&mut self) {
        self.disarm();
        self.pointer_access = PointerAccess::current();
        self.message = Some("无法移动指针。请检查主窗口中的辅助功能权限。".into());
    }
}

fn dispatch_main_action(app: &AppHandle, action: Option<MainAction>) {
    let Some(action) = action else { return };
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || match action {
        MainAction::OpenReset(id) => open_reset(&handle, id),
        MainAction::CloseReset(id) => {
            let _ = close_reset(&handle, id);
        }
        MainAction::Exit(activation_generation) => {
            {
                let state = handle.state::<Shared>();
                let mut runtime = lock(&state);
                if runtime.activation_generation != activation_generation
                    || !runtime.gesture_available()
                    || !runtime.can_sample()
                {
                    return;
                }
                runtime.exiting = true;
                // Reap the tracker before requesting exit, including Tauri's direct-exit fallback.
                runtime.stop(None);
            }
            lock(&handle.state::<PermissionChecks>()).close();
            handle.exit(0);
        }
        MainAction::RestartForPointerAccess => {
            let unregister_pause = {
                let state = handle.state::<Shared>();
                let mut runtime = lock(&state);
                if runtime.exiting || runtime.pointer_access != PointerAccess::Restarting {
                    return;
                }
                runtime.exiting = true;
                // Reap the tracker even if Tauri falls back to restarting without an Exit event.
                runtime.stop(None);
                runtime.pause_available
            };
            if unregister_pause {
                // LaunchServices can start the new process before this process has exited.
                let _ = handle.global_shortcut().unregister(pause_shortcut());
            }
            lock(&handle.state::<PermissionChecks>()).close();
            handle.request_restart();
        }
    });
}

fn open_reset(app: &AppHandle, id: u32) {
    let state = app.state::<Shared>();
    {
        let runtime = lock(&state);
        if runtime.stream.is_none() || !runtime.reset.as_ref().is_some_and(|reset| reset.id == id) {
            return;
        }
    }
    let window = app.get_webview_window("main");
    let result = (|| -> Result<(), String> {
        let window = window.as_ref().ok_or("无法打开快速校准窗口。")?;
        app.show().map_err(|error| error.to_string())?;
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().map_err(|error| error.to_string())?;
        monitor(window)?;
        destroy_snake(app)?;
        window.set_focus().map_err(|error| error.to_string())?;
        window
            .set_simple_fullscreen(true)
            .map_err(|error| error.to_string())?;
        Ok(())
    })();
    let mut runtime = lock(&state);
    let reset = runtime.reset.as_mut().filter(|reset| reset.id == id);
    match (reset, result) {
        (Some(reset), Ok(())) => {
            reset.visible = true;
            reset.expires = Instant::now() + RESET_TARGET_TIMEOUT;
            runtime.reset_fullscreen = Some(id);
            runtime.publish();
        }
        (Some(_), Err(error)) => {
            runtime.cancel_reset();
            runtime.message = Some(error);
            runtime.publish();
        }
        (None, Ok(())) => {
            drop(runtime);
            let _ = window.unwrap().set_simple_fullscreen(false);
        }
        (None, Err(_)) => {}
    }
}

fn close_reset(app: &AppHandle, id: u32) -> Result<(), String> {
    let state = app.state::<Shared>();
    if lock(&state).reset_fullscreen != Some(id) {
        return Ok(());
    }
    let result = app
        .get_webview_window("main")
        .map(|window| window.set_simple_fullscreen(false))
        .transpose()
        .map_err(|error| error.to_string());
    lock(&state).reset_fullscreen = None;
    result.map(|_| ())
}

#[tauri::command]
fn subscribe(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Shared>,
    channel: Channel<Snapshot>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("请从 Eyes Mouse 主窗口订阅状态。".into());
    }
    let action = {
        let mut runtime = lock(&state);
        let action = runtime.cancel_reset();
        runtime.clear_tracking(Quality::Searching);
        runtime.stream = Some(channel);
        runtime.publish();
        action
    };
    dispatch_main_action(&app, action);
    Ok(())
}

#[tauri::command]
fn set_control_mode(
    app: AppHandle,
    state: State<'_, Shared>,
    control_mode: ControlMode,
) -> Result<(), String> {
    let mut runtime = lock(&state);
    if runtime.control_mode == control_mode {
        return Ok(());
    }
    if runtime.calibration_active() {
        return Err("请先退出屏幕校准或验证。".into());
    }
    runtime.clear_tracking(Quality::Searching);
    runtime.neutral = None;
    runtime.message = None;
    let result = preference_path(&app, CONTROL_MODE_FILE)
        .map_err(Into::into)
        .and_then(|path| write_preference(&path, control_mode))
        .map_err(|error| format!("无法保存控制动作。{error}"));
    if result.is_ok() {
        runtime.control_mode = control_mode;
    }
    runtime.publish();
    result
}

#[tauri::command]
fn set_eye_damping(
    app: AppHandle,
    state: State<'_, Shared>,
    eye_damping: EyeDamping,
) -> Result<EyeDamping, String> {
    let mut runtime = lock(&state);
    preference_path(&app, EYE_DAMPING_FILE)
        .map_err(Into::into)
        .and_then(|path| runtime.set_eye_damping(&path, eye_damping))
        .map_err(|error| format!("无法保存眼动阻尼。{error}"))?;
    Ok(runtime.eye_damping)
}

fn tracker_command() -> Result<Command, String> {
    if cfg!(debug_assertions) {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let python = project.join(".venv/bin/python");
        if !python.exists() || !project.join("tracker/face_landmarker.task").exists() {
            return Err("跟踪组件尚未安装。请在项目目录运行 sh tracker/setup.sh。".into());
        }
        let mut command = Command::new(python);
        command.arg(project.join("tracker/track.py"));
        Ok(command)
    } else {
        let binary = std::env::current_exe()
            .map_err(|error| error.to_string())?
            .with_file_name("eyes-mouse-tracker");
        Ok(Command::new(binary))
    }
}

#[tauri::command]
async fn start_camera(app: AppHandle, state: State<'_, Shared>) -> Result<(), String> {
    let mut command = tracker_command()?;
    let generation = {
        let mut runtime = lock(&state);
        if runtime.exiting || matches!(runtime.camera, Camera::Starting | Camera::Running) {
            return Ok(());
        }
        runtime.generation = runtime.generation.wrapping_add(1);
        runtime.clear_tracking(Quality::Searching);
        runtime.camera = Camera::Starting;
        runtime.camera_started = Some(Instant::now());
        runtime.message = None;
        runtime.publish();
        runtime.generation
    };

    let permission = macos::request_camera_access().await;
    let mut runtime = lock(&state);
    if runtime.generation != generation {
        return Ok(());
    }
    if !permission {
        runtime.stop(Some(
            "请在 macOS「隐私与安全性 → 相机」允许 Eyes Mouse，然后重试。".into(),
        ));
        return Ok(());
    }

    let mut child = match command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            runtime.stop(Some(format!("无法启动跟踪组件。{error}")));
            return Ok(());
        }
    };
    let stdout = child.stdout.take().expect("piped tracker stdout");
    runtime.child = Some(child);
    drop(runtime);

    std::thread::spawn(move || {
        let state = app.state::<Shared>();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(packet) = serde_json::from_str::<Packet>(&line) else {
                continue;
            };
            let action = {
                let mut runtime = lock(&state);
                if runtime.generation != generation {
                    return;
                }
                runtime.accept(packet)
            };
            dispatch_main_action(&app, action);
        }
        let action = {
            let mut runtime = lock(&state);
            if runtime.generation != generation {
                return;
            }
            runtime.stop(Some("跟踪组件已退出。请检查相机权限后重试。".into()))
        };
        dispatch_main_action(&app, action);
    });
    Ok(())
}

#[tauri::command]
fn stop_camera(app: AppHandle, state: State<'_, Shared>) {
    let action = lock(&state).stop(None);
    dispatch_main_action(&app, action);
}

#[tauri::command]
fn pause_pointer(state: State<'_, Shared>) {
    let mut runtime = lock(&state);
    runtime.disarm();
    runtime.publish();
}

#[tauri::command]
fn request_pointer_access(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
    require_pointer_controls(&window)?;
    lock(&state).request_pointer_access(PointerAccess::current, macos::prompt_accessibility)
}

#[tauri::command]
fn cancel_pointer_access(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
    require_pointer_controls(&window)?;
    lock(&state).cancel_pointer_access();
    Ok(())
}

#[tauri::command]
fn open_accessibility_settings(
    window: WebviewWindow,
    state: State<'_, Shared>,
) -> Result<(), String> {
    require_pointer_controls(&window)?;
    {
        let runtime = lock(&state);
        if runtime.exiting || runtime.pointer_access != PointerAccess::Waiting {
            return Ok(());
        }
    }
    if macos::open_accessibility_settings() {
        Ok(())
    } else {
        Err("无法打开系统设置。请从苹果菜单打开，再选择「隐私与安全性 → 辅助功能」。".into())
    }
}

fn require_pointer_controls(window: &WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("请从 Eyes Mouse 主窗口管理指针。".into());
    }
    Ok(())
}

#[tauri::command]
fn enable_pointer(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
    require_pointer_controls(&window)?;
    let mut runtime = lock(&state);
    check_pointer_ready(&runtime)?;
    if !macos::can_move_pointer() {
        return runtime.request_pointer_access(
            PointerAccess::without_post_access,
            macos::prompt_accessibility,
        );
    }
    runtime.pointer_access = PointerAccess::Granted;
    runtime.arm();
    runtime.publish();
    Ok(())
}

fn check_pointer_ready(runtime: &Runtime) -> Result<(), String> {
    if runtime.pointer_access.in_progress() {
        return Err("请先完成辅助功能授权或取消等待。".into());
    }
    if runtime.calibration_active()
        || runtime.head_capture.is_some()
        || runtime.exiting
        || !runtime.calibrated()
        || !runtime.can_sample()
    {
        return Err(match runtime.control_mode {
            ControlMode::Eye => "请先完成校准，并让摄像头看到双眼。",
            ControlMode::Head => "请先设定正面位置，并让摄像头看到脸部。",
        }
        .into());
    }
    if !runtime.pause_available {
        return Err("无法注册暂停快捷键 ⌃⌥P。请释放该组合键后重启应用。".into());
    }
    Ok(())
}

fn monitor(window: &WebviewWindow) -> Result<tauri::Monitor, String> {
    let primary = window
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .ok_or("无法读取主显示器。")?;
    let current = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .ok_or("无法读取窗口所在显示器。")?;
    if primary.position() != current.position() || primary.size() != current.size() {
        return Err("请把 Eyes Mouse 窗口移到主显示器再继续。".into());
    }
    Ok(primary)
}

#[tauri::command]
async fn open_snake(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Shared>,
    launch: State<'_, snake::Launch>,
) -> Result<(), String> {
    require_snake_controls(&window)?;
    let _launch = launch.0.lock().await;
    let calibrating = "请结束屏幕校准后再打开贪吃蛇。";
    {
        let runtime = lock(&state);
        if runtime.calibration_active() {
            return Err(calibrating.into());
        }
        match runtime.snake {
            snake::Status::Open { .. } => return Ok(()),
            snake::Status::Closing => return Err("贪吃蛇正在关闭，请重试。".into()),
            snake::Status::Closed => {}
        }
    }
    let (overlay, loaded) = snake::create(&window)?;
    let loaded = matches!(
        tokio::time::timeout(Duration::from_secs(5), loaded).await,
        Ok(Ok(()))
    );

    let (reply, shown) = oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let state = handle.state::<Shared>();
        let result = (|| {
            {
                let runtime = lock(&state);
                if runtime.calibration_active() {
                    return Err(calibrating.into());
                }
                if matches!(runtime.snake, snake::Status::Closing)
                    || handle.get_webview_window(snake::LABEL).is_none()
                    || handle.get_webview_window("main").is_none()
                {
                    return Err("贪吃蛇已关闭。".into());
                }
            }
            if !loaded {
                return Err("贪吃蛇加载失败，请重试。".into());
            }
            snake::show(&overlay).map_err(|error| error.to_string())?;
            let mut runtime = lock(&state);
            runtime.snake = snake::Status::Open { restart: 0 };
            runtime.publish();
            Ok(())
        })();
        if result.is_err() {
            let _ = destroy_snake(&handle);
        }
        let _ = reply.send(result);
    })
    .map_err(|error| error.to_string())?;
    shown.await.map_err(|error| error.to_string())?
}

fn require_snake_controls(window: &WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("请从 Eyes Mouse 主窗口控制贪吃蛇。".into());
    }
    Ok(())
}

fn destroy_snake(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Shared>();
    if matches!(lock(&state).snake, snake::Status::Closing) {
        return Ok(());
    }
    if let Some(window) = app.get_webview_window(snake::LABEL) {
        window.destroy().map_err(|error| error.to_string())?;
        let _ = window.hide();
        lock(&state).snake = snake::Status::Closing;
    }
    Ok(())
}

#[tauri::command]
fn close_snake(window: WebviewWindow) -> Result<(), String> {
    require_snake_controls(&window)?;
    destroy_snake(window.app_handle())
}

#[tauri::command]
fn restart_snake(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
    require_snake_controls(&window)?;
    if let snake::Status::Open { restart } = &mut lock(&state).snake {
        *restart = restart.wrapping_add(1);
        return Ok(());
    }
    Err("贪吃蛇尚未打开。".into())
}

#[tauri::command]
fn snake_pointer(
    window: WebviewWindow,
    state: State<'_, Shared>,
) -> Result<Option<snake::PointerSample>, String> {
    if window.label() != snake::LABEL {
        return Err("只能在贪吃蛇窗口读取游戏指针。".into());
    }
    let snake::Status::Open { restart } = lock(&state).snake else {
        return Ok(None);
    };
    snake::pointer_sample(&window, restart)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn capture_neutral(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
    monitor(&window)?;
    let started = Instant::now();
    let screen = Screen::primary();
    {
        let mut runtime = lock(&state);
        if runtime.control_mode != ControlMode::Head || !runtime.can_sample() {
            return Err("请选择头动，并让摄像头看到脸部。".into());
        }
        if runtime.head_capture.is_some() {
            return Err("正在采样。".into());
        }
        runtime.disarm();
        runtime.head_samples.clear();
        runtime.head_capture = Some(started);
        runtime.point = None;
        runtime.message = None;
        runtime.publish();
    }

    let mut runtime = loop {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let runtime = lock(&state);
        if runtime.head_capture != Some(started) {
            return Err("正面位置采样已取消。请让摄像头看到脸部后重试。".into());
        }
        if runtime.head_samples.len() == HEAD_CAPTURE_SAMPLES
            || started.elapsed() >= HEAD_CAPTURE_TIMEOUT
        {
            break runtime;
        }
    };
    runtime.head_capture = None;
    let poses = std::mem::take(&mut runtime.head_samples);
    if screen != Screen::primary() {
        return Err("主显示器已改变，请重新设定正面位置。".into());
    }
    if poses.len() < HEAD_CAPTURE_SAMPLES || !runtime.can_sample() {
        return Err("有效采样不足。请面向屏幕、保持头部稳定后重试。".into());
    }
    let neutral = std::array::from_fn(|axis| {
        let mut values: Vec<_> = poses.iter().map(|pose| pose[axis]).collect();
        median(&mut values)
    });
    runtime.neutral = Some((screen, neutral));
    runtime.update_head(*poses.last().unwrap(), Instant::now());
    runtime.publish();
    Ok(())
}

#[tauri::command]
fn begin_exercise(
    window: WebviewWindow,
    state: State<'_, Shared>,
    mode: Exercise,
) -> Result<Step, String> {
    monitor(&window)?;
    let mut runtime = lock(&state);
    if runtime.control_mode != ControlMode::Eye
        || !runtime.can_sample()
        || runtime.calibration_active()
    {
        return Err("请让摄像头看到双眼，再开始。".into());
    }
    let screen = Screen::primary();
    if let Exercise::Verify = mode {
        if !runtime
            .mapping
            .as_ref()
            .is_some_and(|(calibrated_screen, _)| *calibrated_screen == screen)
        {
            return Err("请先完成当前显示器的校准。".into());
        }
    }
    runtime.disarm();
    let session = Session {
        mode,
        screen,
        samples: Vec::new(),
        capture: None,
    };
    let step = session.step().unwrap();
    runtime.session = Some(session);
    runtime.message = None;
    runtime.publish();
    drop(runtime);
    let fullscreen = destroy_snake(window.app_handle()).and_then(|()| {
        window
            .set_simple_fullscreen(true)
            .map_err(|error| error.to_string())
    });
    if let Err(error) = fullscreen {
        lock(&state).session = None;
        return Err(error);
    }
    Ok(step)
}

#[tauri::command]
fn cancel_exercise(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Shared>,
) -> Result<(), String> {
    let mut runtime = lock(&state);
    if runtime.reset.is_some() {
        let action = runtime.cancel_reset();
        runtime.message = Some("已取消快速校准。指针已暂停。".into());
        runtime.publish();
        drop(runtime);
        if let Some(MainAction::CloseReset(id)) = action {
            close_reset(&app, id)?;
        }
        return Ok(());
    }
    if runtime.session.take().is_none() {
        return Ok(());
    }
    runtime.closing_exercise = true;
    runtime.gestures.clear_progress();
    drop(runtime);
    close_manual_exercise(&window, &state)
}

fn close_manual_exercise(window: &WebviewWindow, state: &Shared) -> Result<(), String> {
    let result = window
        .set_simple_fullscreen(false)
        .map_err(|error| error.to_string());
    lock(state).closing_exercise = false;
    result
}

#[tauri::command]
fn reset_target_ready(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Shared>,
    id: u32,
    index: usize,
) -> Result<(), String> {
    let pending = |reset: &ResetSession| {
        reset.id == id
            && reset.visible
            && reset.samples.len() + 1 == index
            && reset.target.is_none()
    };
    if !lock(&state).reset.as_ref().is_some_and(pending) {
        return Ok(());
    }
    let point = RESET_POINTS[index - 1];
    let target = (|| {
        if !window.is_focused().map_err(|error| error.to_string())? {
            return Err("快速校准窗口已失去焦点。".into());
        }
        screen_target(&window, point)
    })();
    let mut runtime = lock(&state);
    let Some(reset) = runtime.reset.as_mut().filter(|reset| pending(reset)) else {
        return Ok(());
    };
    match target {
        Ok(target) => reset.target = Some((target, Instant::now())),
        Err(error) => {
            let action = runtime.cancel_reset();
            runtime.message = Some(error);
            runtime.publish();
            drop(runtime);
            dispatch_main_action(&app, action);
        }
    }
    Ok(())
}

fn screen_target(window: &WebviewWindow, point: Point) -> Result<Point, String> {
    let monitor = monitor(window)?;
    let origin = window.inner_position().map_err(|error| error.to_string())?;
    let size = window.inner_size().map_err(|error| error.to_string())?;
    let expected = monitor.size();
    Ok([
        (f64::from(origin.x - monitor.position().x) + point[0] * f64::from(size.width))
            / f64::from(expected.width),
        (f64::from(origin.y - monitor.position().y) + point[1] * f64::from(size.height))
            / f64::from(expected.height),
    ])
}

#[tauri::command]
async fn capture_target(
    window: WebviewWindow,
    state: State<'_, Shared>,
) -> Result<CaptureResult, String> {
    let started = Instant::now();
    {
        let mut runtime = lock(&state);
        if !runtime.can_sample() {
            return Err("暂时看不到双眼。请调整坐姿，再试一次。".into());
        }
        let session = runtime.session.as_mut().ok_or("请先开始校准或验证。")?;
        if session.capture.is_some() {
            return Err("正在采样。".into());
        }
        session.capture = Some(started);
    }

    tokio::time::sleep(Duration::from_millis(960)).await;
    let (step, screen) = {
        let runtime = lock(&state);
        let session = runtime
            .session
            .as_ref()
            .filter(|session| session.capture == Some(started))
            .ok_or("采样已取消。")?;
        (session.step().unwrap(), session.screen)
    };
    let target = screen_target(&window, step.point);

    let mut runtime = lock(&state);
    let session = runtime
        .session
        .as_mut()
        .filter(|session| session.capture == Some(started))
        .ok_or("采样已取消。")?;
    session.capture = None;
    if screen != Screen::primary() {
        return Err("主显示器已改变。请退出并重新校准。".into());
    }
    let target = target?;
    let settled_at = started + Duration::from_millis(260);
    let frames: Vec<&Features> = runtime
        .recent
        .iter()
        .filter(|(time, _)| *time >= settled_at)
        .map(|(_, features)| features)
        .collect();
    if frames.len() < 10 || !runtime.can_sample() {
        return Err("有效采样不足。请持续注视当前目标，然后再按空格。".into());
    }
    let features = std::array::from_fn(|axis| {
        let mut values: Vec<_> = frames.iter().map(|frame| frame[axis]).collect();
        median(&mut values)
    });

    let session = runtime.session.as_mut().unwrap();
    session.samples.push((features, target));
    if let Some(next) = session.step() {
        return Ok(CaptureResult {
            next: Some(next),
            message: None,
        });
    }
    let session = runtime.session.take().unwrap();
    let message = match session.mode {
        Exercise::Calibrate => match Mapping::fit(&session.samples) {
            Ok(mapping) => {
                runtime.mapping = Some((screen, mapping));
                runtime.validation = None;
                runtime.point = None;
                None
            }
            Err(error) => Some(error.to_owned()),
        },
        Exercise::Verify => {
            let (_, mapping) = runtime.mapping.as_ref().ok_or("请先完成校准。")?;
            let errors = session
                .samples
                .into_iter()
                .map(|(features, target)| {
                    let point = mapping.predict(features);
                    ((point[0] - target[0]) * screen.width)
                        .hypot((point[1] - target[1]) * screen.height)
                })
                .collect();
            runtime.validation = Some(Validation::new(errors));
            None
        }
    };
    runtime.closing_exercise = true;
    runtime.publish();
    drop(runtime);
    close_manual_exercise(&window, &state)?;
    Ok(CaptureResult {
        next: None,
        message,
    })
}

fn pause_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyP)
}

fn cancel_pointer_access_on_exit(app: &AppHandle) {
    {
        let state = app.state::<Shared>();
        let mut runtime = lock(&state);
        runtime.exiting = true;
        runtime.cancel_pointer_access();
    }
    lock(&app.state::<PermissionChecks>()).close();
}

pub fn run() {
    let shortcut = pause_shortcut();
    tauri::Builder::default()
        .manage(Shared::default())
        .manage(PermissionChecks::default())
        .manage(snake::Launch::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, key, event| {
                    if *key == shortcut && event.state() == ShortcutState::Pressed {
                        pause_pointer(app.state());
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let registered = app.global_shortcut().register(shortcut).is_ok();
            let state = app.state::<Shared>();
            let mut runtime = lock(&state);
            runtime.pause_available = registered;
            runtime.pointer_access = PointerAccess::current();
            runtime.control_mode = preference_path(app.handle(), CONTROL_MODE_FILE)
                .ok()
                .and_then(|path| fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            if let Ok(path) = preference_path(app.handle(), EYE_DAMPING_FILE) {
                runtime.eye_damping = EyeDamping::load(&path);
            }
            drop(runtime);
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(220));
                let (action, request) = {
                    let state = handle.state::<Shared>();
                    let mut runtime = lock(&state);
                    (runtime.watchdog(), runtime.pending_pointer_access())
                };
                dispatch_main_action(&handle, action);
                let grant = lock(&handle.state::<PermissionChecks>()).poll(request);
                if let Some(request) = grant {
                    let action = lock(&handle.state::<Shared>()).observe_pointer_grant(request);
                    dispatch_main_action(&handle, action);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            subscribe,
            set_control_mode,
            set_eye_damping,
            start_camera,
            stop_camera,
            pause_pointer,
            request_pointer_access,
            cancel_pointer_access,
            open_accessibility_settings,
            enable_pointer,
            capture_neutral,
            begin_exercise,
            cancel_exercise,
            capture_target,
            reset_target_ready,
            open_snake,
            close_snake,
            restart_snake,
            snake_pointer
        ])
        .build(tauri::generate_context!())
        .expect("build Eyes Mouse")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { .. } => {
                cancel_pointer_access_on_exit(app);
            }
            tauri::RunEvent::Exit => {
                cancel_pointer_access_on_exit(app);
                lock(&app.state::<Shared>()).stop(None);
            }
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Destroyed,
                ..
            } if label == snake::LABEL => {
                let state = app.state::<Shared>();
                let mut runtime = lock(&state);
                runtime.snake = snake::Status::Closed;
                runtime.publish();
            }
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Destroyed,
                ..
            } if label == "main" => {
                cancel_pointer_access_on_exit(app);
                let _ = destroy_snake(app);
            }
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Focused(true),
                ..
            } if label == "main" => {
                let state = app.state::<Shared>();
                let mut runtime = lock(&state);
                if !runtime.exiting && !runtime.pointer_access.in_progress() {
                    let access = PointerAccess::current();
                    if access != runtime.pointer_access {
                        runtime.pointer_access = access;
                        if access != PointerAccess::Granted {
                            runtime.disarm();
                        }
                        runtime.publish();
                    }
                }
            }
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Focused(false),
                ..
            } if label == "main" => {
                let action = {
                    let state = app.state::<Shared>();
                    let mut runtime = lock(&state);
                    if !runtime.reset.as_ref().is_some_and(|reset| reset.visible) {
                        return;
                    }
                    runtime.message = Some("快速校准窗口已失去焦点。".into());
                    let action = runtime.cancel_reset();
                    runtime.publish();
                    action
                };
                dispatch_main_action(app, action);
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_access_does_not_prompt_or_restart_when_permission_already_exists() {
        let mut runtime = Runtime::default();
        assert_eq!(runtime.pending_pointer_access(), None);
        for access in [PointerAccess::Granted, PointerAccess::RestartRequired] {
            runtime
                .request_pointer_access(|| access, || panic!("prompted despite permission"))
                .unwrap();
            assert_eq!(runtime.pointer_access, access);
            assert_eq!(runtime.pending_pointer_access(), None);
            assert!(runtime.observe_pointer_grant(0).is_none());
        }
    }

    #[test]
    fn pointer_access_restarts_once_after_an_explicit_request_and_a_new_grant() {
        let mut runtime = Runtime {
            pointer_enabled: true,
            ..Runtime::default()
        };
        let mut prompts = 0;
        runtime
            .request_pointer_access(|| PointerAccess::Missing, || prompts += 1)
            .unwrap();
        let request = runtime.pending_pointer_access().unwrap();
        runtime
            .request_pointer_access(|| panic!("request already pending"), || prompts += 1)
            .unwrap();
        assert_eq!(prompts, 1);
        assert_eq!(runtime.pointer_access, PointerAccess::Waiting);
        assert!(!runtime.pointer_enabled);
        assert_eq!(runtime.pending_pointer_access(), Some(request));
        assert!(matches!(
            runtime.observe_pointer_grant(request),
            Some(MainAction::RestartForPointerAccess)
        ));
        assert_eq!(runtime.pointer_access, PointerAccess::Restarting);
        assert_eq!(runtime.pending_pointer_access(), None);
        assert!(runtime.observe_pointer_grant(request).is_none());
    }

    #[test]
    fn pointer_access_cancels_waiting_and_allows_a_later_retry() {
        let mut runtime = Runtime::default();
        let mut prompts = 0;
        runtime
            .request_pointer_access(|| PointerAccess::Missing, || prompts += 1)
            .unwrap();
        let canceled = runtime.pending_pointer_access().unwrap();
        runtime.cancel_pointer_access();
        assert_eq!(runtime.pointer_access, PointerAccess::Missing);
        assert_eq!(runtime.pending_pointer_access(), None);
        assert!(runtime.observe_pointer_grant(canceled).is_none());
        runtime
            .request_pointer_access(|| PointerAccess::Missing, || prompts += 1)
            .unwrap();
        assert_eq!(prompts, 2);
        assert_eq!(runtime.pointer_access, PointerAccess::Waiting);
        let retried = runtime.pending_pointer_access().unwrap();
        assert_ne!(retried, canceled);
        assert!(runtime.observe_pointer_grant(canceled).is_none());
        runtime.exiting = true;
        assert_eq!(runtime.pending_pointer_access(), None);
        assert!(runtime.observe_pointer_grant(retried).is_none());
    }

    #[test]
    fn eye_damping_keeps_the_default_response_and_bounds_every_setting() {
        for distance in [0.0, 0.04, 0.18, 0.5, 2.0_f64.sqrt()] {
            assert_eq!(
                EyeDamping::default().weight(distance),
                (0.12 + 3.0 * distance).min(0.68)
            );
            let mut previous = 1.0;
            for value in 0..=100 {
                let weight = EyeDamping::try_from(value).unwrap().weight(distance);
                assert!(weight > 0.0 && weight < previous);
                previous = weight;
            }
        }
    }

    #[test]
    fn eye_damping_loads_saved_values_and_survives_invalid_files_and_failed_writes() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("eyes-mouse-{}-{timestamp}", std::process::id()));
        let path = directory.join(EYE_DAMPING_FILE);
        let mut runtime = Runtime::default();
        assert_eq!(EyeDamping::load(&path), runtime.eye_damping);

        let saved = EyeDamping::try_from(80).unwrap();
        runtime.set_eye_damping(&path, saved).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "80");
        assert_eq!(EyeDamping::load(&path), saved);

        fs::create_dir(path.with_extension(format!("{}.tmp", std::process::id()))).unwrap();
        assert!(runtime
            .set_eye_damping(&path, EyeDamping::try_from(20).unwrap())
            .is_err());
        assert_eq!(runtime.eye_damping, saved);
        assert_eq!(EyeDamping::load(&path), saved);

        for malformed in ["", "{", "101", "-1", "25.5", "\"25\""] {
            fs::write(&path, malformed).unwrap();
            assert_eq!(EyeDamping::load(&path), EyeDamping::default());
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn head_velocity_uses_user_directions_from_the_captured_center() {
        let neutral = [0.12, -0.14];
        assert_eq!(head_velocity([0.16, -0.19], neutral), [0.0, 0.0]);
        assert!(head_velocity([-0.04, -0.14], neutral)[0] > 0.0);
        assert!(head_velocity([0.28, -0.14], neutral)[0] < 0.0);
        assert!(head_velocity([0.12, 0.02], neutral)[1] > 0.0);
        assert!(head_velocity([0.12, -0.30], neutral)[1] < 0.0);
        let [x, y] = head_velocity([-0.9, 0.9], neutral);
        assert!((x.hypot(y) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn head_tracking_survives_closed_eyes_but_reacquiring_a_face_does_not_rearm() {
        let frame = |pose| Packet::Sample {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            quality: Quality::EyesClosed,
            features: None,
            pose,
            bilateral_closed: true,
        };
        let mut runtime = Runtime {
            camera: Camera::Running,
            control_mode: ControlMode::Head,
            pointer_enabled: true,
            ..Runtime::default()
        };

        runtime.accept(frame(Some([0.0, 0.0])));
        assert!(runtime.can_sample());
        assert!(runtime.pointer_enabled);

        runtime.point = Some([0.5, 0.5]);
        runtime.head_capture = Some(Instant::now() - HEAD_CAPTURE_SETTLE);
        runtime.accept(frame(Some([0.0, 0.0])));
        assert_eq!(runtime.head_samples.len(), 1);
        runtime.accept(frame(None));
        assert!(!runtime.can_sample());
        assert!(!runtime.pointer_enabled);
        assert!(runtime.point.is_none());
        assert!(runtime.head_capture.is_none());
        assert!(runtime.head_samples.is_empty());

        runtime.accept(frame(Some([0.0, 0.0])));
        assert!(runtime.can_sample());
        assert!(!runtime.pointer_enabled);
    }

    #[test]
    fn neutral_capture_accepts_settled_samples_at_ten_frames_per_second() {
        let started = Instant::now() - Duration::from_secs(1);
        let mut runtime = Runtime {
            camera: Camera::Running,
            head_capture: Some(started),
            ..Runtime::default()
        };

        for frame in 0..9 {
            runtime.track_head([0.12, -0.14], started + Duration::from_millis(frame * 100));
        }
        assert_eq!(runtime.head_samples.len(), 6);
        runtime.track_head([0.12, -0.14], started + Duration::from_millis(900));
        runtime.track_head([0.12, -0.14], started + Duration::from_secs(1));
        assert_eq!(runtime.head_samples.len(), HEAD_CAPTURE_SAMPLES);
        assert!(runtime.can_sample());
    }

    #[test]
    fn stale_frames_clear_preview_and_disarm_pointer() {
        let stale_at = Instant::now() - Duration::from_secs(1);
        let mut runtime = Runtime {
            camera: Camera::Running,
            quality: Quality::Tracked,
            last_frame: Some(stale_at),
            last_tracked: Some(stale_at),
            point: Some([0.5, 0.5]),
            pointer_enabled: true,
            ..Runtime::default()
        };

        runtime.watchdog();

        assert!(runtime.quality == Quality::Stale);
        assert!(runtime.point.is_none());
        assert!(!runtime.pointer_enabled);
    }

    #[test]
    fn rejected_reset_times_out_without_replacing_the_mapping_or_resuming_the_pointer() {
        let now = Instant::now();
        let screen = Screen::primary();
        let features = |[x, y]: Point| [0.59 - 0.18 * x, 0.15 * y, 0.0, 0.0];
        let mapping =
            Mapping::fit(&CALIBRATION_POINTS.map(|point| (features(point), point))).unwrap();
        let previous_point = mapping.predict(features([0.40, 0.60]));
        let mut reset = ResetSession::new(1, screen, now);
        reset.visible = true;
        reset
            .samples
            .push((features(RESET_POINTS[0]), RESET_POINTS[0]));
        reset.target = Some((RESET_POINTS[1], now));
        let mut runtime = Runtime {
            camera: Camera::Running,
            mapping: Some((screen, mapping)),
            stream: Some(Channel::new(|_| Ok(()))),
            reset: Some(reset),
            reset_fullscreen: Some(1),
            ..Runtime::default()
        };
        let mut corner = features(RESET_POINTS[1]);
        corner[2] = 0.40;
        for timestamp in (500..=2_000).step_by(45) {
            let time = now + Duration::from_millis(timestamp);
            assert!(runtime
                .advance_reset(time, timestamp, Some(corner))
                .is_none());
        }
        let reset = runtime.reset.as_mut().unwrap();
        assert_eq!(reset.samples.len(), 1);
        reset.expires = now;
        assert!(matches!(
            runtime.watchdog(),
            Some(MainAction::CloseReset(1))
        ));
        assert!(runtime.reset.is_none());
        assert!(!runtime.pointer_enabled);
        assert_eq!(
            runtime.mapping.unwrap().1.predict(features([0.40, 0.60])),
            previous_point
        );
    }
}
