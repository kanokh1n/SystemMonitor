//! Иконка в строке меню macOS: показывает загрузку CPU и RAM,
//! по клику открывает окно, в меню есть «Открыть» и «Выйти».

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::metrics::Snapshot;

const TRAY_ID: &str = "main";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Открыть System Monitor", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выйти", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        // Шаблонная иконка сама перекрашивается под светлую и тёмную строку меню.
        .icon_as_template(true)
        .title("…")
        .tooltip("System Monitor")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn update(app: &AppHandle, snapshot: &Snapshot) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let mem = &snapshot.memory;
    let ram = if mem.total > 0 { mem.used as f64 / mem.total as f64 * 100.0 } else { 0.0 };
    let mut title = format!("CPU {:.0}%  RAM {:.0}%", snapshot.cpu.usage, ram);
    // Упавшие серверы видно, даже когда окно закрыто.
    let down = crate::servers::down_count();
    if down > 0 {
        title.push_str(&format!("  ⚠ {down}"));
    }
    let _ = tray.set_title(Some(title));
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
