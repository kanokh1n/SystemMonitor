//! Сбор метрик системы: CPU, память, диски, сеть, батарея, температура и процессы.
//!
//! Работает в отдельном потоке: раз в секунду обновляет данные и отправляет их
//! во фронтенд событием `metrics`. Заодно обновляет текст иконки в строке меню.

use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{Components, Disks, Networks, Pid, ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter};

use crate::tray;

const TICK: Duration = Duration::from_secs(1);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub disks: Vec<DiskInfo>,
    pub disk_io: IoRate,
    pub network: NetworkInfo,
    pub battery: Option<BatteryInfo>,
    pub temperatures: Vec<TempInfo>,
    pub processes: Vec<ProcessInfo>,
    pub system: SystemInfo,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CpuInfo {
    pub usage: f32,
    pub per_core: Vec<f32>,
    pub brand: String,
    pub cores: usize,
    pub physical_cores: Option<usize>,
    pub load_avg: [f64; 3],
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInfo {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
}

/// Скорость в байтах в секунду.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct IoRate {
    pub read_per_sec: u64,
    pub write_per_sec: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfo {
    pub rx_per_sec: u64,
    pub tx_per_sec: u64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub interfaces: Vec<InterfaceInfo>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceInfo {
    pub name: String,
    pub ips: Vec<String>,
    pub rx_per_sec: u64,
    pub tx_per_sec: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BatteryInfo {
    pub percent: f32,
    pub state: String,
    pub health_percent: f32,
    pub cycle_count: Option<u32>,
    pub power_watts: f32,
    pub minutes_to_empty: Option<u32>,
    pub minutes_to_full: Option<u32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TempInfo {
    pub label: String,
    pub celsius: f32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub memory: u64,
    pub read_per_sec: u64,
    pub write_per_sec: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub host_name: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub uptime_secs: u64,
}

/// Держит состояние между обновлениями: многие значения (CPU, сеть, диск)
/// считаются как разница с прошлым замером, поэтому объекты живут всё время.
struct Collector {
    sys: System,
    disks: Disks,
    networks: Networks,
    components: Components,
    battery: Option<starship_battery::Manager>,
    last_tick: Instant,
}

impl Collector {
    fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        Self {
            sys,
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            battery: starship_battery::Manager::new().ok(),
            last_tick: Instant::now(),
        }
    }

    fn collect(&mut self) -> Snapshot {
        let elapsed = self.last_tick.elapsed().as_secs_f64().max(0.001);
        self.last_tick = Instant::now();
        // Переводит «байт с прошлого замера» в «байт в секунду».
        let per_sec = |bytes: u64| (bytes as f64 / elapsed) as u64;

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.disks.refresh(true);
        self.networks.refresh(true);
        self.components.refresh(true);

        let cpus = self.sys.cpus();
        let load = System::load_average();
        let cpu = CpuInfo {
            usage: self.sys.global_cpu_usage(),
            per_core: cpus.iter().map(|c| c.cpu_usage()).collect(),
            brand: cpus.first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            cores: cpus.len(),
            physical_cores: System::physical_core_count(),
            load_avg: [load.one, load.five, load.fifteen],
        };

        let memory = MemoryInfo {
            total: self.sys.total_memory(),
            used: self.sys.used_memory(),
            available: self.sys.available_memory(),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
        };

        let mut disk_io = IoRate::default();
        let mut disks = Vec::new();
        for d in self.disks.list() {
            let usage = d.usage();
            disk_io.read_per_sec += per_sec(usage.read_bytes);
            disk_io.write_per_sec += per_sec(usage.written_bytes);
            let mount = d.mount_point().to_string_lossy().to_string();
            // На macOS много служебных томов APFS (/System/Volumes/...), они делят
            // место с основным диском. Показываем корень и подключённые тома.
            if mount == "/" || mount.starts_with("/Volumes/") {
                disks.push(DiskInfo {
                    name: d.name().to_string_lossy().to_string(),
                    mount_point: mount,
                    file_system: d.file_system().to_string_lossy().to_string(),
                    total: d.total_space(),
                    available: d.available_space(),
                    removable: d.is_removable(),
                });
            }
        }

        let mut network = NetworkInfo {
            rx_per_sec: 0,
            tx_per_sec: 0,
            rx_total: 0,
            tx_total: 0,
            interfaces: Vec::new(),
        };
        for (name, data) in self.networks.list() {
            if name.starts_with("lo") {
                continue;
            }
            let rx = per_sec(data.received());
            let tx = per_sec(data.transmitted());
            network.rx_per_sec += rx;
            network.tx_per_sec += tx;
            network.rx_total += data.total_received();
            network.tx_total += data.total_transmitted();
            let ips: Vec<String> = data
                .ip_networks()
                .iter()
                .filter(|ip| ip.addr.is_ipv4())
                .map(|ip| ip.addr.to_string())
                .collect();
            if !ips.is_empty() {
                network.interfaces.push(InterfaceInfo {
                    name: name.clone(),
                    ips,
                    rx_per_sec: rx,
                    tx_per_sec: tx,
                });
            }
        }
        network.interfaces.sort_by(|a, b| a.name.cmp(&b.name));

        let mut temperatures: Vec<TempInfo> = self
            .components
            .list()
            .iter()
            .filter_map(|c| {
                let t = c.temperature()?;
                (t.is_finite() && t > 0.0).then(|| TempInfo {
                    label: c.label().to_string(),
                    celsius: t,
                })
            })
            .collect();
        temperatures.sort_by(|a, b| a.label.cmp(&b.label));

        let processes = self
            .sys
            .processes()
            .values()
            .map(|p| {
                let io = p.disk_usage();
                ProcessInfo {
                    pid: p.pid().as_u32(),
                    name: p.name().to_string_lossy().to_string(),
                    cpu: p.cpu_usage(),
                    memory: p.memory(),
                    read_per_sec: per_sec(io.read_bytes),
                    write_per_sec: per_sec(io.written_bytes),
                }
            })
            .collect();

        let system = SystemInfo {
            host_name: System::host_name().unwrap_or_default(),
            os: System::long_os_version().unwrap_or_default(),
            kernel: System::kernel_version().unwrap_or_default(),
            arch: System::cpu_arch(),
            uptime_secs: System::uptime(),
        };

        Snapshot {
            cpu,
            memory,
            disks,
            disk_io,
            network,
            battery: self.read_battery(),
            temperatures,
            processes,
            system,
        }
    }

    fn read_battery(&self) -> Option<BatteryInfo> {
        use starship_battery::units::{power::watt, ratio::percent, time::minute};
        use starship_battery::State;

        let manager = self.battery.as_ref()?;
        let battery = manager.batteries().ok()?.flatten().next()?;
        let state = match battery.state() {
            State::Charging => "charging",
            State::Discharging => "discharging",
            State::Full => "full",
            State::Empty => "empty",
            State::Paused => "paused",
            State::Unknown => "unknown",
        };
        Some(BatteryInfo {
            percent: battery.state_of_charge().get::<percent>(),
            state: state.to_string(),
            health_percent: battery.state_of_health().get::<percent>(),
            cycle_count: battery.cycle_count(),
            power_watts: battery.energy_rate().get::<watt>(),
            minutes_to_empty: battery.time_to_empty().map(|t| t.get::<minute>() as u32),
            minutes_to_full: battery.time_to_full().map(|t| t.get::<minute>() as u32),
        })
    }
}

/// Запускает фоновый поток сбора метрик.
pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        let mut collector = Collector::new();
        // Первый замер CPU всегда нулевой: даём системе накопить разницу.
        thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(500)));
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
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let process = sys.process(pid).ok_or("Процесс уже завершился")?;
    if process.kill() {
        Ok(())
    } else {
        Err("Не удалось завершить процесс. Возможно, он принадлежит системе или другому пользователю.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_snapshot() {
        let mut c = Collector::new();
        thread::sleep(Duration::from_millis(500));
        let s = c.collect();
        assert!(s.memory.total > 0);
        assert!(s.cpu.cores > 0);
        assert!(!s.processes.is_empty());
        assert!(serde_json::to_string(&s).is_ok());
    }
}
