mod disk;
mod docker;
mod metrics;
mod netinfo;
mod tray;

use tauri::WindowEvent;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            tray::create(app.handle())?;
            metrics::spawn(app.handle().clone());
            docker::spawn(app.handle().clone());
            netinfo::spawn(app.handle().clone());
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
            netinfo::public_ip,
            disk::reveal_in_finder,
            disk::open_full_disk_access,
            disk::has_full_disk_access,
            disk::pick_folder,
            disk::scan::disk_scan_start,
            disk::scan::disk_scan_cancel,
            disk::scan::disk_scan_dir,
            disk::scan::disk_top_files,
            disk::scan::disk_stale,
            disk::apps::disk_apps,
            disk::apps::disk_app_leftovers,
            disk::cleanup::disk_cleanup_scan,
            disk::cleanup::disk_trash,
            disk::cleanup::disk_trash_size,
            disk::cleanup::disk_empty_trash,
            disk::cleanup::disk_docker_usage,
            disk::cleanup::disk_docker_prune,
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
