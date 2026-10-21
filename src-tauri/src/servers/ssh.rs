//! Подключение по SSH через системный `ssh`.
//!
//! Свой SSH‑клиент не пишем: системный уже умеет всё из `~/.ssh/config`
//! (алиасы, ProxyJump, IdentityFile), ключи из `~/.ssh`, ssh‑agent и связку
//! ключей macOS для ключей с паролем. Соединение держим открытым между опросами
//! (ControlMaster), поэтому каждый опрос — это не новый handshake, а одна команда.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::Target;

/// Сколько ждём ответа целиком. Скрипт сам по себе работает около двух секунд.
const TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// Сервер не отвечает: таймаут, порт закрыт, адрес не находится.
    Unreachable(String),
    /// Сервер ответил, но что‑то не так: ключ не подошёл, ключ сервера изменился и т. п.
    Error(String),
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn args(target: &Target) -> Vec<String> {
    let mut args: Vec<String> = [
        "-T",
        // Никогда не спрашивать пароль или подтверждение: окна для ввода у нас нет.
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        // Новый сервер запоминаем сами (как «yes» при первом входе), изменившийся ключ — ошибка.
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ServerAliveInterval=5",
        "-o", "ServerAliveCountMax=3",
        "-o", "ControlMaster=auto",
        // Короткий путь: у unix‑сокета лимит около 100 символов.
        "-o", "ControlPath=/tmp/sysmon-%C",
        "-o", "ControlPersist=120",
        "-o", "LogLevel=ERROR",
    ]
    .map(String::from)
    .to_vec();
    if let Some(port) = target.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    if let Some(user) = target.user.as_deref().filter(|u| !u.is_empty()) {
        args.extend(["-l".into(), user.to_string()]);
    }
    if let Some(key) = target.identity.as_deref().filter(|k| !k.is_empty()) {
        args.extend(["-i".into(), key.to_string(), "-o".into(), "IdentitiesOnly=yes".into()]);
    }
    // `--` — дальше только адрес и команда, даже если адрес вдруг начнётся с «-».
    args.extend(["--".into(), target.host.clone(), "sh -s".into()]);
    args
}

/// Запускает скрипт на сервере. Возвращает вывод и сколько это заняло.
pub fn run(target: &Target, script: &str) -> Result<(String, Duration), Failure> {
    let started = Instant::now();
    let mut child = Command::new("ssh")
        .args(args(target))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::Error(format!("Не удалось запустить ssh: {e}")))?;

    let mut stdin = child.stdin.take().unwrap();
    let script = script.to_string();
    thread::spawn(move || {
        let _ = stdin.write_all(script.as_bytes());
    });
    let read_all = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let stdout = read_all(Box::new(child.stdout.take().unwrap()));
    let stderr = read_all(Box::new(child.stderr.take().unwrap()));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Unreachable("Сервер не ответил за 25 секунд".into()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(Failure::Error(e.to_string())),
        }
    };
    let elapsed = started.elapsed();
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    // 255 — ошибка самого ssh. Иначе это код скрипта, а он нам не важен, если вывод есть.
    if status.code() == Some(255) || (out.trim().is_empty() && !status.success()) {
        return Err(classify(&err));
    }
    Ok((out, elapsed))
}

/// Переводит сообщение ssh в понятное и решает, «лежит» ли сервер.
pub fn classify(stderr: &str) -> Failure {
    let text = stderr.trim();
    let has = |s: &str| text.to_lowercase().contains(&s.to_lowercase());
    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ssh завершился с ошибкой").trim();
    if has("Permission denied") || has("Too many authentication failures") {
        Failure::Error(
            "Ключ не подошёл. Проверь, что публичный ключ есть в ~/.ssh/authorized_keys на сервере. \
             Ключ с паролем добавь в связку: ssh-add --apple-use-keychain ~/.ssh/<ключ>"
                .into(),
        )
    } else if has("REMOTE HOST IDENTIFICATION HAS CHANGED") || has("Host key verification failed") {
        Failure::Error(
            "Ключ сервера изменился (переустановили систему или это другой сервер). \
             Если это ожидаемо, выполни в Терминале: ssh-keygen -R <адрес>"
                .into(),
        )
    } else if has("Could not resolve hostname") || has("nodename nor servname") || has("Name or service not known") {
        Failure::Unreachable("Не удаётся найти адрес сервера (DNS)".into())
    } else if has("Connection refused") {
        Failure::Unreachable("Порт SSH закрыт: сервер отклоняет подключение".into())
    } else if has("timed out") || has("No route to host") || has("Network is unreachable") || has("Host is down") {
        Failure::Unreachable("Сервер не отвечает".into())
    } else if has("Connection reset") || has("Connection closed") || has("closed by remote host") || has("Broken pipe") {
        Failure::Unreachable("Соединение оборвалось".into())
    } else {
        Failure::Error(last.to_string())
    }
}

