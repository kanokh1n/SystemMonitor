//! Метрики системы для окна: раз в секунду снимок из [`sysmon_core::metrics`]
//! уходит во фронтенд событием `metrics` и в текст иконки в строке меню.

use std::thread;
use std::time::{Duration, Instant};

use sysmon_core::metrics::{self, Collector};
use tauri::{AppHandle, Emitter};

use crate::tray;

pub use sysmon_core::metrics::Snapshot;

const TICK: Duration = Duration::from_secs(1);

/// Запускает фоновый поток сбора метрик.
pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        let mut collector = Collector::new();
        // Первый замер CPU всегда нулевой: даём системе накопить разницу.
        thread::sleep(metrics::warmup());
        loop {
            let started = Instant::now();
            let snapshot = collector.collect();
            tray::update(&app, &snapshot);
            let _ = app.emit("metrics", &snapshot);
            thread::sleep(TICK.saturating_sub(started.elapsed()));
        }
    });
}

/// Завершает процесс по PID (как «Завершить» в Мониторинге системы).
#[tauri::command]
pub fn kill_process(pid: u32) -> Result<(), String> {
    metrics::kill_process(pid)
}
