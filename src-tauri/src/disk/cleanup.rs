//! Быстрые чистки: то, что можно удалить почти не глядя.
//!
//! Всё, кроме Docker, уходит в Корзину. Docker хранит образы внутри своей
//! виртуальной машины, поэтому их чистит сам Docker (`prune`), и это навсегда.
//! Тома Docker (там живут базы данных) не трогаем никогда.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use bollard::query_parameters::{
    ListContainersOptionsBuilder, ListImagesOptionsBuilder, PruneBuildOptionsBuilder, PruneImagesOptionsBuilder,
};
use rayon::prelude::*;
use serde::Serialize;

use super::guard::{self, TrashReport};
use super::{du, home, now, scan, Item};

/// Проект без изменений дольше этого считается заброшенным.
const STALE_PROJECT_DAYS: i64 = 30;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: &'static str,
    pub title: &'static str,
    pub hint: &'static str,
    pub items: Vec<Item>,
}

/// Кэши инструментов разработки: путь от домашней папки, подпись, отмечать ли.
const DEV_CACHES: &[(&str, &str, bool)] = &[
    ("Library/Developer/Xcode/DerivedData", "Xcode: промежуточные файлы сборки", true),
    ("Library/Developer/Xcode/iOS DeviceSupport", "Xcode: символы для подключавшихся iPhone", false),
    ("Library/Developer/CoreSimulator/Caches", "Симуляторы iOS: кэш", true),
    ("Library/Caches/go-build", "Go: кэш сборки", true),
    ("go/pkg/mod", "Go: скачанные модули (скачаются заново при сборке)", false),
    (".npm/_cacache", "npm: кэш пакетов", true),
    ("Library/Caches/Yarn", "Yarn: кэш пакетов", true),
    ("Library/pnpm/store", "pnpm: хранилище пакетов", false),
    (".cargo/registry/cache", "Cargo: скачанные архивы крейтов", true),
    (".cargo/registry/src", "Cargo: распакованные исходники крейтов", true),
    (".cargo/git/checkouts", "Cargo: git‑зависимости", true),
    ("Library/Caches/Homebrew", "Homebrew: скачанные пакеты", true),
    ("Library/Caches/pip", "pip: кэш пакетов", true),
    ("Library/Caches/ms-playwright", "Playwright: скачанные браузеры", false),
    (".gradle/caches", "Gradle: кэш", false),
    (".m2/repository", "Maven: локальный репозиторий", false),
];

/// Эти папки из ~/Library/Caches показываем в других разделах, не в «Кэшах программ».
const CACHES_ELSEWHERE: &[&str] = &["go-build", "Yarn", "Homebrew", "pip", "ms-playwright", "JetBrains"];

fn app_caches(home: &Path) -> Vec<Item> {
    let dir = home.join("Library/Caches");
    let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
    let paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            // Кэши самой macOS защищены системой, их всё равно не дадут удалить.
            !name.starts_with("com.apple.") && !CACHES_ELSEWHERE.contains(&name.as_ref())
        })
        .collect();
    let mut items: Vec<Item> = paths
        .par_iter()
        .map(|p| Item::measure(p, p.file_name().unwrap_or_default().to_string_lossy(), true))
        .filter(|i| i.size > 0)
        .collect();
    items.sort_by(|a, b| b.size.cmp(&a.size));
    items
}

fn dev_caches(home: &Path) -> Vec<Item> {
    DEV_CACHES
        .par_iter()
        .map(|(rel, label, selected)| (home.join(rel), label, selected))
        .filter(|(p, ..)| p.exists())
        .map(|(p, label, selected)| Item::measure(&p, *label, *selected))
        .filter(|i| i.size > 0)
        .collect()
}

/// `GoLand2025.2` → ("GoLand", (2025, 2)).
fn jetbrains_version(name: &str) -> Option<(&str, (u32, u32))> {
    let digits = name.find(|c: char| c.is_ascii_digit())?;
    let (product, version) = name.split_at(digits);
    let (year, minor) = version.split_once('.')?;
    if product.is_empty() || !product.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((product, (year.parse().ok()?, minor.parse().ok()?)))
}

