//! Подробности о сети, которых нет в sysinfo: шлюз, DNS, открытые порты
//! и какие программы держат соединения. Данные берём из стандартных утилит
//! macOS (`route`, `scutil`, `lsof`) раз в 5 секунд и отправляем событием `netinfo`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::Command;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

const POLL: Duration = Duration::from_secs(5);

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetDetails {
    pub gateway: Option<String>,
    pub default_interface: Option<String>,
    pub dns: Vec<String>,
    pub listening: Vec<ListeningPort>,
    pub connections: Vec<AppConnections>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    pub port: u16,
    pub address: String,
    pub process: String,
    pub pid: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppConnections {
    pub process: String,
    pub pid: u32,
    pub connections: usize,
    pub remote_hosts: usize,
}

/// Запускает команду и возвращает её вывод, если она отработала успешно.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `route -n get default` печатает строки вида `gateway: 192.168.1.1`.
fn default_route() -> (Option<String>, Option<String>) {
    let Some(text) = run("route", &["-n", "get", "default"]) else { return (None, None) };
    let field = |name: &str| {
        text.lines()
            .filter_map(|l| l.trim().strip_prefix(name))
            .map(|v| v.trim().to_string())
            .next()
    };
    (field("gateway:"), field("interface:"))
}

/// `scutil --dns` печатает `nameserver[0] : 1.1.1.1` для каждого резолвера.
fn dns_servers() -> Vec<String> {
    let Some(text) = run("scutil", &["--dns"]) else { return vec![] };
    let mut seen = HashSet::new();
    text.lines()
        .filter(|l| l.trim_start().starts_with("nameserver["))
        .filter_map(|l| l.split(':').nth(1).map(|v| v.trim().to_string()))
        .filter(|v| seen.insert(v.clone()))
        .take(4)
        .collect()
}

struct Socket {
    pid: u32,
    process: String,
    name: String,
    state: String,
}

/// Разбирает машинный вывод `lsof -F pcnT`: каждая строка начинается с буквы поля
/// (p — PID, c — имя программы, f — новый файл, n — адрес, T — состояние TCP).
fn parse_lsof(text: &str) -> Vec<Socket> {
    let mut sockets = Vec::new();
    let (mut pid, mut process) = (0u32, String::new());
    let mut current: Option<Socket> = None;
    for line in text.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                sockets.extend(current.take());
                pid = value.parse().unwrap_or(0);
            }
            "c" => process = value.to_string(),
            "f" => {
                sockets.extend(current.take());
                current = Some(Socket { pid, process: process.clone(), name: String::new(), state: String::new() });
            }
            "n" => {
                if let Some(s) = current.as_mut() {
                    s.name = value.to_string();
                }
            }
            "T" => {
                if let (Some(s), Some(state)) = (current.as_mut(), value.strip_prefix("ST=")) {
                    s.state = state.to_string();
                }
            }
            _ => {}
        }
    }
    sockets.extend(current);
    sockets
}

/// Порт — всё после последнего двоеточия: `*:3000`, `127.0.0.1:5432`, `[::1]:8080`.
fn split_port(addr: &str) -> Option<(String, u16)> {
    let (host, port) = addr.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

fn sockets_summary(sockets: &[Socket]) -> (Vec<ListeningPort>, Vec<AppConnections>) {
    let mut listening: BTreeMap<(u16, u32), ListeningPort> = BTreeMap::new();
    let mut apps: HashMap<u32, (String, usize, HashSet<String>)> = HashMap::new();

    for s in sockets {
        match s.state.as_str() {
            "LISTEN" => {
                if let Some((address, port)) = split_port(&s.name) {
                    listening.entry((port, s.pid)).or_insert(ListeningPort {
                        port,
                        address,
                        process: s.process.clone(),
                        pid: s.pid,
                    });
                }
            }
            "ESTABLISHED" => {
                let entry = apps.entry(s.pid).or_insert_with(|| (s.process.clone(), 0, HashSet::new()));
                entry.1 += 1;
                if let Some((_, remote)) = s.name.split_once("->") {
                    if let Some((host, _)) = split_port(remote) {
                        entry.2.insert(host);
                    }
                }
            }
            _ => {}
        }
    }

    let mut connections: Vec<AppConnections> = apps
        .into_iter()
        .map(|(pid, (process, connections, hosts))| AppConnections {
            process,
            pid,
            connections,
            remote_hosts: hosts.len(),
        })
        .collect();
    connections.sort_by(|a, b| b.connections.cmp(&a.connections).then(a.process.cmp(&b.process)));
    connections.truncate(15);

    (listening.into_values().collect(), connections)
}

fn collect() -> NetDetails {
    let (gateway, default_interface) = default_route();
    // Без прав администратора lsof видит только процессы текущего пользователя.
    let sockets = run("lsof", &["+c", "0", "-nP", "-iTCP", "-F", "pcnT"])
        .map(|t| parse_lsof(&t))
        .unwrap_or_default();
    let (listening, connections) = sockets_summary(&sockets);
    NetDetails { gateway, default_interface, dns: dns_servers(), listening, connections }
}

pub fn spawn(app: AppHandle) {
    thread::spawn(move || loop {
        let _ = app.emit("netinfo", &collect());
        thread::sleep(POLL);
    });
}

/// Внешний IP узнаём только по кнопке: это запрос в интернет (api.ipify.org).
#[tauri::command]
pub async fn public_ip() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        run("curl", &["-s", "--max-time", "5", "https://api.ipify.org"])
            .map(|ip| ip.trim().to_string())
            .filter(|ip| !ip.is_empty())
            .ok_or_else(|| "Не удалось узнать внешний IP".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsof_output() {
        let text = "p101\ncnode\nf21\nn*:3000\nTST=LISTEN\nTQR=0\nf22\nn192.168.1.5:50000->140.82.112.3:443\nTST=ESTABLISHED\n\
                    p202\ncpostgres\nf5\nn127.0.0.1:5432\nTST=LISTEN\nf6\n[::1]:5432\nf7\nn[::1]:5432\nTST=LISTEN\n";
        let (listening, apps) = sockets_summary(&parse_lsof(text));
        assert_eq!(listening.iter().map(|l| l.port).collect::<Vec<_>>(), vec![3000, 5432]);
        assert_eq!(listening[0].process, "node");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].connections, 1);
        assert_eq!(apps[0].remote_hosts, 1);
    }
}
