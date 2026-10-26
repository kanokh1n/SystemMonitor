//! `sysmon-helper` — источник данных для вкладок System Monitor в панели cyclop.
//!
//! Панель запускает его как дочерний процесс и говорит с ним строками JSON:
//! в stdin — что сейчас смотреть и что сделать, из stdout — снимки.
//! Устроено так же, как помощник Now Playing в самом cyclop.
//!
//! Команды (stdin, по одной на строку):
//! - `{"cmd":"watch","system":true,"devices":false,"docker":false,"servers":true}` —
//!   что опрашивать. Выключенная тема не стоит ничего: её поток спит на условной переменной.
//! - `{"cmd":"refresh","topic":"devices"}` — опросить сейчас, не дожидаясь таймера.
//! - `{"cmd":"docker","id":"…","action":"start"|"stop"|"restart"}`.
//! - `{"cmd":"kill","pid":123}`.
//!
//! События (stdout): `{"type":"system"|"devices"|"docker"|"servers","data":…}`
//! и `{"type":"error","message":"…"}`, когда действие не удалось.
//!
//! Закрытый stdin значит, что панель завершилась, — помощник выходит следом.

use std::io::{self, BufRead, Write};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sysmon_core::servers::{Events, Monitor, Server};
use sysmon_core::{data_dir, devices, docker, metrics};

const SYSTEM_TICK: Duration = Duration::from_secs(1);
/// В окне серверы опрашиваются раз в 30 секунд; здесь, в фоне, — вдвое реже.
const SERVERS_TICK: Duration = Duration::from_secs(60);
/// Сколько процессов отдавать: панели нужен только топ.
const TOP_PROCESSES: usize = 5;

