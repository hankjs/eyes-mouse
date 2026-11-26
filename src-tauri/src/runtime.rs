use crate::calibration::{
    median, Features, Mapping, Point, Validation, CALIBRATION_POINTS, VALIDATION_POINTS,
};
use crate::macos::{self, Screen};
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

const STREAM_STALE: Duration = Duration::from_millis(350);
const TRACKING_LOST: Duration = Duration::from_millis(850);
const HEAD_MAX_SPEED: f64 = 875.0;
const HEAD_MAX_FRAME_TIME: Duration = Duration::from_millis(64);
const HEAD_CAPTURE_SETTLE: Duration = Duration::from_millis(250);
const HEAD_CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
const HEAD_CAPTURE_SAMPLES: usize = 8;

type HeadPose = [f64; 2];

#[derive(Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ControlMode {
    #[default]
    Eye,
    Head,
}

fn control_mode_path(app: &AppHandle) -> Result<PathBuf, tauri::Error> {
    Ok(app.path().app_config_dir()?.join("control-mode.json"))
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Packet {
    Ready,
    Sample {
        timestamp: u64,
        quality: Quality,
        features: Option<Features>,
        pose: Option<HeadPose>,
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
    calibrated: bool,
    pointer_enabled: bool,
    pause_available: bool,
    point: Option<Point>,
    validation: Option<Validation>,
    message: Option<String>,
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

#[derive(Serialize)]
struct Step {
    point: Point,
    index: usize,
    total: usize,
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
    mapping: Option<(Screen, Mapping)>,
    validation: Option<Validation>,
    point: Option<Point>,
    pointer_enabled: bool,
    activation_generation: u64,
    pause_available: bool,
    message: Option<String>,
}

type Shared = Mutex<Runtime>;

fn lock(state: &Shared) -> MutexGuard<'_, Runtime> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

impl Runtime {
    fn publish(&mut self) {
        self.last_publish = Some(Instant::now());
        let snapshot = Snapshot {
            camera: self.camera,
            quality: self.quality,
            control_mode: self.control_mode,
            calibrated: self.calibrated(),
            pointer_enabled: self.pointer_enabled,
            pause_available: self.pause_available,
            point: self.point.filter(|_| self.quality == Quality::Tracked),
            validation: self
                .validation
                .filter(|_| self.control_mode == ControlMode::Eye),
            message: self.message.clone(),
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

    fn disarm(&mut self) {
        self.pointer_enabled = false;
        self.head_motion_at = None;
        self.activation_generation = self.activation_generation.wrapping_add(1);
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

    fn stop(&mut self, message: Option<String>) {
        self.generation = self.generation.wrapping_add(1);
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
    }

    fn watchdog(&mut self) {
        if self.camera == Camera::Starting
            && self
                .camera_started
                .is_some_and(|time| time.elapsed() > Duration::from_secs(45))
        {
            self.stop(Some("相机启动超时。请检查系统相机权限，然后重试。".into()));
            return;
        }
        if self.camera != Camera::Running {
            return;
        }
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
    }

    fn accept(&mut self, packet: Packet) {
        match packet {
            Packet::Ready => {
                self.camera = Camera::Running;
                self.last_frame = Some(Instant::now());
                self.publish();
            }
            Packet::Error { message } => self.stop(Some(message)),
            Packet::Sample {
                timestamp,
                quality,
                features,
                pose,
            } => {
                let elapsed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                if (elapsed.as_millis() as u64).abs_diff(timestamp)
                    > STREAM_STALE.as_millis() as u64
                {
                    return;
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
                match self.control_mode {
                    ControlMode::Eye => {
                        self.quality = quality;
                        if let Some(features) = features.filter(|_| quality == Quality::Tracked) {
                            self.last_tracked = Some(now);
                            self.recent.push_back((now, features));
                            self.update_point(features);
                        } else {
                            self.point = None;
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
            }
        }
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
            let weight = (0.12 + 3.0 * distance).min(0.68);
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
        self.message = Some("无法移动指针。请检查 macOS「隐私与安全性 → 辅助功能」。".into());
    }
}

#[tauri::command]
fn subscribe(state: State<'_, Shared>, channel: Channel<Snapshot>) {
    let mut runtime = lock(&state);
    runtime.clear_tracking(Quality::Searching);
    runtime.stream = Some(channel);
    runtime.publish();
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
    if runtime.session.is_some() {
        return Err("请先退出屏幕校准或验证。".into());
    }
    runtime.clear_tracking(Quality::Searching);
    runtime.neutral = None;
    runtime.message = None;
    let save = || -> Result<(), Box<dyn std::error::Error>> {
        let path = control_mode_path(&app)?;
        fs::create_dir_all(path.parent().ok_or("无法读取设置目录")?)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, serde_json::to_vec(&control_mode)?)?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(temporary);
            return Err(error.into());
        }
        Ok(())
    };
    let result = save().map_err(|error| format!("无法保存控制动作。{error}"));
    if result.is_ok() {
        runtime.control_mode = control_mode;
    }
    runtime.publish();
    result
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
        if matches!(runtime.camera, Camera::Starting | Camera::Running) {
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
            let mut runtime = lock(&state);
            if runtime.generation != generation {
                return;
            }
            runtime.accept(packet);
        }
        let mut runtime = lock(&state);
        if runtime.generation == generation {
            runtime.stop(Some("跟踪组件已退出。请检查相机权限后重试。".into()));
        }
    });
    Ok(())
}

#[tauri::command]
fn stop_camera(state: State<'_, Shared>) {
    lock(&state).stop(None);
}

#[tauri::command]
fn pause_pointer(state: State<'_, Shared>) {
    let mut runtime = lock(&state);
    runtime.disarm();
    runtime.publish();
}

#[tauri::command]
fn enable_pointer(state: State<'_, Shared>) -> Result<(), String> {
    let activation_generation = {
        let runtime = lock(&state);
        check_pointer_ready(&runtime)?;
        runtime.activation_generation
    };
    if !macos::can_move_pointer() && !macos::request_pointer_access() {
        return Err(
            "请在 macOS「隐私与安全性 → 辅助功能」允许 Eyes Mouse，然后重新启用指针。".into(),
        );
    }
    let mut runtime = lock(&state);
    if runtime.activation_generation != activation_generation {
        return Err("启用已取消。请再次选择「启用指针」。".into());
    }
    check_pointer_ready(&runtime)?;
    runtime.pointer_enabled = true;
    runtime.head_motion_at = Some(Instant::now());
    runtime.message = None;
    runtime.publish();
    Ok(())
}

fn check_pointer_ready(runtime: &Runtime) -> Result<(), String> {
    if runtime.session.is_some()
        || runtime.head_capture.is_some()
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
async fn capture_neutral(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
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
        monitor(&window)?;
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
    let mut runtime = lock(&state);
    if runtime.control_mode != ControlMode::Eye
        || !runtime.can_sample()
        || runtime.session.is_some()
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
    monitor(&window)?;
    runtime.disarm();
    window
        .set_simple_fullscreen(true)
        .map_err(|error| error.to_string())?;
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
    Ok(step)
}

#[tauri::command]
fn cancel_exercise(window: WebviewWindow, state: State<'_, Shared>) -> Result<(), String> {
    let mut runtime = lock(&state);
    runtime.session = None;
    drop(runtime);
    window
        .set_simple_fullscreen(false)
        .map_err(|error| error.to_string())
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
    runtime.publish();
    drop(runtime);
    window
        .set_simple_fullscreen(false)
        .map_err(|error| error.to_string())?;
    Ok(CaptureResult {
        next: None,
        message,
    })
}

pub fn run() {
    let shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyP);
    tauri::Builder::default()
        .manage(Shared::default())
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
            runtime.control_mode = control_mode_path(app.handle())
                .ok()
                .and_then(|path| fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            drop(runtime);
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(220));
                lock(&handle.state::<Shared>()).watchdog();
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            subscribe,
            set_control_mode,
            start_camera,
            stop_camera,
            pause_pointer,
            enable_pointer,
            capture_neutral,
            begin_exercise,
            cancel_exercise,
            capture_target
        ])
        .build(tauri::generate_context!())
        .expect("build Eyes Mouse")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                lock(&app.state::<Shared>()).stop(None);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
