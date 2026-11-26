use crate::calibration::Point;
use core_graphics::display::{CGDisplay, CGDisplayChangeSummaryFlags as DisplayChange};
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::WebviewWindow;

pub const CHECK_ACCESSIBILITY_PERMISSION: &str = "--check-accessibility-permission";
const ACCESS_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const ACCESS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGDisplayRegisterReconfigurationCallback(
        callback: extern "C" fn(u32, u32, *mut c_void),
        context: *mut c_void,
    ) -> i32;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

extern "C" {
    fn eyes_mouse_prompt_accessibility();
    fn eyes_mouse_open_accessibility_settings() -> bool;
    fn eyes_mouse_visit_screens(
        visitor: extern "C" fn(*mut c_void, u32, *const c_char),
        context: *mut c_void,
    );
    fn eyes_mouse_place_exercise(
        handle: *mut c_void,
        id: u32,
        original_id: *mut u32,
        original: *mut f64,
    ) -> bool;
    fn eyes_mouse_restore_exercise(handle: *mut c_void, original_id: u32, original: *const f64);
    fn eyes_mouse_exercise_target(
        handle: *mut c_void,
        id: u32,
        x: f64,
        y: f64,
        target: *mut f64,
    ) -> bool;
}

static DISPLAY_CHANGES: LazyLock<Mutex<HashMap<u32, u64>>> = LazyLock::new(Mutex::default);
static DISPLAY_RECONFIGURING: AtomicBool = AtomicBool::new(false);

pub fn watch_display_changes() {
    extern "C" fn changed(id: u32, flags: u32, _: *mut c_void) {
        let flags = DisplayChange::from_bits_retain(flags);
        if flags.contains(DisplayChange::kCGDisplayBeginConfigurationFlag) {
            DISPLAY_RECONFIGURING.store(true, Ordering::SeqCst);
            return;
        }
        let changes = DisplayChange::kCGDisplayMovedFlag
            | DisplayChange::kCGDisplaySetModeFlag
            | DisplayChange::kCGDisplayAddFlag
            | DisplayChange::kCGDisplayRemoveFlag
            | DisplayChange::kCGDisplayEnabledFlag
            | DisplayChange::kCGDisplayDisabledFlag
            | DisplayChange::kCGDisplayMirrorFlag
            | DisplayChange::kCGDisplayUnMirrorFlag;
        if flags.intersects(changes) {
            let mut revisions = DISPLAY_CHANGES
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let revision = revisions.entry(id).or_default();
            *revision = revision.wrapping_add(1);
        }
        DISPLAY_RECONFIGURING.store(false, Ordering::SeqCst);
    }

    unsafe { CGDisplayRegisterReconfigurationCallback(changed, std::ptr::null_mut()) };
}

pub fn display_reconfiguring() -> bool {
    DISPLAY_RECONFIGURING.load(Ordering::SeqCst)
}

fn display_revision(id: u32) -> u64 {
    DISPLAY_CHANGES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&id)
        .copied()
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub id: u32,
    origin: Point,
    pub width: f64,
    pub height: f64,
    rotation: f64,
    mode: i32,
    backing_size: [u64; 2],
    revision: u64,
}

impl Screen {
    fn read(id: u32) -> Option<Self> {
        let revision = display_revision(id);
        let display = CGDisplay::new(id);
        if id == 0 || !display.is_active() || display.mirrors_display() != 0 {
            return None;
        }
        let bounds = display.bounds();
        let mode = display.display_mode()?;
        let screen = Self {
            id,
            origin: [bounds.origin.x, bounds.origin.y],
            width: bounds.size.width,
            height: bounds.size.height,
            rotation: display.rotation(),
            mode: mode.mode_id(),
            backing_size: [mode.pixel_width(), mode.pixel_height()],
            revision,
        };
        if screen
            .origin
            .iter()
            .all(|coordinate| coordinate.is_finite())
            && screen.width.is_finite()
            && screen.height.is_finite()
            && screen.width > 1.0
            && screen.height > 1.0
            && revision == display_revision(id)
        {
            Some(screen)
        } else {
            None
        }
    }

