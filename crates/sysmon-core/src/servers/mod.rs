//! Раздел «Серверы» (v3): мониторинг VPS по SSH без агента.
//!
//! Раз в 30 секунд заходим на каждый сервер системным `ssh` с ключами из `~/.ssh`,
//! запускаем скрипт из [`stats`] и показываем CPU, память, диски, аптайм и Docker.
//! Если сервер перестал отвечать — говорим об этом через [`Events::alert`],
//! когда вернулся — тоже. Как показать уведомление, решает тот, кто держит [`Monitor`].
//!
//! Список серверов хранится в `servers.json` в папке данных приложения.
//! Паролей и ключей там нет: только адрес, пользователь, порт и путь к ключу.

pub mod ssh;
pub mod stats;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use ssh::{ConfigHost, Failure};
use stats::Stats;

pub const POLL: Duration = Duration::from_secs(30);
/// Сколько неудачных опросов подряд, прежде чем считать сервер упавшим.
/// Одна ошибка бывает после сна Mac или из‑за короткого сбоя сети.
const FAILURES_TO_ALERT: u32 = 2;

/// Сколько серверов сейчас лежит — показываем в строке меню.
static DOWN: AtomicUsize = AtomicUsize::new(0);

pub fn down_count() -> usize {
    DOWN.load(Ordering::Relaxed)
}

/// Куда подключаться. `host` — алиас из `~/.ssh/config`, адрес или `user@адрес`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Путь к приватному ключу. Пусто — ssh выберет сам, как в Терминале.
    pub identity: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub target: Target,
}

/// Состояние сервера для интерфейса.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    #[serde(flatten)]
    pub config: ServerConfig,
    /// `checking` — ещё не опрашивали, `online`, `down` — не отвечает,
    /// `error` — отвечает, но не пускает, `noNetwork` — нет сети на самом Mac.
    pub status: String,
    pub error: Option<String>,
    /// Неудачных опросов подряд.
    pub failures: u32,
    pub stats: Option<Stats>,
    /// Сколько миллисекунд занял опрос (без секунды паузы для замера CPU).
    pub latency_ms: Option<u64>,
    /// Когда последний раз ответил (секунды Unix).
    pub last_ok: Option<u64>,
    /// С какого момента не отвечает.
    pub down_since: Option<u64>,
    /// Опрос идёт прямо сейчас.
    pub busy: bool,
}

impl Server {
    fn new(config: ServerConfig) -> Self {
        Self {
            config,
            status: "checking".into(),
            error: None,
            failures: 0,
            stats: None,
            latency_ms: None,
            last_ok: None,
            down_since: None,
            busy: false,
        }
    }
}

/// Что изменилось после опроса и о чём надо сказать пользователю.
#[derive(Debug, PartialEq)]
enum Alert {
    Down(String),
    Back,
}

/// Применяет результат опроса к серверу.
fn apply(s: &mut Server, result: Result<(Stats, u64), Failure>, network_up: bool, now: u64) -> Option<Alert> {
    let was_down = s.status == "down";
    match result {
        Ok((stats, latency)) => {
            s.status = "online".into();
            s.error = None;
            s.failures = 0;
            s.stats = Some(stats);
            s.latency_ms = Some(latency);
            s.last_ok = Some(now);
            s.down_since = None;
            was_down.then_some(Alert::Back)
        }
        Err(_) if !network_up => {
            // Mac без сети — серверы тут ни при чём, счётчик не трогаем.
            s.status = "noNetwork".into();
            s.error = Some("Нет подключения к интернету на этом Mac".into());
            None
        }
        Err(Failure::Error(msg)) => {
            s.status = "error".into();
            s.error = Some(msg);
            s.failures = 0;
            s.down_since = None;
            None
        }
        Err(Failure::Unreachable(msg)) => {
            s.failures += 1;
            s.down_since.get_or_insert(now);
            s.error = Some(msg.clone());
            if s.failures >= FAILURES_TO_ALERT {
                s.status = "down".into();
                // Сервер, который ни разу не отвечал, — скорее опечатка в адресе, чем авария.
                (!was_down && s.last_ok.is_some()).then_some(Alert::Down(msg))
            } else {
                if s.status == "checking" || s.status == "noNetwork" {
                    s.status = "down".into();
                }
                None
            }
        }
    }
}

struct Store {
    path: Option<PathBuf>,
    servers: Mutex<Vec<Server>>,
}