/// Есть ли у самого Mac сеть. Без неё не считаем серверы упавшими.
pub fn local_network_up() -> bool {
    #[cfg(target_os = "macos")]
    {
        Command::new("route")
            .args(["-n", "get", "default"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(not(target_os = "macos"))]
    true
}

// ---------- ~/.ssh: хосты из config и ключи ----------

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfigHost {
    /// Алиас из строки `Host` — его и передаём в ssh.
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
}

/// Читает `~/.ssh/config` и файлы из его `Include`. Шаблоны вроде `*` и `!host` пропускаем.
pub fn config_hosts(ssh_dir: &Path) -> Vec<ConfigHost> {
    let mut hosts = Vec::new();
    read_config(&ssh_dir.join("config"), ssh_dir, &mut hosts, 0);
    hosts
}

fn read_config(path: &Path, ssh_dir: &Path, hosts: &mut Vec<ConfigHost>, depth: u8) {
    if let Ok(text) = fs::read_to_string(path) {
        parse_config(&text, ssh_dir, hosts, depth);
    }
}

fn parse_config(text: &str, ssh_dir: &Path, hosts: &mut Vec<ConfigHost>, depth: u8) {
    // Хосты текущего блока `Host`: настройки ниже относятся к ним.
    let mut current: Vec<usize> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `Ключ значение` или `Ключ=значение`.
        let (key, value) = line.split_once(|c: char| c.is_whitespace() || c == '=').unwrap_or((line, ""));
        let value = value.trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim().trim_matches('"');
        // Как и в ssh, побеждает первое значение.
        let mut set = |field: fn(&mut ConfigHost) -> &mut Option<String>| {
            for &i in &current {
                field(&mut hosts[i]).get_or_insert_with(|| value.to_string());
            }
        };
        match key.to_lowercase().as_str() {
            "host" => {
                current.clear();
                for alias in value.split_whitespace() {
                    if alias.contains(['*', '?', '!']) || hosts.iter().any(|h| h.alias == alias) {
                        continue;
                    }
                    current.push(hosts.len());
                    hosts.push(ConfigHost { alias: alias.to_string(), ..Default::default() });
                }
            }
            "match" => current.clear(),
            "hostname" => set(|h| &mut h.host_name),
            "user" => set(|h| &mut h.user),
            "port" => {
                for &i in &current {
                    if hosts[i].port.is_none() {
                        hosts[i].port = value.parse().ok();
                    }
                }
            }
            "include" if depth < 4 => {
                for pattern in value.split_whitespace() {
                    for file in expand_include(pattern, ssh_dir) {
                        read_config(&file, ssh_dir, hosts, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `config.d/*`, `~/.ssh/work` → список файлов. Относительные пути — от `~/.ssh`.
fn expand_include(pattern: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => ssh_dir.join(pattern),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return vec![] };
    let Ok(entries) = fs::read_dir(dir) else { return vec![] };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| wildcard(&name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

/// Шаблон с `*` и `?`, как в ssh_config.
fn wildcard(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    go(&pattern.chars().collect::<Vec<_>>(), &text.chars().collect::<Vec<_>>())
}

/// Приватные ключи в `~/.ssh` — те файлы, рядом с которыми лежит `.pub`.
pub fn keys(ssh_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(ssh_dir) else { return vec![] };
    let mut keys: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_none_or(|e| e != "pub"))
        .filter(|p| p.with_extension(p.extension().map_or("pub".into(), |e| format!("{}.pub", e.to_string_lossy()))).exists())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    keys.sort();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_config_with_include() {
        let dir = std::env::temp_dir().join(format!("sysmon-ssh-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("config.d")).unwrap();
        fs::write(
            dir.join("config"),
            "Include config.d/*\n\nHost *\n  ServerAliveInterval 30\n\nHost prod web\n  HostName 1.2.3.4\n  User root\n  Port=2222\n\nHost prod\n  User admin\n",
        )
        .unwrap();
        fs::write(dir.join("config.d/work.conf"), "Host db !bastion\n    HostName \"db.example.com\"\n").unwrap();
        let hosts = config_hosts(&dir);
        let _ = fs::remove_dir_all(&dir);

        let aliases: Vec<_> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["db", "prod", "web"]);
        assert_eq!(hosts[0].host_name.as_deref(), Some("db.example.com"));
        assert_eq!(hosts[1].user.as_deref(), Some("root"));
        assert_eq!(hosts[1].port, Some(2222));
        assert_eq!(hosts[2].host_name.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*", "a.conf"));
        assert!(wildcard("*.conf", "a.conf"));
        assert!(!wildcard("*.conf", "a.txt"));
        assert!(wildcard("w?rk", "work"));
    }

    #[test]
    fn classifies_errors() {
        assert!(matches!(classify("root@1.2.3.4: Permission denied (publickey)."), Failure::Error(_)));
        assert!(matches!(classify("ssh: connect to host 1.2.3.4 port 22: Operation timed out"), Failure::Unreachable(_)));
        assert!(matches!(classify("ssh: connect to host 1.2.3.4 port 22: Connection refused"), Failure::Unreachable(_)));
        assert!(matches!(classify("ssh: Could not resolve hostname foo: nodename nor servname provided"), Failure::Unreachable(_)));
        assert_eq!(classify("weird\nlast line\n"), Failure::Error("last line".into()));
    }

    #[test]
    fn option_injection_is_blocked() {
        let t = Target { host: "-oProxyCommand=evil".into(), user: None, port: None, identity: None };
        let a = args(&t);
        let dash = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a[dash + 1], "-oProxyCommand=evil");
    }
}
