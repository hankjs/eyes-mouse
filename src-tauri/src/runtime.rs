use crate::calibration::{
    median, reset_corner, Features, Fixation, Mapping, Point, Validation, CALIBRATION_POINTS,
    RESET_POINTS, VALIDATION_POINTS,
};
use crate::gestures::{Detector, Eyes, Gesture, MAX_FRAME_GAP_MS};
use crate::macos::{self, Display, Motion, MotionResult, Screen, WindowFrame};
use crate::refinement::CorrectionError;
use crate::snake;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
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
const CAPTURE_SETTLE: Duration = Duration::from_millis(260);
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(1_400);
const RESET_TARGET_TIMEOUT: Duration = Duration::from_secs(15);
const RESET_SETTLE: Duration = Duration::from_millis(500);
const RESET_NEEDS_CALIBRATION: &str = "请先对当前显示器做九点校准，再用三次眨眼回正。";
const REFINEMENT_TIMEOUT: Duration = Duration::from_secs(12);
const REFINEMENT_EXPIRED: &str = "局部校正已超时。指针已暂停。";
const REFINEMENT_NEEDS_GAZE: &str = "请看准目标约一秒，再按 ⌃⇧M。";
const EYE_DAMPING_FILE: &str = "eye-damping.json";
const CONTROL_MODE_FILE: &str = "control-mode.json";
const LEGACY_IDENTIFIER: &str = "com.hankjs.eyesmouse";
const DISPLAY_RECONFIGURING: &str = "显示器设置正在更改，请稍后重试。";

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

fn load_preference<T: DeserializeOwned + Default>(path: &Path, legacy_path: &Path) -> T {
    let bytes = match fs::read(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => fs::read(legacy_path),
        result => result,
    };
    bytes
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
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
    displays: Vec<Display>,
    selected_display_id: Option<u32>,
    display_revision: u32,
    calibrated: bool,
    pointer_enabled: bool,
    pointer_access: PointerAccess,
    pause_available: bool,
    refinement_available: bool,
    refinement_active: bool,
    refinement_revision: u32,
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
    capture: Option<Capture>,
}

struct Capture {
    started: Instant,
    last_sample: Option<Instant>,
    fixation: Fixation,
}

impl Capture {
    fn new(started: Instant) -> Self {
        Self {
            started,
            last_sample: None,
            fixation: Fixation::default(),
        }
    }

    fn track(
        &mut self,
        received_at: Instant,
        sampled_at: Instant,
        timestamp: u64,
        features: Option<Features>,
    ) {
        if sampled_at >= self.started + CAPTURE_SETTLE
            && received_at <= self.started + CAPTURE_TIMEOUT
        {
            self.last_sample = Some(sampled_at);
            self.fixation.observe(timestamp, features);
        }
    }

    fn sample(&self) -> Option<Features> {
        let deadline = self.started + CAPTURE_TIMEOUT;
        if deadline - self.last_sample? > Duration::from_millis(MAX_FRAME_GAP_MS) {
            return None;
        }
        self.fixation.sample()
    }
}

fn sample_time(received_at: Instant, received_timestamp: u64, timestamp: u64) -> (Instant, u64) {
    // Clock skew must not make a sample newer than its arrival or lengthen a fixation.
    let timestamp = timestamp.min(received_timestamp);
    let age = Duration::from_millis(received_timestamp - timestamp);
    (received_at - age, timestamp)
}

fn timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        if now - ready < RESET_SETTLE {
            return false;
        }
        let features = features.filter(|features| {
            self.samples
                .first()
                .is_none_or(|(center, _)| reset_corner(*center, *features, self.samples.len()))
        });
        self.fixation.observe(timestamp, features);
        let Some(sample) = self.fixation.sample() else {
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
    CloseExercise,
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

    fn capturing(&self, started: Instant) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.started == started)
    }
}

#[derive(Serialize)]
struct CaptureResult {
    next: Option<Step>,
    message: Option<String>,
}

struct RefinementCapture {
    screen: Screen,
    features: Features,
    expires: Instant,
    resume_pointer: bool,
}

#[derive(Default)]
struct Runtime {
    camera: Camera,
    quality: Quality,
    control_mode: ControlMode,
    eye_damping: EyeDamping,
    displays: Vec<Display>,
    preferred_display_id: Option<u32>,
    selected_screen: Option<Screen>,
    display_revision: u32,
    child: Option<Child>,
    generation: u64,
    camera_started: Option<Instant>,
    last_frame: Option<Instant>,
    last_tracked: Option<Instant>,
    last_publish: Option<Instant>,
    head_samples: Vec<HeadPose>,
    head_capture: Option<Instant>,
    neutral: Option<(Screen, HeadPose)>,
    head_motion_at: Option<Instant>,
    stream: Option<Channel<Snapshot>>,
    session: Option<Session>,
    exercise_frame: Option<WindowFrame>,
    closing_exercise: bool,
    gestures: Detector,
    reset_sequence: u32,
    reset: Option<ResetSession>,
    exiting: bool,
    mapping: Option<(Screen, Mapping)>,
    validation: Option<Validation>,
    point: Option<Point>,
    pointer_enabled: bool,
    pointer_access: PointerAccess,
    pointer_access_request: u64,
    activation_generation: u64,
    pause_available: bool,
    refinement_available: bool,
    refinement_key_down: bool,
    refinement_revision: u32,
    refinement_fixation: Fixation,
    refinement_capture: Option<RefinementCapture>,
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
            displays: self.displays.clone(),
            selected_display_id: self.selected_screen.map(|screen| screen.id),
            display_revision: self.display_revision,
            calibrated: self.calibrated(),
            pointer_enabled: self.pointer_enabled,
            pointer_access: self.pointer_access,
            pause_available: self.pause_available,
            refinement_available: self.refinement_available,
            refinement_active: self.refinement_capture.is_some(),
            refinement_revision: self.refinement_revision,
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

    fn update_displays(&mut self, displays: Vec<Display>) -> Option<MainAction> {
        let target = displays
            .iter()
            .find(|display| match self.preferred_display_id {
                Some(id) => display.screen.id == id,
                None => display.primary,
            })
            .map(|display| display.screen);
        let changed = self.displays != displays;
        self.displays = displays;
        if self.selected_screen != target {
            self.update_selected_screen(target)
        } else {
            if changed {
                self.publish();
            }
            None
        }
    }

