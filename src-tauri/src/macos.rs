use crate::calibration::Point;
use core_graphics::display::CGDisplay;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::geometry::CGPoint;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
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
        let event = CGEvent::new_mouse_event(
            core_graphics::event_source::CGEventSource::new(
                core_graphics::event_source::CGEventSourceStateID::CombinedSessionState,
            )?,
            CGEventType::MouseMoved,
            position,
            CGMouseButton::Left,
        )?;
        event.post(CGEventTapLocation::HID);
        Ok(())
    }
}

pub fn can_move_pointer() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

pub fn request_pointer_access() -> bool {
    unsafe { CGRequestPostEventAccess() }
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
