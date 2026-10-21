//! Программы и их «хвосты», как в AppCleaner.
//!
//! Программа на маке — это папка `.app`. Но после первого запуска она
//! раскладывает свои данные по ~/Library: кэши, настройки, контейнер песочницы.
//! Всё это названо по её bundle id (например, `com.spotify.client`), по нему и ищем.

use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use serde::Serialize;

use super::{du, guard, home, Item};

pub struct BundleInfo {
    pub bundle_id: String,
    pub name: Option<String>,
    pub version: Option<String>,
}

/// Читает Contents/Info.plist программы.
pub fn read_info(app: &Path) -> Option<BundleInfo> {
    let value = plist::Value::from_file(app.join("Contents/Info.plist")).ok()?;
    let dict = value.as_dictionary()?;
    let get = |key: &str| dict.get(key).and_then(|v| v.as_string()).map(str::to_string);
    Some(BundleInfo {
        bundle_id: get("CFBundleIdentifier")?,
        name: get("CFBundleDisplayName").or_else(|| get("CFBundleName")),
        version: get("CFBundleShortVersionString").or_else(|| get("CFBundleVersion")),
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub path: String,
    pub bundle_id: String,
    pub version: Option<String>,
    pub size: u64,
    /// Когда программу последний раз открывали (по данным Spotlight), unix‑время.
    pub last_used: Option<i64>,
    pub protected: Option<String>,
}

/// Все .app в /Applications и ~/Applications (и на уровень глубже, вроде Setapp).
fn find_apps() -> Vec<PathBuf> {
    let mut out = vec![];
    for dir in [PathBuf::from("/Applications"), home().join("Applications")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "app") {
                out.push(path);
            } else if path.is_dir() && !path.is_symlink() {
                if let Ok(inner) = std::fs::read_dir(&path) {
                    out.extend(inner.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "app")));
                }
            }
        }
    }
    out
}

/// Дата последнего открытия из Spotlight для всех программ одним вызовом `mdls`.
/// В режиме `-raw` значения разделены нулевым байтом, пустое значение — `(null)`.
fn last_used(apps: &[PathBuf]) -> Vec<Option<i64>> {
    let out = Command::new("mdls").args(["-raw", "-name", "kMDItemLastUsedDate"]).args(apps).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut values: Vec<Option<i64>> = text.split('\0').map(parse_mdls_date).collect();
    values.resize(apps.len(), None);
    values
}

/// `2026-10-20 08:12:33 +0000` → unix‑время. Spotlight всегда отдаёт UTC.
fn parse_mdls_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(' ')?;
    let time = rest.split(' ').next()?;
    let d: Vec<i64> = date.split('-').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let t: Vec<i64> = time.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if d.len() != 3 || t.len() != 3 {
        return None;
    }
    Some(days_from_civil(d[0], d[1], d[2]) * 86_400 + t[0] * 3600 + t[1] * 60 + t[2])
}

/// Число дней от 1970-01-01 (алгоритм Говарда Хиннанта).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Список программ с размерами. Считается несколько секунд (Xcode — это
/// сотни тысяч файлов), поэтому команда асинхронная.
#[tauri::command]
pub async fn disk_apps() -> Vec<AppInfo> {
    tauri::async_runtime::spawn_blocking(|| {
        let paths = find_apps();
        let used = last_used(&paths);
        let mut apps: Vec<AppInfo> = paths
            .par_iter()
            .zip(used.into_par_iter())
            .filter_map(|(path, last_used)| {
                let info = read_info(path)?;
                Some(AppInfo {
                    name: path.file_stem()?.to_string_lossy().into_owned(),
                    path: path.to_string_lossy().into_owned(),
                    bundle_id: info.bundle_id,
                    version: info.version,
                    size: du(path).0,
                    last_used,
                    protected: guard::check(path).err(),
                })
            })
            .collect();
        apps.sort_by(|a, b| b.size.cmp(&a.size));
        apps
    })
    .await
    .unwrap_or_default()
}

// ---------- «Хвосты» в ~/Library ----------

/// Где программы оставляют данные. Второе — подпись для интерфейса.
const PLACES: &[(&str, &str)] = &[
    ("Application Support", "данные"),
    ("Caches", "кэш"),
    ("Preferences", "настройки"),
    ("Preferences/ByHost", "настройки"),
    ("Containers", "контейнер песочницы"),
    ("Group Containers", "общий контейнер"),
    ("Saved Application State", "состояние окон"),
    ("HTTPStorages", "cookies и сетевой кэш"),
    ("WebKit", "данные веб‑страниц"),
    ("Cookies", "cookies"),
    ("Logs", "логи"),
    ("LaunchAgents", "автозапуск"),
    ("Application Scripts", "скрипты"),
];

