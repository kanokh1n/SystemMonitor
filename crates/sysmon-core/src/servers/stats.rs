//! Что собираем с сервера и как разбираем ответ.
//!
//! Агента на сервере нет: по SSH запускаем короткий POSIX‑скрипт, он печатает
//! содержимое `/proc` и вывод `df` / `docker`, разделы помечены строками `@@имя`.
//! Всё считаем здесь, на Mac. Скрипт рассчитан на Linux (обычный VPS).

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

/// Скрипт уходит в `sh -s` через stdin, поэтому не зависит от того,
/// какой shell у пользователя на сервере (bash, zsh, fish).
pub const SCRIPT: &str = r#"
LC_ALL=C; export LC_ALL
echo @@host; hostname 2>/dev/null || cat /etc/hostname
echo @@os; (. /etc/os-release && echo "$PRETTY_NAME") 2>/dev/null || uname -s; uname -r
echo @@uptime; cat /proc/uptime
echo @@load; cat /proc/loadavg
echo @@cores; nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo
echo @@stat1; head -n 1 /proc/stat
sleep 1
echo @@stat2; head -n 1 /proc/stat
echo @@mem; cat /proc/meminfo
echo @@df; df -kP 2>/dev/null
if command -v docker >/dev/null 2>&1; then
  echo @@docker; docker ps -a --no-trunc --format '{{json .}}' 2>&1 || echo @@docker-error
  echo @@dockerstats; docker stats --no-stream --format '{{json .}}' 2>/dev/null
else
  echo @@docker-missing
fi
"#;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub uptime_secs: u64,
    pub cores: u32,
    pub load_avg: [f64; 3],
    /// Загрузка CPU за секунду между двумя чтениями `/proc/stat`.
    pub cpu_percent: Option<f64>,
    pub memory: Memory,
    pub disks: Vec<Disk>,
    pub docker: Docker,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub mount_point: String,
    pub file_system: String,
    pub total: u64,
    pub used: u64,
    pub available: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Docker {
    /// Docker установлен на сервере.
    pub installed: bool,
    /// Почему не удалось получить список (чаще всего нет прав на сокет).
    pub error: Option<String>,
    pub containers: Vec<Container>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub name: String,
    pub image: String,
    /// `running`, `exited`, `paused`, `restarting`, `created`, `dead`.
    pub state: String,
    pub status: String,
    pub cpu_percent: Option<f64>,
    pub memory_used: Option<u64>,
}

/// Режет вывод скрипта на разделы по строкам `@@имя`.
fn sections(out: &str) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current = "";
    for line in out.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            current = name.trim();
            map.entry(current).or_default();
        } else if !current.is_empty() {
            map.entry(current).or_default().push(line);
        }
    }
    map
}

/// Строка `cpu  user nice system idle iowait irq softirq steal …` → (всего, простой).
fn cpu_times(line: &str) -> Option<(u64, u64)> {
    let nums: Vec<u64> = line.split_whitespace().skip(1).take(8).filter_map(|n| n.parse().ok()).collect();
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    Some((nums.iter().sum(), idle))
}

fn cpu_percent(before: &str, after: &str) -> Option<f64> {
    let (t1, i1) = cpu_times(before)?;
    let (t2, i2) = cpu_times(after)?;
    let total = t2.checked_sub(t1)?;
    let idle = i2.checked_sub(i1)?;
    (total > 0).then(|| ((total - idle.min(total)) as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

/// `/proc/meminfo` пишет в килобайтах.
fn parse_meminfo(lines: &[&str]) -> Memory {
    let field = |key: &str| {
        lines
            .iter()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    let total = field("MemTotal").unwrap_or(0);
    // В очень старых ядрах MemAvailable нет — считаем как free + кэш.
    let available = field("MemAvailable")
        .unwrap_or_else(|| field("MemFree").unwrap_or(0) + field("Buffers").unwrap_or(0) + field("Cached").unwrap_or(0));
    let swap_total = field("SwapTotal").unwrap_or(0);
    Memory { total, available, swap_total, swap_used: swap_total.saturating_sub(field("SwapFree").unwrap_or(0)) }
}

/// `df -kP`: показываем настоящие разделы (`/dev/…`), без snap‑образов и повторов одного устройства.
fn parse_df(lines: &[&str]) -> Vec<Disk> {
    let mut seen = Vec::new();
    let mut disks = Vec::new();
    for line in lines.iter().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }
        let fs = cols[0];
        // Точка монтирования может содержать пробелы.
        let mount = cols[5..].join(" ");
        let real = fs.starts_with("/dev/") && !fs.starts_with("/dev/loop");
        if !(real || mount == "/") || mount.starts_with("/snap/") || seen.contains(&fs) {
            continue;
        }
        let kb = |s: &str| s.parse::<u64>().unwrap_or(0) * 1024;
        seen.push(fs);
        disks.push(Disk {
            mount_point: mount,
            file_system: fs.to_string(),
            total: kb(cols[1]),
            used: kb(cols[2]),
            available: kb(cols[3]),
        });
    }
    // Корень первым, остальные по алфавиту.
    disks.sort_by(|a, b| (a.mount_point != "/", &a.mount_point).cmp(&(b.mount_point != "/", &b.mount_point)));
    disks
}

/// `12.3MiB`, `1.94GiB`, `512kB`, `0B` → байты.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.trim().parse().ok()?;
    let mult = match unit.to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kib" => 1024.0,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        _ => return None,
    };
    Some((n * mult) as u64)
}