    fn update_selected_screen(&mut self, target: Option<Screen>) -> Option<MainAction> {
        let initialized = self.display_revision != 0;
        self.selected_screen = target;
        self.display_revision = self.display_revision.wrapping_add(1);
        self.disarm();
        self.mapping = None;
        self.validation = None;
        self.neutral = None;
        self.point = None;
        self.head_samples.clear();
        self.head_capture = None;
        let action = self.cancel_exercises();
        if initialized {
            self.message = Some(
                match (target, self.control_mode) {
                    (Some(_), ControlMode::Eye) => "目标显示器已更改。指针已暂停，请重新校准。",
                    (Some(_), ControlMode::Head) => {
                        "目标显示器已更改。指针已暂停，请重新设定正面位置。"
                    }
                    (None, _) => {
                        "目标显示器已断开或暂不可用。指针已暂停，请重新连接或选择其他显示器。"
                    }
                }
                .into(),
            );
        }
        self.publish();
        action
    }

    fn selected_screen_is_current(&self) -> bool {
        self.selected_screen
            .is_some_and(|screen| screen.is_current(self.preferred_display_id.is_none()))
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
        if self.pointer_access != PointerAccess::Granted && self.refinement_capture.is_some() {
            self.disarm();
        }
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
        self.refinement_fixation.clear();
        if self.refinement_capture.take().is_some() {
            self.refinement_revision = self.refinement_revision.wrapping_add(1);
            self.message = Some("局部校正已取消。指针已暂停。".into());
        }
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
        self.clear_eye_fixations();
        self.head_samples.clear();
        self.head_capture = None;
    }

    fn clear_eye_fixations(&mut self) {
        self.refinement_fixation.clear();
        if let Some(capture) = self
            .session
            .as_mut()
            .and_then(|session| session.capture.as_mut())
        {
            capture.fixation.clear();
        }
        if let Some(reset) = &mut self.reset {
            reset.fixation.clear();
        }
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
            || self.exercise_frame.is_some()
    }

    fn cancel_reset(&mut self) -> Option<MainAction> {
        self.reset.take()?;
        self.end_exercise()
    }

    fn cancel_exercises(&mut self) -> Option<MainAction> {
        self.session = None;
        self.reset = None;
        self.end_exercise()
    }