/// Старые версии IDE JetBrains: при обновлении новая версия копирует настройки
/// в свою папку, а старые папки с индексами и кэшами остаются навсегда.
fn jetbrains(home: &Path) -> Vec<Item> {
    let places = [("Library/Application Support/JetBrains", "настройки"), ("Library/Caches/JetBrains", "кэш"), ("Library/Logs/JetBrains", "логи")];
    let mut found: Vec<(String, (u32, u32), PathBuf, &str)> = vec![];
    for (rel, label) in places {
        let Ok(entries) = std::fs::read_dir(home.join(rel)) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((product, version)) = jetbrains_version(&name) {
                found.push((product.to_string(), version, entry.path(), label));
            }
        }
    }
    let mut newest: HashMap<&str, (u32, u32)> = HashMap::new();
    for (product, version, ..) in &found {
        let v = newest.entry(product).or_default();
        *v = (*v).max(*version);
    }
    let old: Vec<_> = found.iter().filter(|(product, version, ..)| newest[product.as_str()] != *version).collect();
    let mut items: Vec<Item> = old
        .par_iter()
        .map(|(product, (y, m), path, label)| Item::measure(path, format!("{product} {y}.{m} · {label}"), true))
        .collect();
    items.sort_by(|a, b| b.size.cmp(&a.size));
    items
}

/// Что нашлось при обходе домашней папки.
#[derive(Default)]
struct Found {
    /// (node_modules или target, папка проекта)
    build_dirs: Vec<(PathBuf, PathBuf)>,
    heap_dumps: Vec<PathBuf>,
}

/// Ищет node_modules, target у Rust‑проектов и дампы памяти Java (.hprof).
/// Не заходит в ~/Library, скрытые папки и внутрь найденного.
fn walk_projects(dir: &Path, depth: u32, out: &mut Found) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut subdirs = vec![];
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_file() && name.ends_with(".hprof") {
            out.heap_dumps.push(path);
        } else if ft.is_dir() {
            if name == "node_modules" || (name == "target" && dir.join("Cargo.toml").exists()) {
                out.build_dirs.push((path, dir.to_path_buf()));
            } else if !name.starts_with('.')
                && !(depth == 0 && matches!(name.as_str(), "Library" | "Applications" | "Movies" | "Music" | "Pictures"))
                && !name.ends_with(".app")
                && !name.ends_with(".photoslibrary")
            {
                subdirs.push(path);
            }
        }
    }
    let found: Vec<Found> = subdirs
        .par_iter()
        .map(|p| {
            let mut f = Found::default();
            walk_projects(p, depth + 1, &mut f);
            f
        })
        .collect();
    for f in found {
        out.build_dirs.extend(f.build_dirs);
        out.heap_dumps.extend(f.heap_dumps);
    }
}

/// Когда в проекте последний раз что-то меняли: самое свежее время среди
/// файлов в корне проекта (сами node_modules и target не считаем).
fn project_activity(project: &Path) -> i64 {
    let Ok(entries) = std::fs::read_dir(project) else { return 0 };
    entries
        .flatten()
        .filter(|e| !matches!(e.file_name().to_str(), Some("node_modules" | "target" | ".DS_Store")))
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.mtime())
        .max()
        .unwrap_or(0)
}

fn relative(home: &Path, p: &Path) -> String {
    p.strip_prefix(home).map(|r| format!("~/{}", r.display())).unwrap_or_else(|_| p.display().to_string())
}