/// Старые версии Docker не отдают `State`, только `Status` вида «Up 3 days».
fn state_from_status(status: &str) -> &'static str {
    let s = status.to_lowercase();
    if s.contains("(paused)") {
        "paused"
    } else if s.starts_with("up") {
        "running"
    } else if s.starts_with("restarting") {
        "restarting"
    } else if s.starts_with("created") {
        "created"
    } else if s.starts_with("dead") {
        "dead"
    } else {
        "exited"
    }
}

fn parse_docker(map: &HashMap<&str, Vec<&str>>) -> Docker {
    if map.contains_key("docker-missing") || !map.contains_key("docker") {
        return Docker::default();
    }
    let lines = &map["docker"];
    if map.contains_key("docker-error") {
        let text = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        let error = if text.contains("permission denied") {
            "Нет прав на Docker: добавь пользователя в группу docker (sudo usermod -aG docker $USER)".to_string()
        } else if text.contains("Cannot connect to the Docker daemon") || text.contains("failed to connect to the docker API") {
            "Docker установлен, но не запущен".to_string()
        } else {
            text
        };
        return Docker { installed: true, error: Some(error), containers: vec![] };
    }

    let json = |l: &&str| serde_json::from_str::<Value>(l).ok();
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_string();
    let stats: HashMap<String, Value> = map
        .get("dockerstats")
        .into_iter()
        .flatten()
        .filter_map(json)
        .map(|v| (text(&v, "Name"), v))
        .collect();

    let mut containers: Vec<Container> = lines
        .iter()
        .filter_map(json)
        .map(|v| {
            let name = text(&v, "Names");
            let status = text(&v, "Status");
            let state = Some(text(&v, "State")).filter(|s| !s.is_empty()).unwrap_or_else(|| state_from_status(&status).into());
            let st = stats.get(&name);
            let cpu_percent = st.and_then(|s| s["CPUPerc"].as_str()?.trim_end_matches('%').parse().ok());
            let memory_used = st.and_then(|s| parse_size(s["MemUsage"].as_str()?.split('/').next()?));
            Container { image: text(&v, "Image"), name, state, status, cpu_percent, memory_used }
        })
        .collect();
    containers.sort_by(|a, b| (a.state != "running", &a.name).cmp(&(b.state != "running", &b.name)));
    Docker { installed: true, error: None, containers }
}

