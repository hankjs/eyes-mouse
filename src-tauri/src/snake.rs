use std::sync::Mutex;
use std::time::Duration;
use tauri::webview::PageLoadEvent;
use tauri::window::Color;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::oneshot;

pub const LABEL: &str = "snake";

#[derive(Default)]
pub struct Launch(pub tokio::sync::Mutex<()>);

pub async fn create(main: &WebviewWindow) -> Result<WebviewWindow, String> {
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
    .title("贪吃蛇 · Eyes Mouse")
    .position(position.x, position.y)
    .inner_size(size.width, size.height)
    .visible(false)
    .focused(false)
    .decorations(false)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .always_on_top(true)
    .shadow(false)
    .on_page_load(move |_, page| {
        if page.event() == PageLoadEvent::Finished {
            if let Some(ready) = ready.lock().unwrap().take() {
                let _ = ready.send(());
            }
        }
    })
    .build()
    .map_err(|error| error.to_string())?;

    if !matches!(
        tokio::time::timeout(Duration::from_secs(5), loaded).await,
        Ok(Ok(()))
    ) {
        let _ = window.destroy();
        return Err("贪吃蛇加载失败，请重试。".into());
    }
    Ok(window)
}

pub fn close_for_calibration(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.destroy();
    }
}

#[tauri::command]
pub async fn close_snake(window: WebviewWindow) -> Result<(), String> {
    if window.label() != LABEL {
        return Err("只能从游戏窗口退出贪吃蛇。".into());
    }
    window.close().map_err(|error| error.to_string())
}
