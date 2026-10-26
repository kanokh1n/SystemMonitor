//! Данные System Monitor без привязки к интерфейсу.
//!
//! Этим крейтом пользуются двое: окно на Tauri (`src-tauri`) и `sysmon-helper` —
//! маленький процесс, из которого панель в вырезе (`notch/`) берёт цифры.

pub mod devices;
pub mod docker;
pub mod metrics;
pub mod servers;

use std::path::PathBuf;

/// Папка данных приложения — та же, что `app_data_dir()` у Tauri для
/// `com.kanokhin.systemmonitor`. Окно и панель делят `servers.json` и `devices.json`:
/// сервер, добавленный в окне, сразу виден в панели.
pub fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/com.kanokhin.systemmonitor"))
}