/// Все быстрые чистки, кроме Docker. Считается несколько секунд.
#[tauri::command]
pub async fn disk_cleanup_scan() -> Vec<Category> {
    tauri::async_runtime::spawn_blocking(|| {
        let home = home();
        let mut found = Found::default();
        walk_projects(&home, 0, &mut found);

        let cutoff = now() - STALE_PROJECT_DAYS * 86_400;
        let mut projects: Vec<Item> = found
            .build_dirs
            .par_iter()
            .map(|(dir, project)| {
                let active = project_activity(project);
                let mut item = Item::measure(dir, relative(&home, project), active < cutoff);
                item.newest = active;
                item
            })
            .filter(|i| i.size > 0)
            .collect();
        projects.sort_by(|a, b| b.size.cmp(&a.size));

        let mut logs: Vec<Item> = std::fs::read_dir(home.join("Library/Logs"))
            .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| !p.ends_with("JetBrains")).collect::<Vec<_>>())
            .unwrap_or_default()
            .par_iter()
            .map(|p| Item::measure(p, p.file_name().unwrap_or_default().to_string_lossy(), true))
            .chain(found.heap_dumps.par_iter().map(|p| Item::measure(p, relative(&home, p), true)))
            .filter(|i| i.size > 0)
            .collect();
        logs.sort_by(|a, b| b.size.cmp(&a.size));

        vec![
            Category {
                id: "caches",
                title: "Кэши программ",
                hint: "~/Library/Caches. Программы создадут кэш заново, первый запуск может быть чуть медленнее. Лучше закрыть их перед очисткой.",
                items: app_caches(&home),
            },
            Category {
                id: "dev",
                title: "Кэши разработчика",
                hint: "Go, npm, Cargo, Homebrew, Xcode. Всё скачается или соберётся заново, когда понадобится.",
                items: dev_caches(&home),
            },
            Category {
                id: "projects",
                title: "node_modules и target в проектах",
                hint: "Отмечены проекты, которые не менялись больше месяца. Вернуть: npm install или cargo build.",
                items: projects,
            },
            Category {
                id: "jetbrains",
                title: "Старые версии IDE JetBrains",
                hint: "Папки предыдущих версий GoLand, IntelliJ и других. Текущая версия не трогается.",
                items: jetbrains(&home),
            },
            Category {
                id: "logs",
                title: "Логи и дампы памяти",
                hint: "~/Library/Logs и файлы .hprof (дампы памяти Java, обычно по гигабайту и больше).",
                items: logs,
            },
        ]
    })
    .await
    .unwrap_or_default()
}

/// Переместить в Корзину и убрать удалённое из результатов скана.
#[tauri::command]
pub async fn disk_trash(paths: Vec<String>) -> TrashReport {
    let report = tauri::async_runtime::spawn_blocking(move || guard::to_trash(&paths)).await.unwrap_or_default();
    scan::forget(&report.trashed);
    report
}

/// Сколько лежит в Корзине. Без полного доступа к диску macOS не даёт её прочитать.
#[tauri::command]
pub async fn disk_trash_size() -> Option<u64> {
    let trash = home().join(".Trash");
    std::fs::read_dir(&trash).ok()?;
    tauri::async_runtime::spawn_blocking(move || du(&trash).0).await.ok()
}