    fn end_exercise(&mut self) -> Option<MainAction> {
        self.gestures.clear_progress();
        if self.exercise_frame.is_some() && !self.closing_exercise {
            self.closing_exercise = true;
            Some(MainAction::CloseExercise)
        } else {
            None
        }
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
            self.message =
                Some("快速回正超时，已保留原校准。请保持双眼可见后重试，或重新做九点校准。".into());
            let action = self.cancel_reset();
            self.publish();
            action
        } else {
            None
        };
        let mut changed = false;
        if let Some(capture) = &self.refinement_capture {
            if capture.expires <= Instant::now() {
                self.disarm();
                self.message = Some(REFINEMENT_EXPIRED.into());
                changed = true;
            } else if macos::display_reconfiguring() {
                self.disarm();
                changed = true;
            } else if !macos::can_move_pointer() {
                self.pointer_access_error();
                changed = true;
            }
        }
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
                self.clear_eye_fixations();
                if self.refinement_capture.is_some() {
                    self.disarm();
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
                let now = Instant::now();
                let received_timestamp = timestamp_millis();
                if received_timestamp.abs_diff(timestamp) > STREAM_STALE.as_millis() as u64 {
                    self.gestures.clear_progress();
                    self.clear_eye_fixations();
                    return None;
                }
                if self.calibration_active() || self.head_capture.is_some() {
                    if macos::display_reconfiguring() {
                        self.head_samples.clear();
                        self.clear_eye_fixations();
                        return None;
                    }
                    if self.selected_screen.is_some() && !self.selected_screen_is_current() {
                        return self.update_selected_screen(None);
                    }
                }
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
                        if features.is_some() {
                            self.last_tracked = Some(now);
                        } else {
                            self.point = None;
                        }
                        let (sampled_at, sampled_timestamp) =
                            sample_time(now, received_timestamp, timestamp);
                        if self.refinement_available
                            && self.mapping.is_some()
                            && self.refinement_capture.is_none()
                            && !self.calibration_active()
                        {
                            self.refinement_fixation
                                .observe(sampled_timestamp, features);
                        }
                        if let Some(capture) = self
                            .session
                            .as_mut()
                            .and_then(|session| session.capture.as_mut())
                        {
                            capture.track(now, sampled_at, sampled_timestamp, features);
                        }
                        action = if self.reset.is_some() {
                            self.advance_reset(now, timestamp, features)
                        } else {
                            self.observe_gesture(timestamp, features, pose, bilateral_closed)
                        };
                        if let Some(features) = features {
                            self.update_point(features, was_enabled);
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
            && self.refinement_capture.is_none()
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
                    .filter(|(screen, _)| Some(*screen) == self.selected_screen)
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
                            Some("指针未启用，请查看「辅助功能权限」状态。".into())
                        }
                        Ok(()) => Some(match self.activate_pointer() {
                            Ok(()) => "已用左右眼动手势启用指针。".into(),
                            Err(error) => error,
                        }),
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
                let Some(screen) = self.selected_screen else {
                    self.message = Some("请选择可用的目标显示器，再开始九点校准。".into());
                    self.publish();
                    return None;
                };
                if !self
                    .mapping
                    .as_ref()
                    .is_some_and(|(calibrated, _)| *calibrated == screen)
                {
                    self.message = Some(RESET_NEEDS_CALIBRATION.into());
                    self.publish();
                    return None;
                }
                self.reset_sequence = self.reset_sequence.wrapping_add(1);
                self.reset = Some(ResetSession::new(
                    self.reset_sequence,
                    screen,
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
        if !reset.track(now, timestamp, features) {
            return None;
        }
        if let Ok(samples) = <&[(Features, Point); 3]>::try_from(reset.samples.as_slice()) {
            let screen = reset.screen;
            let result = self
                .mapping
                .as_ref()
                .filter(|(previous_screen, _)| {
                    *previous_screen == screen && self.selected_screen == Some(screen)
                })
                .ok_or(RESET_NEEDS_CALIBRATION)
                .and_then(|(_, mapping)| mapping.reset_center(samples));
            self.message = Some(
                match result {
                    Ok(mapping) => {
                        self.mapping = Some((screen, mapping));
                        self.validation = None;
                        self.point = None;
                        "视线已回正。指针已暂停。"
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

    fn finish_calibration(
        &mut self,
        screen: Screen,
        samples: &[(Features, Point)],
    ) -> Result<(), &'static str> {
        let mapping = Mapping::fit(samples)?;
        self.mapping = Some((screen, mapping));
        self.validation = None;
        self.point = None;
        Ok(())
    }

    fn update_point(&mut self, features: Features, was_enabled: bool) {
        let Some((screen, mapping)) = &self.mapping else {
            return;
        };
        let screen = *screen;
        let mut point = mapping.pointer_point(features);
        if let Some(last) = self.point {
            let distance = (point[0] - last[0]).hypot(point[1] - last[1]);
            let weight = self.eye_damping.weight(distance);
            for axis in 0..2 {
                point[axis] = last[axis] + (point[axis] - last[axis]) * weight;
            }
        }
        self.point = Some(point);
        if self.pointer_enabled && was_enabled {
            self.move_pointer(screen, Motion::Eye(point));
        }
    }

    fn refinement_key_pressed(&mut self, state: ShortcutState) -> bool {
        let pressed = state == ShortcutState::Pressed;
        let was_pressed = std::mem::replace(&mut self.refinement_key_down, pressed);
        self.refinement_available && pressed && !was_pressed
    }

    fn handle_refinement_shortcut(&mut self, state: ShortcutState) {
        if !self.refinement_key_pressed(state) {
            return;
        }
        let revision = self.refinement_revision.wrapping_add(1);
        if let Err(message) = self.toggle_refinement() {
            if self.refinement_capture.is_some() {
                self.disarm();
            }
            self.message = Some(message.into());
        }
        self.refinement_revision = revision;
        self.publish();
    }

    fn toggle_refinement(&mut self) -> Result<(), &'static str> {
        if self.control_mode != ControlMode::Eye
            || self.camera != Camera::Running
            || self
                .last_frame
                .is_none_or(|time| time.elapsed() > STREAM_STALE)
            || self.calibration_active()
            || self.exiting
            || self.stream.is_none()
        {
            return Err("请先在眼动模式完成校准，并保持摄像头开启。");
        }
        if self.pointer_access.in_progress() {
            return Err("请先完成辅助功能授权或取消等待。");
        }
        if !macos::can_move_pointer() {
            self.pointer_access = PointerAccess::without_post_access();
            return Err("请先在主窗口授权辅助功能。");
        }
        if macos::display_reconfiguring() {
            return Err(DISPLAY_RECONFIGURING);
        }
        let screen = self.selected_screen.ok_or("请选择可用的目标显示器。")?;
        let cursor = screen
            .cursor_point(self.preferred_display_id.is_none())
            .ok_or("请确认指针位于当前控制屏幕，再重试局部校正。")?;
        let now = Instant::now();
        self.advance_refinement(screen, cursor, now, timestamp_millis())
    }

    fn advance_refinement(
        &mut self,
        screen: Screen,
        cursor: Point,
        now: Instant,
        timestamp: u64,
    ) -> Result<(), &'static str> {
        let (_, mapping) = self
            .mapping
            .as_mut()
            .filter(|(calibrated_screen, _)| *calibrated_screen == screen)
            .ok_or("请先对当前控制屏幕做九点校准。")?;
        if let Some(capture) = self.refinement_capture.take() {
            if capture.expires <= now {
                return Err(REFINEMENT_EXPIRED);
            }
            if capture.screen != screen {
                return Err("控制屏幕已改变。请重新开始局部校正。");
            }
            mapping
                .refine(capture.features, cursor)
                .map_err(|error| match error {
                    CorrectionError::InvalidPoint => "未取得有效的校正位置。请重试。",
                    CorrectionError::TooFar => "校正偏差过大。请看准目标重试，或重新做九点校准。",
                    CorrectionError::ConflictsWithSample => {
                        "这次校正与附近已保存的位置冲突。请看准目标重试。"
                    }
                })?;
            self.validation = None;
            self.point = Some(cursor);
            if capture.resume_pointer && self.pause_available && self.can_sample() {
                // The cursor is already on the selected screen. Arming here never warps it.
                self.arm();
                self.message = Some("已学习这处校正，眼动指针已恢复。".into());
            } else {
                self.message = Some("已学习这处校正。指针保持暂停。".into());
            }
        } else {
            let features = self
                .refinement_fixation
                .recent_sample(timestamp)
                .filter(|_| self.can_sample())
                .ok_or(REFINEMENT_NEEDS_GAZE)?;
            let resume_pointer = self.pointer_enabled;
            self.disarm();
            self.refinement_capture = Some(RefinementCapture {
                screen,
                features,
                expires: now + REFINEMENT_TIMEOUT,
                resume_pointer,
            });
            self.message =
                Some("用鼠标或触控板把指针移到目标，再按 ⌃⇧M 保存。按 ⌃⌥P 取消。".into());
        }
        Ok(())
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
        let velocity = head_velocity(pose, neutral);
        self.point = Some(velocity.map(|component| 0.5 + 0.44 * component));
        if !self.pointer_enabled {
            return;
        }
        let elapsed = now - self.head_motion_at.replace(now).unwrap_or(now);
        let distance = HEAD_MAX_SPEED * elapsed.min(HEAD_MAX_FRAME_TIME).as_secs_f64();
        let offset = velocity.map(|component| component * distance);
        self.move_pointer(screen, Motion::Head(offset));
    }

    fn activate_pointer(&mut self) -> Result<(), String> {
        if macos::display_reconfiguring() {
            return Err(DISPLAY_RECONFIGURING.into());
        }
        let screen = self.selected_screen.ok_or("请选择可用的目标显示器。")?;
        if self.move_pointer(screen, Motion::Activate) {
            self.pointer_access = PointerAccess::Granted;
            self.arm();
            Ok(())
        } else {
            Err(self
                .message
                .clone()
                .unwrap_or_else(|| "无法启用指针。".into()))
        }
    }

    fn move_pointer(&mut self, screen: Screen, motion: Motion) -> bool {
        if !macos::can_move_pointer() {
            self.pointer_access_error();
            return false;
        }
        match screen.move_pointer(motion, self.preferred_display_id.is_none()) {
            Ok(MotionResult::Inside) => return true,
            Ok(MotionResult::Outside) => {
                self.disarm();
                self.message = Some("指针已移出目标显示器，控制已暂停。".into());
            }
            Ok(MotionResult::DisplayChanged) => {
                self.update_selected_screen(None);
            }
            Ok(MotionResult::Reconfiguring) => {
                if matches!(motion, Motion::Activate) {
                    self.message = Some(DISPLAY_RECONFIGURING.into());
                }
            }
            Err(()) => self.pointer_access_error(),
        }
        false
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
        MainAction::CloseExercise => {
            let _ = close_exercise(&handle);
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
            {
                let state = handle.state::<Shared>();
                let mut runtime = lock(&state);
                if runtime.exiting || runtime.pointer_access != PointerAccess::Restarting {
                    return;
                }
                runtime.exiting = true;
                // Reap the tracker even if Tauri falls back to restarting without an Exit event.
                runtime.stop(None);
            }
            // LaunchServices can start the new process before this process has exited.
            let _ = handle.global_shortcut().unregister_all();
            lock(&handle.state::<PermissionChecks>()).close();
            handle.request_restart();
        }
    });
}

async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (complete, result) = oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = complete.send(task());
    })
    .map_err(|error| error.to_string())?;
    result.await.map_err(|error| error.to_string())?
}