/// Разбирает весь вывод скрипта. `None`, если это совсем не похоже на Linux.
pub fn parse(out: &str) -> Option<Stats> {
    let map = sections(out);
    let first = |key: &str| map.get(key).and_then(|l| l.first()).map(|s| s.trim().to_string());
    let memory = parse_meminfo(map.get("mem")?);
    if memory.total == 0 {
        return None;
    }
    let uptime_secs = first("uptime")
        .and_then(|l| l.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let load: Vec<f64> = first("load")
        .map(|l| l.split_whitespace().take(3).filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default();
    let os = map.get("os").cloned().unwrap_or_default();
    Some(Stats {
        hostname: first("host").unwrap_or_default(),
        os: os.first().map(|s| s.trim().to_string()).unwrap_or_default(),
        kernel: os.get(1).map(|s| s.trim().to_string()).unwrap_or_default(),
        uptime_secs,
        cores: first("cores").and_then(|c| c.parse().ok()).unwrap_or(1),
        load_avg: [0, 1, 2].map(|i| load.get(i).copied().unwrap_or(0.0)),
        cpu_percent: cpu_percent(&first("stat1")?, &first("stat2")?),
        memory,
        disks: parse_df(map.get("df").map(Vec::as_slice).unwrap_or_default()),
        docker: parse_docker(&map),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = r#"@@host
vps-1
@@os
Ubuntu 22.04.4 LTS
5.15.0-105-generic
@@uptime
1036800.52 2000000.10
@@load
0.42 0.30 0.25 1/180 12345
@@cores
2
@@stat1
cpu  1000 0 500 8000 100 0 0 0 0 0
@@stat2
cpu  1060 0 520 8100 120 0 0 0 0 0
@@mem
MemTotal:        2014256 kB
MemFree:          120000 kB
MemAvailable:    1007128 kB
SwapTotal:       1048572 kB
SwapFree:         524286 kB
@@df
Filesystem     1024-blocks     Used Available Capacity Mounted on
tmpfs               201428     1100    200328       1% /run
/dev/vda1         40470732 12000000  28454116      30% /
/dev/loop0           65536    65536         0     100% /snap/core20/2105
/dev/vda15          106858     6186    100672       6% /boot/efi
overlay           40470732 12000000  28454116      30% /var/lib/docker/overlay2/abc/merged
@@docker
{"ID":"a1","Image":"nginx:1.25","Names":"web","State":"running","Status":"Up 3 days","Ports":"0.0.0.0:80->80/tcp"}
{"ID":"b2","Image":"postgres:16","Names":"db","State":"exited","Status":"Exited (0) 2 hours ago","Ports":""}
@@dockerstats
{"CPUPerc":"1.50%","MemUsage":"12.5MiB / 1.92GiB","Name":"web"}
"#;

    #[test]
    fn parses_full_output() {
        let s = parse(OUTPUT).unwrap();
        assert_eq!(s.hostname, "vps-1");
        assert_eq!(s.os, "Ubuntu 22.04.4 LTS");
        assert_eq!(s.kernel, "5.15.0-105-generic");
        assert_eq!(s.uptime_secs, 1036800);
        assert_eq!(s.cores, 2);
        assert_eq!(s.load_avg, [0.42, 0.30, 0.25]);
        // Всего прошло 200 тиков, из них простой 120 → занято 40%.
        assert_eq!(s.cpu_percent, Some(40.0));
        assert_eq!(s.memory.total, 2014256 * 1024);
        assert_eq!(s.memory.available, 1007128 * 1024);
        assert_eq!(s.memory.swap_used, 524286 * 1024);
        let mounts: Vec<_> = s.disks.iter().map(|d| d.mount_point.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/boot/efi"]);
        assert_eq!(s.disks[0].used, 12000000 * 1024);
        assert!(s.docker.installed);
        assert_eq!(s.docker.containers.len(), 2);
        let web = &s.docker.containers[0];
        assert_eq!((web.name.as_str(), web.state.as_str()), ("web", "running"));
        assert_eq!(web.cpu_percent, Some(1.5));
        assert_eq!(web.memory_used, Some((12.5 * 1024.0 * 1024.0) as u64));
        assert_eq!(s.docker.containers[1].memory_used, None);
    }

    #[test]
    fn docker_without_permission() {
        let out = OUTPUT.split("@@docker\n").next().unwrap().to_string()
            + "@@docker\npermission denied while trying to connect to the Docker daemon socket\n@@docker-error\n@@dockerstats\n";
        let d = parse(&out).unwrap().docker;
        assert!(d.installed);
        assert!(d.error.unwrap().contains("группу docker"));
    }

    #[test]
    fn docker_missing() {
        let out = OUTPUT.split("@@docker\n").next().unwrap().to_string() + "@@docker-missing\n";
        assert_eq!(parse(&out).unwrap().docker, Docker::default());
    }

    #[test]
    fn old_docker_status() {
        assert_eq!(state_from_status("Up 2 hours (Paused)"), "paused");
        assert_eq!(state_from_status("Up 5 minutes"), "running");
        assert_eq!(state_from_status("Exited (137) 1 day ago"), "exited");
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("1.5GiB"), Some(1610612736));
        assert_eq!(parse_size(" 300kB "), Some(300_000));
        assert_eq!(parse_size("??"), None);
    }

    #[test]
    fn not_linux() {
        assert_eq!(parse("@@host\nfoo\n@@mem\n"), None);
    }
}