/// Очистить Корзину через Finder. Это навсегда, интерфейс спрашивает подтверждение.
#[tauri::command]
pub async fn disk_empty_trash() -> Result<(), String> {
    let out = Command::new("osascript")
        .args(["-e", "tell application \"Finder\" to empty trash"])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("Finder не очистил Корзину: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

// ---------- Docker ----------

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DockerUsage {
    pub available: bool,
    pub error: Option<String>,
    /// Остановленные контейнеры: сколько и сколько места.
    pub stopped_containers: (u64, u64),
    /// Образы, которые не использует ни один контейнер. Размер приблизительный:
    /// общие слои у разных образов считаются несколько раз.
    pub unused_images: (u64, u64),
    pub build_cache: Option<u64>,
    /// Тома только показываем, удалять их отсюда нельзя.
    pub volumes: (u64, u64),
}

#[tauri::command]
pub async fn disk_docker_usage() -> DockerUsage {
    let docker = match crate::docker::connect().await {
        Ok(d) => d,
        Err(e) => return DockerUsage { error: Some(e), ..Default::default() },
    };
    let containers = docker
        .list_containers(Some(ListContainersOptionsBuilder::new().all(true).size(true).build()))
        .await
        .unwrap_or_default();
    let used_images: HashSet<String> = containers.iter().filter_map(|c| c.image_id.clone()).collect();
    let stopped = containers.iter().filter(|c| {
        c.state.as_ref().is_some_and(|s| matches!(s.to_string().as_str(), "exited" | "created" | "dead"))
    });
    let stopped_containers = stopped.fold((0, 0), |(n, s), c| (n + 1, s + c.size_rw.unwrap_or(0).max(0) as u64));

    let images = docker.list_images(Some(ListImagesOptionsBuilder::new().build())).await.unwrap_or_default();
    let unused_images = images
        .iter()
        .filter(|i| !used_images.contains(&i.id))
        .fold((0, 0), |(n, s), i| (n + 1, s + i.size.max(0) as u64));

    let df = docker.df(None).await.ok();
    let build_cache = df
        .as_ref()
        .and_then(|d| d.build_cache_usage.as_ref())
        .and_then(|b| b.reclaimable.or(b.total_size))
        .map(|v| v.max(0) as u64);
    let volumes = df
        .as_ref()
        .and_then(|d| d.volume_usage.as_ref())
        .map(|v| (v.total_count.unwrap_or(0).max(0) as u64, v.total_size.unwrap_or(0).max(0) as u64))
        .unwrap_or_default();

    DockerUsage { available: true, error: None, stopped_containers, unused_images, build_cache, volumes }
}

/// `targets`: какие из "containers", "images", "buildCache" чистить. Возвращает освобождённые байты.
#[tauri::command]
pub async fn disk_docker_prune(targets: Vec<String>) -> Result<u64, String> {
    let docker = crate::docker::connect().await?;
    let mut freed = 0i64;
    // Сначала контейнеры: тогда их образы тоже станут неиспользуемыми.
    if targets.iter().any(|t| t == "containers") {
        let r = docker.prune_containers(None).await.map_err(|e| e.to_string())?;
        freed += r.space_reclaimed.unwrap_or(0);
    }
    if targets.iter().any(|t| t == "images") {
        // dangling=false — все неиспользуемые образы, а не только безымянные (как `docker image prune -a`).
        let filters = HashMap::from([("dangling", vec!["false"])]);
        let r = docker
            .prune_images(Some(PruneImagesOptionsBuilder::new().filters(&filters).build()))
            .await
            .map_err(|e| e.to_string())?;
        freed += r.space_reclaimed.unwrap_or(0);
    }
    if targets.iter().any(|t| t == "buildCache") {
        let r = docker
            .prune_build(Some(PruneBuildOptionsBuilder::new().all(true).build()))
            .await
            .map_err(|e| e.to_string())?;
        freed += r.space_reclaimed.unwrap_or(0);
    }
    Ok(freed.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jetbrains_folders() {
        assert_eq!(jetbrains_version("GoLand2025.2"), Some(("GoLand", (2025, 2))));
        assert_eq!(jetbrains_version("IntelliJIdea2024.3"), Some(("IntelliJIdea", (2024, 3))));
        assert_eq!(jetbrains_version("Toolbox"), None);
        assert_eq!(jetbrains_version("consentOptions"), None);
    }

    #[test]
    fn finds_build_dirs() {
        let root = std::env::temp_dir().join(format!("sm-walk-{}", std::process::id()));
        std::fs::create_dir_all(root.join("web/node_modules/left-pad")).unwrap();
        std::fs::create_dir_all(root.join("rust/target/debug")).unwrap();
        std::fs::write(root.join("rust/Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(root.join("other/target")).unwrap();
        std::fs::write(root.join("java_pid1.hprof"), "x").unwrap();
        let mut f = Found::default();
        walk_projects(&root, 1, &mut f);
        let mut dirs: Vec<_> = f.build_dirs.iter().map(|(d, _)| d.strip_prefix(&root).unwrap().to_path_buf()).collect();
        dirs.sort();
        assert_eq!(dirs, vec![PathBuf::from("rust/target"), PathBuf::from("web/node_modules")]);
        assert_eq!(f.heap_dumps.len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