/// Одна строка в stdout. Под мьютексом, чтобы строки из разных потоков не перемешались.
fn send(kind: &str, data: Value) {
    let line = json!({ "type": kind, "data": data }).to_string();
    let mut out = io::stdout().lock();
    // Панель закрылась — писать некуда, а выйти помогут stdin и SIGPIPE.
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn send_error(message: &str) {
    let line = json!({ "type": "error", "message": message }).to_string();
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Включена ли тема и сколько раз её просили опросить вне очереди.
#[derive(Default)]
struct GateState {
    on: bool,
    nudges: u64,
}

/// Выключатель темы: поток ждёт на нём, пока тема выключена, и спит между опросами,
/// просыпаясь раньше, если попросили обновить.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}

impl Gate {
    fn set(&self, on: bool) {
        let mut s = self.state.lock().unwrap();
        if s.on != on {
            s.on = on;
            s.nudges += 1;
            self.cv.notify_all();
        }
    }

    fn nudge(&self) {
        let mut s = self.state.lock().unwrap();
        s.nudges += 1;
        self.cv.notify_all();
    }

    fn is_on(&self) -> bool {
        self.state.lock().unwrap().on
    }

    /// Ждёт, пока тему включат.
    fn wait_on(&self) {
        let mut s = self.state.lock().unwrap();
        while !s.on {
            s = self.cv.wait(s).unwrap();
        }
    }

    /// Пауза между опросами. Кончается раньше, если тему выключили или попросили обновить.
    fn pause(&self, timeout: Duration) {
        let s = self.state.lock().unwrap();
        let seen = s.nudges;
        let _ = self.cv.wait_timeout_while(s, timeout, |s| s.nudges == seen);
    }
}

#[derive(Default)]
struct Gates {
    system: Gate,
    devices: Gate,
    docker: Gate,
    servers: Gate,
}

impl Gates {
    fn by_topic(&self, topic: &str) -> Option<&Gate> {
        match topic {
            "system" => Some(&self.system),
            "devices" => Some(&self.devices),
            "docker" => Some(&self.docker),
            "servers" => Some(&self.servers),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
enum Cmd {
    Watch {
        #[serde(default)]
        system: bool,
        #[serde(default)]
        devices: bool,
        #[serde(default)]
        docker: bool,
        #[serde(default)]
        servers: bool,
    },
    Refresh {
        topic: String,
    },
    Docker {
        id: String,
        action: String,
    },
    Kill {
        pid: u32,
    },
}

fn system_loop(gate: &Gate) {
    loop {
        gate.wait_on();
        // Коллектор живёт, только пока вкладку смотрят: список процессов — самое
        // тяжёлое, что тут есть, и держать его в памяти зря незачем.
        let mut collector = metrics::Collector::new();
        thread::sleep(metrics::warmup());
        while gate.is_on() {
            let mut snapshot = collector.collect();
            snapshot.processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
            snapshot.processes.truncate(TOP_PROCESSES);
            send("system", serde_json::to_value(&snapshot).unwrap_or(Value::Null));
            gate.pause(SYSTEM_TICK);
        }
    }
}

fn devices_loop(gate: &Gate) {
    let store = devices::Store::load(data_dir().map(|d| d.join("devices.json")));
    loop {
        gate.wait_on();
        let list = devices::refresh(&store);
        send("devices", serde_json::to_value(&list).unwrap_or(Value::Null));
        gate.pause(devices::POLL);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn docker_loop(gate: &Gate) {
    let rt = runtime();
    let mut poller = docker::Poller::default();
    loop {
        gate.wait_on();
        let snapshot = rt.block_on(poller.snapshot());
        send("docker", serde_json::to_value(&snapshot).unwrap_or(Value::Null));
        gate.pause(docker::POLL);
    }
}

/// Уведомление macOS без своих разрешений: его показывает osascript.
/// Заголовок и текст идут отдельными аргументами, а не внутрь скрипта.
fn notify(title: &str, body: &str) {
    let _ = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ])
        .status();
}

struct StdoutEvents;

impl Events for StdoutEvents {
    fn changed(&self, servers: &[Server]) {
        send("servers", serde_json::to_value(servers).unwrap_or(Value::Null));
    }

    fn alert(&self, title: &str, body: &str) {
        notify(title, body);
    }
}

fn servers_loop(gate: &Gate) {
    let monitor = Monitor::new(data_dir().map(|d| d.join("servers.json")), Box::new(StdoutEvents));
    loop {
        gate.wait_on();
        // Список могли поменять в окне System Monitor.
        monitor.reload();
        monitor.poll_all();
        gate.pause(SERVERS_TICK);
    }
}

fn handle(gates: &Gates, cmd: Cmd) {
    match cmd {
        Cmd::Watch { system, devices, docker, servers } => {
            gates.system.set(system);
            gates.devices.set(devices);
            gates.docker.set(docker);
            gates.servers.set(servers);
        }
        Cmd::Refresh { topic } => {
            if let Some(gate) = gates.by_topic(&topic) {
                gate.nudge();
            }
        }
        Cmd::Docker { id, action } => {
            // Остановка контейнера занимает до 10 секунд — команды из stdin в это время
            // должны читаться дальше.
            thread::spawn(move || {
                if let Err(e) = runtime().block_on(docker::container_action(id, action)) {
                    send_error(&e);
                }
            });
            gates.docker.nudge();
        }
        Cmd::Kill { pid } => {
            if let Err(e) = metrics::kill_process(pid) {
                send_error(&e);
            }
            gates.system.nudge();
        }
    }
}

fn main() {
    let gates: Arc<Gates> = Arc::default();
    let loops: [fn(&Gates); 4] = [
        |g| system_loop(&g.system),
        |g| devices_loop(&g.devices),
        |g| docker_loop(&g.docker),
        |g| servers_loop(&g.servers),
    ];
    for run in loops {
        let gates = Arc::clone(&gates);
        thread::spawn(move || run(&gates));
    }

    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Cmd>(&line) {
            Ok(cmd) => handle(&gates, cmd),
            Err(e) => send_error(&format!("Непонятная команда: {e}")),
        }
    }
}
