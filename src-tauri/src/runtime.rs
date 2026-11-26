use crate::calibration::{
    median, Features, Mapping, Point, Validation, CALIBRATION_POINTS, VALIDATION_POINTS,
};
use crate::macos::{self, Screen};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const STREAM_STALE: Duration = Duration::from_millis(350);
const TRACKING_LOST: Duration = Duration::from_millis(850);

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
    child: Option<Child>,
    generation: u64,
    camera_started: Option<Instant>,
    last_frame: Option<Instant>,
    last_tracked: Option<Instant>,
    last_publish: Option<Instant>,
    recent: VecDeque<(Instant, Features)>,
    stream: Option<Channel<Snapshot>>,
    session: Option<Session>,
    mapping: Option<(Screen, Mapping)>,
    validation: Option<Validation>,
    point: Option<Point>,
    pointer_enabled: bool,
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
            calibrated: self.mapping.is_some(),
            pointer_enabled: self.pointer_enabled,
            pause_available: self.pause_available,
            point: self.point.filter(|_| self.quality == Quality::Tracked),
            validation: self.validation,
            message: self.message.clone(),
        };
        if let Some(channel) = &self.stream {
            if channel.send(snapshot).is_err() {
                self.pointer_enabled = false;
                self.stream = None;
            }
        }
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
        self.pointer_enabled = false;
        self.point = None;
        self.quality = Quality::Searching;
        self.recent.clear();
        self.camera_started = None;
        self.last_frame = None;
        self.last_tracked = None;
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
            self.quality = Quality::Stale;
            self.point = None;
            changed = true;
        }
        if self.pointer_enabled
            && self
                .last_tracked
                .is_none_or(|time| time.elapsed() > TRACKING_LOST)
        {
            self.pointer_enabled = false;
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
                let changed = self.quality != quality;
                self.last_frame = Some(now);
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
                    .is_some_and(|(time, _)| time.elapsed() > Duration::from_secs(2))
                {
                    self.recent.pop_front();
                }
                if changed
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
            self.pointer_enabled = false;
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
            self.pointer_enabled = false;
            self.message = Some("无法移动指针。请检查 macOS「隐私与安全性 → 辅助功能」。".into());
        }
    }
}

#[tauri::command]
fn subscribe(state: State<'_, Shared>, channel: Channel<Snapshot>) {
    let mut runtime = lock(&state);
    runtime.pointer_enabled = false;
    runtime.stream = Some(channel);
    runtime.publish();
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
            runtime.stop(Some(format!("无法启动眼动跟踪组件。{error}")));
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
    runtime.pointer_enabled = false;
    runtime.publish();
}

#[tauri::command]
fn enable_pointer(state: State<'_, Shared>) -> Result<(), String> {
    {
        let runtime = lock(&state);
        check_pointer_ready(&runtime)?;
    }
    if !macos::can_move_pointer() && !macos::request_pointer_access() {
        return Err(
            "请在 macOS「隐私与安全性 → 辅助功能」允许 Eyes Mouse，然后重新启用指针。".into(),
        );
    }
    let mut runtime = lock(&state);
    check_pointer_ready(&runtime)?;
    runtime.pointer_enabled = true;
    runtime.message = None;
    runtime.publish();
    Ok(())
}

fn check_pointer_ready(runtime: &Runtime) -> Result<(), String> {
    if runtime.session.is_some() || runtime.mapping.is_none() || !runtime.can_sample() {
        return Err("请先完成校准，并让摄像头看到双眼。".into());
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
fn begin_exercise(
    window: WebviewWindow,
    state: State<'_, Shared>,
    mode: Exercise,
) -> Result<Step, String> {
    let mut runtime = lock(&state);
    if !runtime.can_sample() || runtime.session.is_some() {
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
    runtime.pointer_enabled = false;
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
            lock(&app.state::<Shared>()).pause_available = registered;
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(220));
                lock(&handle.state::<Shared>()).watchdog();
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            subscribe,
            start_camera,
            stop_camera,
            pause_pointer,
            enable_pointer,
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
