//! Раздел «Серверы» в окне: опрос раз в [`servers::POLL`], событие `servers`
//! и уведомления macOS. Сам опрос и список — в [`sysmon_core::servers`].

use std::sync::Arc;
use std::thread;

use sysmon_core::servers::{self, Events, Monitor, Server, SshSetup, Target};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

pub use sysmon_core::servers::down_count;

struct WindowEvents(AppHandle);

impl Events for WindowEvents {
    fn changed(&self, servers: &[Server]) {
        let _ = self.0.emit("servers", servers);
    }

    fn alert(&self, title: &str, body: &str) {
        let _ = self.0.notification().builder().title(title).body(body).show();
    }
}

type State<'a> = tauri::State<'a, Arc<Monitor>>;

pub fn spawn(app: AppHandle) {
    let path = app.path().app_data_dir().ok().map(|d| d.join("servers.json"));
    let monitor = Arc::new(Monitor::new(path, Box::new(WindowEvents(app.clone()))));
    app.manage(Arc::clone(&monitor));
    thread::spawn(move || loop {
        monitor.poll_all();
        thread::sleep(servers::POLL);
    });
}

#[tauri::command]
pub fn servers_list(monitor: State) -> Vec<Server> {
    monitor.list()
}

/// Добавляет сервер (без `id`) или меняет существующий, и сразу опрашивает его.
#[tauri::command]
pub fn server_save(monitor: State, id: Option<String>, name: String, target: Target) -> Result<String, String> {
    let id = monitor.save(id, name, target)?;
    let monitor = Arc::clone(&monitor);
    let poll_id = id.clone();
    thread::spawn(move || monitor.poll(&poll_id));
    Ok(id)
}

#[tauri::command]
pub fn server_remove(monitor: State, id: String) {
    monitor.remove(&id);
}

/// Кнопка «Обновить»: один сервер или все сразу.
#[tauri::command]
pub async fn server_refresh(app: AppHandle, id: Option<String>) -> Result<(), String> {
    let monitor = Arc::clone(&app.state::<Arc<Monitor>>());
    tauri::async_runtime::spawn_blocking(move || match id {
        Some(id) => monitor.poll(&id),
        None => monitor.poll_all(),
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ssh_setup() -> SshSetup {
    servers::ssh_setup()
}
