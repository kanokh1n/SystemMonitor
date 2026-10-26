//! Docker для окна: раз в несколько секунд снимок из [`sysmon_core::docker`]
//! уходит во фронтенд событием `docker`.

use sysmon_core::docker::{self, Poller};

pub use sysmon_core::docker::connect;
use tauri::{AppHandle, Emitter};

/// Запускает фоновую задачу опроса Docker.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut poller = Poller::default();
        loop {
            let snapshot = poller.snapshot().await;
            let _ = app.emit("docker", &snapshot);
            tokio::time::sleep(docker::POLL).await;
        }
    });
}

#[tauri::command]
pub async fn container_action(id: String, action: String) -> Result<(), String> {
    docker::container_action(id, action).await
}

#[tauri::command]
pub async fn container_logs(id: String) -> Result<String, String> {
    docker::container_logs(id).await
}
