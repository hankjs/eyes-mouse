use crate::calibration::Point;
use core_graphics::display::CGDisplay;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const CHECK_ACCESSIBILITY_PERMISSION: &str = "--check-accessibility-permission";
const ACCESS_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const ACCESS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

extern "C" {
    fn eyes_mouse_prompt_accessibility();
    fn eyes_mouse_open_accessibility_settings() -> bool;
}

#[derive(Clone, Copy, PartialEq)]
pub struct Screen {
    id: u32,
    pub width: f64,
    pub height: f64,
}

impl Screen {
    pub fn primary() -> Self {
        let display = CGDisplay::main();
        let bounds = display.bounds();
        Self {
            id: display.id,
            width: bounds.size.width,
            height: bounds.size.height,
        }
    }

    pub fn move_pointer(self, point: Point) -> Result<(), ()> {
        let [x, y] = point;
        let position = CGPoint::new(x * (self.width - 1.0), y * (self.height - 1.0));
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)?;
        post_pointer_move(source, position)
    }

    pub fn move_pointer_by(self, offset: Point) -> Result<bool, ()> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)?;
        let position = CGEvent::new(source.clone())?.location();
        if !(0.0..self.width).contains(&position.x) || !(0.0..self.height).contains(&position.y) {
            return Ok(false);
        }
        let target = CGPoint::new(
            (position.x + offset[0]).clamp(0.0, self.width - 1.0),
            (position.y + offset[1]).clamp(0.0, self.height - 1.0),
        );
        if target.x != position.x || target.y != position.y {
            post_pointer_move(source, target)?;
        }
        Ok(true)
    }
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