    pub fn is_current(self, follow_primary: bool) -> bool {
        (!follow_primary || CGDisplay::main().id == self.id) && Self::read(self.id) == Some(self)
    }

    fn contains(self, [x, y]: Point) -> bool {
        let [left, top] = self.origin;
        (left..left + self.width).contains(&x) && (top..top + self.height).contains(&y)
    }

    fn position_for(self, [x, y]: Point) -> Point {
        [
            self.origin[0] + x.clamp(0.0, 1.0) * (self.width - 1.0),
            self.origin[1] + y.clamp(0.0, 1.0) * (self.height - 1.0),
        ]
    }

    fn normalized_position(self, position: Point) -> Option<Point> {
        if !self.contains(position) {
            return None;
        }
        Some([
            ((position[0] - self.origin[0]) / (self.width - 1.0)).clamp(0.0, 1.0),
            ((position[1] - self.origin[1]) / (self.height - 1.0)).clamp(0.0, 1.0),
        ])
    }

    fn constrain(self, [x, y]: Point) -> Point {
        let [left, top] = self.origin;
        [
            x.clamp(left, left + self.width - 1.0),
            y.clamp(top, top + self.height - 1.0),
        ]
    }

    fn pointer_target(self, position: Point, motion: Motion) -> Option<Point> {
        let inside = self.contains(position);
        match motion {
            Motion::Activate if !inside => Some(self.position_for([0.5, 0.5])),
            Motion::Activate => Some(position),
            _ if !inside => None,
            Motion::Eye(point) => Some(self.position_for(point)),
            Motion::Head([0.0, 0.0]) => Some(position),
            Motion::Head([x, y]) => Some(self.constrain([position[0] + x, position[1] + y])),
        }
    }