fn refresh_displays(app: &AppHandle) {
    if macos::display_reconfiguring() {
        return;
    }
    let displays = macos::displays();
    let action = lock(&app.state::<Shared>()).update_displays(displays);
    dispatch_main_action(app, action);
}

fn open_reset(app: &AppHandle, id: u32) {
    refresh_displays(app);
    let state = app.state::<Shared>();
    let screen = {
        let runtime = lock(&state);
        let Some(reset) = runtime.reset.as_ref().filter(|reset| reset.id == id) else {
            return;
        };
        if runtime.stream.is_none() {
            return;
        }
        reset.screen
    };
    let window = app.get_webview_window("main");
    let result = (|| {
        if macos::display_reconfiguring() {
            return Err(DISPLAY_RECONFIGURING.into());
        }
        let window = window.as_ref().ok_or("无法打开快速回正窗口。")?;
        app.show().map_err(|error| error.to_string())?;
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().map_err(|error| error.to_string())?;
        destroy_snake(app)?;
        window.set_focus().map_err(|error| error.to_string())?;
        macos::open_exercise(window, screen)
    })();
    let mut runtime = lock(&state);
    let reset = runtime.reset.as_mut().filter(|reset| reset.id == id);
    match (reset, result) {
        (Some(reset), Ok(frame)) => {
            reset.visible = true;
            reset.expires = Instant::now() + RESET_TARGET_TIMEOUT;
            runtime.exercise_frame = Some(frame);
            runtime.publish();
        }
        (Some(_), Err(error)) => {
            runtime.cancel_reset();
            runtime.message = Some(error);
            runtime.publish();
        }
        (None, Ok(frame)) => {
            drop(runtime);
            let _ = macos::close_exercise(window.as_ref().unwrap(), frame);
        }
        (None, Err(_)) => {}
    }
}

