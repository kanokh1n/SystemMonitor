//! Docker: список контейнеров, их нагрузка и управление (start/stop/restart, логи).
//!
//! [`Poller`] держит подключение между опросами. Если Docker не запущен,
//! снимок говорит об этом, а следующий опрос пробует подключиться снова.

use std::time::Duration;

use bollard::models::{ContainerStatsResponse, ContainerSummary};
use bollard::query_parameters::{
    ListContainersOptionsBuilder, LogsOptionsBuilder, StatsOptionsBuilder,
};
use bollard::{Docker, API_DEFAULT_VERSION};
use futures_util::future::join_all;
use futures_util::StreamExt;
use serde::Serialize;

pub const POLL: Duration = Duration::from_secs(3);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DockerSnapshot {
    pub available: bool,
    pub error: Option<String>,
    pub containers: Vec<ContainerInfo>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: Vec<String>,
    pub cpu_percent: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_limit: Option<u64>,
}

/// Docker Desktop, OrbStack и Colima кладут сокет в разные места.
/// Сначала уважаем DOCKER_HOST, потом перебираем известные пути.
pub async fn connect() -> Result<Docker, String> {
    if std::env::var("DOCKER_HOST").is_ok() {
        if let Ok(docker) = Docker::connect_with_defaults() {
            if docker.ping().await.is_ok() {
                return Ok(docker);
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        "/var/run/docker.sock".to_string(),
        format!("{home}/.docker/run/docker.sock"),
        format!("{home}/.orbstack/run/docker.sock"),
        format!("{home}/.colima/default/docker.sock"),
        format!("{home}/.rd/docker.sock"),
    ];
    for path in candidates.iter().filter(|p| std::path::Path::new(p).exists()) {
        if let Ok(docker) = Docker::connect_with_socket(path, 10, API_DEFAULT_VERSION) {
            if docker.ping().await.is_ok() {
                return Ok(docker);
            }
        }
    }
    Err("Docker не запущен или не найден. Запусти Docker Desktop, OrbStack или Colima.".into())
}

/// CPU% считается так же, как в `docker stats`: доля времени контейнера
/// от общего времени системы между двумя замерами, умноженная на число ядер.
fn cpu_percent(stats: &ContainerStatsResponse) -> Option<f64> {
    let cpu = stats.cpu_stats.as_ref()?;
    let pre = stats.precpu_stats.as_ref()?;
    let total = cpu.cpu_usage.as_ref()?.total_usage? as f64;
    let pre_total = pre.cpu_usage.as_ref()?.total_usage? as f64;
    let system = cpu.system_cpu_usage? as f64;
    let pre_system = pre.system_cpu_usage? as f64;
    let cpus = cpu.online_cpus.unwrap_or(1) as f64;
    let (cpu_delta, system_delta) = (total - pre_total, system - pre_system);
    (system_delta > 0.0 && cpu_delta >= 0.0).then(|| cpu_delta / system_delta * cpus * 100.0)
}

async fn container_info(docker: &Docker, c: ContainerSummary) -> ContainerInfo {
    let id = c.id.unwrap_or_default();
    let state = c.state.map(|s| s.to_string()).unwrap_or_default();
    let name = c
        .names
        .and_then(|n| n.into_iter().next())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_else(|| id.chars().take(12).collect());
    let ports = c
        .ports
        .unwrap_or_default()
        .into_iter()
        .map(|p| match p.public_port {
            Some(public) => format!("{public}→{}", p.private_port),
            None => p.private_port.to_string(),
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut info = ContainerInfo {
        id: id.clone(),
        name,
        image: c.image.unwrap_or_default(),
        state: state.clone(),
        status: c.status.unwrap_or_default(),
        ports,
        cpu_percent: None,
        memory_used: None,
        memory_limit: None,
    };

    if state == "running" {
        // stream=false и one_shot=false: Docker сам сделает два замера (~1 c),
        // чтобы можно было посчитать CPU.
        let options = StatsOptionsBuilder::new().stream(false).one_shot(false).build();
        if let Some(Ok(stats)) = docker.stats(&id, Some(options)).next().await {
            info.cpu_percent = cpu_percent(&stats);
            if let Some(mem) = stats.memory_stats {
                info.memory_used = mem.usage;
                info.memory_limit = mem.limit;
            }
        }
    }
    info
}

async fn collect(docker: &Docker) -> Result<Vec<ContainerInfo>, bollard::errors::Error> {
    let options = ListContainersOptionsBuilder::new().all(true).build();
    let containers = docker.list_containers(Some(options)).await?;
    // Статистику всех контейнеров запрашиваем параллельно.
    let mut infos = join_all(containers.into_iter().map(|c| container_info(docker, c))).await;
    infos.sort_by(|a, b| (a.state != "running", &a.name).cmp(&(b.state != "running", &b.name)));
    Ok(infos)
}

/// Опрос Docker с подключением, которое переживает круги опроса.
#[derive(Default)]
pub struct Poller {
    docker: Option<Docker>,
}

impl Poller {
    pub async fn snapshot(&mut self) -> DockerSnapshot {
        if self.docker.is_none() {
            self.docker = connect().await.ok();
        }
        match &self.docker {
            Some(d) => match collect(d).await {
                Ok(containers) => DockerSnapshot { available: true, error: None, containers },
                Err(e) => {
                    self.docker = None;
                    DockerSnapshot { available: false, error: Some(e.to_string()), containers: vec![] }
                }
            },
            None => DockerSnapshot {
                available: false,
                error: connect().await.err(),
                containers: vec![],
            },
        }
    }
}

pub async fn container_action(id: String, action: String) -> Result<(), String> {
    let docker = connect().await?;
    let result = match action.as_str() {
        "start" => docker.start_container(&id, None).await,
        "stop" => docker.stop_container(&id, None).await,
        "restart" => docker.restart_container(&id, None).await,
        other => return Err(format!("Неизвестное действие: {other}")),
    };
    result.map_err(|e| e.to_string())
}

pub async fn container_logs(id: String) -> Result<String, String> {
    let docker = connect().await?;
    let options = LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .timestamps(false)
        .tail("300")
        .build();
    let mut stream = docker.logs(&id, Some(options));
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(line) => out.push_str(&line.to_string()),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(out)
}