    pub fn cursor_point(self, follow_primary: bool) -> Option<Point> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
        let position = pointer_position(source).ok()?;
        if !self.is_current(follow_primary) || display_reconfiguring() {
            return None;
        }
        self.normalized_position(position)
    }

    pub fn move_pointer(self, motion: Motion, follow_primary: bool) -> Result<MotionResult, ()> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)?;
        let position = pointer_position(source.clone())?;
        let current = self.is_current(follow_primary);
        if display_reconfiguring() {
            return Ok(MotionResult::Reconfiguring);
        }
        if !current {
            return Ok(MotionResult::DisplayChanged);
        }
        let Some(target) = self.pointer_target(position, motion) else {
            return Ok(MotionResult::Outside);
        };
        if target != position && self.contains(target) {
            if matches!(motion, Motion::Activate) {
                let local = CGPoint::new(target[0] - self.origin[0], target[1] - self.origin[1]);
                CGDisplay::new(self.id)
                    .move_cursor_to_point(local)
                    .map_err(|_| ())?;
            } else {
                post_pointer_move(source, CGPoint::new(target[0], target[1]))?;
            }
        }
        Ok(MotionResult::Inside)
    }

    #[cfg(test)]
    pub fn fixture(id: u32, origin: Point, width: f64, height: f64) -> Self {
        Self {
            id,
            origin,
            width,
            height,
            rotation: 0.0,
            mode: 0,
            backing_size: [0; 2],
            revision: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Motion {
    Activate,
    Eye(Point),
    Head(Point),
}

pub enum MotionResult {
    Inside,
    Outside,
    DisplayChanged,
    Reconfiguring,
}

#[derive(Clone, PartialEq, Serialize)]
pub struct Display {
    id: u32,
    name: String,
    pub primary: bool,
    #[serde(skip)]
    pub screen: Screen,
}

impl Display {
    #[cfg(test)]
    pub fn fixture(screen: Screen, primary: bool) -> Self {
        Self {
            id: screen.id,
            name: format!("Display {}", screen.id),
            primary,
            screen,
        }
    }
}

// AppKit screen and window calls run on the main thread.
pub fn displays() -> Vec<Display> {
    extern "C" fn visit(context: *mut c_void, id: u32, name: *const c_char) {
        let displays = unsafe { &mut *context.cast::<Vec<Display>>() };
        let Some(screen) = Screen::read(id) else {
            return;
        };
        let name = if name.is_null() {
            format!("显示器 {id}")
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        displays.push(Display {
            id,
            name,
            primary: CGDisplay::main().id == id,
            screen,
        });
    }

    let mut displays: Vec<Display> = Vec::new();
    unsafe { eyes_mouse_visit_screens(visit, (&mut displays as *mut Vec<Display>).cast()) };
    displays
}

#[derive(Default)]
pub struct WindowFrame {
    display_id: u32,
    relative_rect: [f64; 4],
}

pub fn open_exercise(window: &WebviewWindow, screen: Screen) -> Result<WindowFrame, String> {
    if window.is_fullscreen().map_err(|error| error.to_string())? {
        return Err("请先退出 macOS 全屏模式，再开始屏幕校准。".into());
    }
    let handle = window.ns_window().map_err(|error| error.to_string())?;
    let mut original = WindowFrame::default();
    if !unsafe {
        eyes_mouse_place_exercise(
            handle,
            screen.id,
            &mut original.display_id,
            original.relative_rect.as_mut_ptr(),
        )
    } {
        unsafe {
            eyes_mouse_restore_exercise(
                handle,
                original.display_id,
                original.relative_rect.as_ptr(),
            )
        };
        return Err("无法将校准窗口移到目标显示器。".into());
    }

    let result = window
        .set_simple_fullscreen(true)
        .map_err(|error| error.to_string())
        .and_then(|()| exercise_target(window, screen, [0.5, 0.5]));
    match result {
        Ok(_) => Ok(original),
        Err(error) => {
            let _ = close_exercise(window, original);
            Err(error)
        }
    }
}

pub fn close_exercise(window: &WebviewWindow, original: WindowFrame) -> Result<(), String> {
    window
        .set_simple_fullscreen(false)
        .map_err(|error| error.to_string())?;
    let handle = window.ns_window().map_err(|error| error.to_string())?;
    unsafe {
        eyes_mouse_restore_exercise(handle, original.display_id, original.relative_rect.as_ptr())
    };
    Ok(())
}

pub fn exercise_target(
    window: &WebviewWindow,
    screen: Screen,
    [x, y]: Point,
) -> Result<Point, String> {
    let handle = window.ns_window().map_err(|error| error.to_string())?;
    let mut target = [0.0; 2];
    if unsafe { eyes_mouse_exercise_target(handle, screen.id, x, y, target.as_mut_ptr()) } {
        Ok(target)
    } else {
        Err("校准窗口已离开目标显示器，请重新开始。".into())
    }
}

fn pointer_position(source: CGEventSource) -> Result<Point, ()> {
    let cursor = CGEvent::new(source)?.location();
    Ok([cursor.x, cursor.y])
}

fn post_pointer_move(source: CGEventSource, position: CGPoint) -> Result<(), ()> {
    let event = CGEvent::new_mouse_event(
        source,
        CGEventType::MouseMoved,
        position,
        CGMouseButton::Left,
    )?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

pub fn can_move_pointer() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

pub fn prompt_accessibility() {
    unsafe { eyes_mouse_prompt_accessibility() }
}

pub fn open_accessibility_settings() -> bool {
    unsafe { eyes_mouse_open_accessibility_settings() }
}

struct PermissionProbe {
    request: u64,
    child: Child,
    deadline: Instant,
}

#[derive(Default)]
pub struct PermissionChecks {
    probe: Option<PermissionProbe>,
    next: Option<Instant>,
    closed: bool,
}

impl PermissionChecks {
    pub fn poll(&mut self, request: Option<u64>) -> Option<u64> {
        let Some(request) = request.filter(|_| !self.closed) else {
            self.stop_probe();
            self.next = None;
            return None;
        };

        let now = Instant::now();
        if let Some(probe) = &mut self.probe {
            if probe.request == request && now < probe.deadline {
                match probe.child.try_wait() {
                    Ok(Some(status)) => {
                        self.probe = None;
                        return status.success().then_some(request);
                    }
                    Ok(None) => return None,
                    Err(_) => {}
                }
            }
            self.stop_probe();
        }

        if self.next.is_none_or(|next| now >= next) {
            self.next = Some(now + ACCESS_PROBE_INTERVAL);
            // macOS can cache both preflight results for the lifetime of a process.
            // Run the same executable without starting Tauri, opening windows, or asking for consent.
            self.probe = std::env::current_exe().ok().and_then(|executable| {
                let child = Command::new(executable)
                    .arg(CHECK_ACCESSIBILITY_PERMISSION)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()?;
                Some(PermissionProbe {
                    request,
                    child,
                    deadline: now + ACCESS_PROBE_TIMEOUT,
                })
            });
        }
        None
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.stop_probe();
    }

    fn stop_probe(&mut self) {
        if let Some(mut probe) = self.probe.take() {
            let _ = probe.child.kill();
            let _ = probe.child.wait();
        }
    }
}

pub async fn request_camera_access() -> bool {
    use std::ffi::c_void;
    use tokio::sync::oneshot;

    extern "C" {
        fn eyes_mouse_request_camera_access(
            reply: unsafe extern "C" fn(bool, *mut c_void),
            context: *mut c_void,
        );
    }

    unsafe extern "C" fn reply(granted: bool, context: *mut c_void) {
        let sender = Box::from_raw(context.cast::<oneshot::Sender<bool>>());
        let _ = sender.send(granted);
    }

    let (sender, receiver) = oneshot::channel();
    // AVCaptureDevice delivers a single completion callback, which owns the sender.
    unsafe {
        eyes_mouse_request_camera_access(reply, Box::into_raw(Box::new(sender)).cast());
    }
    receiver.await.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_pointer_modes_use_and_clamp_to_the_selected_global_bounds() {
        let screen = Screen::fixture(2, [-1920.0, -150.0], 1920.0, 1080.0);
        let inside = [-1200.0, 300.0];
        assert_eq!(
            screen.pointer_target(inside, Motion::Eye([0.0, 0.0])),
            Some([-1920.0, -150.0])
        );
        assert_eq!(
            screen.pointer_target(inside, Motion::Eye([1.5, 1.5])),
            Some([-1.0, 929.0])
        );
        assert_eq!(
            screen.pointer_target(inside, Motion::Head([-900.0, 1000.0])),
            Some([-1920.0, 929.0])
        );
        assert_eq!(
            screen.pointer_target(inside, Motion::Activate),
            Some(inside)
        );

        let outside = [0.0, 300.0];
        assert_eq!(
            screen.pointer_target(outside, Motion::Head([0.0, 0.0])),
            None
        );
        assert_eq!(
            screen.pointer_target(outside, Motion::Head([-2.0, 0.0])),
            None
        );
        assert_eq!(
            screen.pointer_target(outside, Motion::Eye([0.5, 0.5])),
            None
        );
        assert_eq!(
            screen.pointer_target(outside, Motion::Activate),
            Some([-960.5, 389.5])
        );
    }

    #[test]
    fn normalizes_cursor_positions_on_the_selected_display() {
        let screen = Screen::fixture(2, [-1920.0, -150.0], 1920.0, 1080.0);
        for point in [[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]] {
            assert_eq!(
                screen.normalized_position(screen.position_for(point)),
                Some(point)
            );
        }
        assert_eq!(screen.normalized_position([-0.5, 929.5]), Some([1.0, 1.0]));
        for point in [[0.0, 300.0], [-1920.1, 300.0], [-1200.0, 930.0]] {
            assert_eq!(screen.normalized_position(point), None);
        }
    }
}