impl Store {
    fn load(path: Option<PathBuf>) -> Self {
        let configs: Vec<ServerConfig> = path
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, servers: Mutex::new(configs.into_iter().map(Server::new).collect()) }
    }

    fn save(&self, servers: &[Server]) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let configs: Vec<&ServerConfig> = servers.iter().map(|s| &s.config).collect();
        if let Ok(json) = serde_json::to_vec_pretty(&configs) {
            let _ = fs::write(path, json);
        }
    }

    fn list(&self) -> Vec<Server> {
        self.servers.lock().unwrap().clone()
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Куда сообщать о переменах: окну — событием и уведомлением macOS, панели — строкой в stdout.
pub trait Events: Send + Sync {
    /// Список или состояние серверов изменились.
    fn changed(&self, servers: &[Server]);
    /// Сервер упал или вернулся.
    fn alert(&self, title: &str, body: &str);
}

/// Список серверов, их опрос и правка.
pub struct Monitor {
    store: Store,
    events: Box<dyn Events>,
}

fn fetch(target: &Target) -> Result<(Stats, u64), Failure> {
    let (out, elapsed) = ssh::run(target, stats::SCRIPT)?;
    let stats = stats::parse(&out)
        .ok_or_else(|| Failure::Error("Сервер ответил, но это не похоже на Linux: нет /proc/meminfo".into()))?;
    // Секунду скрипт спит, чтобы замерить CPU, — в задержку её не считаем.
    Ok((stats, elapsed.as_millis().saturating_sub(1000) as u64))
}

impl Monitor {
    pub fn new(path: Option<PathBuf>, events: Box<dyn Events>) -> Self {
        Self { store: Store::load(path), events }
    }

    pub fn list(&self) -> Vec<Server> {
        self.store.list()
    }

    fn emit(&self) {
        let list = self.store.list();
        DOWN.store(list.iter().filter(|s| s.status == "down").count(), Ordering::Relaxed);
        self.events.changed(&list);
    }

    /// Опрашивает один сервер. Пока опрос идёт, второй для того же сервера не запускаем.
    pub fn poll(&self, id: &str) {
        let (target, name) = {
            let mut servers = self.store.servers.lock().unwrap();
            let Some(s) = servers.iter_mut().find(|s| s.config.id == id && !s.busy) else { return };
            s.busy = true;
            (s.config.target.clone(), s.config.name.clone())
        };
        self.emit();

        let result = fetch(&target);
        let network_up = result.is_ok() || ssh::local_network_up();

        let alert = {
            let mut servers = self.store.servers.lock().unwrap();
            // Сервер могли удалить, пока шёл опрос.
            let Some(s) = servers.iter_mut().find(|s| s.config.id == id) else { return };
            s.busy = false;
            apply(s, result, network_up, now())
        };
        self.emit();
        match alert {
            Some(Alert::Down(reason)) => self.events.alert(&format!("{name} не отвечает"), &reason),
            Some(Alert::Back) => self.events.alert(&format!("{name} снова в строю"), "Сервер отвечает по SSH"),
            None => {}
        }
    }

    pub fn poll_all(&self) {
        let ids: Vec<String> = self.store.list().into_iter().map(|s| s.config.id).collect();
        thread::scope(|scope| {
            for id in &ids {
                scope.spawn(move || self.poll(id));
            }
        });
    }

    /// Добавляет сервер (без `id`) или меняет существующий. Возвращает его `id`;
    /// опросить его сразу — забота вызывающего, чтобы не держать его ждать ssh.
    pub fn save(&self, id: Option<String>, name: String, target: Target) -> Result<String, String> {
        let target = validate(target)?;
        let name = clean(Some(name)).unwrap_or_else(|| target.host.clone());
        let id = {
            let mut servers = self.store.servers.lock().unwrap();
            let id = match id.and_then(|id| servers.iter().position(|s| s.config.id == id)) {
                Some(i) => {
                    let s = &mut servers[i];
                    let changed = s.config.target != target;
                    s.config.name = name;
                    s.config.target = target;
                    if changed {
                        // Новый адрес — начинаем с чистого листа.
                        *s = Server::new(s.config.clone());
                    }
                    s.config.id.clone()
                }
                None => {
                    let config = ServerConfig { id: new_id(), name, target };
                    servers.push(Server::new(config));
                    servers.last().unwrap().config.id.clone()
                }
            };
            self.store.save(&servers);
            id
        };
        self.emit();
        Ok(id)
    }

    pub fn remove(&self, id: &str) {
        {
            let mut servers = self.store.servers.lock().unwrap();
            servers.retain(|s| s.config.id != id);
            self.store.save(&servers);
        }
        self.emit();
    }

    /// Перечитывает `servers.json`: его могло поменять окно, пока панель открыта.
    /// Состояние уже известных серверов сохраняется.
    pub fn reload(&self) {
        let fresh = Store::load(self.store.path.clone()).list();
        {
            let mut servers = self.store.servers.lock().unwrap();
            let mut next = Vec::with_capacity(fresh.len());
            for f in fresh {
                match servers.iter().position(|s| s.config.id == f.config.id && s.config.target == f.config.target) {
                    Some(i) => {
                        let mut s = servers.swap_remove(i);
                        s.config.name = f.config.name;
                        next.push(s);
                    }
                    None => next.push(f),
                }
            }
            *servers = next;
        }
        self.emit();
    }
}

// ---------- Правка списка ----------

fn clean(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Проверяет то, что пришло из формы. Адрес и пользователь уходят в ssh отдельными
/// аргументами, но «-» в начале и пробелы всё равно не пропускаем.
fn validate(mut t: Target) -> Result<Target, String> {
    t.host = t.host.trim().to_string();
    t.user = clean(t.user);
    t.identity = clean(t.identity);
    let bad = |s: &str| s.starts_with('-') || s.chars().any(|c| c.is_whitespace() || c.is_control());
    if t.host.is_empty() {
        return Err("Укажи адрес сервера или алиас из ~/.ssh/config".into());
    }
    if bad(&t.host) || t.user.as_deref().is_some_and(bad) {
        return Err("В адресе и имени пользователя не должно быть пробелов и «-» в начале".into());
    }
    if let Some(key) = &t.identity {
        let path = PathBuf::from(key);
        if !path.is_file() {
            return Err(format!("Ключ не найден: {key}"));
        }
    }
    if t.port == Some(0) {
        t.port = None;
    }
    Ok(t)
}

fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSetup {
    /// Хосты из `~/.ssh/config`, которых ещё нет в списке.
    pub hosts: Vec<ConfigHost>,
    /// Приватные ключи из `~/.ssh`.
    pub keys: Vec<String>,
}

pub fn ssh_setup() -> SshSetup {
    let dir = ssh::home().join(".ssh");
    SshSetup { hosts: ssh::config_hosts(&dir), keys: ssh::keys(&dir) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server::new(ServerConfig {
            id: "1".into(),
            name: "vps".into(),
            target: Target { host: "vps".into(), user: None, port: None, identity: None },
        })
    }

    fn ok() -> Result<(Stats, u64), Failure> {
        let out = "@@mem\nMemTotal: 1000 kB\nMemAvailable: 500 kB\n@@stat1\ncpu 1 1 1 1\n@@stat2\ncpu 2 2 2 2\n";
        Ok((stats::parse(out).unwrap(), 40))
    }

    fn down() -> Result<(Stats, u64), Failure> {
        Err(Failure::Unreachable("Сервер не отвечает".into()))
    }

    #[test]
    fn alerts_after_two_failures_and_on_recovery() {
        let mut s = server();
        assert_eq!(apply(&mut s, ok(), true, 100), None);
        assert_eq!(s.status, "online");
        // Один сбой — ещё не авария.
        assert_eq!(apply(&mut s, down(), true, 130), None);
        assert_eq!(s.status, "online");
        assert_eq!(apply(&mut s, down(), true, 160), Some(Alert::Down("Сервер не отвечает".into())));
        assert_eq!(s.status, "down");
        assert_eq!(s.down_since, Some(130));
        // Дальше не повторяем.
        assert_eq!(apply(&mut s, down(), true, 190), None);
        assert_eq!(apply(&mut s, ok(), true, 220), Some(Alert::Back));
        assert_eq!(s.down_since, None);
    }

    #[test]
    fn no_alert_without_local_network() {
        let mut s = server();
        apply(&mut s, ok(), true, 100);
        for t in [130, 160, 190] {
            assert_eq!(apply(&mut s, down(), false, t), None);
        }
        assert_eq!(s.status, "noNetwork");
        assert_eq!(s.failures, 0);
    }

    #[test]
    fn never_seen_server_does_not_alert() {
        let mut s = server();
        assert_eq!(apply(&mut s, down(), true, 100), None);
        assert_eq!(s.status, "down");
        assert_eq!(apply(&mut s, down(), true, 130), None);
    }

    #[test]
    fn auth_error_is_not_down() {
        let mut s = server();
        apply(&mut s, ok(), true, 100);
        assert_eq!(apply(&mut s, Err(Failure::Error("Ключ не подошёл".into())), true, 130), None);
        assert_eq!(s.status, "error");
    }

    #[test]
    fn validates_input() {
        let t = |host: &str, user: Option<&str>| Target { host: host.into(), user: user.map(Into::into), port: Some(0), identity: Some(" ".into()) };
        let v = validate(t(" root@1.2.3.4 ", None)).unwrap();
        assert_eq!(v.host, "root@1.2.3.4");
        assert_eq!(v.port, None);
        assert_eq!(v.identity, None);
        assert!(validate(t("", None)).is_err());
        assert!(validate(t("-oProxyCommand=x", None)).is_err());
        assert!(validate(t("host", Some("a b"))).is_err());
    }
}

/// Живая проверка на настоящем сервере: `SYSMON_TEST_HOST=user@host cargo test -- --ignored live`.
#[cfg(test)]
#[test]
#[ignore]
fn live_ssh() {
    let host = std::env::var("SYSMON_TEST_HOST").expect("SYSMON_TEST_HOST");
    let port = std::env::var("SYSMON_TEST_PORT").ok().and_then(|p| p.parse().ok());
    let target = Target { host, user: None, port, identity: None };
    let (stats, latency) = fetch(&target).unwrap();
    println!("{latency} мс\n{stats:#?}");
    assert!(stats.memory.total > 0);
    assert!(!stats.disks.is_empty());
}
