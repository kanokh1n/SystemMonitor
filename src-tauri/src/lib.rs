mod docker;
mod metrics;
mod tray;

use tauri::WindowEvent;

pub fn run() {
    let app = tauri::Builder::default()
        .setup(|app| {
            tray::create(app.handle())?;
            metrics::spawn(app.handle().clone());
            docker::spawn(app.handle().clone());
            Ok(())
        })
        // Крестик прячет окно, а не закрывает приложение: оно продолжает жить в строке меню.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            metrics::kill_process,
            docker::container_action,
            docker::container_logs,
        ])
        .build(tauri::generate_context!())
        .expect("не удалось запустить приложение");

    app.run(|_app, _event| {
        // Клик по иконке в Доке, когда окно спрятано.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            tray::show_main_window(_app);
        }
    });
}
