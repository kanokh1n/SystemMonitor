//! Bluetooth‑устройства для окна: опрос раз в [`devices::POLL`] и событие `devices`.
//! Сам опрос и память о заряде — в [`sysmon_core::devices`].

use std::thread;

use sysmon_core::devices::{self, Device, Store};
use tauri::{AppHandle, Emitter, Manager};

fn refresh(app: &AppHandle) -> Vec<Device> {
    let list = devices::refresh(&app.state::<Store>());
    let _ = app.emit("devices", &list);
    list
}

pub fn spawn(app: AppHandle) {
    let path = app.path().app_data_dir().ok().map(|d| d.join("devices.json"));
    app.manage(Store::load(path));
    thread::spawn(move || loop {
        refresh(&app);
        thread::sleep(devices::POLL);
    });
}

/// Кнопка «Обновить»: опрашиваем устройства сразу, не дожидаясь таймера.
#[tauri::command]
pub async fn refresh_devices(app: AppHandle) -> Result<Vec<Device>, String> {
    tauri::async_runtime::spawn_blocking(move || refresh(&app)).await.map_err(|e| e.to_string())
}
