use serde::Serialize;
use std::ffi::c_void;
use std::sync::Mutex;
use tauri::utils::config::BackgroundThrottlingPolicy;
use tauri::webview::PageLoadEvent;
use tauri::window::Color;
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::oneshot;

pub const LABEL: &str = "snake";

#[derive(Default)]
pub struct Launch(pub tokio::sync::Mutex<()>);

#[derive(Default)]
pub enum Status {
    #[default]
    Closed,
    Open {
        restart: u32,
    },
    Closing,
}

#[derive(Serialize)]
pub struct PointerSample {
    pub point: Option<[f64; 2]>,
    pub restart: u32,
}

extern "C" {
    fn eyes_mouse_show_snake(handle: *mut c_void);
    fn eyes_mouse_snake_pointer(handle: *mut c_void, x: *mut f64, y: *mut f64) -> bool;
}

pub fn create(main: &WebviewWindow) -> Result<(WebviewWindow, oneshot::Receiver<()>), String> {
    let monitor = main
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .ok_or("无法读取主显示器。")?;
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let position = area.position.to_logical::<f64>(scale);
    let size = area.size.to_logical::<f64>(scale);
    let (ready, loaded) = oneshot::channel();
    let ready = Mutex::new(Some(ready));
    let window = WebviewWindowBuilder::new(
        main.app_handle(),
        LABEL,
        WebviewUrl::App("snake.html".into()),
    )
    .title("贪吃喵 · IngridMeow")
    .position(position.x, position.y)
    .inner_size(size.width, size.height)
    .visible(false)
    .focused(false)
    .focusable(false)
    .decorations(false)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .always_on_top(true)
    .shadow(false)
    .disable_drag_drop_handler()
    .background_throttling(BackgroundThrottlingPolicy::Disabled)
    .on_page_load(move |_, page| {
        if page.event() == PageLoadEvent::Finished {
            if let Some(ready) = ready.lock().unwrap().take() {
                let _ = ready.send(());
            }
        }
    })
    .build()
    .map_err(|error| error.to_string())?;

    Ok((window, loaded))
}

pub fn show(window: &WebviewWindow) -> tauri::Result<()> {
    window.set_ignore_cursor_events(true)?;
    let handle = window.ns_window()?;
    // Called on the main thread. Tao's show() uses makeKeyAndOrderFront.
    unsafe { eyes_mouse_show_snake(handle) };
    Ok(())
}

pub fn pointer_sample(window: &WebviewWindow, restart: u32) -> tauri::Result<PointerSample> {
    let handle = window.ns_window()?;
    let (mut x, mut y) = (0.0, 0.0);
    // Wry dispatches synchronous WKWebView commands on the main thread.
    let inside = unsafe { eyes_mouse_snake_pointer(handle, &mut x, &mut y) };
    Ok(PointerSample {
        point: inside.then_some([x, y]),
        restart,
    })
}
