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
    pub breakdown: Option<MemoryBreakdown>,
}

/// Разбивка памяти как в «Мониторинге системы» macOS, в байтах.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBreakdown {
    /// Память программ (анонимные страницы без «очищаемых»).
    pub app: u64,
    /// Память ядра и драйверов, её нельзя выгрузить.
    pub wired: u64,
    /// Сжатая память: macOS сжимает редко используемые страницы, чтобы не уходить в swap.
    pub compressed: u64,
    /// Кэш файлов: система отдаст его программам, как только понадобится.
    pub cached: u64,
    /// Давление памяти в процентах (0 — свободно, 100 — всё занято).
    pub pressure: Option<u32>,
    /// Уровень давления по мнению системы: normal, warning или critical.
    pub pressure_level: Option<String>,
}

/// Разбирает вывод `vm_stat`: строки вида `Pages wired down:   123456.`
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_vm_stat(text: &str) -> Option<MemoryBreakdown> {
    let page_size: u64 = text
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let purgeable = pages("Pages purgeable:");
    Some(MemoryBreakdown {
        app: pages("Anonymous pages:").saturating_sub(purgeable) * page_size,
        wired: pages("Pages wired down:") * page_size,
        compressed: pages("Pages occupied by compressor:") * page_size,
        cached: (pages("File-backed pages:") + purgeable) * page_size,
        pressure: None,
        pressure_level: None,
    })
}

#[cfg(target_os = "macos")]
fn memory_breakdown() -> Option<MemoryBreakdown> {
    use std::process::Command;
    let sysctl = |name: &str| -> Option<u32> {
        let out = Command::new("sysctl").args(["-n", name]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    };
    let out = Command::new("vm_stat").output().ok()?;
    let mut breakdown = parse_vm_stat(&String::from_utf8_lossy(&out.stdout))?;
    // memorystatus_level — сколько процентов памяти система считает свободной.
    breakdown.pressure = sysctl("kern.memorystatus_level").map(|free| 100u32.saturating_sub(free));
    breakdown.pressure_level = sysctl("kern.memorystatus_vm_pressure_level").map(|level| {
        match level {
            4 => "critical",
            2 => "warning",
            _ => "normal",
        }
        .to_string()
    });
    Some(breakdown)
}

#[cfg(not(target_os = "macos"))]
fn memory_breakdown() -> Option<MemoryBreakdown> {
    None
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
    pub mac: String,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub rx_per_sec: u64,
    pub tx_per_sec: u64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub errors: u64,
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
    /// Приложение, внутри которого лежит программа (`/Applications/Slack.app/...` → `Slack`).
    pub app: Option<String>,
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
            breakdown: memory_breakdown(),
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
            let (v4, v6): (Vec<&sysinfo::IpNetwork>, Vec<_>) = data.ip_networks().iter().partition(|ip| ip.addr.is_ipv4());
            // Служебные интерфейсы macOS без адресов и трафика не показываем.
            if v4.is_empty() && v6.is_empty() && data.total_received() == 0 {
                continue;
            }
            network.interfaces.push(InterfaceInfo {
                name: name.clone(),
                mac: data.mac_address().to_string(),
                ipv4: v4.iter().map(|ip| format!("{}/{}", ip.addr, ip.prefix)).collect(),
                ipv6: v6.iter().map(|ip| ip.addr.to_string()).collect(),
                rx_per_sec: rx,
                tx_per_sec: tx,
                rx_total: data.total_received(),
                tx_total: data.total_transmitted(),
                errors: data.total_errors_on_received() + data.total_errors_on_transmitted(),
            });
        }
        // Сначала интерфейсы с IPv4 (обычно Wi‑Fi en0 и VPN), потом остальные.
        network.interfaces.sort_by(|a, b| (a.ipv4.is_empty(), &a.name).cmp(&(b.ipv4.is_empty(), &b.name)));

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
                    app: p.exe().and_then(app_name),
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

/// Ищет в пути самый внешний `*.app`: у помощников вроде
/// `Google Chrome.app/.../Google Chrome Helper.app` приложением считается Chrome.
fn app_name(path: &std::path::Path) -> Option<String> {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .find_map(|c| c.strip_suffix(".app"))
        .map(str::to_string)
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

    #[test]
    fn finds_outer_app() {
        let p = std::path::Path::new(
            "/Applications/Google Chrome.app/Contents/Frameworks/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/x",
        );
        assert_eq!(app_name(p).as_deref(), Some("Google Chrome"));
        assert_eq!(app_name(std::path::Path::new("/usr/libexec/logd")), None);
    }

    #[test]
    fn parses_vm_stat() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
            Pages free:                                5000.\n\
            Pages wired down:                         100000.\n\
            Pages purgeable:                           2000.\n\
            File-backed pages:                        50000.\n\
            Anonymous pages:                         300000.\n\
            Pages occupied by compressor:             40000.\n";
        let b = parse_vm_stat(text).unwrap();
        assert_eq!(b.wired, 100_000 * 16384);
        assert_eq!(b.app, 298_000 * 16384);
        assert_eq!(b.cached, 52_000 * 16384);
        assert_eq!(b.compressed, 40_000 * 16384);
    }
}