/// Подходит ли файл или папка `entry` программе с этим bundle id или именем.
/// Возвращает длину совпавшего id, чтобы при споре двух программ
/// (`com.foo.app` и `com.foo.app.pro`) хвост достался более точной.
fn match_len(entry: &str, bundle_id: &str, names: &[String], place: &str) -> Option<usize> {
    let lower = entry.to_lowercase();
    let stem = [".plist", ".savedstate", ".binarycookies"]
        .iter()
        .find_map(|ext| lower.strip_suffix(ext))
        .unwrap_or(&lower);
    let id = bundle_id.to_lowercase();
    if stem == id || stem.starts_with(&format!("{id}.")) {
        return Some(id.len());
    }
    // Общие контейнеры называются `TEAMID.com.foo.app` или `group.com.foo.app`.
    if place == "Group Containers" && (stem.ends_with(&format!(".{id}")) || stem.contains(&format!(".{id}."))) {
        return Some(id.len());
    }
    // Некоторые программы пишут в папку со своим названием (Application Support/Telegram Desktop).
    let by_name = matches!(place, "Application Support" | "Caches" | "Logs");
    if by_name && names.iter().any(|n| n.chars().count() >= 4 && n.to_lowercase() == stem) {
        return Some(0);
    }
    None
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uninstall {
    pub app: Item,
    pub leftovers: Vec<Item>,
}

/// Программа и всё, что она оставила в ~/Library.
#[tauri::command]
pub async fn disk_app_leftovers(path: String) -> Result<Uninstall, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app = PathBuf::from(&path);
        let info = read_info(&app).ok_or("Не удалось прочитать Info.plist программы")?;
        let mut names = vec![app.file_stem().unwrap_or_default().to_string_lossy().into_owned()];
        names.extend(info.name.clone());
        // Остальные программы нужны, чтобы не отдать этой чужой хвост.
        let others: Vec<String> =
            find_apps().iter().filter(|p| **p != app).filter_map(|p| read_info(p)).map(|i| i.bundle_id).collect();

        let library = home().join("Library");
        let mut found: Vec<(PathBuf, &str)> = vec![];
        for (place, label) in PLACES {
            let Ok(entries) = std::fs::read_dir(library.join(place)) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(len) = match_len(&name, &info.bundle_id, &names, place) else { continue };
                let stolen = others.iter().any(|o| match_len(&name, o, &[], place).is_some_and(|l| l > len));
                if !stolen {
                    found.push((entry.path(), label));
                }
            }
        }
        let leftovers = found
            .par_iter()
            .map(|(p, label)| {
                let place = p.parent().and_then(|d| d.strip_prefix(&library).ok()).unwrap_or(Path::new(""));
                Item::measure(p, format!("{label} · {}", place.display()), true)
            })
            .collect();
        Ok(Uninstall { app: Item::measure(&app, info.name.unwrap_or_default(), true), leftovers })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spotlight_date() {
        assert_eq!(parse_mdls_date("1970-01-02 00:00:10 +0000"), Some(86_410));
        assert_eq!(parse_mdls_date("2026-10-20 08:12:33 +0000"), Some(1_792_483_953));
        assert_eq!(parse_mdls_date("(null)"), None);
    }

    #[test]
    fn matches_leftovers() {
        let names = vec!["Spotify".to_string()];
        let id = "com.spotify.client";
        assert!(match_len("com.spotify.client", id, &names, "Caches").is_some());
        assert!(match_len("com.spotify.client.plist", id, &names, "Preferences").is_some());
        assert!(match_len("com.spotify.client.savedState", id, &names, "Saved Application State").is_some());
        assert!(match_len("com.spotify.client.ABC-123.plist", id, &names, "Preferences/ByHost").is_some());
        assert!(match_len("Spotify", id, &names, "Application Support").is_some());
        assert!(match_len("2FUY5GN2.group.com.spotify.client", id, &names, "Group Containers").is_some());
        assert!(match_len("com.spotify.clientx", id, &names, "Caches").is_none());
        assert!(match_len("Spotify", id, &names, "Preferences").is_none());
    }
}