fn close_exercise(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Shared>();
    let frame = lock(&state).exercise_frame.take();
    let Some(frame) = frame else {
        return Ok(());
    };
    let result = app
        .get_webview_window("main")
        .map(|window| macos::close_exercise(&window, frame))
        .transpose();
    lock(&state).closing_exercise = false;
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
        return Err("请从 Gaze Mouse 主窗口订阅状态。".into());
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
async fn set_target_display(window: WebviewWindow, display_id: u32) -> Result<(), String> {
    require_pointer_controls(&window)?;
    let app = window.app_handle().clone();
    let handle = app.clone();
    on_main(&app, move || {
        if macos::display_reconfiguring() {
            return Err(DISPLAY_RECONFIGURING.into());
        }
        let displays = macos::displays();
        let available = displays
            .iter()
            .any(|display| display.screen.id == display_id);
        let action = {
            let state = handle.state::<Shared>();
            let mut runtime = lock(&state);
            if available {
                runtime.preferred_display_id = Some(display_id);
            }
            runtime.update_displays(displays)
        };
        if matches!(action, Some(MainAction::CloseExercise)) {
            close_exercise(&handle)?;
        }
        if !available {
            return Err("该显示器暂不可用，请重新选择。".into());
        }
        Ok(())
    })
    .await
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
            "请在 macOS「隐私与安全性 → 相机」允许 Gaze Mouse，然后重试。".into(),
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
        return Err("请从 Gaze Mouse 主窗口管理指针。".into());
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
    let result = runtime.activate_pointer();
    runtime.publish();
    result
}

fn check_pointer_ready(runtime: &Runtime) -> Result<(), String> {
    if runtime.refinement_capture.is_some() {
        return Err("请先保存或取消局部校正。".into());
    }
    if runtime.pointer_access.in_progress() {
        return Err("请先完成辅助功能授权或取消等待。".into());
    }
    if macos::display_reconfiguring() {
        return Err(DISPLAY_RECONFIGURING.into());
    }
    if !runtime.selected_screen_is_current() {
        return Err("请选择可用的目标显示器。".into());
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
        return Err("请从 Gaze Mouse 主窗口控制贪吃蛇。".into());
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
    require_pointer_controls(&window)?;
    let started = Instant::now();
    let (screen, display_revision) = {
        let mut runtime = lock(&state);
        if runtime.control_mode != ControlMode::Head || !runtime.can_sample() {
            return Err("请选择头动，并让摄像头看到脸部。".into());
        }
        if macos::display_reconfiguring() {
            return Err(DISPLAY_RECONFIGURING.into());
        }
        let screen = runtime
            .selected_screen
            .filter(|_| runtime.selected_screen_is_current())
            .ok_or("请选择可用的目标显示器。")?;
        if runtime.head_capture.is_some() {
            return Err("正在采样。".into());
        }
        runtime.disarm();
        runtime.head_samples.clear();
        runtime.head_capture = Some(started);
        runtime.point = None;
        runtime.message = None;
        runtime.publish();
        (screen, runtime.display_revision)
    };

    let mut runtime = loop {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let runtime = lock(&state);
        if runtime.display_revision != display_revision {
            return Err(match runtime.selected_screen {
                Some(_) => "目标显示器已改变，请重新设定正面位置。",
                None => "目标显示器已断开或暂不可用，请选择可用的显示器，再设定正面位置。",
            }
            .into());
        }
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
    if macos::display_reconfiguring() {
        return Err(DISPLAY_RECONFIGURING.into());
    }
    if !runtime.selected_screen_is_current() {
        runtime.update_selected_screen(None);
        return Err("目标显示器已改变，请重新设定正面位置。".into());
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
async fn begin_exercise(window: WebviewWindow, mode: Exercise) -> Result<Step, String> {
    require_pointer_controls(&window)?;
    let app = window.app_handle().clone();
    on_main(&app, move || begin_exercise_on_main(&window, mode)).await
}

fn begin_exercise_on_main(window: &WebviewWindow, mode: Exercise) -> Result<Step, String> {
    if macos::display_reconfiguring() {
        return Err(DISPLAY_RECONFIGURING.into());
    }
    let app = window.app_handle();
    refresh_displays(app);
    let state = app.state::<Shared>();
    let mut runtime = lock(&state);
    if runtime.control_mode != ControlMode::Eye
        || !runtime.can_sample()
        || runtime.calibration_active()
    {
        return Err("请让摄像头看到双眼，再开始。".into());
    }
    let screen = runtime
        .selected_screen
        .filter(|_| runtime.selected_screen_is_current())
        .ok_or("请选择可用的目标显示器。")?;
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
    let fullscreen = destroy_snake(app).and_then(|()| macos::open_exercise(window, screen));
    let mut runtime = lock(&state);
    match fullscreen {
        Ok(frame) if runtime.session.is_some() => {
            runtime.exercise_frame = Some(frame);
            Ok(step)
        }
        Ok(frame) => {
            drop(runtime);
            let _ = macos::close_exercise(window, frame);
            Err("校准已取消，请重新开始。".into())
        }
        Err(error) => {
            runtime.session = None;
            Err(error)
        }
    }
}

#[tauri::command]
async fn cancel_exercise(window: WebviewWindow) -> Result<(), String> {
    require_pointer_controls(&window)?;
    let app = window.app_handle().clone();
    let handle = app.clone();
    on_main(&app, move || {
        let state = handle.state::<Shared>();
        let action = {
            let mut runtime = lock(&state);
            let resetting = runtime.reset.is_some();
            let action = runtime.cancel_exercises();
            if resetting {
                runtime.message = Some("已取消快速回正。指针已暂停。".into());
                runtime.publish();
            }
            action
        };
        if matches!(action, Some(MainAction::CloseExercise)) {
            close_exercise(&handle)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn reset_target_ready(window: WebviewWindow, id: u32, index: usize) -> Result<(), String> {
    require_pointer_controls(&window)?;
    let app = window.app_handle().clone();
    on_main(&app, move || reset_target_ready_on_main(&window, id, index)).await
}

fn reset_target_ready_on_main(window: &WebviewWindow, id: u32, index: usize) -> Result<(), String> {
    let app = window.app_handle();
    let state = app.state::<Shared>();
    let pending = |reset: &ResetSession| {
        reset.id == id
            && reset.visible
            && reset.samples.len() + 1 == index
            && reset.target.is_none()
    };
    let Some(screen) = lock(&state)
        .reset
        .as_ref()
        .filter(|reset| pending(reset))
        .map(|reset| reset.screen)
    else {
        return Ok(());
    };
    let point = RESET_POINTS[index - 1];
    let target = (|| {
        if macos::display_reconfiguring() {
            return Err(DISPLAY_RECONFIGURING.into());
        }
        if !window.is_focused().map_err(|error| error.to_string())? {
            return Err("快速回正窗口已失去焦点。".into());
        }
        macos::exercise_target(window, screen, point)
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
            dispatch_main_action(app, action);
        }
    }
    Ok(())
}

#[tauri::command]
async fn capture_target(
    window: WebviewWindow,
    state: State<'_, Shared>,
) -> Result<CaptureResult, String> {
    require_pointer_controls(&window)?;
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
        session.capture = Some(Capture::new(started));
    }

    tokio::time::sleep_until((started + CAPTURE_TIMEOUT).into()).await;
    let app = window.app_handle().clone();
    on_main(&app, move || finish_capture(&window, started)).await
}

fn finish_capture(window: &WebviewWindow, started: Instant) -> Result<CaptureResult, String> {
    let app = window.app_handle();
    refresh_displays(app);
    let state = app.state::<Shared>();
    let (step, screen) = {
        let runtime = lock(&state);
        let session = runtime
            .session
            .as_ref()
            .filter(|session| session.capturing(started))
            .ok_or("采样已取消。")?;
        (session.step().unwrap(), session.screen)
    };
    let target = macos::exercise_target(window, screen, step.point);

    let mut runtime = lock(&state);
    let capture = runtime
        .session
        .as_mut()
        .filter(|session| session.capturing(started))
        .and_then(|session| session.capture.take())
        .ok_or("采样已取消。")?;
    if macos::display_reconfiguring() {
        return Err(DISPLAY_RECONFIGURING.into());
    }
    if !runtime.selected_screen_is_current() {
        let action = runtime.update_selected_screen(None);
        drop(runtime);
        dispatch_main_action(app, action);
        return Err("目标显示器已改变，请重新校准。".into());
    }
    let target = match target {
        Ok(target) => target,
        Err(error) => {
            let action = runtime.cancel_exercises();
            runtime.display_revision = runtime.display_revision.wrapping_add(1);
            runtime.message = Some(error.clone());
            runtime.publish();
            drop(runtime);
            dispatch_main_action(app, action);
            return Err(error);
        }
    };
    let features = capture
        .sample()
        .filter(|_| runtime.can_sample())
        .ok_or("没有取得连续稳定的注视。请看准当前亮点，再按空格并保持视线。")?;

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
        Exercise::Calibrate => runtime
            .finish_calibration(screen, &session.samples)
            .err()
            .map(str::to_owned),
        Exercise::Verify => {
            let (_, mapping) = runtime.mapping.as_ref().ok_or("请先完成校准。")?;
            let errors = session
                .samples
                .into_iter()
                .map(|(features, target)| {
                    let point = mapping.pointer_point(features);
                    ((point[0] - target[0]) * (screen.width - 1.0))
                        .hypot((point[1] - target[1]) * (screen.height - 1.0))
                })
                .collect();
            runtime.validation = Some(Validation::new(errors));
            None
        }
    };
    let action = runtime.end_exercise();
    runtime.publish();
    drop(runtime);
    if matches!(action, Some(MainAction::CloseExercise)) {
        close_exercise(app)?;
    }
    Ok(CaptureResult {
        next: None,
        message,
    })
}

fn pause_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyP)
}

fn refinement_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyM)
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
    let refine = refinement_shortcut();
    tauri::Builder::default()
        .manage(Shared::default())
        .manage(PermissionChecks::default())
        .manage(snake::Launch::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, key, event| {
                    if *key == shortcut && event.state() == ShortcutState::Pressed {
                        pause_pointer(app.state());
                    } else if *key == refine {
                        let state = app.state::<Shared>();
                        lock(&state).handle_refinement_shortcut(event.state());
                    }
                })
                .build(),
        )
        .setup(move |app| {
            macos::watch_display_changes();
            refresh_displays(app.handle());
            let registered = app.global_shortcut().register(shortcut).is_ok();
            let refinement_registered =
                registered && app.global_shortcut().register(refine).is_ok();
            let state = app.state::<Shared>();
            let mut runtime = lock(&state);
            runtime.pause_available = registered;
            runtime.refinement_available = refinement_registered;
            runtime.pointer_access = PointerAccess::current();
            if let (Ok(config), Ok(config_root)) =
                (app.path().app_config_dir(), app.path().config_dir())
            {
                let legacy = config_root.join(LEGACY_IDENTIFIER);
                runtime.control_mode = load_preference(
                    &config.join(CONTROL_MODE_FILE),
                    &legacy.join(CONTROL_MODE_FILE),
                );
                runtime.eye_damping = load_preference(
                    &config.join(EYE_DAMPING_FILE),
                    &legacy.join(EYE_DAMPING_FILE),
                );
            }
            drop(runtime);
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(220));
                let display_handle = handle.clone();
                let _ = handle.run_on_main_thread(move || refresh_displays(&display_handle));
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
            set_target_display,
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
        .expect("build Gaze Mouse")
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
                    runtime.message = Some("快速回正窗口已失去焦点。".into());
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
        let legacy_path = directory.join(LEGACY_IDENTIFIER).join(EYE_DAMPING_FILE);
        let mut runtime = Runtime::default();
        assert_eq!(
            load_preference::<EyeDamping>(&path, &legacy_path),
            runtime.eye_damping
        );

        write_preference(&legacy_path, 101).unwrap();
        assert_eq!(
            load_preference::<EyeDamping>(&path, &legacy_path),
            EyeDamping::default()
        );

        let legacy = EyeDamping::try_from(20).unwrap();
        write_preference(&legacy_path, legacy).unwrap();
        assert_eq!(load_preference::<EyeDamping>(&path, &legacy_path), legacy);
        assert!(!path.exists());

        let saved = EyeDamping::try_from(80).unwrap();
        runtime.set_eye_damping(&path, saved).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "80");
        assert_eq!(load_preference::<EyeDamping>(&path, &legacy_path), saved);
        assert_eq!(fs::read_to_string(&legacy_path).unwrap(), "20");

        fs::create_dir(path.with_extension(format!("{}.tmp", std::process::id()))).unwrap();
        assert!(runtime
            .set_eye_damping(&path, EyeDamping::try_from(20).unwrap())
            .is_err());
        assert_eq!(runtime.eye_damping, saved);
        assert_eq!(load_preference::<EyeDamping>(&path, &legacy_path), saved);

        for malformed in ["", "{", "101", "-1", "25.5", "\"25\""] {
            fs::write(&path, malformed).unwrap();
            assert_eq!(
                load_preference::<EyeDamping>(&path, &legacy_path),
                EyeDamping::default()
            );
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
    fn target_follows_the_main_display_until_explicitly_selected() {
        let first = Screen::fixture(1, [0.0, 0.0], 1440.0, 900.0);
        let second = Screen::fixture(2, [-1920.0, -200.0], 1920.0, 1080.0);
        let mut runtime = Runtime::default();
        let displays = |main| {
            vec![
                Display::fixture(first, main == 1),
                Display::fixture(second, main == 2),
            ]
        };

        runtime.update_displays(displays(1));
        assert_eq!(runtime.selected_screen, Some(first));
        runtime.update_displays(displays(2));
        assert_eq!(runtime.selected_screen, Some(second));
        runtime.preferred_display_id = Some(2);
        let revision = runtime.display_revision;
        runtime.update_displays(displays(1));
        assert_eq!(runtime.selected_screen, Some(second));
        assert_eq!(runtime.display_revision, revision);
    }

    #[test]
    fn display_disconnection_cancels_capture_and_never_falls_back_to_main() {
        let main = Screen::fixture(1, [0.0, 0.0], 1440.0, 900.0);
        let external = Screen::fixture(2, [0.0, -1080.0], 1920.0, 1080.0);
        let mut runtime = Runtime {
            preferred_display_id: Some(external.id),
            ..Runtime::default()
        };
        let online = || {
            vec![
                Display::fixture(main, true),
                Display::fixture(external, false),
            ]
        };
        runtime.update_displays(online());
        runtime.session = Some(Session {
            mode: Exercise::Verify,
            screen: external,
            samples: Vec::new(),
            capture: Some(Capture::new(Instant::now())),
        });
        runtime.exercise_frame = Some(WindowFrame::default());
        runtime.neutral = Some((external, [0.0; 2]));
        runtime.point = Some([0.5, 0.5]);
        runtime.pointer_enabled = true;
        let revision = runtime.display_revision;

        assert!(matches!(
            runtime.update_displays(vec![Display::fixture(main, true)]),
            Some(MainAction::CloseExercise)
        ));
        assert_eq!(runtime.selected_screen, None);
        assert_ne!(runtime.display_revision, revision);
        assert!(!runtime.pointer_enabled);
        assert!(runtime.session.is_none());
        assert!(runtime.neutral.is_none());
        assert!(runtime.point.is_none());
        assert!(runtime.calibration_active());
        assert!(runtime.update_displays(online()).is_none());
        assert_eq!(runtime.selected_screen, Some(external));
        assert!(runtime.neutral.is_none());

        runtime.exercise_frame = None;
        runtime.closing_exercise = false;
        runtime.head_capture = Some(Instant::now());
        let revised = Screen::fixture(external.id, [-1920.0, 0.0], 1920.0, 1080.0);
        runtime.update_displays(vec![
            Display::fixture(main, true),
            Display::fixture(revised, false),
        ]);
        assert_eq!(runtime.selected_screen, Some(revised));
        assert!(runtime.head_capture.is_none());
    }

    #[test]
    fn manual_capture_uses_the_stable_end_of_the_sampling_window() {
        let started = Instant::now();
        let early = Some([0.45, 0.03, 0.0, 0.0]);
        let settled = Some([0.50, 0.06, 0.0, 0.0]);
        let mut capture = Capture::new(started);
        for timestamp in (0..=1_500).step_by(50) {
            let features = if (700..=1_400).contains(&timestamp) {
                settled
            } else {
                early
            };
            let now = started + Duration::from_millis(timestamp);
            capture.track(now, now, timestamp, features);
        }
        assert_eq!(capture.sample(), settled);

        let mut interrupted = Capture::new(started);
        for timestamp in (0..=1_400).step_by(50) {
            let features = if timestamp == 1_100 { None } else { settled };
            let now = started + Duration::from_millis(timestamp);
            interrupted.track(now, now, timestamp, features);
        }
        assert_eq!(interrupted.sample(), None);
        assert_eq!(Capture::new(started + CAPTURE_TIMEOUT).sample(), None);
    }

    #[test]
    fn manual_capture_uses_the_camera_timestamp_for_settling_and_freshness() {
        let started = Instant::now();
        let epoch_ms = 10_000;
        let features = Some([0.50, 0.06, 0.0, 0.0]);
        let track = |capture: &mut Capture, receipt_ms, timestamp| {
            let received_at = started + Duration::from_millis(receipt_ms);
            let (sampled_at, bounded_timestamp) =
                sample_time(received_at, epoch_ms + receipt_ms, timestamp);
            capture.track(received_at, sampled_at, bounded_timestamp, features);
        };

        let mut delayed = Capture::new(started);
        track(&mut delayed, 300, epoch_ms - 40);
        track(&mut delayed, 500, epoch_ms + 160);
        assert!(delayed.last_sample.is_none());
        for sample_ms in (260..=1_010).step_by(50) {
            track(&mut delayed, sample_ms + 340, epoch_ms + sample_ms);
        }
        assert_eq!(delayed.fixation.sample(), features);
        assert_eq!(delayed.sample(), None);

        let mut recent = Capture::new(started);
        for sample_ms in (700..=1_350).step_by(50) {
            track(&mut recent, sample_ms + 30, epoch_ms + sample_ms);
        }
        assert_eq!(recent.sample(), features);

        assert_eq!(
            sample_time(started, epoch_ms, epoch_ms + 150),
            (started, epoch_ms)
        );
    }

    #[test]
    fn failed_nine_point_fit_preserves_an_existing_mapping_and_validation() {
        let screen = Screen::fixture(1, [0.0, 0.0], 1440.0, 900.0);
        let features = |[x, y]: Point| [0.59 - 0.18 * x, 0.15 * y, 0.0, 0.0];
        let samples = CALIBRATION_POINTS.map(|point| (features(point), point));
        let probe = features([0.40, 0.60]);
        let mut runtime = Runtime::default();
        runtime.finish_calibration(screen, &samples).unwrap();
        let original = runtime.mapping.as_ref().unwrap().1.predict(probe);
        runtime.validation = Some(Validation::new(vec![15.0]));
        let misses = CALIBRATION_POINTS.map(|point| (features([0.5; 2]), point));

        assert!(runtime.finish_calibration(screen, &misses).is_err());
        assert!(runtime.validation.is_some());
        assert_eq!(runtime.mapping.unwrap().1.predict(probe), original);
    }

    #[test]
    fn local_correction_requires_recent_gaze_and_a_second_keypress() {
        let now = Instant::now();
        let screen = Screen::fixture(2, [-1920.0, 0.0], 1920.0, 1080.0);
        let features = |[x, y]: Point| [0.59 - 0.18 * x, 0.15 * y, 0.0, 0.0];
        let mut runtime = Runtime {
            camera: Camera::Running,
            quality: Quality::Tracked,
            last_tracked: Some(now),
            stream: Some(Channel::new(|_| Ok(()))),
            selected_screen: Some(screen),
            pointer_enabled: true,
            pause_available: true,
            refinement_available: true,
            ..Runtime::default()
        };
        runtime
            .finish_calibration(
                screen,
                &CALIBRATION_POINTS.map(|point| (features(point), point)),
            )
            .unwrap();
        runtime.validation = Some(Validation::new(vec![15.0]));
        let target = features([0.42, 0.58]);
        for timestamp in (100..=800).step_by(50) {
            runtime.refinement_fixation.observe(timestamp, Some(target));
        }
        let initial = runtime.mapping.as_ref().unwrap().1.pointer_point(target);
        assert_eq!(
            runtime.advance_refinement(screen, initial, now, 800 + MAX_FRAME_GAP_MS + 1),
            Err(REFINEMENT_NEEDS_GAZE)
        );
        assert!(runtime.pointer_enabled);
        assert!(runtime.gesture_available());
        assert!(runtime.refinement_key_pressed(ShortcutState::Pressed));
        runtime
            .advance_refinement(screen, initial, now, 810)
            .unwrap();
        assert!(!runtime.pointer_enabled);
        assert!(!runtime.gesture_available());
        assert!(!runtime.refinement_key_pressed(ShortcutState::Pressed));
        assert!(!runtime.refinement_key_pressed(ShortcutState::Released));
        assert!(runtime.refinement_key_pressed(ShortcutState::Pressed));

        let cursor = [0.49, 0.54];
        runtime
            .advance_refinement(screen, cursor, now, 1_400)
            .unwrap();
        let point = runtime.mapping.as_ref().unwrap().1.pointer_point(target);
        assert!((point[0] - cursor[0]).hypot(point[1] - cursor[1]) < 1e-12);
        assert!(runtime.refinement_capture.is_none());
        assert!(runtime.validation.is_none());
        assert!(runtime.pointer_enabled);
    }

    #[test]
    fn each_refinement_shortcut_outcome_has_a_distinct_revision() {
        let mut runtime = Runtime {
            refinement_available: true,
            ..Runtime::default()
        };
        runtime.handle_refinement_shortcut(ShortcutState::Pressed);
        assert_eq!(runtime.refinement_revision, 1);
        assert!(runtime.refinement_capture.is_none());
        let retry_message = runtime.message.clone().unwrap();

        runtime.handle_refinement_shortcut(ShortcutState::Pressed);
        runtime.handle_refinement_shortcut(ShortcutState::Released);
        assert_eq!(runtime.refinement_revision, 1);

        runtime.handle_refinement_shortcut(ShortcutState::Pressed);
        assert_eq!(runtime.refinement_revision, 2);
        assert_eq!(runtime.message.as_deref(), Some(retry_message.as_str()));
    }

    #[test]
    fn local_correction_cancels_without_recording_or_resuming() {
        let now = Instant::now();
        let screen = Screen::fixture(1, [0.0, 0.0], 1440.0, 900.0);
        let features = |[x, y]: Point| [0.59 - 0.18 * x, 0.15 * y, 0.0, 0.0];
        let samples = CALIBRATION_POINTS.map(|point| (features(point), point));
        let target = features([0.5, 0.5]);
        let mut runtime = Runtime {
            camera: Camera::Running,
            quality: Quality::Tracked,
            last_tracked: Some(now),
            selected_screen: Some(screen),
            pointer_enabled: true,
            ..Runtime::default()
        };
        runtime.finish_calibration(screen, &samples).unwrap();
        let initial = runtime.mapping.as_ref().unwrap().1.pointer_point(target);
        for timed_out in [false, true] {
            for timestamp in (100..=800).step_by(50) {
                runtime.refinement_fixation.observe(timestamp, Some(target));
            }
            runtime
                .advance_refinement(screen, initial, now, 810)
                .unwrap();
            let revision = runtime.refinement_revision;
            if timed_out {
                runtime.refinement_capture.as_mut().unwrap().expires = now;
                runtime.watchdog();
                assert_eq!(runtime.message.as_deref(), Some(REFINEMENT_EXPIRED));
            } else {
                runtime.disarm();
            }
            assert!(runtime.refinement_capture.is_none());
            assert_eq!(runtime.refinement_revision, revision + 1);
            assert!(!runtime.pointer_enabled);
            assert_eq!(
                runtime.mapping.as_ref().unwrap().1.pointer_point(target),
                initial
            );
        }
    }

    #[test]
    fn three_blinks_requires_nine_point_calibration_on_the_selected_display() {
        let screen = Screen::fixture(2, [-1920.0, 0.0], 1920.0, 1080.0);
        let other = Screen::fixture(1, [0.0, 0.0], 1440.0, 900.0);
        let features = |[x, y]: Point| [0.59 - 0.18 * x, 0.15 * y, 0.0, 0.0];
        let samples = CALIBRATION_POINTS.map(|point| (features(point), point));
        for calibration_screen in [None, Some(other), Some(screen)] {
            let mut runtime = Runtime {
                camera: Camera::Running,
                selected_screen: Some(screen),
                stream: Some(Channel::new(|_| Ok(()))),
                pointer_enabled: calibration_screen == Some(screen),
                ..Runtime::default()
            };
            if let Some(calibration_screen) = calibration_screen {
                runtime
                    .finish_calibration(calibration_screen, &samples)
                    .unwrap();
            }
            let mut actions = Vec::new();
            for frame in 0..22 {
                let closed = matches!(frame, 3 | 4 | 9 | 10 | 15 | 16);
                runtime.quality = if closed {
                    Quality::EyesClosed
                } else {
                    Quality::Tracked
                };
                actions.extend(runtime.observe_gesture(
                    frame * 45,
                    (!closed).then(|| features([0.5; 2])),
                    Some([0.0; 2]),
                    closed,
                ));
            }
            assert!(!runtime.pointer_enabled);
            assert_eq!(
                runtime.mapping.as_ref().map(|(screen, _)| *screen),
                calibration_screen
            );
            if calibration_screen == Some(screen) {
                assert!(matches!(actions.as_slice(), [MainAction::OpenReset(1)]));
                assert_eq!(runtime.reset.unwrap().screen, screen);
            } else {
                assert!(actions.is_empty());
                assert!(runtime.reset.is_none());
                assert_eq!(runtime.message.as_deref(), Some(RESET_NEEDS_CALIBRATION));
            }
        }
    }

    #[test]
    fn completed_reset_only_replaces_the_mapping_and_validation_when_corners_agree() {
        let started = Instant::now();
        let screen = Screen::fixture(2, [-1920.0, 0.0], 1920.0, 1080.0);
        let features = |[x, y]: Point| [0.61 - 0.24 * x, 0.02 + 0.16 * y, 0.0, 0.0];
        let samples = CALIBRATION_POINTS.map(|point| (features(point), point));
        let probe = features([0.55, 0.46]);
        for span in [1.0, 0.27] {
            let mut runtime = Runtime {
                selected_screen: Some(screen),
                stream: Some(Channel::new(|_| Ok(()))),
                reset: Some(ResetSession::new(1, screen, started)),
                exercise_frame: Some(WindowFrame::default()),
                ..Runtime::default()
            };
            runtime.finish_calibration(screen, &samples).unwrap();
            runtime.validation = Some(Validation::new(vec![15.0]));
            let original = runtime.mapping.as_ref().unwrap().1.predict(probe);
            let mut actions = Vec::new();
            for (index, [x, y]) in RESET_POINTS.into_iter().enumerate() {
                let ready_at = index as u64 * 1_300;
                runtime.reset.as_mut().unwrap().target =
                    Some(([x, y], started + Duration::from_millis(ready_at)));
                let sample = features([0.55 + span * (x - 0.5), 0.46 + span * (y - 0.5)]);
                for timestamp in (ready_at + 500..=ready_at + 1_175).step_by(45) {
                    actions.extend(runtime.advance_reset(
                        started + Duration::from_millis(timestamp),
                        timestamp,
                        Some(sample),
                    ));
                }
            }
            assert!(matches!(actions.as_slice(), [MainAction::CloseExercise]));
            assert!(runtime.reset.is_none());
            assert!(!runtime.pointer_enabled);
            let updated = runtime.mapping.unwrap().1.predict(probe);
            if span == 1.0 {
                assert!(runtime.validation.is_none());
                assert_eq!(updated, [0.5; 2]);
            } else {
                assert!(runtime.validation.is_some());
                assert_eq!(updated, original);
                assert!(runtime.message.unwrap().contains("九点校准"));
            }
        }
    }

    #[test]
    fn rejected_reset_times_out_without_replacing_the_mapping_or_resuming_the_pointer() {
        let now = Instant::now();
        let screen = Screen::fixture(1, [0.0, 0.0], 1440.0, 900.0);
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
            exercise_frame: Some(WindowFrame::default()),
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
            Some(MainAction::CloseExercise)
        ));
        assert!(runtime.reset.is_none());
        assert!(!runtime.pointer_enabled);
        assert_eq!(
            runtime.mapping.unwrap().1.predict(features([0.40, 0.60])),
            previous_point
        );
    }
}
